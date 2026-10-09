#!/usr/bin/env python3
"""Configure only the portal's Access apps and dedicated GitHub login provider.

Secrets come from CLOUDFLARE_API_TOKEN[_FILE] and
PTA_ACCESS_GITHUB_CLIENT_ID[_FILE]/PTA_ACCESS_GITHUB_CLIENT_SECRET[_FILE].
The repository GitHub connection and existing tunnels/DNS are never changed.
"""
import argparse
import fnmatch
import json
import os
import re
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

APP_NAME = "personal-teams-assistant-web"
IDP_NAME = "personal-teams-assistant-login"
APP_WRITE = "Access: Apps and Policies Write"
IDP_WRITE = "Access: Identity Providers Write (or Access: Organizations, Identity Providers, and Groups Write)"


class SetupError(Exception):
    pass


def secret(name, required=True):
    file = os.environ.get(name + "_FILE")
    if file:
        path = Path(file)
        if path.stat().st_mode & 0o077:
            raise SetupError(f"{name}_FILE must be private (0600).")
        value = path.read_text().strip()
    else:
        value = os.environ.get(name, "").strip()
    if required and not value:
        raise SetupError(f"{name} is missing; use a private file or environment secret binding, never chat.")
    return value


def api(token, method, path, data=None):
    permission = IDP_WRITE if "/identity_providers" in path else APP_WRITE
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
        if error.code == 403 and method != "GET":
            raise SetupError(f"Cloudflare rejected {method} {path} (403). Required account permission: {permission}.") from None
        raise SetupError(f"Cloudflare rejected {method} {path} (HTTP {error.code}).") from None
    except (urllib.error.URLError, TimeoutError):
        raise SetupError("Cloudflare API is unavailable through the configured network.") from None
    if len(body) > 1_000_000:
        raise SetupError("Cloudflare response exceeded the size limit.")
    result = json.loads(body)
    if not result.get("success"):
        raise SetupError(f"Cloudflare did not accept {method} {path}.")
    return result


def result(token, method, path, data=None):
    return api(token, method, path, data)["result"]


def listing(token, path):
    items = []
    for page in range(1, 21):
        body = api(token, "GET", f"{path}?page={page}&per_page=100")
        items.extend(body["result"])
        info = body.get("result_info") or {}
        if not info or page >= info.get("total_pages", 1):
            return items
    raise SetupError("Access resource listing exceeded the page limit.")


