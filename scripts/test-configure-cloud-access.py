#!/usr/bin/env python3
"""Synthetic checks for the cloud Access setup: managed resources only, exact policies, no secrets in reports."""
import argparse
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("cloud_access_setup", Path(__file__).with_name("configure-cloud-access.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)

ACCOUNT = "a" * 32
API_SECRET = "synthetic-api-secret"


class CloudAccessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.report = Path(self.temp.name) / "report.json"
        self.apps = {}
        self.policies = {}
        self.tokens = {}
        self.calls = []
        self.worker_secrets = {}
        env = patch.dict(os.environ, {"CLOUDFLARE_API_TOKEN": API_SECRET}, clear=True)
        env.start()
        self.addCleanup(env.stop)
        self.real_put_worker_secret = setup.put_worker_secret
        for name, value in {
            "api": self.fake_api,
            "put_worker_secret": lambda environment, name, value: self.worker_secrets.__setitem__((environment, name), value),
            "wait_for_configuration": lambda hostname, seconds=300: {"configured": True, "features": ["cas", "erase", "origin", "rebind", "plan", "versions"]},
            "wait_for_admission": lambda hostname, token, seconds=180: None,
            "wait_for_refusal": lambda hostname, token, seconds=180: True,
        }.items():
            patcher = patch.object(setup, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def fake_api(self, token, method, path, data=None):
        self.assertEqual(token, API_SECRET)
        self.calls.append((method, path.split("?")[0], data))
        base = f"/accounts/{ACCOUNT}/access"
        path = path.split("?")[0]
        if path == "/accounts":
            value = [{"id": ACCOUNT}]
        elif path == base + "/organizations":
            value = {"auth_domain": "synthetic.cloudflareaccess.com"}
        elif path == base + "/service_tokens" and method == "GET":
            value = list(self.tokens.values())
        elif path == base + "/service_tokens" and method == "POST":
            ident = f"token-{len(self.tokens)}"
            value = {"id": ident, "name": data["name"], "client_id": f"{ident}.access", "client_secret": f"secret-of-{ident}"}
            self.tokens[ident] = {"id": ident, "name": data["name"], "client_id": value["client_id"]}
        elif path.startswith(base + "/service_tokens/") and path.endswith("/rotate") and method == "POST":
            ident = path.split("/")[-2]
            self.assertEqual(data, {})  # the previous secret is revoked at once
            self.rotations = getattr(self, "rotations", 0) + 1
            value = {**self.tokens[ident], "client_secret": f"secret-of-{ident}-rotated-{self.rotations}",
                     "expires_at": "2027-11-03T00:00:00Z"}
        elif path.startswith(base + "/service_tokens/") and method == "DELETE":
            value = self.tokens.pop(path.rsplit("/", 1)[1])
        elif path == base + "/apps" and method == "GET":
            value = list(self.apps.values())
        elif path == base + "/apps" and method == "POST":
            ident = f"app-{len(self.apps)}"
            self.apps[ident] = {"id": ident, "aud": "b" * 64, **{k: v for k, v in data.items() if k != "policies"}}
            self.policies[ident] = data["policies"]
            value = self.apps[ident]
        else:
            suffix = path.removeprefix(base + "/apps/")
            ident = suffix.split("/")[0]
            if suffix.endswith("/policies"):
                value = self.policies[ident]
            else:
                if method == "PUT":
                    self.apps[ident] = {"id": ident, "aud": "b" * 64, **{k: v for k, v in data.items() if k != "policies"}}
                    self.policies[ident] = data["policies"]
                value = self.apps[ident]
        return {"success": True, "result": value}

    def run_setup(self, environment, **extra):
        extra.setdefault("portal_user", None)
        extra.setdefault("rotate", False)
        extra.setdefault("retire", False)
        args = argparse.Namespace(environment=environment, report=self.report, label="server", **extra)
        code = setup.run(args)
        report = self.report.read_text()
        self.assertNotIn(API_SECRET, report)
        self.assertNotIn("secret-of-", report)
        self.assertEqual(self.report.stat().st_mode & 0o777, 0o600)
        return code, json.loads(report)

    def app(self, domain):
        return next(app for app in self.apps.values() if app["domain"] == domain)

    def test_production_protects_the_hostname_and_connects_with_stdin_only(self):
        runs = []

        def fake_run(command, **kwargs):
            runs.append((command, kwargs.get("input")))
            if command[-2:] == ["cloud", "status"]:
                return setup.subprocess.CompletedProcess(command, 0, json.dumps({"data": {"enrolled": False}}), "")
            return setup.subprocess.CompletedProcess(command, 0, json.dumps({"code": "ok", "data": {"installation": "i-1"}}), "")

        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production")
        self.assertEqual(code, 0, report)
        _, hostname = setup.worker_settings("production")
        health = self.app(hostname + "/v1/health")
        self.assertEqual(self.policies[health["id"]][0]["decision"], "bypass")
        main = self.app(hostname)
        policy = self.policies[main["id"]][0]
        self.assertEqual(policy["decision"], "non_identity")
        self.assertEqual(policy["include"], [{"service_token": {"token_id": "token-0"}}])
        self.assertNotIn({"everyone": {}}, policy["include"])
        # The exact public exception exists before the hostname is protected.
        created = [data["domain"] for method, _, data in self.calls if method == "POST" and data and "domain" in data]
        self.assertEqual(created, [hostname + "/v1/health", hostname])
        connect = [entry for entry in runs if "connect" in entry[0]]
        self.assertEqual(len(connect), 1)
        command, stdin = connect[0]
        self.assertNotIn("secret-of-token-0", " ".join(command))
        self.assertEqual(json.loads(stdin), {"client_id": "token-0.access", "client_secret": "secret-of-token-0"})
        self.assertEqual(self.worker_secrets[("production", "ACCESS_AUD")], "b" * 64)
        self.assertEqual(self.worker_secrets[("production", "ACCESS_TEAM_DOMAIN")], "https://synthetic.cloudflareaccess.com")
        self.assertEqual(report["connect"]["installation"], "i-1")

    def test_wrangler_never_receives_the_access_api_token(self):
        seen = []

        def fake_run(command, **kwargs):
            seen.append(kwargs.get("env"))
            return setup.subprocess.CompletedProcess(command, 0, "", "")

        with patch.object(setup, "put_worker_secret", self.real_put_worker_secret), patch.object(setup, "wrangler", lambda: ["wrangler"]), \
                patch.object(setup.subprocess, "run", fake_run):
            setup.put_worker_secret("staging", "ACCESS_AUD", "synthetic-audience")
        self.assertEqual(len(seen), 1)
        self.assertNotIn("CLOUDFLARE_API_TOKEN", seen[0])
        self.assertEqual(os.environ["CLOUDFLARE_API_TOKEN"], API_SECRET)

    def test_a_portal_account_gets_its_own_token_through_stdin_only(self):
        runs = []

        def fake_run(command, **kwargs):
            runs.append((command, kwargs.get("input")))
            return setup.subprocess.CompletedProcess(command, 0, json.dumps({"code": "ok", "data": {"enabled": True}}), "")

        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", portal_user="synthetic-user")
        self.assertEqual(code, 0, report)
        self.assertEqual(len(runs), 1)
        command, stdin = runs[0]
        self.assertEqual(command[1:7], ["--json", "web", "users", "cloud", "enable", "synthetic-user"])
        self.assertNotIn("secret-of-token-0", " ".join(command))
        self.assertEqual(json.loads(stdin), {"client_id": "token-0.access", "client_secret": "secret-of-token-0"})
        self.assertIn("--token-expires", command)
        self.assertEqual(self.tokens["token-0"]["name"], "pta-cloud production portal synthetic-user")
        self.assertFalse(report["portal_user"]["replaced"])

    def test_rotating_keeps_the_client_id_and_hands_the_secret_through_stdin_only(self):
        self.tokens["token-0"] = {"id": "token-0", "name": "pta-cloud production server", "client_id": "token-0.access"}
        self.tokens["token-1"] = {"id": "token-1", "name": "pta-cloud production portal synthetic-user", "client_id": "token-1.access"}
        runs = []

        def fake_run(command, **kwargs):
            runs.append((command, kwargs.get("input")))
            return setup.subprocess.CompletedProcess(command, 0, json.dumps({"code": "ok", "data": {}}), "")

        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", rotate=True)
        self.assertEqual(code, 0, report)
        command, stdin = runs[0]
        self.assertEqual(command[1:4], ["--json", "cloud", "connect"])
        self.assertEqual(command[-2:], ["--token-expires", "2027-11-03"])
        self.assertNotIn("rotated", " ".join(command))
        self.assertEqual(json.loads(stdin), {"client_id": "token-0.access", "client_secret": "secret-of-token-0-rotated-1"})
        # Only that token was rotated; nothing was created or deleted.
        self.assertEqual(set(self.tokens), {"token-0", "token-1"})
        self.assertFalse([call for call in self.calls if call[0] == "DELETE" or call[1].endswith("/service_tokens") and call[0] == "POST"])
        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", rotate=True, portal_user="synthetic-user")
        self.assertEqual(code, 0, report)
        command, stdin = runs[1]
        self.assertEqual(command[1:7], ["--json", "web", "users", "cloud", "enable", "synthetic-user"])
        self.assertEqual(json.loads(stdin)["client_id"], "token-1.access")

    def test_retiring_a_portal_account_deletes_only_its_token_after_it_was_disabled(self):
        self.tokens["token-0"] = {"id": "token-0", "name": "pta-cloud production server", "client_id": "token-0.access"}
        self.tokens["token-1"] = {"id": "token-1", "name": "pta-cloud production portal synthetic-user", "client_id": "token-1.access"}
        setup.Access(API_SECRET, ACCOUNT, "production").configure({"token-0", "token-1"})
        enabled = {"value": True}

        def fake_run(command, **kwargs):
            self.assertEqual(command[1:], ["--json", "web", "users", "list"])
            self.assertIsNone(kwargs.get("input"))
            return setup.subprocess.CompletedProcess(command, 0, json.dumps({"code": "ok", "data": [
                {"username": "synthetic-user", "cloud_enabled": enabled["value"]}]}), "")

        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", portal_user="synthetic-user", retire=True)
        self.assertEqual(code, 1)
        self.assertIn("still enabled", report["error"])
        self.assertEqual(set(self.tokens), {"token-0", "token-1"})
        enabled["value"] = False
        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", portal_user="synthetic-user", retire=True)
        self.assertEqual(code, 0, report)
        self.assertEqual(set(self.tokens), {"token-0"})
        self.assertEqual(report["retire"], {"portal_user": "synthetic-user", "deleted": 1})
        _, hostname = setup.worker_settings("production")
        policy = self.policies[self.app(hostname)["id"]][0]
        self.assertEqual(policy["include"], [{"service_token": {"token_id": "token-0"}}])
        # Repeating is safe: nothing is left to delete.
        with patch.object(setup.subprocess, "run", fake_run):
            code, report = self.run_setup("production", portal_user="synthetic-user", retire=True)
        self.assertEqual((code, report["retire"]["deleted"]), (0, 0))

    def test_rotating_without_a_token_of_the_account_changes_nothing(self):
        code, report = self.run_setup("production", rotate=True)
        self.assertEqual(code, 1)
        self.assertIn("no single service token", report["error"])
        self.assertFalse([call for call in self.calls if call[1].endswith("/rotate")])

    def test_unmanaged_applications_on_the_hostname_are_never_changed(self):
        _, hostname = setup.worker_settings("staging")
        self.apps["foreign"] = {"id": "foreign", "name": "someone else", "domain": hostname, "type": "self_hosted"}
        self.policies["foreign"] = []
        code, report = self.run_setup("staging")
        self.assertEqual(code, 1)
        self.assertIn("unmanaged", report["error"])
        self.assertFalse([call for call in self.calls if call[0] in ("POST", "PUT", "DELETE")])

    def test_staging_verification_removes_its_temporary_tokens(self):
        responses = iter([])

        def fake_request(url, method="GET", headers=None, body=None):
            return next(responses)

        def scripted():
            yield 401, b""  # no service token
            for name in ("a", "b"):
                yield 201, json.dumps({"installation": f"inst-{name}", "token": f"ptac_{name}"}).encode()
            yield 200, json.dumps({"installation": {"id": "inst-a"}}).encode()
            yield 409, json.dumps({"error": {"code": "outdated"}}).encode()  # an old host
            yield 200, b"{}"  # an updated host
            yield 200, json.dumps({"installations": [{"id": "inst-a", "host_version": "0.99.1"}]}).encode()
            yield 401, b"{}"  # a under b
            yield 200, b"{}"  # write
            yield 200, b"{}"  # read back
            yield 404, b"{}"  # b reads a
            yield 200, json.dumps({"records": []}).encode()
            for outcome in ("conflict", "updated", "conflict"):  # compare-and-swap
                yield 200, json.dumps({"results": [{"result": outcome}]}).encode()
            yield 200, b"{}"  # b erases its own store
            yield 200, b"{}"  # a's record survives
            yield 201, json.dumps({"installation": "inst-a2", "token": "ptac_a2"}).encode()
            yield 200, json.dumps({"records": [{"own": True}]}).encode()  # a lists its record
            yield 200, json.dumps({"records": [{"own": False}]}).encode()  # a2 lists it
            yield 409, b"{}"  # a cannot erase with a2 active
            yield 200, b"{}"  # a2 revokes itself
            yield 200, json.dumps({"erased": 1, "backups": 0, "more": False}).encode()
            yield 404, b"{}"  # erased record
            yield 404, b"{}"  # b revokes a
            yield 200, json.dumps({"rebound": True, "installation": "inst-a"}).encode()  # a moves to c
            yield 401, b"{}"  # a under its former token
            yield 200, b"{}"  # a under c
            yield 409, b"{}"  # b cannot take c
            yield 200, json.dumps({"plan": {"accounts": 2, "oversubscribed": False}}).encode()
            yield 200, json.dumps({"installation": {"id": "inst-a"}}).encode()  # a under c's rotated secret
            yield 200, json.dumps({"rebound": False, "installation": "inst-a"}).encode()  # no rebinding needed
            yield 200, b"{}"
            yield 200, b"{}"
            yield 401, b"{}"  # revoked

        responses = scripted()
        with patch.object(setup, "request", fake_request):
            code, report = self.run_setup("staging")
        self.assertEqual(code, 0, report)
        self.assertTrue(report["verified"])
        self.assertTrue(all(check["passed"] for check in report["checks"]))
        self.assertEqual(self.tokens, {})
        _, hostname = setup.worker_settings("staging")
        self.assertEqual(self.policies[self.app(hostname)["id"]], [])

    def test_failed_verification_still_cleans_up(self):
        def failing(url, method="GET", headers=None, body=None):
            return 500, b""

        with patch.object(setup, "request", failing):
            code, report = self.run_setup("staging")
        self.assertEqual(code, 1)
        self.assertFalse(report["ok"])
        self.assertEqual(self.tokens, {})

    def test_a_named_account_needs_no_account_listing(self):
        with patch.dict(os.environ, {"CLOUDFLARE_ACCOUNT_ID": ACCOUNT}):
            self.assertEqual(setup.cloudflare_account(API_SECRET), ACCOUNT)
        self.assertFalse([call for call in self.calls if call[1] == "/accounts"])
        with patch.dict(os.environ, {"CLOUDFLARE_ACCOUNT_ID": "not-an-id"}):
            with self.assertRaises(setup.SetupError):
                setup.cloudflare_account(API_SECRET)

    def test_hostnames_come_from_the_wrangler_configuration(self):
        production = setup.worker_settings("production")
        staging = setup.worker_settings("staging")
        self.assertNotEqual(production, staging)
        self.assertTrue(staging[0].endswith("-staging"))


if __name__ == "__main__":
    unittest.main()
