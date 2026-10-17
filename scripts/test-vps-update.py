#!/usr/bin/env python3
"""Synthetic checks for scripts/vps-update.py: fake cargo, systemctl, pta binaries and crates.io
index, so no network, service or real profile is touched."""
import http.server
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("vps-update.py")

# `pta`/`personal-teams-assistant` of one version; status describes the host the fake systemd runs.
BINARY = """#!/bin/sh
case "$1" in
  --version) echo "pta {version} (contract 1)" ;;
  --json) exec "$WORLD/status" ;;
esac
"""
STATUS = """#!/bin/sh
running=$(cat "$WORLD/running")
if grep -qx "$running" "$WORLD/unhealthy" 2>/dev/null; then fine=false; subs=0; else fine=true; subs=1; fi
printf '{"ok":true,"data":{"host_running":true,"host_version":"%s","running":%s,"active_subscriptions":%s,"tunnel_running":%s}}\\n' \
  "$running" "$fine" "$subs" "$fine"
"""
CARGO = """#!/bin/sh
echo "cargo $*" >> "$WORLD/calls"
while [ $# -gt 0 ]; do [ "$1" = --version ] && version=$2; shift; done
[ "$CARGO_INCREMENTAL" = 0 ] || exit 3
if grep -qx "$version" "$WORLD/install-fails" 2>/dev/null; then echo "error: could not compile" >&2; exit 101; fi
for name in pta personal-teams-assistant; do
  sed "s/{{version}}/$version/" "$WORLD/binary" > "$CARGO_HOME/bin/$name"
  chmod +x "$CARGO_HOME/bin/$name"
done
echo "personal-teams-assistant $version (registry)" > "$CARGO_HOME/.crates.toml"
if [ -f "$WORLD/busy-after-install" ]; then
  python3 -c 'import sqlite3,sys; sqlite3.connect(sys.argv[1]).execute("UPDATE runs SET status=?", ("running",)).connection.commit()' "$WORLD/data/activity-registration.db"
fi
"""
SYSTEMCTL = """#!/bin/sh
echo "systemctl $*" >> "$WORLD/calls"
case "$2" in
  is-active) cat "$WORLD/service"; [ "$(cat "$WORLD/service")" = active ] ;;
  restart) if [ -f "$WORLD/broken-restart" ]; then echo none; else "$CARGO_HOME/bin/pta" --version | cut -d' ' -f2; fi > "$WORLD/running" ;;
esac
"""


