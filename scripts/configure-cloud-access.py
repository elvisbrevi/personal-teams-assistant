#!/usr/bin/env python3
"""Configure Cloudflare Access for the account store Worker (cloud/) and enroll this installation.

Run it with CLOUDFLARE_API_TOKEN (or CLOUDFLARE_API_TOKEN_FILE, a private file) holding a token
with the account permissions Access: Apps and Policies Write, Access: Service Tokens Write and
Access: Organizations Read, and CLOUDFLARE_ACCOUNT_ID when the token cannot list accounts. Hostnames
and Worker names come from cloud/wrangler.jsonc. Every run writes a report without secrets to
--report.

  staging     Access applications and Worker secrets for the staging Worker, then an end-to-end check
              with two temporary service tokens (enrollment, cross-account refusals, revocation);
              the temporary tokens are removed afterwards.
  production  Access applications and Worker secrets for the production Worker, then one service
              token for this account, whose client ID and secret go only to `pta cloud connect` on
              standard input.

Each hostname gets a self-hosted application whose only policy is Service Auth for the listed service
tokens, plus an exact Bypass application for /v1/health. Applications this script did not create are
never changed. Worker secrets (ACCESS_TEAM_DOMAIN, ACCESS_AUD) are set with the maintainer's wrangler
login.
"""
import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLOUD = ROOT / "cloud"
APP_PREFIX = "pta-cloud"
SERVICE_POLICY = "Assistant installations"
HEALTH_POLICY = "Public health only"
TOKEN_DURATION = "8760h"
PERMISSIONS = {
    "/service_tokens": "Access: Service Tokens Write",
    "/organizations": "Access: Organizations Read",
}
APP_WRITE = "Access: Apps and Policies Write"


class SetupError(Exception):
    pass


def secret(name):
    file = os.environ.get(name + "_FILE")
    if file:
        path = Path(file)
        if path.stat().st_mode & 0o077:
            raise SetupError(f"{name}_FILE must be private (0600).")
        value = path.read_text().strip()
    else:
        value = os.environ.get(name, "").strip()
    if not value:
        raise SetupError(f"{name} is missing; pass it through an approved launcher, never chat.")
    return value


def api(token, method, path, data=None):
    permission = next((value for key, value in PERMISSIONS.items() if key in path), APP_WRITE)
    request = urllib.request.Request(
        "https://api.cloudflare.com/client/v4" + path,
        data=json.dumps(data).encode() if data is not None else None,
        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=25) as response:
            body = response.read(1_000_001)
    except urllib.error.HTTPError as error:
        if error.code == 403:
            raise SetupError(f"Cloudflare rejected {method} {strip_ids(path)} (403). Required permission: {permission}.") from None
        raise SetupError(f"Cloudflare rejected {method} {strip_ids(path)} (HTTP {error.code}).") from None
    except (urllib.error.URLError, TimeoutError):
        raise SetupError("Cloudflare API is unavailable through the configured network.") from None
    if len(body) > 1_000_000:
        raise SetupError("Cloudflare response exceeded the size limit.")
    result = json.loads(body)
    if not result.get("success"):
        raise SetupError(f"Cloudflare did not accept {method} {strip_ids(path)}.")
    return result


def strip_ids(path):
    return re.sub(r"[0-9a-f]{32}|[0-9a-f-]{36}", "<id>", path.split("?")[0])


def result(token, method, path, data=None):
    return api(token, method, path, data)["result"]


def listing(token, path):
    items = []
    for page in range(1, 21):
        body = api(token, "GET", f"{path}?page={page}&per_page=100")
        items.extend(body["result"] or [])
        info = body.get("result_info") or {}
        if not info or page >= info.get("total_pages", 1):
            return items
    raise SetupError("Access resource listing exceeded the page limit.")


def worker_settings(environment):
    """Worker name and hostname of an environment, read from cloud/wrangler.jsonc."""
    text = (CLOUD / "wrangler.jsonc").read_text()
    # JSONC: drop comment lines; the file keeps comments on their own lines.
    config = json.loads("\n".join(line for line in text.splitlines() if not line.lstrip().startswith("//")))
    section = config if environment == "production" else config["env"][environment]
    routes = section["routes"]
    if len(routes) != 1 or not routes[0].get("custom_domain"):
        raise SetupError("Each Worker environment must have exactly one custom domain.")
    hostname = routes[0]["pattern"]
    if not re.fullmatch(r"[a-z0-9-]+(?:\.[a-z0-9-]+)+", hostname):
        raise SetupError("Invalid Worker hostname in cloud/wrangler.jsonc.")
    return section["name"], hostname


