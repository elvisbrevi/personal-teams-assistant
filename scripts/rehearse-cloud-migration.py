#!/usr/bin/env python3
"""Rehearse the cloud migration on a copy of a profile, against the staging account store.

  python3 scripts/rehearse-cloud-migration.py --bin DIR --report PATH [--profile DIR]

DIR holds the `personal-teams-assistant` and `pta` executables to rehearse (a branch build). The
profile (by default this user's, `${XDG_CONFIG_HOME:-~/.config}/dev.personalteams.assistant`) is
copied into a private temporary directory and never changed: databases through SQLite's online
backup, the settings files as they are, and nothing else. The copy leaves out credentials, keys,
account variables, the cloud link and ledger, machine files and backups; its data directory, map
path and listener are rewritten to the copy, the tunnel and the `[cloud]` section are removed and
its stored Microsoft tokens, subscriptions and send nonces are cleared. Its host starts headless
without `--start`, so the Teams pipeline never runs and nothing reaches Microsoft Graph.

A temporary staging service token is created and admitted (CLOUDFLARE_API_TOKEN or
CLOUDFLARE_API_TOKEN_FILE, as for configure-cloud-access.py), and the copy runs connect, migrate
(dry run, then confirmed), verify, promote, a configuration edit and a note through the store,
pull, rollback and erase, checking that local data never shrinks and that the store ends empty.
The token, its admission and the copy are removed afterwards. The report holds steps, codes and
counts only: no secrets and no content. Never point it at the production Worker.
"""
import argparse
import importlib.util
import json
import os
import re
import secrets
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROFILE_NAME = "dev.personalteams.assistant"
# What a copy never takes from the profile directory.
EXCLUDED = {
    "credentials", "keys", "variables", "cloud", "backups", "maintenance", "portable", "web", "skills",
    "control.json", "control.lock", "host-path.txt", "launchers.json", "settings-rollback.json",
}
EXCLUDED_IMPLEMENTATION = {"connection.json", "installation.json", "registry.json", "requests"}
# Rows a copy must not keep: Microsoft tokens, webhook subscriptions and send nonces.
CLEARED = {"assistant.db": ["vault", "subscriptions", "outputs"]}


def load_access():
    spec = importlib.util.spec_from_file_location("cloud_access", ROOT / "scripts" / "configure-cloud-access.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def private_dir(path):
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)
    return path


