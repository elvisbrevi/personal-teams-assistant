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
        for name, value in {
            "api": self.fake_api,
            "put_worker_secret": lambda environment, name, value: self.worker_secrets.__setitem__((environment, name), value),
            "wait_for_configuration": lambda hostname, seconds=300: {"configured": True},
            "wait_for_admission": lambda hostname, token, seconds=180: None,
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
            yield 401, b"{}"  # a under b
            yield 200, b"{}"  # write
            yield 200, b"{}"  # read back
            yield 404, b"{}"  # b reads a
            yield 200, json.dumps({"records": []}).encode()
            yield 404, b"{}"  # b revokes a
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