def cloudflare_account(token):
    # A token limited to Access permissions cannot list accounts: name the account instead.
    named = os.environ.get("CLOUDFLARE_ACCOUNT_ID", "").strip()
    if named:
        if not re.fullmatch(r"[0-9a-f]{32}", named):
            raise SetupError("CLOUDFLARE_ACCOUNT_ID must be the 32-character account ID.")
        return named
    accounts = result(token, "GET", "/accounts")
    if len(accounts) != 1 or not re.fullmatch(r"[0-9a-f]{32}", accounts[0].get("id", "")):
        raise SetupError("The API token must reach exactly one Cloudflare account.")
    return accounts[0]["id"]


def domains(app):
    values = [app.get("domain")] + (app.get("self_hosted_domains") or [])
    values += [d.get("uri") or d.get("hostname") for d in (app.get("destinations") or []) if d.get("type") == "public"]
    return list(dict.fromkeys(value for value in values if value))


class Access:
    def __init__(self, token, account, environment):
        self.token = token
        self.base = f"/accounts/{account}/access"
        self.environment = environment
        self.worker, self.hostname = worker_settings(environment)

    def team_domain(self):
        domain = result(self.token, "GET", self.base + "/organizations")["auth_domain"]
        if not re.fullmatch(r"[a-z0-9-]+\.cloudflareaccess\.com", domain):
            raise SetupError("Unexpected Cloudflare team domain.")
        return domain

    def tokens(self):
        prefix = f"{APP_PREFIX} {self.environment} "
        return [t for t in listing(self.token, self.base + "/service_tokens") if t.get("name", "").startswith(prefix)]

    def apps(self):
        """The managed applications on this hostname; refuses overlapping unmanaged ones."""
        wanted = {self.hostname: self.app_name(), self.hostname + "/v1/health": self.app_name() + " health"}
        found = {}
        for app in listing(self.token, self.base + "/apps"):
            for domain in domains(app):
                if domain.split("/")[0] != self.hostname:
                    continue
                if domain not in wanted or app.get("name") != wanted[domain] or app.get("type") != "self_hosted":
                    raise SetupError("An overlapping or unmanaged Access application exists on the Worker hostname.")
                found[domain] = app
        return found

    def app_name(self):
        return f"{APP_PREFIX} {self.environment}"

    def service_policy(self, token_ids):
        return {"name": SERVICE_POLICY, "decision": "non_identity",
                "include": [{"service_token": {"token_id": ident}} for ident in sorted(token_ids)]}

    def configure(self, token_ids):
        """Create or update both applications; returns the protected application's audience."""
        existing = self.apps()
        for domain, app in existing.items():
            policies = result(self.token, "GET", f"{self.base}/apps/{app['id']}/policies")
            expected = HEALTH_POLICY if domain.endswith("/v1/health") else SERVICE_POLICY
            if any(policy.get("name") != expected for policy in policies):
                raise SetupError("A managed application has unmanaged policies; inspect it before changing Access.")
        # The exact public exception first, then the protected hostname.
        health = {"name": self.app_name() + " health", "domain": self.hostname + "/v1/health", "type": "self_hosted",
                  "app_launcher_visible": False,
                  "policies": [{"name": HEALTH_POLICY, "decision": "bypass", "include": [{"everyone": {}}]}]}
        self.upsert(existing.get(health["domain"]), health)
        if not token_ids:
            # An include list cannot be empty; until a token exists nothing is admitted.
            token_ids = []
        main = {"name": self.app_name(), "domain": self.hostname, "type": "self_hosted", "app_launcher_visible": False,
                "session_duration": "24h", "service_auth_401_redirect": True,
                "policies": [self.service_policy(token_ids)] if token_ids else []}
        app = self.upsert(existing.get(self.hostname), main)
        if not re.fullmatch(r"[0-9a-f]{64}", app.get("aud", "")):
            raise SetupError("Cloudflare returned an invalid application audience.")
        return app["aud"]

    def upsert(self, previous, definition):
        if previous:
            return result(self.token, "PUT", f"{self.base}/apps/{previous['id']}", definition)
        return result(self.token, "POST", self.base + "/apps", definition)

    def admitted(self):
        app = self.apps().get(self.hostname)
        if not app:
            return set()
        ids = set()
        for policy in result(self.token, "GET", f"{self.base}/apps/{app['id']}/policies"):
            for rule in policy.get("include") or []:
                ident = (rule.get("service_token") or {}).get("token_id")
                if ident:
                    ids.add(ident)
        return ids

    def create_token(self, label):
        name = f"{APP_PREFIX} {self.environment} {label}"
        created = result(self.token, "POST", self.base + "/service_tokens", {"name": name, "duration": TOKEN_DURATION})
        if not created.get("id") or not created.get("client_id") or not created.get("client_secret"):
            raise SetupError("Cloudflare returned an incomplete service token.")
        return created

    def delete_token(self, ident):
        result(self.token, "DELETE", f"{self.base}/service_tokens/{ident}")


