`pta start`, `pta stop` and `pta restart` manage the assistant; `pta status` (without `--json`) summarizes whether the host and the assistant run, the mode, Teams and the models. `pta help` describes every command.

**Update.** `cargo install personal-teams-assistant --locked` replaces the binaries (if the old `personal-teams-desktop` package is still installed, first `cargo uninstall personal-teams-desktop`: both install `pta`), but the previous host keeps running until the new version is first used: the first `pta` or opening the app stops it (assistant and tunnel). If the assistant was running, the terminal asks whether to keep it running with the new version; with `--json`, `--non-interactive` or without a terminal the previous state is kept (it starts again); the app shows a notice with «Keep running» / «Leave it stopped». Verify afterwards with `pta status`. A host started from another path is never stopped this way, except the `personal-teams-desktop` host of the old package in the same folder or a host whose executable was already uninstalled. Repeated starts and stops are safe. The host can live hidden without the service started; `pta app open` shows its window, `pta app hide` hides it and `pta app quit` ends the host after stopping the service and its tunnel. A short CLI call may end while the host or service it asked for keeps running.

`pta --json status` tells the persisted and loaded configuration apart; an older version without a control channel must be closed from its GUI before starting the new one. An open listener alone does not prove Teams authorization or reception. Check the subscription health and the audit log too: `pta status` shows `Teams: N active subscription(s)`.

`pta tunnel configure token`, `pta tunnel configure file PATH` or `pta tunnel configure external` choose a mode. The token is stored as `CLOUDFLARE_TUNNEL_TOKEN` with credentials set, never in arguments. `pta tunnel validate` checks the mode and the local requirements; it does not prove connectivity from the Internet.

The administrative channel uses another loopback port, an instance token in a private file and a profile lock. The Graph tunnel keeps only its configured origin. Changing the configuration validates first; a failed restart requires checking whether it was applied and stopped. After an abrupt crash, the next host recovers the pending configuration transaction; ambiguous Graph sends are never resent.

**Headless host (Linux/servers).** Install it with `cargo install personal-teams-assistant --no-default-features --locked` (no Tauri/WebKit) or force it with `personal-teams-assistant --headless` / `PTA_HEADLESS=1`. `pta --json status` shows `headless: true`; `app open/hide` answer `not_ready`; `app quit`, SIGTERM or Ctrl-C stop the service and tunnel before exiting. `--start` tries to start the service at launch and keeps the host alive if it fails, to diagnose it. On Linux, credentials are stored with `pta credentials set NAME` in `~/.config/dev.personalteams.assistant/credentials/default/` (0600) or given as `NAME`/`NAME_FILE` in the host's environment. Remote login: `pta auth microsoft login --no-browser`, authorize the URL on any device and paste the `http://localhost:PORT/?code=…` address the browser could not open into `pta auth microsoft finish --redirect 'URL'`. systemd user service in `~/.config/systemd/user/pta.service` (`systemctl --user enable --now pta`, with `loginctl enable-linger` so it keeps running without a session):

```ini
[Unit]
Description=Personal Teams Assistant (headless)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=%h/.cargo/bin/personal-teams-assistant --headless --start
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

Never keep two instances of the same account active at once (another computer, the local GUI or a server): they would duplicate answers and cause loops in the personal chat. Leave the other one stopped or in `pta mode observe`.
