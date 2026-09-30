"""Export the reviewed working tree without private Git history or ignored state.

Usage: python3 scripts/export-public.py /absolute/path/source.tar.gz
The destination must not exist. Review the archive before publishing it.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BLOCKED_NAMES = {"config.toml", "knowledge-map.toml", ".env", "assistant.db", "desktop-chat.db"}
BLOCKED_PARTS = {".git", "target", "data", "knowledge", "gen", "AppIcon.iconset", ".cloudflared", ".agents", ".codex"}


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: export-public.py /absolute/path/source.tar.gz")
    requested = Path(sys.argv[1])
    output = requested.resolve()
    if not requested.is_absolute() or output.exists():
        raise SystemExit("destination must be an unused absolute path")
    if output.is_relative_to(ROOT):
        raise SystemExit("destination must be outside the source repository")
    names = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=ROOT
    ).split(b"\0")
    with tempfile.TemporaryDirectory(prefix="personal-teams-public-") as temp:
        staging = Path(temp)
        copied = 0
        for raw in names:
            if not raw:
                continue
            relative = Path(os.fsdecode(raw))
            if (relative.name in BLOCKED_NAMES
                    or (relative.name.startswith(".env") and relative.name != ".env.example")
                    or any(part in BLOCKED_PARTS for part in relative.parts)):
                continue
            if relative.suffix.lower() in {".db", ".pem", ".key", ".p12", ".pfx"}:
                continue
            source = ROOT / relative
            if source.is_symlink() or not source.is_file():
                raise SystemExit(f"cannot export non-regular file: {relative}")
            target = staging / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            copied += 1
        if not shutil.which("gitleaks"):
            raise SystemExit("install gitleaks before exporting public source")
        subprocess.run(["gitleaks", "dir", "--redact", "--no-banner", str(staging)], check=True)
        with tarfile.open(output, "w:gz") as archive:
            for path in sorted(staging.rglob("*")):
                if path.is_file():
                    archive.add(path, arcname=path.relative_to(staging), recursive=False)
    print(f"Exported {copied} files to {output}")


if __name__ == "__main__":
    main()