class Index(http.server.BaseHTTPRequestHandler):
    entries = []

    def do_GET(self):
        if self.path != "/pe/rs/personal-teams-assistant":
            self.send_error(404)
            return
        body = "".join(json.dumps(e) + "\n" for e in Index.entries).encode()
        self.send_response(200)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Updater(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.HTTPServer(("127.0.0.1", 0), Index)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.world = root / "world"
        self.cargo = root / "cargo"
        self.state = root / "state"
        fakes = root / "fakes"
        for d in (self.world, self.cargo / "bin", fakes, self.world / "data"):
            d.mkdir(parents=True)
        (self.world / "binary").write_text(BINARY.replace("{version}", "{{version}}"))
        for path, text in ((self.world / "status", STATUS), (fakes / "cargo", CARGO), (fakes / "systemctl", SYSTEMCTL)):
            path.write_text(text)
            path.chmod(0o755)
        (self.world / "service").write_text("active\n")
        self.install("0.7.28")
        (self.world / "running").write_text("0.7.28\n")
        self.publish("0.7.27", "0.7.28")
        # The profile keeps its data elsewhere; a web portal profile has its own.
        profile = root / "config" / "dev.personalteams.assistant"
        (profile / "web" / "profiles" / "guest").mkdir(parents=True)
        (profile / "config.toml").write_text(f'[server]\ndata_dir = "{self.world / "data"}"\n')
        (profile / "web" / "profiles" / "guest" / "config.toml").write_text('[server]\ndata_dir = "data"\n')
        self.guest = profile / "web" / "profiles" / "guest" / "data"
        self.guest.mkdir()
        for directory in (self.world / "data", self.guest):
            db = sqlite3.connect(directory / "activity-registration.db")
            db.executescript("CREATE TABLE runs(id TEXT, status TEXT); INSERT INTO runs VALUES('r','done');"
                             "CREATE TABLE entries(id TEXT, state TEXT);")
            db.close()
            db = sqlite3.connect(directory / "assistant.db")
            db.executescript("CREATE TABLE jobs(resource TEXT, status TEXT); INSERT INTO jobs VALUES('a','sent');")
            db.close()
        self.env = dict(
            os.environ,
            PATH=f"{fakes}:{os.environ['PATH']}",
            HOME=str(root),
            WORLD=str(self.world),
            CARGO_HOME=str(self.cargo),
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "share"),
            PTA_UPDATE_STATE_DIR=str(self.state),
            PTA_UPDATE_TARGET_DIR=str(root / "target"),
            PTA_UPDATE_INDEX=f"http://127.0.0.1:{self.server.server_port}",
            PTA_UPDATE_MIN_FREE_GIB="0",
            PTA_UPDATE_HEALTH_SECONDS="1",
            PTA_UPDATE_BUSY_SECONDS="0.5",
            PTA_UPDATE_POLL_SECONDS="0.05",
        )

    def tearDown(self):
        self.temp.cleanup()

    def install(self, version):
        for name in ("pta", "personal-teams-assistant"):
            path = self.cargo / "bin" / name
            path.write_text(BINARY.replace("{version}", version))
            path.chmod(0o755)
        (self.cargo / ".crates.toml").write_text(f"personal-teams-assistant {version} (path)\n")

    def publish(self, *versions, yanked=()):
        Index.entries = [{"name": "personal-teams-assistant", "vers": v, "yanked": False} for v in versions]
        Index.entries += [{"name": "personal-teams-assistant", "vers": v, "yanked": True} for v in yanked]

    def run_update(self, *args):
        result = subprocess.run([sys.executable, "-I", str(SCRIPT), *args], env=self.env,
                                capture_output=True, text=True, timeout=60)
        return result.returncode, result.stdout + result.stderr

    def calls(self):
        path = self.world / "calls"
        calls = path.read_text().splitlines() if path.exists() else []
        path.unlink(missing_ok=True)
        return calls

    def installed(self):
        return subprocess.run([self.cargo / "bin" / "pta", "--version"], capture_output=True, text=True).stdout.split()[1]

    def running(self):
        return (self.world / "running").read_text().strip()

    def history(self):
        return (self.state / "history.log").read_text()

    def set_busy(self, directory, table, column, value):
        db = sqlite3.connect(directory / table)
        db.execute(f"UPDATE {'runs' if table.startswith('activity') else 'jobs'} SET {column}=?", (value,))
        db.commit()
        db.close()

    def test_nothing_to_do_when_current_or_only_yanked_or_older_releases(self):
        self.publish("0.7.27", "0.7.28", yanked=("0.7.29",))
        code, output = self.run_update()
        self.assertEqual(code, 0, output)
        self.assertIn("nothing to do", output)
        self.install("0.7.30")
        self.assertEqual(self.run_update()[0], 0)
        self.assertNotIn("install", " ".join(self.calls()))
        self.assertEqual(self.installed(), "0.7.30")

    def test_updates_restarts_and_verifies(self):
        self.publish("0.7.28", "0.7.29")
        code, output = self.run_update()
        self.assertEqual(code, 0, output)
        calls = self.calls()
        self.assertIn("cargo install personal-teams-assistant --version 0.7.29 --locked --no-default-features --force", calls)
        self.assertIn("systemctl --user restart personal-teams-assistant-web.service", calls)
        self.assertEqual((self.installed(), self.running()), ("0.7.29", "0.7.29"))
        self.assertIn("updated 0.7.28 -> 0.7.29", self.history())
        self.assertEqual(self.run_update()[0], 0)
        self.assertNotIn("restart", " ".join(self.calls()))

    def test_unhealthy_release_is_rolled_back_and_blocked_until_a_newer_one(self):
        self.publish("0.7.28", "0.7.29")
        (self.world / "unhealthy").write_text("0.7.29\n")
        code, output = self.run_update()
        self.assertEqual(code, 1, output)
        self.assertEqual((self.installed(), self.running()), ("0.7.28", "0.7.28"))
        self.assertIn("(path)", (self.cargo / ".crates.toml").read_text())
        self.assertIn("rolled back 0.7.29 -> 0.7.28", self.history())
        self.calls()
        code, output = self.run_update()
        self.assertEqual(code, 0)
        self.assertIn("0.7.29 is blocked", output)
        self.assertEqual(self.calls(), [])
        self.publish("0.7.28", "0.7.29", "0.7.30")
        self.assertEqual(self.run_update()[0], 0)
        self.assertEqual((self.installed(), self.running()), ("0.7.30", "0.7.30"))

    def test_rollback_that_cannot_recover_needs_a_person(self):
        self.publish("0.7.28", "0.7.29")
        (self.world / "broken-restart").touch()
        code, _ = self.run_update()
        self.assertEqual(code, 2)
        self.assertIn("ROLLBACK FAILED", self.history())
        self.assertEqual(self.installed(), "0.7.28")

    def test_failed_install_keeps_the_portal_and_gives_up_after_three_attempts(self):
        self.publish("0.7.28", "0.7.29")
        (self.world / "install-fails").write_text("0.7.29\n")
        for attempt in (1, 2, 3):
            code, output = self.run_update()
            self.assertEqual(code, 1, output)
            self.assertNotIn("restart", " ".join(self.calls()))
            self.assertIn(f"attempt {attempt} of 3", self.history())
        self.assertEqual((self.installed(), self.running()), ("0.7.28", "0.7.28"))
        self.assertIn("is blocked", self.run_update()[1])

    def test_defers_while_work_is_in_flight(self):
        self.publish("0.7.28", "0.7.29")
        for directory, table, column, value in (
            (self.world / "data", "activity-registration.db", "status", "running"),
            (self.guest, "assistant.db", "status", "sending"),
        ):
            self.set_busy(directory, table, column, value)
            code, output = self.run_update()
            self.assertEqual(code, 0)
            self.assertIn("deferred 0.7.29", output)
            self.assertNotIn("cargo", " ".join(self.calls()))
            self.set_busy(directory, table, column, "done")
        self.assertEqual(self.run_update()[0], 0)
        self.assertEqual(self.running(), "0.7.29")

    def test_work_started_during_the_build_restores_the_files_without_restarting(self):
        self.publish("0.7.28", "0.7.29")
        (self.world / "busy-after-install").touch()
        code, output = self.run_update()
        self.assertEqual(code, 0, output)
        self.assertIn("deferred 0.7.29 after building it", output)
        self.assertNotIn("restart", " ".join(self.calls()))
        self.assertEqual((self.installed(), self.running()), ("0.7.28", "0.7.28"))

    def test_leaves_a_stopped_service_alone(self):
        self.publish("0.7.28", "0.7.29")
        (self.world / "service").write_text("inactive\n")
        code, output = self.run_update()
        self.assertEqual(code, 0)
        self.assertIn("not active", output)
        self.assertEqual(self.installed(), "0.7.28")

    def test_check_changes_nothing(self):
        self.publish("0.7.28", "0.7.29")
        code, output = self.run_update("--check")
        self.assertEqual(code, 0)
        self.assertIn("would update 0.7.28 -> 0.7.29", output)
        self.assertEqual([c for c in self.calls() if not c.startswith("systemctl --user is-active")], [])

    def test_rollback_drill_without_a_newer_release_reinstalls_and_restores_the_installed_files(self):
        code, output = self.run_update("--rollback-drill")
        self.assertEqual(code, 0, output)
        calls = self.calls()
        self.assertIn("cargo install personal-teams-assistant --version 0.7.28 --locked --no-default-features --force", calls)
        self.assertEqual(sum(c.startswith("systemctl --user restart") for c in calls), 2)
        self.assertEqual((self.installed(), self.running()), ("0.7.28", "0.7.28"))
        self.assertIn("(path)", (self.cargo / ".crates.toml").read_text())
        self.assertIn("rollback drill: 0.7.28 was healthy, rolled back to 0.7.28", self.history())
        self.assertFalse((self.state / "state.json").exists())

    def test_rollback_drill_returns_to_the_previous_version_without_blocking(self):
        self.publish("0.7.28", "0.7.29")
        code, output = self.run_update("--rollback-drill")
        self.assertEqual(code, 0, output)
        self.assertEqual((self.installed(), self.running()), ("0.7.28", "0.7.28"))
        self.assertIn("rollback drill: 0.7.29 was healthy", self.history())
        self.assertEqual(self.run_update()[0], 0)
        self.assertEqual(self.running(), "0.7.29")


if __name__ == "__main__":
    unittest.main()
