#!/usr/bin/env python3
"""Run the assistant locally with a temporary HTTPS endpoint; no third-party Python packages."""
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
os.chdir(ROOT)
children = []

def stop(*_):
    for child in reversed(children):
        if child.poll() is None:
            child.terminate()
    for child in reversed(children):
        try:
            child.wait(timeout=15)
        except subprocess.TimeoutExpired:
            child.kill()
    sys.exit(0)


def main():
    for tool in ("cloudflared", "az", "lazy-workflow"):
        if not shutil.which(tool):
            raise RuntimeError("Missing command: " + tool)
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
    subprocess.run([cargo, "build", "--locked"], check=True)
    binary = ROOT / "target/debug/personal-teams-assistant"
    config_path = sys.argv[1] if len(sys.argv) > 1 else "config.toml"
    info = json.loads(subprocess.check_output([str(binary), "local-info", config_path], text=True))
    if not info["bind"].startswith("127.0.0.1:"):
        raise RuntimeError("Local mode requires server.bind = 127.0.0.1:PORT")
    # Capture credentials directly in memory; neither values nor subprocess output are logged.
    env = os.environ.copy()
    required = ("ENTRA_CLIENT_SECRET", "TYPESAFE_API_KEY", "DEEPSEEK_API_KEY", "ADMIN_AUTH_KEY", "GRAPH_WEBHOOK_SECRET", "STATE_ENCRYPTION_KEY")
    for name in (*required, *info.get("additional_secrets", [])):
        if not env.get(name) and not env.get(name + "_FILE"):
            result = subprocess.run(["lazy-workflow", "credentials-get", "--name", name, "--force", "--no-log-file"], capture_output=True, text=True)
            if result.returncode:
                raise RuntimeError("Missing credential: " + name)
            env[name] = result.stdout.strip()
    data = ROOT / "data"
    data.mkdir(mode=0o700, exist_ok=True)
    log = data / "tunnel.log"
    with log.open("w") as output:
        tunnel = subprocess.Popen(["cloudflared", "tunnel", "--url", "http://" + info["bind"], "--no-autoupdate"], stdout=output, stderr=subprocess.STDOUT)
        children.append(tunnel)
        url = None
        for _ in range(90):
            if tunnel.poll() is not None:
                raise RuntimeError("Tunnel stopped; inspect data/tunnel.log")
            found = re.search(r"https://[a-z0-9-]+\.trycloudflare\.com", log.read_text())
            if found:
                url = found.group(0)
                break
            time.sleep(0.5)
        if not url:
            raise RuntimeError("Tunnel did not return an HTTPS URL")
        # This app belongs to this project. Replace only its callback registration.
        subprocess.run(["az", "ad", "app", "update", "--id", info["client_id"], "--web-redirect-uris", url + "/oauth/callback", "--only-show-errors"], check=True)
        env["PUBLIC_URL"] = url
        app = subprocess.Popen([str(binary), "serve", config_path], env=env)
        children.append(app)
        for _ in range(60):
            if app.poll() is not None:
                raise RuntimeError("Assistant failed to start")
            try:
                with urllib.request.urlopen("http://" + info["bind"] + "/healthz", timeout=1):
                    break
            except Exception:
                time.sleep(0.5)
        (data / "local-url.txt").write_text(url + "\n")
        print("Local assistant: " + url + "/oauth/login", flush=True)
        print("Use ADMIN_AUTH_KEY from your credential store to authorize your configured Microsoft account.", flush=True)
        print("Ctrl-C stops both processes. This temporary hostname changes on restart.", flush=True)
        while app.poll() is None and tunnel.poll() is None:
            if "Tunnel not found" in log.read_text()[-5000:]:
                raise RuntimeError("Temporary tunnel expired; restart this command to register a new URL")
            time.sleep(2)
        raise RuntimeError("A process stopped; restart local.py")


if __name__ == "__main__":
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    try:
        main()
    except Exception as exc:
        print(str(exc), file=sys.stderr)
        for process in children:
            if process.poll() is None:
                process.terminate()
        sys.exit(1)