def copy_database(source, target):
    """An online, consistent copy (the host may be writing), then the rows a copy must not keep."""
    src = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
    dst = sqlite3.connect(target)
    try:
        src.backup(dst)
        tables = {row[0] for row in dst.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        for table in CLEARED.get(source.name, []):
            if table in tables:
                dst.execute(f"DELETE FROM {table}")
        dst.commit()
    finally:
        src.close()
        dst.close()
    target.chmod(0o600)


def rewrite_config(text, profile, data, port):
    """Point the copied config.toml at the copy and turn off what could reach the outside."""
    lines, section, skipping = [], "", False
    for line in text.splitlines():
        header = re.match(r"^\s*\[\[?([^\]]+)\]\]?\s*$", line)
        if header:
            section = header.group(1).strip()
            skipping = section == "cloud" or section.startswith("cloud.")
            if skipping:
                continue
        if skipping:
            continue
        key = re.match(r"^\s*([A-Za-z0-9_]+)\s*=", line)
        name = key.group(1) if key else None
        if section == "" and name == "knowledge_map":
            line = f"knowledge_map = {json.dumps(str(profile / 'knowledge-map.toml'))}"
        elif section == "server" and name == "data_dir":
            line = f"data_dir = {json.dumps(str(data))}"
        elif section == "server" and name == "bind":
            line = f'bind = "127.0.0.1:{port}"'
        elif section == "server" and name == "cloudflare_tunnel":
            line = "cloudflare_tunnel = false"
        lines.append(line)
    return "\n".join(lines) + "\n"


def copy_profile(source, root, port):
    """The copy described above; returns (profile, data). `source` is never written."""
    import tomllib

    config = tomllib.loads((source / "config.toml").read_text())
    source_data = Path(config["server"]["data_dir"]).expanduser()
    profile = private_dir(root / "home" / ".config" / PROFILE_NAME)
    data = private_dir(root / "home" / ".local" / "share" / PROFILE_NAME)
    for entry in sorted(source.iterdir()):
        if entry.name in EXCLUDED or entry.is_symlink():
            continue
        if entry.is_file() and entry.suffix == ".toml":
            shutil.copyfile(entry, profile / entry.name)
            (profile / entry.name).chmod(0o600)
        elif entry.is_file() and entry.suffix == ".db":
            copy_database(entry, profile / entry.name)
        elif entry.is_dir() and entry.name == "implementation":
            target = private_dir(profile / "implementation")
            for item in entry.iterdir():
                if item.name not in EXCLUDED_IMPLEMENTATION and item.is_file() and item.suffix == ".db":
                    copy_database(item, target / item.name)
    if source_data.is_dir():
        for entry in sorted(source_data.iterdir()):
            if entry.is_file() and entry.suffix == ".db" and not entry.is_symlink():
                copy_database(entry, data / entry.name)
    config_path = profile / "config.toml"
    config_path.write_text(rewrite_config(config_path.read_text(), profile, data, port))
    config_path.chmod(0o600)
    return profile, data


def free_port():
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Rehearsal:
    def __init__(self, binaries, root, profile):
        self.binaries = binaries
        self.home = root / "home"
        self.profile = profile
        self.env = {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin",
            "PTA_PROFILE_DIR": str(profile),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "XDG_DATA_HOME": str(self.home / ".local" / "share"),
            "TOKIO_WORKER_THREADS": "2",
        }
        self.steps = []
        self.host = None

    def pta(self, *args, stdin=None, timeout=900):
        done = subprocess.run([str(self.binaries / "pta"), "--json", *args], input=stdin, capture_output=True,
                              text=True, env=self.env, timeout=timeout)
        try:
            return json.loads(done.stdout)
        except json.JSONDecodeError:
            return {"ok": False, "code": "unparsed"}

    def step(self, name, reply, **facts):
        entry = {"step": name, "ok": bool(reply.get("ok")), "code": reply.get("code"), **facts}
        self.steps.append(entry)
        print(json.dumps(entry), flush=True)
        return entry

    def expect(self, condition, name):
        self.steps.append({"step": name, "ok": bool(condition), "check": True})
        print(json.dumps(self.steps[-1]), flush=True)
        if not condition:
            raise RuntimeError("Check failed: " + name)

    def start(self):
        self.host = subprocess.Popen([str(self.binaries / "personal-teams-assistant"), "--headless"], env=self.env,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(600):
            if (self.profile / "control.json").exists():
                return
            time.sleep(0.1)
        raise RuntimeError("The rehearsal host did not start.")

    def stop(self):
        if not self.host:
            return
        self.pta("app", "quit", timeout=60)
        try:
            self.host.wait(30)
        except subprocess.TimeoutExpired:
            self.host.terminate()
            try:
                self.host.wait(20)
            except subprocess.TimeoutExpired:
                self.host.kill()


def counts(plan):
    return {name: entry["records"] for name, entry in (plan.get("data", {}).get("plan", {}).get("collections") or {}).items()}


def rehearse(run, endpoint, service, label):
    """The sequence on the copy; raises on the first failed check."""
    payload = json.dumps({"client_id": service["client_id"], "client_secret": service["client_secret"]})
    r = run.pta("cloud", "connect", "--endpoint", endpoint, "--label", label, "--confirmed", stdin=payload)
    run.step("connect", r)
    run.expect(r.get("ok"), "the copy enrolls with the staging store")
    r = run.pta("cloud", "status")
    run.expect((r.get("data") or {}).get("worker", {}).get("environment") == "staging", "the store is staging")
    before = run.pta("cloud", "migrate", "--dry-run")
    local = counts(before)
    run.step("migrate --dry-run", before, records=sum(local.values()),
             stays_local=len((before.get("data") or {}).get("stays_local") or []))
    run.expect(before.get("ok") and sum(local.values()) > 0, "the dry run lists the records")
    r = run.pta("cloud", "migrate", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("migrate", r, complete=data.get("complete"), uploaded=data.get("uploaded"),
             backup_files=(data.get("backup") or {}).get("files"), mode=data.get("mode"))
    run.expect(r.get("ok") and data.get("complete") and data.get("mode") == "mirror", "the migration completes in mirror")
    run.expect(Path((data.get("backup") or {}).get("path", "/missing")).is_dir(), "the migration backed up first")
    r = run.pta("cloud", "verify", timeout=1800)
    run.step("verify", r, verified=(r.get("data") or {}).get("ok"), elapsed_ms=(r.get("data") or {}).get("elapsed_ms"))
    run.expect((r.get("data") or {}).get("ok") is True, "the copy verifies")
    r = run.pta("cloud", "promote", "--confirmed", timeout=1800)
    run.step("promote", r, mode=(r.get("data") or {}).get("mode"))
    run.expect(r.get("ok") and (r.get("data") or {}).get("mode") == "primary", "the store becomes the source")
    greeting = f"Rehearsal {secrets.token_hex(3)}"
    r = run.pta("config", "set", "policy.greeting", json.dumps(greeting))
    run.step("config set (through the store)", r)
    run.expect(r.get("ok"), "a configuration edit goes through the store")
    r = run.pta("notes", "add", stdin=json.dumps({"title": "Rehearsal", "text": "A synthetic note for the rehearsal."}))
    run.step("note add (through the store)", r)
    note = (r.get("data") or {}).get("id")
    run.expect(r.get("ok") and note, "a note goes through the store")
    r = run.pta("notes", "remove", note)
    run.step("note remove (through the store)", r)
    run.expect(r.get("ok"), "a note removal goes through the store")
    bind = run.pta("config", "get", "server.bind").get("data")
    r = run.pta("cloud", "pull", "--replace", "--confirmed")
    run.step("pull --replace", r, conflicts=(r.get("data") or {}).get("conflicts"))
    run.expect(r.get("ok") and (r.get("data") or {}).get("conflicts") == 0, "the pull finds no conflict")
    run.expect(run.pta("config", "get", "server.bind").get("data") == bind,
               "the account's configuration never replaces this installation's own settings")
    store_before = ((run.pta("cloud", "status").get("data") or {}).get("store") or {}).get("collections") or {}
    r = run.pta("cloud", "rollback", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("rollback", r, mode=data.get("mode"), only_in_store=data.get("only_in_store"),
             verified=(data.get("verify") or {}).get("ok"))
    run.expect(r.get("ok") and data.get("mode") == "off", "the rollback returns to off")
    status = run.pta("cloud", "status").get("data") or {}
    store_after = (status.get("store") or {}).get("collections") or {}
    run.expect({k: v.get("records") for k, v in store_after.items()} == {k: v.get("records") for k, v in store_before.items()},
               "the rollback leaves the copy in the store")
    after = counts(run.pta("cloud", "migrate", "--dry-run"))
    run.expect(all(after.get(name, 0) >= number for name, number in local.items()), "no local record was lost")
    r = run.pta("config", "get", "policy.greeting")
    run.expect(json.dumps(greeting) in json.dumps(r.get("data")), "the edit made through the store stays local")
    r = run.pta("setup", "status")
    store = next((s for s in (r.get("data") or {}).get("steps", []) if s.get("id") == "account_store"), {})
    run.step("setup status", r, account_store=store.get("state"))
    r = run.pta("cloud", "erase", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("erase", r, erased=data.get("erased"), key_retired=bool(data.get("key_retired")))
    run.expect(r.get("ok") and data.get("erased", 0) > 0, "the erasure empties the copy")
    status = run.pta("cloud", "status").get("data") or {}
    remaining = sum(v.get("records", 0) + v.get("tombstones", 0) for v in ((status.get("store") or {}).get("collections") or {}).values())
    run.expect(remaining == 0, "the store holds nothing afterwards")
    after = counts(run.pta("cloud", "migrate", "--dry-run"))
    run.expect(all(after.get(name, 0) >= number for name, number in local.items()), "the erasure deleted nothing local")
    r = run.pta("cloud", "disconnect", "--forget-service-token", "--confirmed")
    run.step("disconnect", r, store_notified=(r.get("data") or {}).get("store_notified"))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", type=Path, required=True, help="directory with the executables to rehearse")
    parser.add_argument("--report", type=Path, required=True, help="private JSON report without secrets")
    default = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / PROFILE_NAME
    parser.add_argument("--profile", type=Path, default=default, help="profile to copy (never changed)")
    parser.add_argument("--account", help="Cloudflare account ID (instead of CLOUDFLARE_ACCOUNT_ID)")
    args = parser.parse_args()
    if args.account:
        # A launcher passes fixed arguments and only the variables it names.
        os.environ["CLOUDFLARE_ACCOUNT_ID"] = args.account
    access_module = load_access()
    report = {"started_at": int(time.time()), "profile": "copy", "environment": "staging"}
    root = Path(tempfile.mkdtemp(prefix="pta-rehearsal-"))
    root.chmod(0o700)
    run = None
    access = service = None
    try:
        profile, _ = copy_profile(args.profile.resolve(), root, free_port())
        token = access_module.secret("CLOUDFLARE_API_TOKEN")
        access = access_module.Access(token, access_module.cloudflare_account(token), "staging")
        label = f"rehearsal {secrets.token_hex(4)}"
        service = access.create_token(label)
        access_module.configure_worker(access, access.admitted() | {service["id"]})
        access_module.wait_for_configuration(access.hostname)
        access_module.wait_for_admission(access.hostname, service)
        run = Rehearsal(args.bin.resolve(), root, profile)
        run.start()
        rehearse(run, f"https://{access.hostname}", service, label)
        report["ok"] = True
    except Exception as error:  # the report says which step failed; never the content
        report["ok"] = False
        report["error"] = str(error) if isinstance(error, (RuntimeError, access_module.SetupError)) else type(error).__name__
    finally:
        if run:
            run.stop()
            report["steps"] = run.steps
        try:
            if access and service:
                access_module.configure_worker(access, access.admitted() - {service["id"]})
        finally:
            if access and service:
                access.delete_token(service["id"])
            shutil.rmtree(root, ignore_errors=True)
        report["cleaned_up"] = not root.exists()
        report["finished_at"] = int(time.time())
        access_module.private_json(args.report, report)
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
