# Personal Teams Assistant

A personal Microsoft Teams assistant written in Rust. It reads and answers **as you** (delegated Microsoft Graph OAuth, no bot) when someone writes to you directly, mentions you in a group or asks it in your personal chat. It answers only with evidence from sources you authorized for that conversation — files from Git repositories, URLs, Azure DevOps (activity and Wiki), your own Teams messages and other read-only tools — and writes with the language model you choose: Codex or Claude Code through their installed CLIs, or the DeepSeek API, in a configurable fallback order. Code checks references, links and sensitive data; if an answer cites something that cannot be verified or contains sensitive data, it is not sent (in your personal chat you get a notice) and it stays visible in the Messages tab.

Ask it, for example, which work you did in the last two weeks that no task records: it compares your commits, pull requests, pipeline runs, releases, Wiki edits and (if you authorize them) your Teams messages with your Azure DevOps work items, and answers with concrete candidates, each with its date, a verified link and a proposed task title.

Answers to Teams are written in Spanish or English (`llm.language`); the app itself — GUI, CLI and audit log — is in English.

## Installation

One Cargo package, `personal-teams-assistant`, installs everything: the tray/menu bar app and host `personal-teams-assistant`, the `pta` CLI and the agent skill built into `pta`.

```sh
cargo install personal-teams-assistant --locked   # from crates.io
cargo install --path . --locked                   # from this checkout
```

If you had the old `personal-teams-desktop` package (0.4.0 or earlier), uninstall it first: both install `pta` and Cargo does not overwrite another package's binary. Your profile, credentials and account are kept.

```sh
cargo uninstall personal-teams-desktop
cargo install personal-teams-assistant --locked
pta status
```

Requirements: Rust (version pinned in `rust-toolchain.toml`), the native prerequisites of Tauri 2, Git and `~/.cargo/bin` in `PATH`. To receive Teams messages you also need a stable HTTPS URL that reaches the local port (e.g. `cloudflared` with a token) and the computer on.

On Linux or a server, install the **headless** variant (no Tauri or WebKit) and operate it only with `pta`:

```sh
cargo install personal-teams-assistant --no-default-features --locked
personal-teams-assistant --headless --start    # foreground; fits a systemd service
```

Its credentials are stored with `pta credentials set` in private profile files or come from `NAME`/`NAME_FILE` variables. The Microsoft login is completed from any device with `pta auth microsoft finish --redirect 'URL'`. Details in the [architecture](docs/architecture.md#headless-host-linux-and-servers). Never keep two instances of the same account active.

GUI and CLI share the same profile, Keychain/Credential Manager credentials and service. Reinstalling does not ask for credentials again: the `dev.personalteams.assistant` profile, the data directory and the connected Microsoft account are kept.

### Update

```sh
cargo install personal-teams-assistant --locked
pta status
```

Cargo runs nothing after installing, so the previous version keeps running until the new one is first used. The first `pta` command (any; `pta status` will do) or opening the app detects that the installed binary changed, stops the previous host (assistant and tunnel) and, if the assistant was running, asks whether to keep it running with the new version (in the app, with a notice in the window). Without an interactive terminal (`--non-interactive`, `--json`, systemd) it keeps the previous state.

From 0.6.1 on, a profile saved by the app no longer has the `[jev]` section that older versions require: do not go back to an older version with the same profile.

## Quick use

```sh
personal-teams-assistant          # opens the window (or pta app open)
pta help                          # every command, with its description
pta status                        # running? host, assistant, mode, Teams and models
pta start                         # starts the assistant
pta stop                          # stops it
pta auth microsoft login          # OAuth PKCE with the system browser
pta auth microsoft finish --wait  # waits for you to finish the consent
pta mode active                   # turns real sends on after reviewing the proposals
pta test simulate <<< '{"session":"demo","text":"What work did I do that is not registered?","sources":["azure-devops-status","azure-devops-wikis"]}'
```

`pta help` (or `pta`, `pta --help`) lists every command with its description; `--json` returns the full result as JSON. `pta skill show` gives the operating manual for agents.

## Minimal setup

1. Credentials (on stdin, never as an argument): `DEEPSEEK_API_KEY` if you use DeepSeek and the ones your sources reference, e.g. `pta credentials set DEEPSEEK_API_KEY < file`. `GRAPH_WEBHOOK_SECRET` and `STATE_ENCRYPTION_KEY` are generated.
2. Language models: in the GUI (Settings → Language models) turn on and order Codex, Claude Code and DeepSeek, with their model and effort. Codex and Claude use the installed CLI (`codex`, `claude`) with its own login. Default: Codex `gpt-6.1-sol` (medium) → Claude `claude-opus-5-5` (medium) → DeepSeek `deepseek-flash` (maximum). Every call starts with the first and moves to the next only if it fails (e.g. out of credits). From the CLI: `pta llm providers` and `pta config set llm.chain JSON`.
3. An Entra registration with the *Mobile and desktop* platform (`http://localhost`) and the delegated permissions `User.Read`, `Chat.Read`, `ChatMessage.Send`, `offline_access`. Set `graph.tenant_id` and `graph.client_id`.
4. Public URL (`server.public_url`) and tunnel: `pta tunnel configure token|file PATH|external`.
5. Knowledge: add repositories (`pta repos add`, or GitHub with `pta auth github login`), register sources with `pta sources add` (they start disabled and without audiences) and authorize them per conversation with `pta sources audience`.

## Documentation

- Project site: [teams-assistant.elvisbrevi.cl](https://teams-assistant.elvisbrevi.cl/), HTML and CSS in [`site/`](site/) published as a Cloudflare Worker.
- [Architecture](docs/architecture.md): components, pipeline, data, security, CLI contract and change recipes.
- [Proposed improvements](docs/proposed-improvements.md).
- [Operating skill](desktop/skills/personal-teams-assistant/SKILL.md) and [guide for agents](AGENTS.md).

## License

MIT.
