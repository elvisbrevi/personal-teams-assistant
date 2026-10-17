#!/usr/bin/env python3
"""Keeps the web portal's server on the newest crates.io release of personal-teams-assistant.

A systemd user timer runs it every five minutes (scripts/systemd/, docs/delivery.md). Each run
compares `pta --version` with the newest version in the crates.io index. When a newer one exists,
the portal service is active and nothing is in flight (an activity review, a task being written to
Azure DevOps, a Teams message being answered or sent), it backs up the installed binaries, installs
that version headless with `cargo install`, restarts the portal and waits until `pta --json status`
reports the new host with the health it had before. If a step fails it restores the backup,
restarts, and blocks that version until a newer one is published.

It only reads public crates.io data and the profile's databases (read-only): it opens no port and
holds no token. The standard library is enough; the server runs it with `python3 -I`.

Usage: vps-update.py [--check | --rollback-drill]
  --check           report what a run would do and change nothing
  --rollback-drill  update, then roll back as if the new version had failed (an acceptance test);
                    the version is not blocked, so the next run installs it for good
Exit status: 0 nothing to do, deferred, updated or drill passed; 1 the update failed and the
previous version is back; 2 the rollback failed too and the portal needs a person.
"""
import argparse
import fcntl
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

CRATE = "personal-teams-assistant"
BINARIES = ("personal-teams-assistant", "pta")
# Cargo's record of installed packages, restored with the binaries so it matches them.
TRACKING = (".crates.toml", ".crates2.json")
USER_AGENT = "personal-teams-assistant-vps-update (github.com/elvisbrevi/personal-teams-assistant)"
MAX_INSTALL_ATTEMPTS = 3

HOME = Path.home()
SERVICE = os.environ.get("PTA_UPDATE_SERVICE", "personal-teams-assistant-web.service")
INDEX = os.environ.get("PTA_UPDATE_INDEX", "https://index.crates.io").rstrip("/")
CARGO_HOME = Path(os.environ.get("CARGO_HOME") or HOME / ".cargo")
BIN = CARGO_HOME / "bin"
STATE = Path(os.environ.get("PTA_UPDATE_STATE_DIR") or HOME / ".local/state/personal-teams-assistant-update")
BACKUP = STATE / "previous"
# A build cache of its own, outside ~/src, so dependencies are not rebuilt on every release.
TARGET = Path(os.environ.get("PTA_UPDATE_TARGET_DIR") or HOME / ".cache/personal-teams-assistant-update/target")
PROFILE = Path(os.environ.get("XDG_CONFIG_HOME") or HOME / ".config") / "dev.personalteams.assistant"
DEFAULT_DATA = Path(os.environ.get("XDG_DATA_HOME") or HOME / ".local/share") / "dev.personalteams.assistant"
GIB = 2**30
MIN_FREE = float(os.environ.get("PTA_UPDATE_MIN_FREE_GIB", "5")) * GIB
MAX_TARGET = float(os.environ.get("PTA_UPDATE_MAX_TARGET_GIB", "10")) * GIB
HEALTH_SECONDS = float(os.environ.get("PTA_UPDATE_HEALTH_SECONDS", "300"))
BUSY_SECONDS = float(os.environ.get("PTA_UPDATE_BUSY_SECONDS", "900"))
POLL_SECONDS = float(os.environ.get("PTA_UPDATE_POLL_SECONDS", "5"))
INSTALL_SECONDS = 60 * 60

# Work a restart would interrupt, per database of each profile's data directory.
IN_FLIGHT = {
    "activity-registration.db": (
        ("an activity review is running", "SELECT count(*) FROM runs WHERE status='running'"),
        ("an activity task is being written", "SELECT count(*) FROM entries WHERE state IN ('sending','refining')"),
    ),
    "assistant.db": (
        ("a Teams message is being answered or sent", "SELECT count(*) FROM jobs WHERE status IN ('processing','sending','refining')"),
    ),
}


def log(message):
    print(message, flush=True)


def record(event):
    """Logs to the journal and keeps a line in history.log."""
    with open(STATE / "history.log", "a", encoding="utf-8") as history:
        history.write(f"{datetime.now(timezone.utc).isoformat(timespec='seconds')} {event}\n")
    log(event)


def parse(text):
    parts = (text or "").split(".")
    return tuple(map(int, parts)) if len(parts) == 3 and all(p.isdigit() for p in parts) else None


def show(version):
    return ".".join(map(str, version)) if version else "none"


def index_path(name):
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def latest_version():
    """The newest stable, not yanked version in the sparse index (the one cargo reads)."""
    request = urllib.request.Request(f"{INDEX}/{index_path(CRATE)}", headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=30) as response:
        lines = response.read().decode().splitlines()
    versions = [parse(entry["vers"]) for entry in map(json.loads, filter(None, lines)) if not entry.get("yanked")]
    return max(filter(None, versions), default=None)