def wrangler():
    local = CLOUD / "node_modules" / ".bin" / "wrangler"
    if local.exists():
        return [str(local)]
    npx = shutil.which("npx")
    if not npx:
        raise SetupError("wrangler is unavailable: run `bun install` in cloud/.")
    return [npx, "--yes", "wrangler@4.151.0"]


def put_worker_secret(environment, name, value):
    command = wrangler() + ["secret", "put", name]
    if environment != "production":
        command += ["--env", environment]
    completed = subprocess.run(command, input=value, text=True, cwd=CLOUD, capture_output=True, timeout=120)
    if completed.returncode != 0:
        raise SetupError(f"wrangler could not set the Worker secret {name}; check the maintainer's wrangler login.")


def configure_worker(access, token_ids):
    audience = access.configure(token_ids)
    team = access.team_domain()
    put_worker_secret(access.environment, "ACCESS_TEAM_DOMAIN", "https://" + team)
    put_worker_secret(access.environment, "ACCESS_AUD", audience)
    return team


def request(url, method="GET", headers=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    # Cloudflare's browser integrity check refuses urllib's default User-Agent (error 1010).
    headers = {"User-Agent": "personal-teams-assistant-setup", **(headers or {})}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=25) as response:
            return response.status, response.read(1_000_000)
    except urllib.error.HTTPError as error:
        return error.code, error.read(1_000_000)
    except (urllib.error.URLError, TimeoutError):
        return 0, b""


def edge_headers(token):
    return {"CF-Access-Client-Id": token["client_id"], "CF-Access-Client-Secret": token["client_secret"]}


