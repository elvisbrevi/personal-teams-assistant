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

Two temporary staging service tokens are created and admitted (CLOUDFLARE_API_TOKEN or
CLOUDFLARE_API_TOKEN_FILE, as for configure-cloud-access.py), and the copy runs connect, migrate
(dry run, then confirmed), verify, a repair that has nothing to repair, promote, a configuration edit and a note through the store and
a pull with replace; then a second computer of the account (a fresh profile with the content key a
trust confirmation hands over) is refused without adopting, adopts, verifies, joins primary and
edits through the store, which the first pulls; a conflict (the second edits through the store, the
first by hand) that the first sees field by field, resolves keeping its own and the second receives;
another account (the second token) sees none of
it; then rollback, an erasure refused while the second computer is connected, its revocation and
the erasure, checking that local data never shrinks and that the store ends empty; last, the backup
the migration took is restored (the host restarts itself, keeps the data from just before and comes
back with the configuration it had before migrating and the same account store link). The tokens,
their admission, every host started and the copies are removed afterwards. The report holds steps, codes and
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
# Every host a rehearsal starts, stopped at the end whatever happened.
STARTED = []
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


def fresh_profile(root, port):
    """An empty profile for another computer: the template configuration pointed at its own copy."""
    profile = private_dir(root / "home" / ".config" / PROFILE_NAME)
    data = private_dir(root / "home" / ".local" / "share" / PROFILE_NAME)
    config = profile / "config.toml"
    config.write_text(rewrite_config((ROOT / "config.example.toml").read_text(), profile, data, port))
    config.chmod(0o600)
    return profile


def hand_over_key(source, target):
    """The account content key a Trust this machine confirmation with the account store hands over."""
    keys = private_dir(target / "keys")
    shutil.copyfile(source / "keys" / "cloud-content.json", keys / "cloud-content.json")
    (keys / "cloud-content.json").chmod(0o600)


def free_port():
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Rehearsal:
    def __init__(self, binaries, root, profile):
        self.binaries = binaries
        self.root = root
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
        STARTED.append(self)
        for _ in range(600):
            if (self.profile / "control.json").exists():
                return
            time.sleep(0.1)
        raise RuntimeError("The rehearsal host did not start.")

    def stop(self):
        if self.host and self.host.poll() is not None:
            # A restore restarted the host as a successor this process did not start: quit it too.
            if self.pta("status", timeout=30).get("ok"):
                self.pta("app", "quit", timeout=60)
                for _ in range(60):
                    if not self.pta("status", timeout=30).get("ok"):
                        break
                    time.sleep(0.5)
            return
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


