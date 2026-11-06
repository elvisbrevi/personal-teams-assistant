#!/usr/bin/env python3
"""Exercise a clean MSVC Cargo installation's headless host on a disposable Windows CI account.

The host has no window: it must start without a console, answer `pta`, refuse to open its local
page where no browser session exists (the CI runner is a service), keep the profile when the
application is launched a second time, quit cleanly and start again. `--headless` and
`PTA_HEADLESS` are accepted and ignored.
"""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time

GUI_SUBSYSTEM = 2
CONSOLE_SUBSYSTEM = 3


def pe_header(binary):
    """The stack reserve and commit, and the subsystem, of a PE executable."""
    data = binary.read_bytes()
    assert data[:2] == b"MZ", f"Not a PE executable: {binary}"
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe + 4] == b"PE\0\0"
    optional = pe + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    assert magic in (0x10B, 0x20B), f"Unknown PE optional header: {magic:#x}"
    subsystem = struct.unpack_from("<H", data, optional + 68)[0]
    stack = struct.unpack_from("<II" if magic == 0x10B else "<QQ", data, optional + 72)
    return stack, subsystem


def main():
    assert sys.platform == "win32", "Run this check on Windows"
    binaries = Path(sys.argv[1]).resolve()
    host = binaries / "personal-teams-assistant.exe"
    cli = binaries / "pta.exe"
    (host_stack, host_subsystem), (cli_stack, cli_subsystem) = pe_header(host), pe_header(cli)
    assert host_stack == (8388608, 4096), "Host needs an 8 MiB reserve and default commit"
    assert cli_stack == (1048576, 4096), "The CLI must keep the MSVC defaults"
    # The host is a GUI-subsystem program, so starting it never opens a console window; pta is a
    # console program.
    assert host_subsystem == GUI_SUBSYSTEM, f"The host must not open a console (subsystem {host_subsystem})"
    assert cli_subsystem == CONSOLE_SUBSYSTEM, f"pta must be a console program (subsystem {cli_subsystem})"
    print("PE headers: host reserve=8388608 and GUI subsystem, CLI reserve=1048576 and console; commit=4096",
          flush=True)

    profile = Path(os.environ["APPDATA"]) / "dev.personalteams.assistant"
    assert not profile.exists(), "Use a disposable Windows account without an existing PTA profile"
    assert not os.environ.get("PTA_PROFILE_DIR"), "Use the installation's own profile"

    def run(*args):
        result = subprocess.run(
            [str(cli), "--json", "--non-interactive", *args],
            capture_output=True, text=True, encoding="utf-8", timeout=130,
        )
        return result.returncode, json.loads(result.stdout)

    def call(*args):
        code, reply = run(*args)
        assert code == 0 and reply["ok"], f"pta {args}: {code} {reply}"
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

    def visible_windows(process):
        found = []

        @callback_type
        def visit(window, _):
            owner = wintypes.DWORD()
            user32.GetWindowThreadProcessId(window, ctypes.byref(owner))
            if owner.value == process.pid and user32.IsWindowVisible(window):
                found.append(window)
            return True

        assert user32.EnumWindows(visit, 0), "Cannot enumerate windows"
        return found

    # A person's launch, then the flags and variable older launchers pass (accepted and ignored).
    for cycle, (flags, variables) in enumerate([([], {}), (["--headless"], {"PTA_HEADLESS": "1"})], 1):
        descriptor = profile / "control.json"
        descriptor.unlink(missing_ok=True)
        log = binaries.parent / f"windows-startup-{cycle}.stderr.log"
        env = {**os.environ, **variables}
        with log.open("w", encoding="utf-8") as stderr:
            process = subprocess.Popen([str(host), *flags], stderr=stderr, env=env)

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
                status = call("status")
                assert status["host_running"] is True
                assert status["host_version"] == version
                assert status["headless"] is True
                assert status["running"] is False, "Startup must leave the Teams assistant stopped"
                assert not visible_windows(process), "The host must show no window or console"
                # The runner is a service session: the page answers that there is no browser to
                # open it in, and never hands out a sign-in.
                code, reply = run("app", "open")
                assert (code, reply["ok"], reply["code"]) == (3, False, "not_ready"), f"app open: {code} {reply}"
                assert "pta app open" in reply["message"], reply["message"]
                assert "code=" not in json.dumps(reply), "The page's sign-in code must never be returned"
                # Launching the application again leaves the running host in place.
                again = subprocess.run([str(host)], env=env, timeout=60, capture_output=True)
                assert again.returncode != 0, "A second launch without a browser must say so"
                assert process.poll() is None, "A second launch must not stop the running host"
                assert call("status")["host_version"] == version
                call("app", "quit")
                assert process.wait(timeout=30) == 0, f"Host did not exit cleanly; see {log}"
                print(f"Cycle {cycle} {flags or '(no flags)'}: host {version} healthy without a window, "
                      "Teams stopped, app open answered without a browser, second launch kept the host, "
                      "quit cleanly", flush=True)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=10)


if __name__ == "__main__":
    main()
