<p align="center"><img src="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/icon.png" width="120" height="120" alt=""></p>

<h1 align="center">Personal Teams Assistant</h1>

<p align="center"><strong>Answers in Teams as you, and only with what you authorize.</strong><br>
<a href="https://teams-assistant.elvisbrevi.cl/">Website</a> · <a href="https://crates.io/crates/personal-teams-assistant">crates.io</a> · <a href="docs/architecture.md">Architecture</a> · <a href="desktop/skills/personal-teams-assistant/SKILL.md">Agent skill</a></p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-overview-dark.jpg">
    <img src="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-overview-light.jpg" width="640" alt="Home: the assistant's face shows it is running in observation mode, with recent outcomes, setup and the status of each part.">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-phone-home-dark.jpg">
    <img src="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-phone-home-light.jpg" width="190" alt="The same Home on a phone, with the floating tab bar.">
  </picture>
</p>

A personal Microsoft Teams assistant written in Rust. It reads and answers **as you** (delegated Microsoft Graph OAuth, no bot) when someone writes to you directly, mentions you in a group or asks it in your personal chat. It answers only with evidence from sources you authorized for that conversation — files from Git repositories, URLs, Azure DevOps (activity and Wiki), your own Teams messages and other read-only tools — and writes with the language model you choose: Codex, Claude Code or OpenCode 2 through their installed CLIs, or the DeepSeek API, in a configurable fallback order. Code checks references, links and sensitive data; if an answer cites something that cannot be verified or contains sensitive data, it is not sent (in your personal chat you get a notice) and it stays visible in the Messages tab.

Ask it, for example, which work you did in the last two weeks that no task records: it compares your commits, pull requests, pipeline runs, releases, Wiki edits and (if you authorize them) your Teams messages with your Azure DevOps work items, and answers briefly with each unregistered piece of work, its verified link and the work items where it could go. Activities keeps daily lists of your existing Azure tasks, with verified links and hours, so questions about what you did can be answered concisely from the saved record. The schedule only saves these lists; the app does not create, link or change Azure tasks.

The app and its answers to Teams follow the configured language, Spanish or English (`llm.language`); the CLI and audit log are in English. The GUI also works on phones: the web portal has a floating tab bar, follows the system's light or dark mode (or the one you choose in Settings › General) and installs on the home screen with its own icon.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-chat-dark.jpg">
    <img src="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-chat-light.jpg" width="640" alt="Test chat: a review of the work done in the last two weeks that no task records, with the user story each piece could go under.">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-phone-activities-dark.jpg">
    <img src="https://raw.githubusercontent.com/elvisbrevi/personal-teams-assistant/main/site/public/assets/app-phone-activities-light.jpg" width="190" alt="Activities on a phone: review a day, the hours registered against the daily target and the activities that need your input.">
  </picture>
</p>

You can also register work on a schedule: once a day or every interval in a time window. Select your Azure DevOps activity (commits, pull requests, pipeline runs, releases and stage approvals), Wiki creation/edits and Teams messages/call sources, and a separate work-item write credential. The app proposes or creates **Task** items under the matching HU or bug that is not closed (in development counts), assigned to you, with a description, effort in hours, evidence links and the fields required for **Done**. It deducts hours already registered from the daily target (9 by default), groups related artifacts and never invents work to fill the remaining hours. Unclear descriptions, effort, HUs or calls produce a system notification that opens a separate review window with suggestions and an **Other / more context** field. The **Activities** tab shows pending decisions and history by day. Everything is also available through `pta`; see the [activity registration guide](desktop/skills/personal-teams-assistant/references/activity-registration.md).

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

### Web and phone access

The same GUI can run in a browser, including settings, messages, activity decisions
and history, knowledge and the test chat. Each login has its own Teams account,
configuration, credentials and data. Create users with `pta web users add USER`
and associate their verified GitHub identities locally with `pta web users bind USER`,
configure Cloudflare Access and the HTTPS origin with `pta web configure`,
and run `personal-teams-assistant --web` on an always-on machine behind Cloudflare
Tunnel. `--current-profile` gives one account access to the existing desktop profile
without copying its history or reconnecting Teams. See the
[web and Cloudflare setup guide](desktop/skills/personal-teams-assistant/references/web-access.md).