def private_json(path, value):
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(prefix=".access-settings-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as file:
            json.dump(value, file, indent=2)
            file.write("\n")
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
        path.chmod(0o600)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def callback_paths(file, hostname):
    value = json.loads(file.read_text())
    if "data" in value:  # pta --json envelope
        value = value["data"]
    if value.get("hostname") != hostname or not isinstance(value.get("paths"), list):
        raise SetupError("Callbacks must come from `pta --json web callbacks` on the production Mac, for this hostname.")
    paths = value["paths"]
    if len(paths) > 200 or len(set(paths)) != len(paths):
        raise SetupError("Invalid or duplicate callback paths.")
    pattern = r"/webhooks/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/graph/(notifications|lifecycle)"
    if any(not isinstance(path, str) or not re.fullmatch(pattern, path) for path in paths):
        raise SetupError("Only exact per-profile Graph notification/lifecycle paths can bypass Access; wildcards are forbidden.")
    return sorted(paths)


def domains(app):
    values = [app.get("domain")] + (app.get("self_hosted_domains") or [])
    values += [d.get("uri") or d.get("hostname") for d in (app.get("destinations") or []) if d.get("type") == "public"]
    return [value for value in values if value]


def normalized_policy(policy):
    return {key: policy.get(key, []) for key in ["name", "decision", "include", "require", "exclude"]}


def upsert_app(token, base, previous, definition):
    if previous:
        policies = result(token, "GET", f"{base}/apps/{previous['id']}/policies")
        if any(p.get("name") != definition["policies"][0]["name"] for p in policies):
            raise SetupError("Existing portal application has unmanaged policies; inspect it before changing Access.")
        updated = result(token, "PUT", f"{base}/apps/{previous['id']}", definition)
    else:
        updated = result(token, "POST", base + "/apps", definition)
    actual = result(token, "GET", f"{base}/apps/{updated['id']}")
    policies = result(token, "GET", f"{base}/apps/{updated['id']}/policies")
    wanted = definition["policies"]
    if actual.get("domain") != definition["domain"] or actual.get("type") != "self_hosted" or (actual.get("allowed_idps") or []) != (definition.get("allowed_idps") or []):
        raise SetupError("Access application readback did not match the requested hostname/provider.")
    if [normalized_policy(p) for p in policies] != [normalized_policy(p) for p in wanted]:
        raise SetupError("Access policy readback did not match; deployment must wait for inspection.")
    return actual


def configure(args):
    token = secret("CLOUDFLARE_API_TOKEN")
    account = args.account_id or os.environ.get("CLOUDFLARE_ACCOUNT_ID", "")
    if not re.fullmatch(r"[0-9a-f]{32}", account) or not re.fullmatch(r"[a-z0-9-]+(?:\.[a-z0-9-]+)+", args.hostname):
        raise SetupError("Invalid account ID or hostname.")
    callbacks = callback_paths(args.callbacks_file, args.hostname)
    emails = sorted(set(args.allow_email or []))
    if not emails or any(not re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", email) for email in emails):
        raise SetupError("Provide the explicit authorized GitHub email allowlist; everyone/domain-wide admission is forbidden.")
    base = f"/accounts/{account}/access"
    organization = result(token, "GET", base + "/organizations")
    auth_domain = organization["auth_domain"]
    if not re.fullmatch(r"[a-z0-9-]+\.cloudflareaccess\.com", auth_domain):
        raise SetupError("Unexpected Cloudflare team domain.")
    providers = listing(token, base + "/identity_providers")
    matching = [provider for provider in providers if provider.get("name") == IDP_NAME]
    if len(matching) > 1 or (matching and matching[0].get("type") != "github"):
        raise SetupError("The dedicated login provider name conflicts with existing resources.")
    provider = matching[0] if matching else None
    client_id = secret("PTA_ACCESS_GITHUB_CLIENT_ID", required=False)
    client_secret = secret("PTA_ACCESS_GITHUB_CLIENT_SECRET", required=False)
    if provider and client_id and (provider.get("config") or {}).get("client_id") != client_id:
        raise SetupError("Existing dedicated GitHub login provider uses a different OAuth app; it was not replaced.")
    existing = listing(token, base + "/apps")
    definitions = [{"name": APP_NAME, "domain": args.hostname}]
    for path in callbacks:
        definitions.append({"name": APP_NAME + " Graph " + path.split('/')[2] + " " + path.rsplit('/', 1)[1], "domain": args.hostname + path})
    wanted = {item["domain"]: item["name"] for item in definitions}
    by_domain = {}
    for app in existing:
        for domain in domains(app):
            host = domain.split('/')[0]
            if not fnmatch.fnmatchcase(args.hostname, host):
                continue
            if domain not in wanted or app.get("name") != wanted[domain] or app.get("type") != "self_hosted" or domain in by_domain:
                raise SetupError("An overlapping or unmanaged Access application exists; it was not replaced.")
            by_domain[domain] = app
    # Inspect every existing managed policy before performing any writes.
    for domain, app in by_domain.items():
        policies = result(token, "GET", f"{base}/apps/{app['id']}/policies")
        expected_name = "Authorized GitHub identities" if domain == args.hostname else "Graph callback only"
        if any(policy.get("name") != expected_name for policy in policies):
            raise SetupError("Existing portal application has unmanaged policies; inspect it before changing Access.")
    if args.dry_run:
        print(json.dumps({"writes":False,"team_domain":auth_domain,"dedicated_github_provider_exists":bool(provider),
            "oauth_callback":"https://" + auth_domain + "/cdn-cgi/access/callback",
            "applications":definitions,"authorized_identity_count":len(emails),
            "required_permissions":[APP_WRITE, IDP_WRITE]}, indent=2))
        return
    if not provider:
        if not client_id or not client_secret:
            raise SetupError("Create the dedicated GitHub OAuth App in GitHub Developer Settings, then supply its client ID/secret through the private login bindings. Repository OAuth credentials must not be reused.")
        provider = result(token, "POST", base + "/identity_providers", {"name":IDP_NAME,"type":"github",
            "config":{"client_id":client_id,"client_secret":client_secret}})
        provider = result(token, "GET", base + "/identity_providers/" + provider["id"])
        if provider.get("type") != "github" or (provider.get("config") or {}).get("client_id") != client_id:
            raise SetupError("Dedicated GitHub provider readback did not match.")
    # Prepare exact exceptions first, before protecting the enclosing hostname.
    for item in definitions[1:]:
        upsert_app(token, base, by_domain.get(item["domain"]), {**item,"type":"self_hosted","app_launcher_visible":False,
            "policies":[{"name":"Graph callback only","decision":"bypass","include":[{"everyone":{}}]}]})
    main = upsert_app(token, base, by_domain.get(args.hostname), {**definitions[0],"type":"self_hosted",
        "session_duration":"12h","app_launcher_visible":False,"allowed_idps":[provider["id"]],
        "auto_redirect_to_identity":True,"http_only_cookie":True,
        "policies":[{"name":"Authorized GitHub identities","decision":"allow", "include":[{"email":{"email":email}} for email in emails],
            "require":[{"login_method":{"id":provider["id"]}}]}]})
    if not re.fullmatch(r"[0-9a-f]{64}", main.get("aud", "")):
        raise SetupError("Cloudflare returned an invalid application audience.")
    settings = {"bind":"127.0.0.1:38656","public_url":"https://" + args.hostname,"session_hours":12,
        "access":{"issuer":"https://" + auth_domain,"audience":main["aud"],"account_id":account,"github_idp_id":provider["id"]}}
    private_json(args.settings_file, settings)
    print(f"Access configuration read back successfully. Private settings: {args.settings_file.resolve()}. GitHub authorization, local identity association, Mac deployment and browser QA still need verification.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--account-id")
    parser.add_argument("--hostname", default="assistant.elvisbrevi.cl")
    parser.add_argument("--allow-email", action="append", required=True)
    parser.add_argument("--callbacks-file", type=Path, required=True)
    parser.add_argument("--settings-file", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not args.dry_run and not args.settings_file:
        parser.error("--settings-file is required for configuration")
    try:
        configure(args)
    except (SetupError, OSError, ValueError, KeyError, TypeError) as error:
        message = str(error) if isinstance(error, SetupError) else "Access setup could not complete; inspect inputs and private file paths."
        parser.exit(1, message + "\n")


if __name__ == "__main__":
    main()
