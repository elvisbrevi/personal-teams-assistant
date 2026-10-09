#!/usr/bin/env python3
"""Configure a dedicated web-portal Tunnel and DNS, without changing other sites.

Credentials come only from CLOUDFLARE_API_TOKEN or CLOUDFLARE_API_TOKEN_FILE.
The new connector token is written to a private file, never stdout or argv.
"""
import argparse
import base64
import json
import os
import secrets
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


class SetupError(Exception):
    pass


def api(token, method, path, data=None):
    request = urllib.request.Request(
        "https://api.cloudflare.com/client/v4" + path,
        data=json.dumps(data).encode() if data is not None else None,
        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read(1_000_001)
    except urllib.error.HTTPError as error:
        raise SetupError(f"Cloudflare rejected {method} (HTTP {error.code}); check API token permissions.") from None
    except (urllib.error.URLError, TimeoutError):
        raise SetupError("Cannot reach the Cloudflare API through the configured network.") from None
    if len(body) > 1_000_000:
        raise SetupError("Cloudflare response exceeded the size limit.")
    result = json.loads(body)
    if not result.get("success"):
        raise SetupError("Cloudflare did not accept the operation; check token permissions and configuration.")
    return result["result"]


def private_file(path, value):
    path = path.resolve()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".web-token-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as file:
            file.write(value)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def configure(args):
    token_path = os.environ.get("CLOUDFLARE_API_TOKEN_FILE")
    token = Path(token_path).read_text().strip() if token_path else os.environ.get("CLOUDFLARE_API_TOKEN", "")
    if not token:
        raise SetupError("CLOUDFLARE_API_TOKEN is missing. Configure it in the environment's secret store; a Tunnel token cannot administer DNS.")
    domain = args.domain.lower().strip().rstrip(".")
    hostname = args.hostname.lower().strip().rstrip(".")
    if not hostname.endswith("." + domain) or hostname == domain or "/" in hostname:
        raise SetupError("The hostname must be a subdomain of the selected zone.")
    origin = urllib.parse.urlsplit(args.origin)
    if origin.scheme != "http" or origin.hostname not in ("127.0.0.1", "localhost", "::1") or not origin.port or origin.path not in ("", "/") or origin.username or origin.query or origin.fragment:
        raise SetupError("The origin must be an HTTP loopback listener with an explicit port.")
    query = urllib.parse.urlencode({"name": domain, "status": "active"})
    zones = api(token, "GET", "/zones?" + query)
    if len(zones) != 1:
        raise SetupError("Expected one active Cloudflare zone for the selected domain.")
    zone, account = zones[0]["id"], zones[0]["account"]["id"]
    records = api(token, "GET", f"/zones/{zone}/dns_records?" + urllib.parse.urlencode({"name": hostname}))
    name = "personal-teams-assistant-web"
    tunnels = api(token, "GET", f"/accounts/{account}/cfd_tunnel?" + urllib.parse.urlencode({"name": name, "is_deleted": "false"}))
    if len(tunnels) > 1:
        raise SetupError("Several tunnels have the portal name; select the intended tunnel in Cloudflare first.")
    tunnel = tunnels[0]["id"] if tunnels else None
    target = f"{tunnel}.cfargotunnel.com" if tunnel else None
    if records and (len(records) != 1 or records[0]["type"] != "CNAME" or records[0]["content"].rstrip(".") != target):
        raise SetupError("This subdomain already has another DNS record; it was not changed.")
    desired = {"ingress": [
        {"hostname": hostname, "service": args.origin.rstrip("/"), "originRequest": {"httpHostHeader": hostname}},
        {"service": "http_status:404"},
    ]}
    if tunnel:
        previous = api(token, "GET", f"/accounts/{account}/cfd_tunnel/{tunnel}/configurations")
        ingress = (previous.get("config") or {}).get("ingress", [])
        if ingress and ingress != desired["ingress"]:
            raise SetupError("The existing tunnel has different routes; they were not replaced.")
    else:
        created = api(token, "POST", f"/accounts/{account}/cfd_tunnel", {
            "name": name, "config_src": "cloudflare",
            "tunnel_secret": base64.b64encode(secrets.token_bytes(32)).decode(),
        })
        tunnel = created["id"]
        target = f"{tunnel}.cfargotunnel.com"
    api(token, "PUT", f"/accounts/{account}/cfd_tunnel/{tunnel}/configurations", {"config": desired})
    connector = api(token, "GET", f"/accounts/{account}/cfd_tunnel/{tunnel}/token")
    private_file(args.token_file, connector)
    if not records:
        api(token, "POST", f"/zones/{zone}/dns_records", {
            "type": "CNAME", "name": hostname, "content": target, "proxied": True, "ttl": 1,
        })
    elif not records[0]["proxied"]:
        api(token, "PATCH", f"/zones/{zone}/dns_records/{records[0]['id']}", {"proxied": True})
    print(f"Configured https://{hostname} → {args.origin}. Start the portal and its connector, then verify /login over HTTPS.")
    print(f"Connector credential saved privately to {args.token_file.resolve()}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--domain", required=True)
    parser.add_argument("--hostname", required=True)
    parser.add_argument("--origin", default="http://127.0.0.1:38656")
    parser.add_argument("--token-file", type=Path, required=True)
    args = parser.parse_args()
    try:
        configure(args)
    except (SetupError, OSError, ValueError, KeyError) as error:
        # API bodies, tokens and dependency error chains never belong in output.
        message = str(error) if isinstance(error, SetupError) else "Cloudflare setup could not complete; check inputs and the private output directory."
        parser.exit(1, message + "\n")


if __name__ == "__main__":
    main()