def second_installation(run, endpoint, service, label, greeting):
    """Another computer of the same account joins: it adopts the account's configuration, keeps its own
    history and settings, and reads and writes the configuration through the store. Returns it, running."""
    payload = json.dumps({"client_id": service["client_id"], "client_secret": service["client_secret"]})
    root = private_dir(run.root / "second")
    profile = fresh_profile(root, free_port())
    hand_over_key(run.profile, profile)
    other = Rehearsal(run.binaries, root, profile)
    other.steps = run.steps
    other.start()
    r = other.pta("cloud", "connect", "--endpoint", endpoint, "--label", label + " b", "--confirmed", stdin=payload)
    other.step("second: connect", r)
    run.expect(r.get("ok"), "a second computer of the account enrolls")
    r = other.pta("cloud", "status").get("data") or {}
    run.expect(r.get("adopt") is True, "the second computer is told to adopt the account's configuration")
    r = other.pta("cloud", "migrate", "--confirmed", timeout=1800)
    other.step("second: migrate without adopting", r)
    run.expect(r.get("code") == "state_conflict", "a second computer never uploads its configuration over the account's")
    bind = other.pta("config", "get", "server.bind").get("data")
    r = other.pta("cloud", "migrate", "--adopt", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    differing = [{k: c.get(k) for k in ("name", "local", "remote", "pending", "missing_remote", "unknown_remote",
                                        "differing", "undecryptable", "content_differs", "others")}
                 for c in (data.get("verify") or {}).get("collections", []) if not c.get("ok")]
    other.step("second: migrate --adopt", r, applied=(data.get("adopted") or {}).get("applied"), mode=data.get("mode"),
               differing=differing)
    run.expect(r.get("ok") and data.get("mode") == "mirror", "the second computer adopts and migrates")
    r = other.pta("config", "get", "policy.greeting")
    run.expect(json.dumps(greeting) in json.dumps(r.get("data")), "the second computer has the account's configuration")
    run.expect(other.pta("config", "get", "server.bind").get("data") == bind, "the second computer keeps its own settings")
    for who, installation in (("second", other), ("first", run)):
        r = installation.pta("cloud", "verify", timeout=1800)
        verify = (r.get("data") or {}).get("verify") or {}
        others = sum(c.get("others", 0) for c in verify.get("collections", []))
        installation.step(f"{who}: verify", r, verified=verify.get("ok"), others=others)
        run.expect(verify.get("ok") is True and others > 0, f"the {who} computer verifies, counting the other's records apart")
    r = run.pta("cloud", "installations")
    run.expect((r.get("data") or {}).get("active") == 2, "the account lists both computers")
    r = other.pta("cloud", "promote", "--confirmed", timeout=1800)
    other.step("second: promote", r, mode=(r.get("data") or {}).get("mode"))
    run.expect(r.get("ok") and (r.get("data") or {}).get("mode") == "primary", "the second computer uses the store as its source")
    second = f"Rehearsal b {secrets.token_hex(3)}"
    r = other.pta("config", "set", "policy.greeting", json.dumps(second))
    other.step("second: config set (through the store)", r)
    run.expect(r.get("ok"), "the second computer edits the configuration through the store")
    r = run.pta("cloud", "pull")
    run.step("first: pull", r, applied=((r.get("data") or {}).get("pulled") or {}).get("applied"))
    r = run.pta("config", "get", "policy.greeting")
    run.expect(r.get("ok") and json.dumps(second) in json.dumps(r.get("data")), "the first computer receives the second's edit")
    return other, conflict(run, other)


def conflict(run, other):
    """Both computers change the greeting: the second through the store, the first by hand (never
    acknowledged). The first sees which field differs, keeps its own, and the second receives it."""
    theirs = f"Rehearsal theirs {secrets.token_hex(3)}"
    mine = f"Rehearsal mine {secrets.token_hex(3)}"
    r = other.pta("config", "set", "policy.greeting", json.dumps(theirs))
    run.expect(r.get("ok"), "the second computer edits the greeting again through the store")
    config = run.profile / "config.toml"
    text, n = re.subn(r'(?m)^greeting = .*$', "greeting = " + json.dumps(mine), config.read_text(), count=1)
    run.expect(n == 1, "the first computer's greeting is edited by hand")
    config.write_text(text)
    r = run.pta("cloud", "pull")
    run.step("first: pull with a hand edit", r, conflicts=(r.get("data") or {}).get("conflicts"))
    r = run.pta("cloud", "conflicts")
    found = (r.get("data") or {}).get("conflicts") or []
    fields = {f.get("path"): f for c in found for f in c.get("fields", [])}
    run.step("cloud conflicts", r, records=len(found), fields=len(fields))
    greeting = fields.get("policy.greeting", {})
    run.expect(r.get("ok") and len(found) == 1 and greeting.get("here") == mine and greeting.get("store") == theirs,
               "the conflict shows the field that differs on each side")
    record = found[0]
    r = run.pta("cloud", "resolve", record["collection"], record["record"], "--keep", "here", "--confirmed")
    run.step("cloud resolve --keep here", r, remaining=(r.get("data") or {}).get("remaining"))
    run.expect(r.get("ok") and (r.get("data") or {}).get("remaining") == 0, "keeping this computer's version settles the conflict")
    r = other.pta("cloud", "pull")
    r = other.pta("config", "get", "policy.greeting")
    run.expect(r.get("ok") and json.dumps(mine) in json.dumps(r.get("data")), "the second computer receives the version kept")
    return mine


def second_account(binaries, root, endpoint, service, label, first_records):
    """Another account of the same Worker: its own store, which holds none of the first account's records."""
    payload = json.dumps({"client_id": service["client_id"], "client_secret": service["client_secret"]})
    base = private_dir(root / "account-c")
    profile = fresh_profile(base, free_port())
    other = Rehearsal(binaries, base, profile)
    other.start()
    try:
        r = other.pta("cloud", "connect", "--endpoint", endpoint, "--label", label + " c", "--confirmed", stdin=payload)
        other.step("other account: connect", r)
        status = other.pta("cloud", "status").get("data") or {}
        held = sum(v.get("records", 0) for v in ((status.get("store") or {}).get("collections") or {}).values())
        steps = other.steps
        return r.get("ok"), held, status.get("adopt"), steps
    finally:
        other.pta("cloud", "disconnect", "--forget-service-token", "--confirmed")
        other.stop()


def restore(run, greeting, local):
    """Restore the backup the migration took: the host restarts, keeps the current data first and
    comes back with the configuration it had before migrating and the same account store link."""
    listed = run.pta("cloud", "backups").get("data") or {}
    backup = next((b["name"] for b in listed.get("backups", []) if b.get("reason") == "migrate" and b.get("restorable")), None)
    run.expect(backup, "the migration's backup can be restored")
    link = (run.pta("cloud", "status").get("data") or {}).get("installation")
    r = run.pta("cloud", "restore", backup, "--confirmed")
    run.step("restore", r, restarting=(r.get("data") or {}).get("restarting"))
    run.expect(r.get("ok") and (r.get("data") or {}).get("restarting"), "the restore restarts the host")
    outcome = {}
    for _ in range(240):
        time.sleep(0.5)
        listed = run.pta("cloud", "backups", timeout=30)
        data = listed.get("data") or {}
        if listed.get("ok") and not data.get("pending") and data.get("last_restore"):
            outcome = data["last_restore"]
            break
    run.step("restore outcome", {"ok": outcome.get("ok")}, files=outcome.get("files"), before=bool(outcome.get("before")))
    run.expect(outcome.get("ok") and outcome.get("backup") == backup, "the restarted host restored the backup")
    run.expect(outcome.get("before") in data.get("before_restore", []), "the data from just before the restore is kept")
    r = run.pta("config", "get", "policy.greeting")
    run.expect(r.get("ok") and json.dumps(greeting) not in json.dumps(r.get("data")),
               "the configuration is back as it was before migrating")
    run.expect((run.pta("cloud", "status").get("data") or {}).get("installation") == link,
               "the restore keeps this computer's account store link")
    after = counts(run.pta("cloud", "migrate", "--dry-run"))
    run.expect(all(after.get(name, 0) >= number for name, number in local.items()), "the restore lost no record")


def rehearse(run, endpoint, service, label, second_service=None):
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
    r = run.pta("cloud", "repair", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("repair (nothing differs)", r, requeued=data.get("requeued"), verified=data.get("ok"))
    run.expect(r.get("ok") and data.get("requeued") == 0 and data.get("ok") is True,
               "a repair of a matching copy sends nothing and still verifies")
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
    other, greeting = second_installation(run, endpoint, service, label, greeting)
    if second_service:
        enrolled, held, adopt, steps = second_account(run.binaries, run.root, endpoint, second_service, label, sum(local.values()))
        run.steps.extend(steps)
        run.expect(enrolled and held == 0 and adopt is False, "another account's store holds none of this account's records")
    # Both computers' outboxes empty first, so nothing they still send changes the counts below.
    for _ in range(120):
        pending = [((i.pta("cloud", "status").get("data") or {}).get("mirror") or {}).get("outbox", {}).get("pending")
                   for i in (run, other)]
        if pending == [0, 0]:
            break
        time.sleep(1)
    store_before = ((run.pta("cloud", "status").get("data") or {}).get("store") or {}).get("collections") or {}
    r = run.pta("cloud", "rollback", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("rollback", r, mode=data.get("mode"), only_in_store=data.get("only_in_store"),
             verified=(data.get("verify") or {}).get("ok"))
    run.expect(r.get("ok") and data.get("mode") == "off", "the rollback returns to off")
    status = run.pta("cloud", "status").get("data") or {}
    store_after = (status.get("store") or {}).get("collections") or {}
    before_counts = {k: v.get("records") for k, v in store_before.items()}
    after_counts = {k: v.get("records") for k, v in store_after.items()}
    run.step("store after the rollback", {"ok": True},
             changed={k: [before_counts.get(k), after_counts.get(k)] for k in set(before_counts) | set(after_counts)
                      if before_counts.get(k) != after_counts.get(k)})
    # History may still grow meanwhile (the copy's own scheduled activity review): nothing is removed.
    run.expect(all((after_counts.get(k) or 0) >= (n or 0) for k, n in before_counts.items()),
               "the rollback leaves the copy in the store")
    after = counts(run.pta("cloud", "migrate", "--dry-run"))
    run.expect(all(after.get(name, 0) >= number for name, number in local.items()), "no local record was lost")
    r = run.pta("config", "get", "policy.greeting")
    run.expect(json.dumps(greeting) in json.dumps(r.get("data")), "the edit made through the store stays local")
    r = run.pta("setup", "status")
    store = next((s for s in (r.get("data") or {}).get("steps", []) if s.get("id") == "account_store"), {})
    run.step("setup status", r, account_store=store.get("state"))
    r = run.pta("cloud", "erase", "--confirmed", timeout=1800)
    run.step("erase while another computer is connected", r)
    run.expect(r.get("code") == "state_conflict", "the copy is never erased while another computer uses it")
    installations = (run.pta("cloud", "installations").get("data") or {}).get("installations") or []
    second_id = next((i["id"] for i in installations if not i.get("current") and not i.get("revoked_at")), None)
    r = run.pta("cloud", "installations", "revoke", str(second_id), "--confirmed")
    run.step("revoke the second computer", r)
    run.expect(r.get("ok"), "the first computer revokes the second in the store")
    other.stop()
    r = run.pta("cloud", "erase", "--confirmed", timeout=1800)
    data = r.get("data") or {}
    run.step("erase", r, erased=data.get("erased"), key_retired=bool(data.get("key_retired")))
    run.expect(r.get("ok") and data.get("erased", 0) > 0, "the erasure empties the copy")
    status = run.pta("cloud", "status").get("data") or {}
    remaining = sum(v.get("records", 0) + v.get("tombstones", 0) for v in ((status.get("store") or {}).get("collections") or {}).values())
    run.expect(remaining == 0, "the store holds nothing afterwards")
    after = counts(run.pta("cloud", "migrate", "--dry-run"))
    run.expect(all(after.get(name, 0) >= number for name, number in local.items()), "the erasure deleted nothing local")
    restore(run, greeting, local)
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
    access = service = second_service = None
    try:
        profile, _ = copy_profile(args.profile.resolve(), root, free_port())
        token = access_module.secret("CLOUDFLARE_API_TOKEN")
        access = access_module.Access(token, access_module.cloudflare_account(token), "staging")
        # Temporary tokens a rehearsal stopped half way left behind: unadmitted and deleted first.
        stale = {t["id"] for t in access.tokens() if t.get("name", "").startswith("pta-cloud staging rehearsal ")}
        if stale:
            access_module.configure_worker(access, access.admitted() - stale)
            for ident in stale:
                access.delete_token(ident)
        report["stale_tokens_removed"] = len(stale)
        label = f"rehearsal {secrets.token_hex(4)}"
        service = access.create_token(label)
        # A second account of the same Worker, to check that the stores stay apart.
        second_service = access.create_token(label + " c")
        access_module.configure_worker(access, access.admitted() | {service["id"], second_service["id"]})
        access_module.wait_for_configuration(access.hostname)
        access_module.wait_for_admission(access.hostname, service)
        access_module.wait_for_admission(access.hostname, second_service)
        run = Rehearsal(args.bin.resolve(), root, profile)
        run.start()
        rehearse(run, f"https://{access.hostname}", service, label, second_service)
        report["ok"] = True
    except Exception as error:  # the report says which step failed; never the content
        report["ok"] = False
        report["error"] = str(error) if isinstance(error, (RuntimeError, access_module.SetupError)) else type(error).__name__
    finally:
        for started in reversed(STARTED):
            started.stop()
        if run:
            report["steps"] = run.steps
        temporary = {t["id"] for t in (service, second_service) if t}
        try:
            if access and temporary:
                access_module.configure_worker(access, access.admitted() - temporary)
        finally:
            for ident in temporary if access else ():
                access.delete_token(ident)
            shutil.rmtree(root, ignore_errors=True)
        report["cleaned_up"] = not root.exists()
        report["finished_at"] = int(time.time())
        access_module.private_json(args.report, report)
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
