#!/usr/bin/env python3
"""Synthetic checks for Access resource ownership, callback scope and secret handling."""
import argparse
import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("web_access_setup", Path(__file__).with_name("configure-web-access.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)

ACCOUNT = "a" * 32
PROVIDER = "ab388e54-14b8-4593-a48f-0fb263c1a29c"
PROFILE = "f35bce11-8a9b-41f6-b6ac-3396b84dd94c"


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.callbacks = root / "callbacks.json"
        self.paths = [f"/webhooks/{PROFILE}/graph/{kind}" for kind in ["notifications", "lifecycle"]]
        self.callbacks.write_text(json.dumps({"hostname":"assistant.example.com","paths":self.paths}))
        self.args = argparse.Namespace(account_id=ACCOUNT, hostname="assistant.example.com", callbacks_file=self.callbacks,
            owner_github_id=1001, allow_email=["authorized@example.test"], settings_file=root / "settings.json", dry_run=False)
        self.apps = {}
        self.providers = []
        self.calls = []
        self.env = patch.dict(os.environ, {"CLOUDFLARE_API_TOKEN":"synthetic-api-secret",
            "PTA_ACCESS_GITHUB_CLIENT_ID":"synthetic-login-app", "PTA_ACCESS_GITHUB_CLIENT_SECRET":"synthetic-login-secret"}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)

    def fake_api(self, token, method, path, data=None):
        self.calls.append((method, path, data))
        base = f"/accounts/{ACCOUNT}/access"
        if path == base + "/organizations":
            value = {"auth_domain":"synthetic.cloudflareaccess.com"}
        elif path.startswith(base + "/identity_providers?"):
            value = self.providers
        elif path == base + "/identity_providers" and method == "POST":
            value = {"id":PROVIDER, **data}
            self.providers.append(value)
        elif path == base + "/identity_providers/" + PROVIDER:
            value = self.providers[0]
        elif path.startswith(base + "/apps?"):
            value = list(self.apps.values())
        elif path == base + "/apps" and method == "POST":
            value = {"id":f"app-{len(self.apps)}", "aud":"b" * 64, **data}
            self.apps[value["id"]] = value
        else:
            suffix = path.removeprefix(base + "/apps/")
            ident = suffix.split('/')[0]
            if method == "PUT":
                self.apps[ident] = {"id":ident,"aud":"b" * 64,**data}
            value = self.apps[ident]["policies"] if suffix.endswith("/policies") else self.apps[ident]
        return {"success":True,"result":value}

    def run_setup(self):
        output = io.StringIO()
        with patch.object(setup, "api", self.fake_api), contextlib.redirect_stdout(output):
            setup.configure(self.args)
        for secret in ["synthetic-api-secret","synthetic-login-secret"]:
            self.assertNotIn(secret, output.getvalue())
        return output.getvalue()

    def test_exact_exceptions_precede_panel_protection_and_resources_are_reused(self):
        self.run_setup()
        writes = [(method, data) for method, _, data in self.calls if method == "POST"]
        self.assertEqual([data["type"] for _, data in writes], ["github"] + ["self_hosted"] * 9)
        self.assertEqual([data["policies"][0]["decision"] for _, data in writes[1:]], ["bypass"] * 8 + ["allow"])
        self.assertEqual(writes[-2][1]["domain"], self.args.hostname + "/api/machines")
        self.assertEqual(writes[-2][1]["policies"][0]["name"], setup.MACHINE_POLICY)
        # Phones fetch home-screen icons without cookies: each static icon gets its own exact bypass.
        assets = [data for _, data in writes[3:8]]
        self.assertEqual([data["domain"] for data in assets], [self.args.hostname + path for path in setup.PUBLIC_ASSETS])
        self.assertEqual({data["policies"][0]["name"] for data in assets}, {setup.ASSET_POLICY})
        main = writes[-1][1]
        self.assertEqual(main["allowed_idps"], [PROVIDER])
        self.assertTrue(main["auto_redirect_to_identity"])
        self.assertTrue(main["http_only_cookie_attribute"])
        self.assertEqual(main["policies"][0]["require"], [{"login_method":{"id":PROVIDER}}])
        self.assertNotIn({"everyone":{}}, main["policies"][0]["include"])
        settings = json.loads(self.args.settings_file.read_text())
        self.assertEqual(settings["access"]["github_idp_id"], PROVIDER)
        self.assertEqual(settings["owner_github_id"], 1001)
        self.assertEqual(self.args.settings_file.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("secret", self.args.settings_file.read_text())
        for app in self.apps.values():
            app["self_hosted_domains"] = [app["domain"]]
            app["destinations"] = [{"type":"public", "uri":app["domain"]}]
        self.calls.clear()
        self.run_setup()
        self.assertFalse(any(method == "POST" for method, _, _ in self.calls))
        self.assertEqual(len(self.apps), 9)

    def test_managed_oauth_is_opt_in_on_the_panel_application_only(self):
        self.run_setup()
        self.assertTrue(all("oauth_configuration" not in (data or {}) for _, _, data in self.calls))
        self.calls.clear()
        self.args.managed_oauth = True
        self.args.oauth_redirect_uri = ["https://agent.example.test/api/mcp/auth_callback"]
        output = self.run_setup()
        writes = [data for method, _, data in self.calls if method in ("POST", "PUT") and data and data.get("type") == "self_hosted"]
        main = [data for data in writes if data["domain"] == self.args.hostname]
        self.assertEqual(len(main), 1)
        oauth = main[0]["oauth_configuration"]
        self.assertTrue(oauth["enabled"])
        self.assertEqual(oauth["dynamic_client_registration"]["allowed_uris"], ["https://agent.example.test/api/mcp/auth_callback"])
        self.assertTrue(oauth["dynamic_client_registration"]["allow_any_on_loopback"])
        self.assertEqual(oauth["grant"]["access_token_lifetime"], setup.OAUTH_TOKEN_LIFETIME)
        # Same policy and identity provider: OAuth only carries the same login.
        self.assertEqual(main[0]["policies"][0]["require"], [{"login_method":{"id":PROVIDER}}])
        self.assertTrue(all("oauth_configuration" not in data for data in writes if data["domain"] != self.args.hostname))
        self.assertIn("/mcp", output)
        settings = json.loads(self.args.settings_file.read_text())
        self.assertNotIn("oauth", json.dumps(settings))

    def test_managed_oauth_needs_its_readback_and_exact_redirects(self):
        self.args.managed_oauth = True
        self.args.oauth_redirect_uri = []
        def dropped(token, method, path, data=None):
            response = self.fake_api(token, method, path, data)
            value = response["result"]
            if method == "GET" and isinstance(value, dict) and value.get("domain") == self.args.hostname:
                response["result"] = {k: v for k, v in value.items() if k != "oauth_configuration"}
            return response
        with patch.object(setup, "api", dropped), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(setup.SetupError, "managed OAuth"):
                setup.configure(self.args)
        self.assertFalse(self.args.settings_file.exists())
        for uri in ["http://agent.example.test/cb", "https://*.example.test/cb", "https://agent.example.test/cb?x=1", "https://agent.example.test"]:
            self.calls.clear()
            self.args.oauth_redirect_uri = [uri]
            with self.assertRaises(setup.SetupError):
                self.run_setup()
            self.assertFalse(self.calls)
        self.args.managed_oauth = False
        self.args.oauth_redirect_uri = ["https://agent.example.test/cb"]
        with self.assertRaises(setup.SetupError):
            self.run_setup()

    def test_cookie_readback_is_required_before_writing_settings(self):
        def missing_cookie(token, method, path, data=None):
            response = self.fake_api(token, method, path, data)
            value = response["result"]
            if method == "GET" and isinstance(value, dict) and value.get("domain") == self.args.hostname:
                response["result"] = {**value, "http_only_cookie_attribute": False}
            return response
        with patch.object(setup, "api", missing_cookie), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(setup.SetupError, "cookie"):
                setup.configure(self.args)
        self.assertFalse(self.args.settings_file.exists())

    def test_dry_run_has_no_writes_or_secret_output(self):
        self.args.dry_run = True
        value = json.loads(self.run_setup())
        self.assertFalse(value["writes"])
        self.assertFalse(value["dedicated_github_provider_exists"])
        self.assertTrue(all(method == "GET" for method, _, _ in self.calls))
        self.assertFalse(self.args.settings_file.exists())

    def test_owner_and_single_email_are_required_before_cloudflare_changes(self):
        self.args.owner_github_id = 0
        with self.assertRaises(setup.SetupError):
            self.run_setup()
        self.assertFalse(self.calls)
        self.args.owner_github_id = 1001
        self.args.allow_email.append("another@example.test")
        with self.assertRaises(setup.SetupError):
            self.run_setup()
        self.assertFalse(self.calls)

    def test_broad_or_non_graph_exceptions_are_rejected(self):
        for path in ["/webhooks/*", "/webhooks/*/graph/notifications", "/api/control", "/graph/notifications",
            f"/webhooks/{PROFILE}/graph/notifications/extra", f"/webhooks/{PROFILE}/api/control"]:
            self.callbacks.write_text(json.dumps({"hostname":self.args.hostname,"paths":[path]}))
            with self.assertRaises(setup.SetupError):
                setup.callback_paths(self.callbacks,self.args.hostname)
        self.callbacks.write_text(json.dumps({"hostname":"other.example","paths":[]}))
        with self.assertRaises(setup.SetupError):
            setup.callback_paths(self.callbacks,self.args.hostname)

    def test_overlapping_foreign_apps_and_unmanaged_policies_are_preserved(self):
        for domain in [self.args.hostname,"*.example.com",self.args.hostname + "/webhooks/*",self.args.hostname + "/favicon.png"]:
            self.apps = {"foreign":{"id":"foreign","domain":domain,"name":"other application","type":"self_hosted"}}
            self.calls.clear()
            with self.assertRaises(setup.SetupError):
                self.run_setup()
            self.assertTrue(all(method == "GET" for method, _, _ in self.calls))
        self.apps.clear()
        self.run_setup()
        self.calls.clear()
        for app in self.apps.values():
            app["policies"].append({"name":"unmanaged","decision":"allow","include":[{"everyone":{}}]})
        with self.assertRaises(setup.SetupError):
            self.run_setup()
        self.assertTrue(all(method == "GET" for method, _, _ in self.calls))

    def test_missing_login_credentials_never_reuses_repository_oauth(self):
        del os.environ["PTA_ACCESS_GITHUB_CLIENT_SECRET"]
        os.environ["GITHUB_OAUTH_TOKENS"] = "synthetic-repository-credential"
        with self.assertRaises(setup.SetupError):
            self.run_setup()
        self.assertTrue(all(method == "GET" for method, _, _ in self.calls))

    def test_effective_403_reports_operation_and_exact_permission_without_secrets(self):
        for resource, permission in [("apps",setup.APP_WRITE),("identity_providers",setup.IDP_WRITE)]:
            path = f"/accounts/{ACCOUNT}/access/{resource}"
            error = urllib.error.HTTPError("https://api.cloudflare.com",403,"forbidden",{},None)
            with patch.object(setup.urllib.request,"urlopen",side_effect=error):
                with self.assertRaises(setup.SetupError) as caught:
                    setup.api("synthetic-api-secret","POST",path,{"client_secret":"synthetic-login-secret"})
            self.assertIn(permission,str(caught.exception))
            self.assertIn("POST " + path,str(caught.exception))
            self.assertNotIn("synthetic",str(caught.exception))


APP_ID = "0d9f5e2a-7c1b-4f3e-9a2d-6b8c4e1f0a37"
POLICY_ID = "5c2e9b1a-3d4f-4e6a-8b7c-1f0e2d3c4b5a"


class ManagedOAuthOnlyTests(unittest.TestCase):
    """The narrow mode against a double shaped like production: two include rules, one GitHub provider."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.report = Path(self.temp.name) / "private" / "report.json"
        self.hostname = "assistant.example.com"
        self.policy = {"id":POLICY_ID,"name":setup.PANEL_POLICY,"decision":"allow","precedence":1,"reusable":False,
            "include":[{"email":{"email":"owner@example.test"}},{"email":{"email":"second@example.test"}}],
            "require":[{"login_method":{"id":PROVIDER}}],"exclude":[],
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}
        self.panel = {"id":APP_ID,"aud":"c" * 64,"name":setup.APP_NAME,"domain":self.hostname,"type":"self_hosted",
            "self_hosted_domains":[self.hostname],"destinations":[{"type":"public","uri":self.hostname}],
            "session_duration":"12h","allowed_idps":[PROVIDER],"auto_redirect_to_identity":True,
            "http_only_cookie_attribute":True,"app_launcher_visible":False,"policies":[dict(self.policy)],
            "tags":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}
        self.apps = {APP_ID:self.panel,
            "asset":{"id":"asset","name":setup.APP_NAME + " asset favicon.png","domain":self.hostname + "/favicon.png",
                "type":"self_hosted","policies":[]},
            "elsewhere":{"id":"elsewhere","name":"another team app","domain":"other.example.org","type":"self_hosted"}}
        self.calls = []
        self.puts = 0
        self.env = patch.dict(os.environ, {"CLOUDFLARE_API_TOKEN":"synthetic-api-secret"}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)

    def args(self, enable=True, **extra):
        values = dict(account_id=ACCOUNT, hostname=self.hostname, oauth_redirect_uri=None, report=self.report,
            dry_run=False, managed_oauth_only=enable, managed_oauth_off=not enable)
        values.update(extra)
        return argparse.Namespace(**values)

    def fake_api(self, token, method, path, data=None):
        self.calls.append((method, path, json.loads(json.dumps(data)) if data is not None else None))
        base = f"/accounts/{ACCOUNT}/access"
        self.assertFalse(path.startswith(base + "/identity_providers") or path.startswith(base + "/organizations"))
        if path.startswith(base + "/apps?"):
            return {"success":True,"result":json.loads(json.dumps(list(self.apps.values())))}
        suffix = path.removeprefix(base + "/apps/")
        ident = suffix.split('/')[0]
        if method == "PUT":
            # Like Cloudflare's schema: generated fields are not part of the update body.
            for field in setup.READ_ONLY_APP_FIELDS:
                self.assertNotIn(field, data)
            self.puts += 1
            previous = self.apps[ident]
            self.apps[ident] = {"id":ident,"aud":previous["aud"],"created_at":previous["created_at"],
                "updated_at":"2026-11-06T00:00:00Z",**json.loads(json.dumps(data))}
        app = self.apps[ident]
        value = app.get("policies", []) if suffix.endswith("/policies") else app
        return {"success":True,"result":json.loads(json.dumps(value))}

    def run_narrow(self, args, api=None):
        output = io.StringIO()
        with patch.object(setup, "api", api or self.fake_api), contextlib.redirect_stdout(output):
            setup.run_narrow(args)
        self.assertNotIn("synthetic-api-secret", output.getvalue())
        return json.loads(output.getvalue())

    def test_one_update_differs_only_in_oauth_and_preserves_policies_byte_for_byte(self):
        read = json.loads(json.dumps(self.panel))
        policies = json.dumps(read["policies"])
        self.run_narrow(self.args(oauth_redirect_uri=["https://agent.example.test/api/mcp/auth_callback"]))
        puts = [(path, data) for method, path, data in self.calls if method != "GET"]
        self.assertEqual(len(puts), 1)
        path, body = puts[0]
        self.assertEqual(path, f"/accounts/{ACCOUNT}/access/apps/{APP_ID}")
        expected = {key: value for key, value in read.items() if key not in setup.READ_ONLY_APP_FIELDS}
        self.assertEqual({k: v for k, v in body.items() if k != "oauth_configuration"},
            {k: v for k, v in expected.items() if k != "oauth_configuration"})
        self.assertEqual(json.dumps(body["policies"]), policies)
        self.assertEqual(body["oauth_configuration"], {"enabled":True,"dynamic_client_registration":{"enabled":True,
            "allow_any_on_localhost":True,"allow_any_on_loopback":True,
            "allowed_uris":["https://agent.example.test/api/mcp/auth_callback"]}})
        report = json.loads(self.report.read_text())
        self.assertEqual(self.report.stat().st_mode & 0o777, 0o600)
        self.assertTrue(report["ok"])
        self.assertTrue(report["writes"])
        self.assertEqual(report["mcp_endpoint"], "https://assistant.example.com/mcp")
        self.assertEqual(list(report["changes"]), ["oauth_configuration"])
        self.assertTrue(report["readback"]["policies_preserved"])
        self.assertEqual(report["read"]["policies"]["count"], 1)
        self.assertEqual(report["read"]["policies"]["digest"], report["readback"]["policies"]["digest"])
        # Secret-free and without the people the include rules name.
        for text in ["synthetic-api-secret", "owner@example.test", "second@example.test"]:
            self.assertNotIn(text, self.report.read_text())

    def test_production_state_keeps_grant_as_read_and_a_second_run_writes_nothing(self):
        self.panel["oauth_configuration"] = {"enabled":False,"grant":{"session_duration":"12h"}}
        self.run_narrow(self.args())
        oauth = self.apps[APP_ID]["oauth_configuration"]
        self.assertEqual(oauth["grant"], {"session_duration":"12h"})
        self.assertEqual(oauth["dynamic_client_registration"]["allowed_uris"], [])
        self.calls.clear()
        report = self.run_narrow(self.args())
        self.assertEqual(self.puts, 1)
        self.assertFalse(report["writes"])
        self.assertEqual(report["changes"], {})
        self.assertTrue(report["managed_oauth_enabled"])

    def test_off_reverses_only_enabled_and_is_a_no_op_when_already_off(self):
        self.run_narrow(self.args())
        policies = json.dumps(self.apps[APP_ID]["policies"])
        self.run_narrow(self.args(enable=False))
        oauth = self.apps[APP_ID]["oauth_configuration"]
        self.assertFalse(oauth["enabled"])
        self.assertTrue(oauth["dynamic_client_registration"]["allow_any_on_loopback"])
        self.assertEqual(json.dumps(self.apps[APP_ID]["policies"]), policies)
        self.assertEqual(self.puts, 2)
        report = self.run_narrow(self.args(enable=False))
        self.assertEqual(self.puts, 2)
        self.assertFalse(report["managed_oauth_enabled"])
        with self.assertRaises(setup.SetupError):
            self.run_narrow(self.args(enable=False, oauth_redirect_uri=["https://agent.example.test/cb"]))

    def test_dry_run_reads_only(self):
        report = self.run_narrow(self.args(dry_run=True))
        self.assertTrue(all(method == "GET" for method, _, _ in self.calls))
        self.assertFalse(report["writes"])
        self.assertTrue(report["dry_run"])
        self.assertIn("oauth_configuration", report["changes"])
        self.assertTrue(json.loads(self.report.read_text())["ok"])

    def test_readback_that_differs_fails_and_the_report_is_not_a_success(self):
        changes = [
            lambda app: {**app, "policies":[{**app["policies"][0], "include":app["policies"][0]["include"][:1]}]},
            lambda app: {**app, "allowed_idps":[]},
            lambda app: {**app, "aud":"d" * 64},
            lambda app: {k: v for k, v in app.items() if k != "oauth_configuration"},
        ]
        for change in changes:
            self.setUp()
            def drifted(token, method, path, data=None, change=change):
                response = self.fake_api(token, method, path, data)
                if self.puts and method == "GET" and path.endswith(APP_ID):
                    response["result"] = change(response["result"])
                if self.puts and method == "GET" and path.endswith("/policies"):
                    response["result"] = change({"policies":response["result"]}).get("policies", [])
                return response
            with self.assertRaises(setup.SetupError):
                self.run_narrow(self.args(), api=drifted)
            self.assertEqual(self.puts, 1)
            report = json.loads(self.report.read_text())
            self.assertFalse(report["ok"])
            self.assertIn("error", report)

    def test_missing_duplicated_foreign_or_unmanaged_applications_are_refused_before_any_write(self):
        cases = [
            lambda: self.apps.pop(APP_ID),
            lambda: self.apps.update({"copy":{**self.panel, "id":"copy"}}),
            lambda: self.panel.update({"type":"bookmark"}),
            lambda: self.apps.update({"foreign":{"id":"foreign","name":"other application","domain":"*.example.com","type":"self_hosted"}}),
            lambda: self.apps.update({"foreign":{"id":"foreign","name":"other application","domain":self.hostname + "/api/*","type":"self_hosted"}}),
            lambda: self.panel["policies"].append({"id":"other","name":"unmanaged","decision":"allow","include":[{"everyone":{}}]}),
            lambda: self.panel.update({"self_hosted_domains":[self.hostname, "extra.example.com"]}),
        ]
        for case in cases:
            self.setUp()
            case()
            with self.assertRaises(setup.SetupError):
                self.run_narrow(self.args())
            self.assertTrue(all(method == "GET" for method, _, _ in self.calls))
            self.assertFalse(json.loads(self.report.read_text())["ok"])

    def test_cli_keeps_narrow_and_full_modes_apart(self):
        script = str(Path(__file__).with_name("configure-web-access.py"))
        base = ["--hostname", self.hostname, "--dry-run"]
        for extra in [["--managed-oauth-only", "--managed-oauth-off"], ["--managed-oauth-only", "--allow-email", "a@example.test"],
                ["--managed-oauth-off", "--callbacks-file", "x"], ["--managed-oauth-only", "--managed-oauth"],
                ["--report", "x"], []]:
            with patch.object(sys, "argv", [script] + base + extra), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as caught:
                    setup.main()
            self.assertEqual(caught.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