def run(args, timeout=120):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=False)


def installed_version():
    try:
        words = run([str(BIN / "pta"), "--version"], timeout=30).stdout.split()
    except (OSError, subprocess.TimeoutExpired):
        return None
    return parse(words[1]) if len(words) >= 2 and words[0] == "pta" else None


def service_active():
    try:
        return run(["systemctl", "--user", "is-active", SERVICE], timeout=30).stdout.strip() == "active"
    except (OSError, subprocess.TimeoutExpired):
        return False


def restart():
    try:
        return run(["systemctl", "--user", "restart", SERVICE], timeout=180).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def health():
    """What `pta --json status` says about the host now; empty while it is not answering."""
    try:
        data = json.loads(run([str(BIN / "pta"), "--json", "status"], timeout=150).stdout).get("data") or {}
    except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError):
        data = {}
    return {
        "host": data.get("host_running") is True,
        "version": parse(data.get("host_version")),
        "assistant": data.get("running") is True,
        "subscriptions": data.get("active_subscriptions") or 0,
        "tunnel": data.get("tunnel_running") is True,
    }


def healthy(now, before, version):
    """The host runs `version` and is as healthy as before the update."""
    if not now["host"] or now["version"] != version:
        return False
    if before["assistant"] and not now["assistant"]:
        return False
    if before["subscriptions"] and not now["subscriptions"]:
        return False
    return now["tunnel"] or not before["tunnel"]


def wait_healthy(before, version):
    deadline = time.monotonic() + HEALTH_SECONDS
    while True:
        now = health() if service_active() else None
        if now and healthy(now, before, version):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(POLL_SECONDS)


def data_dirs():
    """The data directory of the profile and of each isolated web portal profile."""
    found = []
    for profile in [PROFILE, *sorted((PROFILE / "web" / "profiles").glob("*"))]:
        default = DEFAULT_DATA if profile == PROFILE else profile / "data"
        try:
            with open(profile / "config.toml", "rb") as config:
                configured = tomllib.load(config).get("server", {}).get("data_dir")
        except (OSError, tomllib.TOMLDecodeError):
            configured = None
        directory = Path(configured).expanduser() if configured else default
        found.append(directory if directory.is_absolute() else profile / directory)
    return found


def in_flight():
    """Why a restart now would interrupt work; an unreadable database counts as busy."""
    reasons = []
    for directory in data_dirs():
        for name, checks in IN_FLIGHT.items():
            path = directory / name
            if not path.is_file():
                continue
            try:
                db = sqlite3.connect(f"{path.as_uri()}?mode=ro", uri=True, timeout=10)
                try:
                    for reason, query in checks:
                        try:
                            if db.execute(query).fetchone()[0]:
                                reasons.append(f"{reason} ({path})")
                        except sqlite3.OperationalError as error:
                            if "no such table" not in str(error):
                                raise
                finally:
                    db.close()
            except sqlite3.Error as error:
                reasons.append(f"{path} could not be read: {error}")
    return reasons


def free_bytes():
    TARGET.parent.mkdir(parents=True, exist_ok=True)
    stats = os.statvfs(TARGET.parent)
    return stats.f_bavail * stats.f_frsize


def size(path):
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file() and not f.is_symlink()) if path.exists() else 0


def room_to_build():
    if free_bytes() >= MIN_FREE:
        return True
    if TARGET.exists():
        shutil.rmtree(TARGET, ignore_errors=True)
        record(f"low disk: removed this updater's build cache {TARGET}")
    return free_bytes() >= MIN_FREE


def backup():
    shutil.rmtree(BACKUP, ignore_errors=True)
    BACKUP.mkdir(mode=0o700)
    for name in BINARIES:
        shutil.copy2(BIN / name, BACKUP / name)
    for name in TRACKING:
        if (CARGO_HOME / name).is_file():
            shutil.copy2(CARGO_HOME / name, BACKUP / name)


def restore():
    """Puts the backed-up files back, each with one rename; size and time are kept, so a host
    started from them is not taken for an outdated one."""
    for name in BINARIES + TRACKING:
        source = BACKUP / name
        if source.is_file():
            destination = (BIN if name in BINARIES else CARGO_HOME) / name
            staged = destination.with_name(f".{name}.restore")
            shutil.copy2(source, staged)
            os.replace(staged, destination)


def install(version):
    environment = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_TARGET_DIR=str(TARGET))
    command = ["cargo", "install", CRATE, "--version", version, "--locked", "--no-default-features", "--force"]
    with open(STATE / "install.log", "w", encoding="utf-8") as output:
        try:
            # From the state directory, so no rust-toolchain.toml nearby picks the toolchain.
            return subprocess.run(command, cwd=STATE, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                   timeout=INSTALL_SECONDS, check=False).returncode == 0
        except (OSError, subprocess.TimeoutExpired) as error:
            output.write(f"\n{error}\n")
            return False


