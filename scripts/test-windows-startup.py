#!/usr/bin/env python3
"""Exercise a clean MSVC Cargo installation on a disposable Windows CI account."""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time


def stack_sizes(binary):
    data = binary.read_bytes()
    assert data[:2] == b"MZ", f"Not a PE executable: {binary}"
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe + 4] == b"PE\0\0"
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    assert magic in (0x10B, 0x20B), f"Unknown PE optional header: {magic:#x}"
    return struct.unpack_from("<II" if magic == 0x10B else "<QQ", data, optional + 72)


def main():
    assert sys.platform == "win32", "Run this check on Windows"
    binaries = Path(sys.argv[1]).resolve()
    host = binaries / "personal-teams-assistant.exe"
    cli = binaries / "pta.exe"
    assert stack_sizes(host) == (8388608, 4096), "Host needs an 8 MiB reserve and default commit"
    assert stack_sizes(cli) == (1048576, 4096), "The CLI must keep the MSVC defaults"
    print("PE headers: host reserve=8388608, CLI reserve=1048576; commit=4096", flush=True)

    profile = Path(os.environ["APPDATA"]) / "dev.personalteams.assistant"
    assert not profile.exists(), "Use a disposable Windows account without an existing PTA profile"
    assert not os.environ.get("PTA_PROFILE_DIR"), "Use the native GUI profile"
    assert os.environ.get("PTA_HEADLESS", "0") in ("", "0"), "The GUI must be enabled"

    def call(*args):
        result = subprocess.run(
            [str(cli), "--json", "--non-interactive", *args],
            capture_output=True, text=True, encoding="utf-8", timeout=130,
        )
        assert result.returncode == 0, f"pta {args}: {result.stdout} {result.stderr}"
        reply = json.loads(result.stdout)
        assert reply["ok"], f"pta {args}: {reply['code']}"
        return reply["data"]

    version = call("--version")["cli_version"]
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user32.EnumWindows.restype = wintypes.BOOL
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.GetWindowThreadProcessId.restype = wintypes.DWORD
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.IsWindowVisible.restype = wintypes.BOOL
    user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user32.GetWindowTextW.restype = ctypes.c_int
    user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    user32.PostMessageW.restype = wintypes.BOOL

    def visible_window(process):
        matches = []

        @callback_type
        def visit(window, _):
            owner = wintypes.DWORD()
            user32.GetWindowThreadProcessId(window, ctypes.byref(owner))
            title = ctypes.create_unicode_buffer(256)
            user32.GetWindowTextW(window, title, len(title))
            if (owner.value == process.pid and user32.IsWindowVisible(window)
                    and title.value == "Personal Teams Assistant"):
                matches.append(window)
            return True

        assert user32.EnumWindows(visit, 0), "Cannot enumerate native windows"
        return matches[0] if matches else None

    for cycle in (1, 2):
        descriptor = profile / "control.json"
        descriptor.unlink(missing_ok=True)
        log = binaries.parent / f"windows-startup-{cycle}.stderr.log"
        with log.open("w", encoding="utf-8") as stderr:
            process = subprocess.Popen([str(host), "--host"], stderr=stderr)

            def wait_for(check):
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    assert process.poll() is None, f"Host exited with {process.returncode}; see {log}"
                    value = check()
                    if value:
                        return value
                    time.sleep(0.1)
                raise AssertionError(f"Host did not reach the expected state; see {log}")

            try:
                wait_for(descriptor.is_file)
                assert call("app", "open")["visible"] is True
                window = wait_for(lambda: visible_window(process))
                status = call("status")
                assert status["host_running"] is True
                assert status["host_version"] == version
                assert status["headless"] is False
                assert status["running"] is False, "Startup must leave the Teams assistant stopped"
                # The native close button hides the window; reopening must reuse this host.
                assert user32.PostMessageW(window, 0x0010, 0, 0)  # WM_CLOSE
                wait_for(lambda: not visible_window(process))
                assert call("app", "open")["visible"] is True
                wait_for(lambda: visible_window(process))
                assert call("status")["running"] is False
                call("app", "quit")
                assert process.wait(timeout=30) == 0, f"Host did not exit cleanly; see {log}"
                print(f"Cycle {cycle}: window opened, host {version} healthy, Teams stopped, "
                      "window closed/reopened, host quit cleanly", flush=True)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=10)


if __name__ == "__main__":
    main()
