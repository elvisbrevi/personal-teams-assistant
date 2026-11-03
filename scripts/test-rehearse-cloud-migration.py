#!/usr/bin/env python3
"""Synthetic checks for the rehearsal's profile copy: nothing secret is taken, the copy points at
itself and cannot reach the outside, and the source profile is never changed."""
import hashlib
import importlib.util
import sqlite3
import tempfile
import tomllib
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("rehearse", Path(__file__).with_name("rehearse-cloud-migration.py"))
rehearse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rehearse)

CONFIG = """knowledge_map = "{profile}/knowledge-map.toml"

[server]
bind = "0.0.0.0:443"
public_url = "https://assistant.example.com"
data_dir = "{data}"
cloudflare_tunnel = true

[policy]
greeting = "Hello"

[cloud]
endpoint = "https://store.example.com"
installation = "00000000-0000-4000-8000-000000000001"
mode = "mirror"

[secrets]
"""


def digest(root):
    files = sorted(p for p in root.rglob("*") if p.is_file())
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}


class CopyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.source = base / "source" / "profile"
        self.data = base / "source" / "data"
        for directory in (self.source / "credentials" / "default", self.source / "keys", self.source / "cloud",
                          self.source / "variables", self.source / "implementation", self.source / "backups" / "cloud-1",
                          self.data / "repositories" / "example"):
            directory.mkdir(parents=True)
        (self.source / "config.toml").write_text(CONFIG.format(profile=self.source, data=self.data))
        (self.source / "knowledge-map.toml").write_text('[organization]\nname = "Contoso"\n')
        (self.source / "credentials" / "default" / "STATE_ENCRYPTION_KEY").write_text("synthetic-key")
        (self.source / "keys" / "cloud-content.json").write_text("{}")
        (self.source / "control.json").write_text("{}")
        (self.source / "implementation" / "connection.json").write_text("{}")
        for path in (self.source / "cloud" / "mirror.db", self.source / "variables" / "variables.db",
                     self.source / "implementation" / "runs.db", self.data / "knowledge-index.db"):
            sqlite3.connect(path).execute("CREATE TABLE t (x)").connection.commit()
        db = sqlite3.connect(self.data / "assistant.db")
        db.executescript("""
            CREATE TABLE vault (k, v); INSERT INTO vault VALUES ('graph', 'synthetic sealed token');
            CREATE TABLE subscriptions (id); INSERT INTO subscriptions VALUES ('s');
            CREATE TABLE outputs (nonce); INSERT INTO outputs VALUES ('n');
            CREATE TABLE jobs (id); INSERT INTO jobs VALUES ('finished');
        """)
        db.commit()
        db.close()
        self.before = digest(base / "source")
        self.root = base / "copy"
        self.root.mkdir()
        self.profile, self.copied_data = rehearse.copy_profile(self.source, self.root, 45678)

    def test_the_source_profile_is_never_changed(self):
        self.assertEqual(digest(self.source.parent), self.before)

    def test_secrets_keys_links_and_machine_files_stay_behind(self):
        names = {p.relative_to(self.profile).as_posix() for p in self.profile.rglob("*")}
        self.assertEqual(names, {"config.toml", "knowledge-map.toml", "implementation", "implementation/runs.db"})
        self.assertEqual({p.name for p in self.copied_data.iterdir()}, {"assistant.db", "knowledge-index.db"})
        db = sqlite3.connect(self.copied_data / "assistant.db")
        for table in ("vault", "subscriptions", "outputs"):
            self.assertEqual(db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0], 0, table)
        self.assertEqual(db.execute("SELECT COUNT(*) FROM jobs").fetchone()[0], 1)

    def test_the_copy_points_at_itself_without_a_tunnel_or_a_link(self):
        config = tomllib.loads((self.profile / "config.toml").read_text())
        self.assertEqual(config["knowledge_map"], str(self.profile / "knowledge-map.toml"))
        self.assertEqual(config["server"]["data_dir"], str(self.copied_data))
        self.assertEqual(config["server"]["bind"], "127.0.0.1:45678")
        self.assertFalse(config["server"]["cloudflare_tunnel"])
        self.assertNotIn("cloud", config)
        self.assertEqual(config["policy"]["greeting"], "Hello")

    def test_the_copy_is_private(self):
        for path in [self.profile, self.copied_data, *self.profile.rglob("*"), *self.copied_data.rglob("*")]:
            mode = path.stat().st_mode & 0o777
            self.assertEqual(mode, 0o700 if path.is_dir() else 0o600, path)


if __name__ == "__main__":
    unittest.main()