def load_state():
    try:
        state = json.loads((STATE / "state.json").read_text())
    except (OSError, ValueError):
        state = {}
    return {"blocked": state.get("blocked", {}), "attempts": state.get("attempts", {})}


def save_state(state):
    staged = STATE / ".state.json"
    staged.write_text(json.dumps(state, indent=1, sort_keys=True))
    os.replace(staged, STATE / "state.json")


def roll_back(installed, target, reason, drill, before, state):
    restore()
    back = restart() and wait_healthy(before, installed)
    if not drill:
        state["blocked"][show(target)] = reason
        save_state(state)
    if not back:
        record(f"ROLLBACK FAILED: {show(target)} {reason}; the {show(installed)} binaries are restored but "
               f"the portal is not healthy: check `systemctl --user status {SERVICE}` and `pta status`")
        return 2
    if drill:
        record(f"rollback drill: {show(target)} was healthy, rolled back to {show(installed)}, which is healthy again")
        return 0
    record(f"rolled back {show(target)} -> {show(installed)}: {reason}; {show(target)} stays blocked until a newer release")
    return 1


def update(check=False, drill=False):
    installed = installed_version()
    if installed is None:
        log(f"`{BIN / 'pta'} --version` gave no version: install the package first")
        return 1
    try:
        latest = latest_version()
    except (OSError, urllib.error.URLError, ValueError, KeyError) as error:
        log(f"crates.io index unavailable ({error}); will try again")
        return 0
    if latest is None or latest <= installed:
        log(f"{show(installed)} is installed; crates.io has {show(latest)}: nothing to do")
        return 0
    target = show(latest)
    state = load_state()
    if target in state["blocked"] and not drill:
        log(f"{target} is blocked ({state['blocked'][target]}); waiting for a newer release")
        return 0
    if not service_active():
        log(f"{SERVICE} is not active: not updating to {target}")
        return 0
    busy = in_flight()
    if busy:
        log(f"deferred {target}: " + "; ".join(busy))
        return 0
    if check:
        log(f"would update {show(installed)} -> {target}; {free_bytes() / GIB:.1f} GiB free, build cache {size(TARGET) / GIB:.1f} GiB")
        return 0
    if not room_to_build():
        record(f"deferred {target}: less than {MIN_FREE / GIB:.0f} GiB free")
        return 0

    before = health()
    backup()
    record(f"installing {target} (installed {show(installed)})")
    started = time.monotonic()
    if not install(target) or installed_version() != latest:
        restore()
        attempts = state["attempts"].get(target, 0) + 1
        state["attempts"][target] = attempts
        if attempts >= MAX_INSTALL_ATTEMPTS:
            state["blocked"][target] = f"cargo install failed {attempts} times"
        save_state(state)
        record(f"install of {target} failed (attempt {attempts} of {MAX_INSTALL_ATTEMPTS}); {show(installed)} kept, "
               f"the portal was not restarted; see {STATE / 'install.log'}")
        return 1
    built = time.monotonic() - started

    # The running portal keeps serving from the replaced files until the restart.
    deadline = time.monotonic() + BUSY_SECONDS
    while busy := in_flight():
        if time.monotonic() >= deadline:
            restore()
            record(f"deferred {target} after building it: " + "; ".join(busy) + f"; {show(installed)} files restored")
            return 0
        time.sleep(POLL_SECONDS)

    started = time.monotonic()
    if not restart():
        return roll_back(installed, latest, f"`systemctl --user restart {SERVICE}` failed", drill, before, state)
    if not wait_healthy(before, latest):
        return roll_back(installed, latest, f"not healthy within {HEALTH_SECONDS:.0f} s of the restart", drill, before, state)
    if drill:
        return roll_back(installed, latest, "drill", drill, before, state)
    state["attempts"].pop(target, None)
    save_state(state)
    record(f"updated {show(installed)} -> {target} (built in {built:.0f} s, healthy {time.monotonic() - started:.0f} s after the restart)")
    if size(TARGET) > MAX_TARGET:
        shutil.rmtree(TARGET, ignore_errors=True)
        record(f"build cache above {MAX_TARGET / GIB:.0f} GiB: removed {TARGET}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="report what a run would do and change nothing")
    mode.add_argument("--rollback-drill", action="store_true", help="update, then roll back as if the update had failed")
    args = parser.parse_args()
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(STATE / "lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            log("another update is running")
            return 0
        return update(check=args.check, drill=args.rollback_drill)


if __name__ == "__main__":
    sys.exit(main())