### Update

Versions 0.7.25 to 0.7.27 give the app a new look: a «sticker sheet» with ink
outlines, one color per section and the assistant's face, which shows its state on
Home (awake while it runs, reading aside while it observes, asleep when stopped).
Home leads with outcome tiles that open Messages filtered; on phones the sections
sit in a floating tab bar. Settings › General › Appearance chooses Light, Dark or
the system's mode on each device. The web portal now has an icon and a web app
manifest: on an iPhone, remove the home-screen shortcut and add it again from
Safari to get the new icon. Nothing changes in the assistant's behavior,
configuration or CLI.

Version 0.7.13 answers status rounds from the work item itself: an HU named in
the question is read even after it was handed over (its state, since when and
who holds it now, its tasks, linked code and comments), and your own recent Wiki
pages, such as a production deployment plan, give commitments and blockers. A
withheld or observed answer can now be improved with your context and sent by
hand from the Messages tab or with `pta answer refine|send`. Version 0.7.14 also
recognizes «PAP» deployment pages and keeps their installation date and pending
production items in the answer, describes a named HU only when you took part in
it, never looks back more than five weeks, and adds the work recorded in the
Activities section, by day, to status answers and activity reviews.

Version 0.7.15 drops the «Fuentes» and «Páginas consultadas» sections: each work
item, pull request, pipeline, stage or Wiki page is linked on the words of the
answer that name it («[#26477 — PR Pipeline Transversal](…)», «[PR !4848](…)»),
and a pull request named in a status question is read so it can be linked. The
full list of cited references and Wiki pages read stays in the message history
(Messages tab and `pta audit show`).

Version 0.7.23 adds an MCP server for agents: `pta mcp` exposes every operation of
the app — messages, answers, sources, contexts, cases, notes, people, ratings and
activity registration — to Claude Code, Codex or any MCP client (`pta mcp config`
prints how to add it). Sending to Teams, writing to Azure DevOps, authorizing sources
and changing the mode need your confirmation, asked by the client itself when it
supports it.

Version 0.7.22 lets the assistant improve from what you approve. In Messages,
rate an answer (good or needs work, with a comment), keep a good one as an example
for similar requests (it guides structure and tone, never facts), or save it as a
verified note. Knowledge › People says how you address each person; the assistant
follows it only when answering them. Agents can do the same with `pta answer
feedback` and `pta people`.

Version 0.7.21 makes the kinds of request configurable. In Knowledge › Cases (or
`pta cases`), each built-in case — every answer, status round, what you did,
activity review, documentation, request for you, informational message — shows how
the model recognizes it and how it is answered; you can change those texts, add
words that identify it, choose the sources it reads or turn it off. The rules on
evidence, links and sensitive data stay. A context with a description is now also
recognized by meaning.

Version 0.7.20 adds a knowledge index. A whole repository can be a source of its
own, with its description: each answer reads the sections that best match the
request. Verified notes (Knowledge › Notes, `pta notes`) are searched the same way,
and contexts such as «billing» group the sources, keywords and instructions of one
subject: a request that names it reads that context's sources. Search is full-text
(SQLite FTS5), and also by meaning with an optional embeddings endpoint (Settings ›
Advanced › Semantic search).

Version 0.7.19 makes the activity record a source of its own: in Knowledge,
**Add activity record** lists what you approved in Activities as a source you
enable, authorize for chosen chats and pick on its own when answering a message.
Answers about what you did, status rounds and activity reviews read it from that
source (maps without one keep reading it through Azure DevOps activity). The
Azure DevOps catalog (organizations, projects and your author email) can now live
in the map itself (`catalog`), so no knowledge repository is required.

Version 0.7.18 lets you answer what the assistant left unanswered: an informational
message or a request for you shows **Generate answer** in Messages. The assistant
writes your reply from the conversation (and the sources you choose, or those of
the chat); then you can add context and **Regenerate answer**, or **Send answer**
once. Any held answer can use chosen sources too (`pta answer refine RESOURCE
--source ID`).

Version 0.7.17 answers «¿qué hice ayer?», «del 5 al 9 de octubre» or "what did I
do last week?" from the work you approved in Activities: a short day-by-day reply
with each task, its HU and hours. Activities adds a History view of that record,
grouped by day between two dates (`pta activity history FROM..TO`). On phones the
page no longer scrolls sideways or bounces, Settings sections are a grid instead
of a sideways strip, controls are at least 44 px and the colors were retuned for
contrast in both themes.

Version 0.7.16 reorganizes the GUI around daily work: Home, Messages and
Activities first, then Knowledge and Test chat, with Settings at the bottom and
split into categories (General, Microsoft Teams, Language models, Activity
registration, Credentials, Advanced). Home's status rows open the setting each one
depends on, Messages can be searched, fields that do not apply stay hidden, and
the interface meets WCAG 2.2 AA contrast. Nothing changes in the assistant's
behavior, configuration or CLI.

Version 0.7.12 reviews explicitly mentioned activity even when its artifacts have
a different date. Such work stays pending until you explain why it belongs to the
selected day, through the GUI or `date_attribution` in the CLI. Existing task links
and saved decisions still prevent duplicate registration. For Chilean work days on
a server, select `America/Santiago` instead of the server's local time zone.

Version 0.7.0 replaces the web portal's password login with Cloudflare Access
and a dedicated GitHub OAuth App. Before updating a password-based portal,
prepare Access using the [migration guide](desktop/skills/personal-teams-assistant/references/web-access.md),
update the host and CLI together, then associate existing users with their
verified GitHub identities.
The native app, CLI, Microsoft session and existing profiles are preserved.

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
pta activity status              # scheduled/manual activity process
pta activity run                 # review today's work with the saved settings
pta activity pending             # activities needing context or an HU
pta activity history 2026-10-08   # completed tasks and process outcomes by day
pta activity history 2026-10-01..2026-10-15   # the activities of every day in a range
```

`pta help` (or `pta`, `pta --help`) lists every command with its description; `--json` returns the full result as JSON. `pta skill show` gives the operating manual for agents.

## Minimal setup

1. Credentials (on stdin, never as an argument): `DEEPSEEK_API_KEY` if you use DeepSeek and the ones your sources reference, e.g. `pta credentials set DEEPSEEK_API_KEY < file`. `GRAPH_WEBHOOK_SECRET` and `STATE_ENCRYPTION_KEY` are generated.
2. Language models: in the GUI (Settings → Language models) add and order models from Codex, Claude Code, OpenCode 2 and DeepSeek (providers may repeat, up to 16 entries), with their model and effort. Codex, Claude and OpenCode use the installed CLI (`codex`, `claude`, `opencode` version 2) with its own login. Default: Codex `gpt-6.1-sol` (medium) → Claude `claude-opus-5-5` (medium) → DeepSeek `deepseek-flash` (maximum). Every call starts with the first and moves to the next only if it fails (e.g. out of credits). From the CLI: `pta llm providers` and `pta config set llm.chain JSON`.
3. An Entra registration with the *Mobile and desktop* platform (`http://localhost`) and the delegated permissions `User.Read`, `Chat.Read`, `ChatMessage.Send`, `offline_access`. Set `graph.tenant_id` and `graph.client_id`.
4. Public URL (`server.public_url`) and tunnel: `pta tunnel configure token|file PATH|external`.
5. Knowledge: add repositories (`pta repos add`, or GitHub with `pta auth github login`), register sources with `pta sources add` (they start disabled and without audiences) and authorize them per conversation with `pta sources audience`.

## Documentation

- Project site: [teams-assistant.elvisbrevi.cl](https://teams-assistant.elvisbrevi.cl/), HTML and CSS in [`site/`](site/) published as a Cloudflare Worker.
- [Architecture](docs/architecture.md): components, pipeline, data, security, CLI contract and change recipes.
- [Proposed improvements](docs/proposed-improvements.md).
- [Operating skill](desktop/skills/personal-teams-assistant/SKILL.md) and [guide for agents](AGENTS.md).
- [Activity registration](desktop/skills/personal-teams-assistant/references/activity-registration.md): scheduling, required task fields, calls, clarification and CLI commands.

## License

MIT.