def wait_for_admission(hostname, token, seconds=180):
    """Access policy changes take a while to reach the edge: wait until the Worker itself answers."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        status, body = request(f"https://{hostname}/v1/whoami", headers=edge_headers(token))
        # The Worker answers 401 JSON for a missing installation credential; Access answers 403 or a redirect.
        if status == 401 and body.startswith(b"{"):
            if b"installation credential" in body:
                return
        time.sleep(5)
    raise SetupError("The service token was not admitted by Access in time.")


def wait_for_configuration(hostname, seconds=300):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        status, body = request(f"https://{hostname}/v1/health")
        if status == 200 and json.loads(body).get("configured") is True:
            return json.loads(body)
        time.sleep(5)
    raise SetupError("The Worker does not report Access as configured.")


def check(condition, message, checks):
    checks.append({"check": message, "passed": bool(condition)})
    if not condition:
        raise SetupError("Check failed: " + message)


def verify(access, report):
    """End-to-end check of the staging Worker with two temporary service tokens."""
    run = secrets.token_hex(4)
    first = access.create_token(f"verify {run} a")
    second = access.create_token(f"verify {run} b")
    temporary = [first["id"], second["id"]]
    checks = report.setdefault("checks", [])
    try:
        configure_worker(access, access.admitted() | set(temporary))
        health = wait_for_configuration(access.hostname)
        report["health"] = health
        base = f"https://{access.hostname}"
        status, _ = request(base + "/v1/whoami")
        check(status in (302, 401, 403), "a request without a service token is refused", checks)
        wait_for_admission(access.hostname, first)
        wait_for_admission(access.hostname, second)
        enrolled = {}
        for name, token in (("a", first), ("b", second)):
            status, body = request(base + "/v1/enroll", "POST", edge_headers(token),
                                   {"label": f"verification {name}", "host_version": "0.0.0"})
            check(status == 201, f"installation {name} enrolls", checks)
            enrolled[name] = json.loads(body)
        a = {**edge_headers(first), "Authorization": "Bearer " + enrolled["a"]["token"]}
        b = {**edge_headers(second), "Authorization": "Bearer " + enrolled["b"]["token"]}
        a_under_b = {**edge_headers(second), "Authorization": "Bearer " + enrolled["a"]["token"]}
        status, body = request(base + "/v1/whoami", headers=a)
        check(status == 200 and json.loads(body)["installation"]["id"] == enrolled["a"]["installation"],
              "installation a is identified", checks)
        check(request(base + "/v1/whoami", headers=a_under_b)[0] == 401,
              "a credential presented under another service token is refused", checks)
        sealed = b"synthetic sealed bytes"
        record = {"collection": "notes", "id": f"verify-{run}", "revision": 1, "schema": 1,
                  "digest": hashlib.sha256(sealed).hexdigest(), "sealed": base64.b64encode(sealed).decode()}
        status, _ = request(base + "/v1/records", "POST", a, {"records": [record]})
        check(status == 200, "account a writes a record", checks)
        check(request(base + f"/v1/records/notes/verify-{run}", headers=a)[0] == 200, "account a reads it back", checks)
        check(request(base + f"/v1/records/notes/verify-{run}", headers=b)[0] == 404,
              "account b cannot read account a's record", checks)
        status, body = request(base + "/v1/records/notes", headers=b)
        check(status == 200 and json.loads(body)["records"] == [], "account b lists none of account a's records", checks)
        check(request(base + f"/v1/installations/{enrolled['a']['installation']}", "DELETE", b)[0] == 404,
              "account b cannot revoke account a's installation", checks)
        for name, headers in (("a", a), ("b", b)):
            check(request(base + "/v1/installations/self", "DELETE", headers)[0] == 200, f"installation {name} revokes itself", checks)
        check(request(base + "/v1/whoami", headers=a)[0] == 401, "a revoked credential is refused", checks)
    finally:
        try:
            configure_worker(access, access.admitted() - set(temporary))
        finally:
            for ident in temporary:
                access.delete_token(ident)
    report["verified"] = True


def connect(access, label, report):
    """One service token for this installation's account; its secret goes only to `pta cloud connect`."""
    pta = shutil.which("pta") or str(Path.home() / ".cargo" / "bin" / "pta")
    status = subprocess.run([pta, "--json", "cloud", "status"], capture_output=True, text=True, timeout=60)
    current = json.loads(status.stdout or "{}").get("data") or {}
    endpoint = f"https://{access.hostname}"
    if current.get("endpoint") == endpoint and current.get("enrolled"):
        report["connect"] = "already connected"
        return
    token = access.create_token(label)
    previous = [t["id"] for t in access.tokens() if t["id"] != token["id"] and t.get("name") == token.get("name")]
    configure_worker(access, (access.admitted() - set(previous)) | {token["id"]})
    for ident in previous:
        access.delete_token(ident)
    wait_for_configuration(access.hostname)
    wait_for_admission(access.hostname, token)
    payload = json.dumps({"client_id": token["client_id"], "client_secret": token["client_secret"]})
    completed = subprocess.run([pta, "--json", "cloud", "connect", "--endpoint", endpoint, "--label", label, "--confirmed"],
                               input=payload, capture_output=True, text=True, timeout=120)
    outcome = json.loads(completed.stdout or "{}")
    report["connect"] = {"exit_code": completed.returncode, "code": outcome.get("code"),
                         "installation": (outcome.get("data") or {}).get("installation")}
    if completed.returncode != 0:
        raise SetupError("`pta cloud connect` did not complete; see the report.")


def private_json(path, value):
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(prefix=".cloud-access-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as file:
            json.dump(value, file, indent=2)
            file.write("\n")
        os.replace(temporary, path)
        path.chmod(0o600)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def run(args):
    report = {"environment": args.environment, "started_at": int(time.time())}
    try:
        token = secret("CLOUDFLARE_API_TOKEN")
        access = Access(token, cloudflare_account(token), args.environment)
        report["hostname"] = access.hostname
        if args.environment == "staging":
            team = configure_worker(access, access.admitted())
            report["team_domain"] = team
            verify(access, report)
        else:
            team = configure_worker(access, access.admitted())
            report["team_domain"] = team
            report["health"] = wait_for_configuration(access.hostname)
            connect(access, args.label, report)
        report["ok"] = True
    except (SetupError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        report["ok"] = False
        report["error"] = str(error) if isinstance(error, SetupError) else f"{type(error).__name__} during setup"
    report["finished_at"] = int(time.time())
    private_json(args.report, report)
    return 0 if report["ok"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("environment", choices=["staging", "production"])
    parser.add_argument("--report", type=Path, required=True, help="private JSON report without secrets")
    parser.add_argument("--label", default="server", help="installation label (production)")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z0-9][a-z0-9 -]{0,40}", args.label):
        parser.error("--label: lowercase letters, digits, spaces and dashes")
    sys.exit(run(args))


if __name__ == "__main__":
    main()
