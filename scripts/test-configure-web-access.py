#!/usr/bin/env python3
"""Synthetic checks for Access resource ownership, callback scope and secret handling."""
import argparse
import contextlib
import importlib.util
import io
import json
import os
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
        self.callbacks.write_text(json.dumps({"hostname":"assistant.elvisbrevi.cl","paths":self.paths}))
        self.args = argparse.Namespace(account_id=ACCOUNT, hostname="assistant.elvisbrevi.cl", callbacks_file=self.callbacks,
            allow_email=["authorized@example.test"], settings_file=root / "settings.json", dry_run=False)
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
        self.assertEqual([data["type"] for _, data in writes], ["github","self_hosted","self_hosted","self_hosted"])
        self.assertEqual([data["policies"][0]["decision"] for _, data in writes[1:]], ["bypass","bypass","allow"])
        main = writes[-1][1]
        self.assertEqual(main["allowed_idps"], [PROVIDER])
        self.assertTrue(main["auto_redirect_to_identity"])
        self.assertEqual(main["policies"][0]["require"], [{"login_method":{"id":PROVIDER}}])
        self.assertNotIn({"everyone":{}}, main["policies"][0]["include"])
        settings = json.loads(self.args.settings_file.read_text())
        self.assertEqual(settings["access"]["github_idp_id"], PROVIDER)
        self.assertEqual(self.args.settings_file.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("secret", self.args.settings_file.read_text())
        self.calls.clear()
        self.run_setup()
        self.assertFalse(any(method == "POST" for method, _, _ in self.calls))
        self.assertEqual(len(self.apps), 3)

    def test_dry_run_has_no_writes_or_secret_output(self):
        self.args.dry_run = True
        value = json.loads(self.run_setup())
        self.assertFalse(value["writes"])
        self.assertFalse(value["dedicated_github_provider_exists"])
        self.assertTrue(all(method == "GET" for method, _, _ in self.calls))
        self.assertFalse(self.args.settings_file.exists())

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
        for domain in [self.args.hostname,"*.elvisbrevi.cl",self.args.hostname + "/webhooks/*"]:
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


if __name__ == "__main__":
    unittest.main()
