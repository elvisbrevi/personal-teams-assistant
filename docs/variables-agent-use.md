# Account variables and agents: analysis and design proposal

Status: **proposal, awaiting the owner's approval** (sections 3 and 4). Section 2 describes the
code as of 0.10.9. When the proposal is implemented, its content moves into
[architecture §3](architecture.md#account-variables) and the skill's
[variables reference](../desktop/skills/personal-teams-assistant/references/variables.md), and
this file is removed.

## 1. The case that motivated it

On 2026-11-01 an agent working on another project (`blog-editor`, a Cloudflare Worker) needed two
secrets the owner had already stored as account variables from the PWA (`BLOG_EDITOR_GITHUB_TOKEN`
and `BlogEditorOpenAiApiKey`). Four problems showed up:

1. The agent looked in `pta credentials list` first. That list is the installation's internal
   credentials, so the variables were not there.
2. A variable could not be renamed. The only workaround, creating a copy and deleting the
   original, needs the value to pass through the agent. **Fixed in 0.10.9** by `variables rename`.
3. There was no way to take the value from PTA to the editor's Settings page without the agent
   seeing it. The owner copied and pasted it by hand, and an agent-written handoff told them to run
   `wrangler secret put` and paste the value too.
4. The agent could not tell what each variable is usable for, from where, and with which
   confirmation.

## 2. How it works today

| Piece | What it does | Reference to the variable | Who approves |
| --- | --- | --- | --- |
| Store (`src/variables.rs`) | Name, description and AES-GCM-SIV ciphertext per entry. The AAD binds account, entry ID, **name**, revision and data-key version, so every metadata edit is resealed inside one host transaction | — | — |
| Catalog sync (`app/variables/sync.rs`) | Signed-in installations mirror names, descriptions and versions, never values. The coordinator decides every edit | ID + version | — |
| Built-in consumer `deepseek` | The host sends the key to a destination fixed in code | Entry ID (binding) | The user confirms `bind` / `check` |
| Application lookups by name | `DEEPSEEK_API_KEY` (unbound), `[secrets]` targets, `ENTRA_*`, `TEAMS_USER_ID` | **Name** | Configuration |
| Launcher | An approved local program receives the named variables as environment variables. Its output is not captured | Entry ID (`--env ENV=ID`), optional version pin | Window-only approval; the owner's trusted machine only. Denied to shared-portal accounts, browsers and machines |
| MCP / CLI | List, search, protected entry, edit, rename, delete, consumers, launchers. No operation takes or returns a value | — | Confirmation on `delete`, `bind`, `check`, `launcher_*` |

Gaps:

- **Discovery.** `variable_search` matches the whole query as one substring of the name or
  description, so "the editor's OpenAI key" finds nothing. No reply says whether a variable is
  usable from here and by which path. `credentials list` and `variables list` are easy to confuse.
- **Delivery.** The only generic path is a launcher, and on this server it is not available:
  - the server is headless, so no window can approve one;
  - the owner's portal session is a browser, and launchers are denied to browsers and portal
    accounts;
  - the VPS is not a trusted machine.

  Even where launchers work, the agent writes or controls the program that receives the value. The
  owner then has to read that program to trust it, and nothing stops the agent from changing files
  the program loads (`node_modules`, for example).
- **Guidance.** The skill says what never to do with a value, but not how to finish a task
  ("put this secret into system X") without it. Agents fall back to asking the user to copy and
  paste.

## 3. Proposal: discovery (point 2)

### 3.1 `variable_usage` (MCP) and `pta variables usage [QUERY]` (CLI)

The reply contains metadata only, and nothing in it ever carries a value. For each variable:

```json
{
  "id": "…", "name": "BLOG_EDITOR_OPENAI_API_KEY", "description": "…", "version": 3,
  "usable_here": {"state": "live|unknown|offline|revoked|signed_out", "trusted_machine": false,
                  "reason": "Launchers need the owner's trusted machine."},
  "paths": [
    {"kind": "consumer", "id": "deepseek", "bound": true, "confirmation": "check"},
    {"kind": "application", "reference": "secret://azure/reader"},
    {"kind": "launcher", "id": "deploy", "approved": true, "available_here": false},
    {"kind": "delivery", "destination": "blog-editor-openai", "approved": true, "confirmation": "deliver"}
  ]
}
```

- `usable_here` reuses `standing` and the launcher eligibility checks that exist today.
  `available_here` is false with a `reason` instead of being omitted, so an agent can tell the
  user what is missing.
- **Search.** Matching becomes token-based. The query and the name are split on spaces, `_`, `-`
  and camelCase. Words shorter than three letters and a short Spanish/English stopword list
  ("la", "de", "the"…) are dropped. A variable matches when every remaining query word is a prefix
  of a word in the name or description, ignoring case and accents. "la key de OpenAI del editor"
  then finds `BLOG_EDITOR_OPENAI_API_KEY` (`key`, `openai`, `editor`), and so does
  `BlogEditorOpenAiApiKey`. `variable_search` keeps its arguments and uses the same matcher.
  Results are ordered by the number of name matches.

### 3.2 Skill and MCP instructions

- In `SKILL.md` and the MCP instructions: "`pta credentials list` shows this installation's
  internal credentials. To answer 'do I have the secret X?', use `pta variables search` /
  `variable_usage`."
- A new section, *Use a secret without seeing it*:
  1. Search (`variable_usage QUERY`). Pick by description.
  2. If a path exists, use it: consumer, launcher or delivery. Each needs the user's confirmation
     of that exact action.
  3. If the variable exists but no path fits, say which path is missing and propose it (for
     example a delivery destination for the user to approve). Do not ask for the value.
  4. If no variable exists, start a protected entry (`variable_entry_request`).
  5. Never: ask for the value in the chat; ask the user to reveal or copy it for you; put it in
     argv, environment exports or files; or write handoff steps such as "run `wrangler secret
     put` and paste the value" when the secret is already a PTA variable. In that last case,
     reference the variable and the delivery instead.
  6. A delivered value can only be withdrawn by rotating it at its provider and then replacing the
     variable.

## 4. Proposal: delivery to another system (point 3)

### 4.1 Approved delivery destinations

A **destination** is a definition the host stores per account (`deliveries.json` beside the
launchers). The host itself performs the request. The value never passes through the CLI, MCP,
chat, argv, environment or logs.

```json
{
  "id": "blog-editor-openai",
  "description": "Blog editor: OpenAI key in Settings",
  "kind": "http",
  "url": "https://editor.elvisbrevi.cl/api/import/secrets/OPENAI_API_KEY",
  "method": "PUT",
  "headers": {"X-Blog-Editor": "1"},
  "body": {"value": "{{value}}"},
  "auth": {"header": "Authorization", "prefix": "Bearer ", "variable": "<entry ID of the import key>"},
  "variables": ["<entry ID allowed to go there>"],
  "confirm_field": "/last4"
}
```

- **Kinds.**
  - `http`: generic. HTTPS only. The URL, method (`PUT`/`POST`/`PATCH`) and headers are fixed;
    headers may not set `Host`, `Cookie` or `Content-Length`. The JSON body has exactly one
    `{{value}}` leaf and no other placeholder.
  - Built-in adapters written in Rust, with fixed endpoints and request shapes. They play the role
    of the "closed launcher" without running any program:
    - `cloudflare_worker_secret` (account ID, script, secret name, API-token variable) calls
      `PUT /accounts/{id}/workers/scripts/{script}/secrets` and replaces `wrangler secret put`;
    - `github_actions_secret` (repository, secret name, token variable; libsodium sealed box), in
      a later phase because it adds a crypto dependency.
- **Allowed variables.** A destination lists the entry IDs it may receive. An agent cannot send
  the OpenAI key to the destination approved for the GitHub token.
- **Approval.** An agent may only *propose* a destination (`delivery_define`, confirmation).
  - It is approved only by a person, in the application window or an authenticated portal browser
    session: the same transports that may reveal a value, and never the local channel, MCP, tools
    or machines.
  - The approval screen shows the full URL, method, headers, body template, auth variable and
    allowed variables.
  - The approval records the SHA-256 of the canonical definition. Any change voids it, including
    the URL, a header or an allowed variable.
  - Removing a destination needs no approval.
- **No local-command kind.** A destination that runs `wrangler`/`gh` from a project folder would
  hand the value to code the agent can modify. Launchers remain the tool for that, with their
  existing limits.

### 4.2 `variable_deliver` (MCP) and `pta variables deliver ID --to DESTINATION --confirmed`

The host performs these checks in order and stops at the first failure:

1. The caller transport is local, window or browser. Machines are refused.
2. `confirmed` is set, and the MCP asks the user.
3. The destination exists and is approved, its stored digest matches, and the variable is in its
   allowed list.
4. Rate limit: at most 1 delivery per destination per minute and 20 per account per hour,
   successful or not.
5. Live resolution of the value and of the auth variable, exactly like a consumer check. On a
   signed-in installation that means `variable_resolve` at the coordinator. Offline is denied and
   revoked is denied, with no fallback.
6. Network rules:
   - DNS is resolved once and the request is pinned to that IP;
   - loopback, private, link-local and CGNAT addresses are refused unless the destination was
     approved with `allow_private: true` by the owner (never by a shared-portal account);
   - redirects are refused;
   - timeouts are 10 s to connect and 20 s in total;
   - the response is read up to 64 KiB and then discarded.
7. The reply to the caller is `{destination, variable: {id, name, version}, status: ok|rejected|failed|unreachable, http_status, confirmed_by_destination}`:
   - `confirmed_by_destination` is true only when `confirm_field` exists in a JSON response and
     equals the last four characters of the delivered value, computed in the host;
   - the response body, its headers and the four characters are never returned.

   I recommend this boolean over echoing `last4`, which would give the agent part of the
   secret.
8. Audit: action `deliver`, the entry, the destination, the outcome and the HTTP status.
   Approvals and removals of destinations are audited too.
9. Nothing is retried automatically. After `unreachable`, the outcome is unknown: the agent asks
   the user to check the destination before repeating.

GUI: Settings › Variables › *Where variables are used* gains a *Delivery destinations* list
(approve, remove). Each row offers *Deliver now*, with a two-step confirmation and the same reply.

### 4.3 Risks and how the design answers them

| Risk | Answer |
| --- | --- |
| An agent registers its own server and sends a secret there | Destinations are only proposals until a person approves them in a human-only transport. Any change to the URL, headers or variables voids the approval. Each destination lists the variables it may receive |
| An agent alters the program that receives the value | No program runs: the host builds the request, and adapters are code in PTA |
| The response echoes the value | The body is never forwarded. Only the status and a host-computed boolean leave |
| Replay / flooding | `confirmed` per call, rate limit, audit, no automatic retry |
| SSRF into the server's own services | HTTPS only, IP pinning, private ranges refused by default, no redirects |
| Revocation | A delivered copy lives at the destination. Deleting or rotating the variable does not retract it: the skill and the GUI say "rotate at the provider" |
| Stale offline use | Every delivery is a live use, so it is denied when offline or revoked |

### 4.4 What `blog-editor` must offer (separate task in that repository)

`PUT /api/settings/secrets/:name` needs a browser session, so PTA cannot call it. Proposed
additions to the editor:

1. **A write-only import key** generated in Settings, under *Connect PTA*:
   - 32 random bytes, shown once; only its SHA-256 is stored;
   - it can be revoked and regenerated;
   - it is scoped to writing secrets: it cannot read them, list them or call any other API.

   The owner stores it in PTA through a protected entry (masked field). It becomes the destination's
   `auth.variable`.
2. **`PUT /api/import/secrets/:name`**:
   - accepts only the known secret names (`OPENAI_API_KEY`, `GITHUB_TOKEN`) and
     `{"value": "…"}` up to 16 KiB;
   - authenticates `Authorization: Bearer <import key>` with a constant-time comparison;
   - is rate-limited (for example 10 requests per hour);
   - stores the value with the same encryption as Settings;
   - replies `{"ok": true, "name": "…", "last4": "…"}`;
   - is logged in the editor's audit without the value.
3. CORS is not needed: the call comes from a server, and no `Origin` is required on this route.
   The session routes stay as they are.

A one-time token per delivery is safer still, but each delivery would then need the owner to type
a new token into PTA, which removes the benefit. The revocable write-only key is the recommended
balance: someone who steals it can only overwrite the editor's secrets, never read them.

## 5. Delivery plan after approval

1. PR *variables usage*: discovery, token search, skill/MCP text (section 3).
2. PR *delivery destinations*: the `http` kind and `cloudflare_worker_secret`, GUI approval, CLI,
   MCP, audit, tests with synthetic values and a mock server (section 4).
3. Later PR: `github_actions_secret`.
4. A prompt for the `blog-editor` session (section 4.4).
