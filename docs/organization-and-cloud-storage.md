# Organization layer and per-account cloud storage

Design proposal, 2026-11-02. Phase 1 (analysis) of the move described in §14 of the
[architecture](architecture.md#14-current-decisions-and-why). Nothing here is implemented yet;
the decisions in [§11](#11-decisions-the-owner-must-take) must be taken before the phases that
depend on them start. Cloudflare limits and prices were read from the official documentation on
2026-11-02 ([sources](#sources)); re-read them before each implementation phase.

## 1. Goals and non-goals

Goals:

1. **Organization layer, generic and optional.** What belongs to the user's employer (norms,
   reference architecture, process conventions, work item guides, glossary, templates) is data,
   never code. The repository offers one generic model, *Organization*, and never names a real
   organization: the user can change employer and keep the same flow, and organization specifics
   change over time. The layer may not exist at all.
2. **Per-account data in Cloudflare.** Configuration, knowledge and history live in Cloudflare,
   one isolated database per assistant account, with the organization layer as a separate,
   optional and detachable part.

Non-goals (unchanged invariants): secrets, the Microsoft session and its vault, provider sessions
(Codex, Claude, OpenCode, GitHub, Azure CLI), checkouts, execution roots and the machine fleet
stay local. No secret leaves in clear. No Graph scope or OAuth change. `Config`/`KnowledgeMap`
only gain `#[serde(default)]` fields. New sources start disabled. CI keeps no secrets and standard
runners only. The Teams pipeline never waits on Cloudflare to answer.

## 2. Inventory of current stores

Sizes are the production profile's on 2026-11-02 (one account, about six weeks of use).
Classes: **U** user data, **O** organization data (today mixed into user data), **C** cache
derived from another system, **S** secret, **R** local runtime state (meaningful only to the
process or machine that wrote it).

| Store | Contents | Size | Written / read by | Class | Destination |
| --- | --- | --- | --- | --- | --- |
| `config.toml` | `Config`: language, style, model chain, policy, `[secrets]` references, server/Graph IDs, tunnel, embeddings, message style, activity schedule | 4 KB | host (`commit_settings`), GUI/CLI/MCP | U + R | portable part → cloud (already an allowlist in `portable.rs`); bind, URLs, paths, tunnel, Graph IDs, `[secrets]` stay local |
| `knowledge-map.toml` | sources, contexts, questions and associated requests, people, `work_item_guides`, `agent_prompts`, repository bindings, legacy cases | 36 KB | host, GUI/CLI/MCP | U + O | sources/contexts/questions/people/prompts → account cloud store; guides → organization layer; `repositories` bindings stay local |
| `assistant.db` › `jobs` | every Teams message handled: status and the audit JSON (question, evidence trace, proposal, answer, feedback) | ≈5.6 MB (with WAL) | pipeline worker; GUI/CLI `audit` | U (sensitive) | audit history → cloud; in-flight rows (`pending`/`processing`/`sending`) stay local |
| `assistant.db` › `vault` | Microsoft OAuth tokens sealed with `STATE_ENCRYPTION_KEY` | small | OAuth adapter | S | local, never uploaded |
| `assistant.db` › `subscriptions`, `outputs` | Graph subscriptions; send nonces (single-send guarantee) | small | runtime | R | local |
| `assistant.db` › `conversation_context` | last question/answer per conversation | small | pipeline | U (sensitive) | local (short-lived context, regenerable from Teams) |
| `assistant.db` › `events` | diagnostics event log (`pta logs`) | small | host | R | local |
| `assistant.db` › `activity_cache` | Azure DevOps repository/activity index cache | small | `ado` | C | local cache |
| `knowledge-index.db` › `notes`, `question_feedback`, `examples` | verified notes, answer evaluations, approved examples | 72 KB total | GUI/CLI/MCP, pipeline | U | cloud |
| `knowledge-index.db` › `documents`, `chunks`, `chunks_fts`, embeddings | FTS5 passages of repository sources and notes, optional vectors | (same file) | index refresh | C | local derived cache, rebuilt from the cloud records and local checkouts |
| `activity-registration.db` | daily task snapshots, runs, legacy decisions and claims | 1.3 MB | activity scheduler; Activities, recaps | U | cloud |
| `todo-cache.db` | Azure process metadata (types, states, fields) | 1.7 MB | To-Do | C | local cache |
| `activity-cache.db` | legacy evidence cache | 0.4 MB | legacy | C | local (may be deleted later) |
| `desktop-chat.db` | local test chat and simulations | 2.3 MB | local chat | U (low value) | local by default; optional upload later |
| `implementation/runs.db` | agent runs, events, session index (coordinator copy of enrolled machines), request runs | 0.3 MB | implementation host | U + R | finished runs and request runs → cloud; in-flight intents and send ledger stay local until finished |
| `implementation/registry.json`, `connection.json`, `installation.json`, `requests/` | machine settings and roots, coordinator link, installation key, request scratch directories | small | implementation host | R + S | local |
| `variables/variables.db`, `keys/master-N.key` | account variables (ciphertext), audit, bindings; wrapping keys | 0.6 MB | variable operations | S | local, never uploaded |
| `variables/catalog-mirror.json`, `account-bindings.json`, `microsoft-setup-account.json`, `launchers*.json`, `deliveries*.json` | variable catalog mirror and references, launchers, delivery destinations | small | consumers, deliveries | R (references to secrets) | local |
| `portable/revision.json`, `replica.json`, `backups/`, `account-content` | account configuration revision, replica status, backups | small | portable sync | R | local (the cloud store replaces their role; see §7) |
| `web/*.json` (`settings`, `accounts`, `access-revocations`, `machine-logins`, `machine-pairings`, `resume/`) | portal owner, account registry, revocations, enrollment requests | 0.2 MB | portal | R (identity) | local; the account registry gains the cloud account ID |
| `tunnel.json`, `control.json`, `control.lock`, `host-path.txt`, `cloudflared-path.txt`, `settings-rollback.json`, `maintenance/` | process descriptors, tunnel, journals, update journal | small | host | R | local |
| `credentials/` (Linux) or Keychain / Credential Manager | `STATE_ENCRYPTION_KEY`, `GRAPH_WEBHOOK_SECRET`, `DEEPSEEK_API_KEY`, tunnel token, GitHub tokens, `PTA_MACHINE_TOKEN`, … | small | security | S | local |
| `repositories/`, `skills/` | Git checkouts, installed skill | varies | GitHub, skill | R | local |

In short: about 9 MB of user and organization data for six weeks of one account; everything
classified U or O migrates, caches are rebuilt locally, secrets and runtime state never move.

## 3. Organization-specific remnants in the repository

A broad search (organization, domain, project, repository and people names, custom process
types, fields and states, real work item and pull request numbers) found no organization name,
domain or Azure DevOps organization URL, but it found the following. They are listed here by
category only; the concrete strings were reported to the owner privately and are not repeated in
the repository or its pull requests (the repository is public).

| Category | Where | Kind |
| --- | --- | --- |
| Repository names carrying the employer's prefix | `src/ado/progress.rs` (tests), `src/ado/wiki.rs` (tests), `src/evidence.rs` (doc comments and tests) | test data, doc comments |
| Real project and system names (team projects, a payments portal, a QR payments story) | `src/ado/progress.rs`, `src/ado/wiki.rs`, `src/ado/review.rs`, `src/activity/recorded.rs`, `src/evidence.rs`, `tests/integration.rs`, `README.md`, `docs/architecture.md` §6 and §14 | test data, examples, decision history |
| A real colleague's name | `src/ado/progress.rs`, `tests/integration.rs`, `desktop/ui/app.js` (a placeholder) with its `i18n.js` entry | test data, **GUI text** |
| Real work item and pull request numbers and titles | `src/ado/progress.rs`, `src/ado.rs`, `src/evidence.rs`, `tests/integration.rs`, `README.md`, `docs/architecture.md`, `src/llm/prompts.rs` | doc comments, tests, prompt example |
| The organization's custom process types (a development bug type in the task category, a documentation and a test task type) | **`src/ado/todo.rs:139` (production logic)**, **`desktop/ui/app.js:205,763` (production logic)**, `src/app/tools/mod.rs` (tool descriptions), `src/index.rs` (tests), `scripts/test-activity-ui.cjs`, the skill's `tools-and-todo.md`, `docs/architecture.md` §11 and §16 | code, tool descriptions, docs |
| The organization's custom fields and workflow states (month, effort, «sent to validation») | `src/ado/implementation.rs`, `src/ado/registration.rs`, `src/work_items.rs` (tests and a doc comment), the skill's `tools-and-todo.md`, `tests/integration.rs` | tests, docs |
| Organization phrases used as examples (hotfix registration rule, a «collections» term) | `knowledge-map.example.toml`, `scripts/ui-preview/mock.js`, `scripts/test-ui-i18n.cjs`, the skill's `tests-and-diagnostics.md` | examples |

Two are behavior, not data, and need a generic rule:

- **To-Do lists one specific task-category type together with bugs** (`src/ado/todo.rs:139`).
  Generic replacement: a work item guide says how its type is listed (new optional guide field,
  e.g. `todo = "bug"`); without a guide only the requirement and bug categories are listed, as for
  any process. The production guide for that type gets the field as data in the same delivery.
- **The GUI decides "this is a task" from a fixed list of four type names**
  (`desktop/ui/app.js:763`) and from one name (`:205`). Generic replacement: To-Do items already
  come from process categories in `ado/todo.rs`; they carry an additive `category`
  (`requirement`, `bug`, `task`) and the GUI uses it. `Microsoft.TaskCategory` decides.

The repository owner's own name and GitHub handle (package metadata, site, README links) are not
organization data and stay.

## 4. Data model

### Owners

| Owner | What it is | Owns |
| --- | --- | --- |
| **User** | A person, identified by the immutable provider identity the portal already binds (Access + GitHub subject). | One assistant account today. |
| **Account** | The assistant account (today: a portal account with its own profile, host and coordinator). The unit of tenancy and of the cloud database. | Portable settings, sources (definitions, never credentials), contexts, questions and associated requests, verified notes, answer evaluations, approved examples, person profiles, agent instructions, activity records, message audit history, finished implementation and request runs. |
| **Organization** (optional) | The employer's shared conventions. 0 or 1 per account; N accounts to 1 organization. | Work item guides, norms, reference architecture, process conventions, glossary terms, templates, and the list of sources that document the organization. Default agent instructions may come from it later. |
| **Installation** | One computer's host and profile. | Paths, repository bindings, roots, tunnel, bind and public URL, Graph IDs and vault, every secret and provider session, the job queue's in-flight rows and send nonces, caches. |

An account without an organization keeps its work item guides in its own layer: the layer is
optional, the guides are not lost when it is absent. Leaving an organization removes its entries
from the installation's cache (as `portable::leave` does for an account) and never copies them
into the account.

### The organization layer

The layer is a set of **organization entries** plus references to **sources** that document the
organization. It does not introduce a second way of reading documents:

- **Entries** are stored records with a `kind`: `work_item_guide` (the existing
  `[[work_item_guides]]` structure, unchanged), `norm`, `architecture`, `convention`,
  `glossary_term` and `template`; each with `id`, `title`, `text` (bounded Markdown),
  optional `projects` (empty = all, as guides already work) and `updated_at`.
- **Providers** say where the content comes from. An entry provider is the store itself (own
  entries). A Wiki, repository or index provider is **an existing source referenced by ID**
  (`azure_devops_wiki`, `repository`, `file`, `url`), so access, audiences, external processing
  and the code checks stay exactly those of sources. The organization only labels those sources as
  "documentation of the organization" so the implementation agents and the GUI can find them.
- **Use in Teams answers** goes through one new source kind, `organization`, analogous to `notes`:
  it reads the organization entries from the knowledge index. Like every new source it starts
  disabled, without audiences and without external processing. Contexts can reference it like any
  source. Entries reach models as untrusted data.
- **Use by implementation agents** keeps today's rules for guides (§16) and adds the norms,
  architecture and conventions of the parent's project as untrusted input. A guide or norm never
  authorizes a destination, type, field or value; code still checks them.

Compatibility: `[[work_item_guides]]` keeps its name and shape (fields cannot be renamed); the new
`[organization]` section of `KnowledgeMap` (`#[serde(default)]`, omitted when empty) holds
`name` (display only), `entries` and `sources` (source IDs). Guides therefore need no local
migration: the Organization view shows them as its `work_item_guide` entries. In the cloud they
are stored in the organization database when the account belongs to one, otherwise in the
account's.

### Proposed vocabulary (for `CONTEXT.md` once implemented)

- **Organization**: the optional, shared conventions of the user's employer — guides, norms,
  architecture, conventions, glossary and templates — kept as data and detachable from the
  account. _Avoid_: company, tenant (Microsoft's), workspace.
- **Organization entry**: one stored item of the organization: a work item guide, a norm, an
  architecture description, a convention, a glossary term or a template. _Avoid_: rule, policy
  (code enforces policy, not entries).
- **Organization source**: a source the user marked as documenting the organization (a Wiki, a
  repository, a file or a URL); its access stays that of the source. _Avoid_: provider (in
  user-facing text).
- **Account store**: where an account's configuration, knowledge and history are kept: the local
  profile, or the account's cloud database with the local profile as its replica. _Avoid_: cloud
  sync, backup.
- **Local replica**: an installation's readable copy of the account store, plus the caches
  derived from it; it answers while offline. _Avoid_: offline mode.

"Work item guide" keeps its meaning and becomes a kind of organization entry.

## 5. Cloudflare options

Verified on 2026-11-02 in the official documentation ([sources](#sources)).

| | D1, one database per account | Durable Objects with SQLite, one object per account | R2 | Vectorize | KV |
| --- | --- | --- | --- | --- | --- |
| Fit | per-account SQL database | per-account SQL database inside a single-threaded object | blobs: backups, exports, attachments | vector search | edge cache |
| Count limits | Free 10 databases; Paid 1,000,000 (100,000 included, then $0.10/month each) | objects unlimited on both plans; classes 100 (Free) / 500 (Paid) | — | indexes 100 (Free) / 50,000 (Paid default); 50,000 namespaces per index (Paid) | namespaces 1,000 |
| Size limits | 500 MB (Free) / 10 GB (Paid) per database, not raisable; account 5 GB / 1 TB | 10 GB per object (the FAQ says 1 GB on Free); account 5 GB (Free) / unlimited (Paid) | — | 1536 dimensions, 20 M vectors per index, 10 KiB metadata | value 25 MiB |
| Row / statement | 2 MB row, 100 KB statement, 100 bound parameters | 2 MB row, 100 KB statement | — | — | — |
| How the Worker reaches a per-account instance | bindings are static in `wrangler` config; a database created at runtime is reachable only through the REST API with an account API token, or after redeploying with a new binding | `idFromName(account)` from one binding; created on first use, no API token, no redeploy | binding | binding | binding |
| FTS5 | yes | yes | — | — | — |
| Consistency | single primary per database; read replicas (sessions API) in every region | strongly consistent, single-threaded per object, synchronous `sql.exec`, `transactionSync` | strong per object | eventually consistent | eventually consistent (≈60 s) |
| Backups | Time Travel: 7 days (Free) / 30 days (Paid); **export is not supported for databases with virtual tables (FTS5)** | point-in-time recovery, 30 days (`getBookmarkForTime`, `onNextSessionRestoreBookmark`) | lifecycle rules | — | — |
| Location | jurisdictions `eu`, `us`, `fedramp` (at creation); hints exclude South America | jurisdictions `eu`, `us`, `fedramp`; hint `sam` spawns in eastern North America | jurisdictions | — | global |
| Price (Paid) | rows read 25 B/month, written 50 M/month included; storage 5 GB included then $0.75/GB-month | same row prices; requests 1 M/month then $0.15/M; duration 400,000 GB-s; storage 5 GB-month then $0.20/GB-month | $0.015/GB-month; free tier 10 GB, 1 M class A, 10 M class B; no egress | stored and queried dimensions | 10 M reads, 1 M writes |
| Free plan | 5 M rows read / 100,000 written per day, 5 GB | same, plus 100,000 requests/day, 13,000 GB-s/day | 10 GB | 5 M stored dimensions | 1 GB |

Workers Paid costs a $5/month account minimum and includes 10 M requests and 30 M CPU ms; Free
has 100,000 requests/day and 10 ms of CPU per invocation.

**Recommendation: one Worker with SQLite-backed Durable Objects** — an `AccountStore` object per
account, an `OrganizationStore` per organization and a small `Registry` object (identities,
installation credentials, memberships) — plus one R2 bucket for encrypted backups and exports.
No Vectorize and no KV for now.

- **Isolation by construction.** The object is the database: a request routed to
  `AccountStore(account)` cannot read another account's rows, and the Worker derives `account`
  only from the authenticated credential (§6).
- **No privileged API token.** Per-account D1 would need the Worker (or the host) to hold a
  Cloudflare API token able to create and query every database, or to redeploy for each new
  account. Durable Objects need neither.
- **Same engine as today.** SQLite with FTS5 and JSON; synchronous transactions serialize writes
  per account, which is what the host's operations lock does now.
- **Backups.** 30-day point-in-time recovery on both plans, and R2 exports that D1's export
  limitation with FTS5 would block.
- **Why not Vectorize.** Embeddings are optional and off in production; the index is 72 KB and a
  brute-force cosine scan in the local replica is enough for years. Vectors in clear also leak
  meaning (embedding inversion), which conflicts with §6. Revisit only for a large, clear
  organization corpus.
- **Why not KV.** Caches stay next to the code that uses them, locally.
- **Latency.** Requests from the server and the computers reach the nearest Cloudflare location
  and then the object's home (for an account created from South America, likely eastern North
  America: there is no South American placement). Expect on the order of 100–200 ms per round
  trip; phase 3 measures it before anything depends on it. The pipeline and the GUI read the local
  replica, so latency only affects writes and synchronization.
- **Cost for one account.** Well inside the Free plan (thousands of requests and writes a day at
  most, megabytes of storage). Paid ($5/month) mainly buys headroom, 1 TB of account storage and
  longer limits; it is the owner's call (§11).

## 6. Security and privacy

The stores hold Teams message content, answers and audits, internal documentation passages,
person profiles and activity records. Data classes:

| Class | Examples | In Cloudflare |
| --- | --- | --- |
| Secrets | variable values and keys, credentials, OAuth vault, machine tokens, provider sessions | never |
| Sensitive content | audits, answers, notes, evaluations, examples, people, activity records, run prompts and outputs, organization entries | **encrypted by the client** (recommended; §11 decides) |
| Configuration structure | sources (URLs, tool catalogs with organization URLs and emails), contexts, questions, prompts | encrypted by the client |
| Operational metadata | record IDs, collection, revision, content digest, sizes, timestamps, schema versions | clear (needed for synchronization, never content) |

**Client-side encryption.** Each record is sealed by the host with AES-256-GCM-SIV (the existing
dependency) under a per-account **content key**; the associated data binds a domain string, the
account, collection, record ID and revision, so ciphertext moved to another account, record or
revision fails authentication (the same construction as account variables). The content key is
random, never sent to Cloudflare, kept in the installation's private `keys/` directory, given to a
new installation only at trust time (the existing browser «Trust this machine» approval) and
exportable with the existing operator-local key backup. Rotation re-encrypts the account (small
data: seconds). Destroying the key crypto-shreds the cloud copy and its R2 backups at once.

Cost of encryption: **no server-side search**. FTS5 and vectors run in the local replica, which
is rebuilt from decrypted records (the current index is already a derived cache, so this is the
existing model). Blind-index tokens (keyed hashes of words) would allow server search but leak
word frequencies; not recommended. Without client-side encryption, Cloudflare (and anyone with
the Cloudflare account or a Worker bug) can read everything, and server-side FTS becomes possible;
that only pays off with many installations that cannot hold a replica, which is not the case.

Trust model: the same as account variables (ADR 0001). The host that holds the content key
decrypts for unattended operation; this protects against Cloudflare and its account, not against
a compromised server.

**Organization store.** Shared between accounts, so a key would need distribution to every
member (per-member key wrapping). Two options for §11: (a) encrypted with an organization key
wrapped per member, or (b) clear, because norms and guides are less sensitive and clear text
allows server search across members. Until there are other users the organization lives inside
the account store, encrypted with the account key.

**Installation authentication.**

- *Edge:* the Worker's hostname is a Cloudflare Access application with a Service Auth policy;
  each account has an Access service token (headers `CF-Access-Client-Id` /
  `CF-Access-Client-Secret`, chosen duration, renewable). Unauthenticated traffic never reaches
  the Worker. The Worker still validates the `Cf-Access-Jwt-Assertion` (team JWKS and audience).
- *Application:* a Worker-issued installation credential (256 random bits; the `Registry` keeps
  only its SHA-256), bound to account, installation and account version, issued after the
  existing trust confirmation, revocable per installation — the same shape as today's machine
  credential, stored in the credential store under a new name (`CLOUD_STORE_TOKEN`). mTLS was
  considered and rejected: it adds a certificate authority and issuance for no isolation gain.
- *Authority:* `authenticate(request) → Principal {account, installation, organization roles}`
  is the only source of identity. Paths and bodies never carry an account ID; the stub is
  `AccountStore.idFromName(principal.account)`; an organization request is admitted only if the
  `Registry` lists the account as a member (role `member` reads, `admin` writes). No endpoint lists
  or reads across accounts. Contract tests include cross-account attempts.
- *Logs:* the Worker logs method, route, status and duration only; never bodies, headers or IDs
  beyond an opaque request ID.

**Retention and deletion.** Audit history keeps today's behavior (no automatic expiry) with an
optional retention window per collection. Deleting a record writes a tombstone (as variables do),
so an old replica cannot resurrect it. Deleting the account erases the object (`deleteAll`) and
its R2 prefix; point-in-time recovery can still restore it for 30 days, which the GUI states; with
client-side encryption, destroying the content key makes that window irrelevant.

## 7. Offline and consistency

- **Reads are always local.** Each installation keeps its replica (the current files and
  databases become exactly that) and the caches derived from it. Offline, everything stays
  readable, as account standing already promises.
- **Append-only records** produced by one installation (message audits, activity snapshots,
  finished runs, evaluations and examples) are written locally first, as today, and pushed by a
  durable **outbox** as idempotent upserts keyed by collection, record ID and revision. Retrying a
  storage write is safe; this is unrelated to the never-retry rule for Graph sends, which is
  untouched (send nonces and in-flight jobs never leave the installation).
- **Shared mutable configuration** (settings, sources, contexts, questions, notes, people,
  prompts, organization entries) keeps the rules of today's account configuration: a write needs a
  live answer and names the revision it read (compare-and-swap); a stale revision is a conflict
  resolved by synchronizing; nothing is queued. Offline, configuration is read-only on a connected
  installation, as now.
- **Revisions.** Each record carries a monotonically increasing revision and a content digest;
  each collection a generation, like `portable/revision.json`. A replica pulls what changed since
  its last generation.
- **Standing.** Cloud exchanges report `live`, `offline` or `revoked` through the existing
  standing model (a new link kind), so the GUI and `setup_status` show one coherent state.
- **Coordinator.** Variables, machines, the Teams lease and the coordinator's other duties stay
  where they are. The portable configuration protocol (`portable_*`) reads from and writes to the
  account store once the cloud is primary, so every installation has the same source of truth
  without the coordinator being online.

## 8. Migration

Optional, per account, reversible, with a prior backup and no loss of history.

1. `pta cloud status` / `connect` (GUI: Settings › Account › Cloud storage; MCP `cloud_*`) enrolls
   the installation and shows the mode: `off` (default), `mirror` or `primary`, kept in an
   installation-local `[cloud]` section of `Config` (`#[serde(default)]`, not portable).
2. `pta cloud migrate --dry-run` lists per collection what would be uploaded (counts, sizes) and
   what stays local and why.
3. `pta cloud migrate` takes a **backup** first (SQLite online backup of each database plus the
   profile files into a private `backups/cloud-<time>/`, reusing the data-safety copies of the
   updater), uploads every collection encrypted, then **verifies by reading back**: per
   collection, the record count and a digest over sorted `(id, revision, ciphertext digest)`
   computed on both sides, plus a full decryption and comparison of every record. Only then does
   the mode become `mirror`. Local stores are never deleted.
4. `mirror` keeps local as the source of truth and pushes every change. `pta cloud verify`
   compares at any time. `pta cloud promote` switches to `primary` after a clean verification.
5. `pta cloud rollback` pulls everything to local, verifies, switches to `off` and leaves the cloud
   copy untouched until an explicit `pta cloud erase`.

Versions: the Worker exposes `/v1/…` with additive changes and reports its protocol and schema
versions; hosts advertise additive `cloud_store_support`, and a CLI or portal refuses `cloud_*`
to a host without it. The Worker keeps the previous protocol version working through a release,
and is deployed before the host release that needs it. An older host cannot load a profile with
`[cloud]` (the existing forward-only rule for profiles); it is not affected otherwise.

## 9. Deployment

- **Code.** A TypeScript Worker under `cloud/`, outside the Cargo package like `site/` (a
  `workers-rs` crate would break the single-package rule): `src/index.ts` (routing and
  authentication), `account-store.ts`, `organization-store.ts`, `registry.ts`, and
  `wrangler.jsonc` with the Durable Object bindings (`new_sqlite_classes` migrations) and the R2
  bucket.
- **Publishing.** Workers Builds connected to the repository with root directory `cloud/`, as the
  landing does: Cloudflare builds and deploys on merge to `main`, so GitHub holds no deploy secret.
  Non-production branch builds are **disabled** for this Worker (preview versions would share the
  production objects); a separate staging Worker with its own namespaces covers testing.
- **CI.** `ci.yml` gains a path-selected job: type check and unit tests of the Worker with Bun and
  the Workers test runtime, no secrets, standard runner.
- **Schema.** Each object stores `schema_version` and applies forward-only, additive migrations in
  its constructor inside `blockConcurrencyWhile`; destructive changes take two releases.
  Point-in-time recovery is the rollback for data, Workers versions/rollback for code.
- **`docs/delivery.md`** gains: the Worker's delivery path, the order Worker-then-host, the health
  check (`/v1/health` with version and schema), how to verify a deployment, and the manual
  fallback (`wrangler deploy` from a maintainer machine) when Workers Builds fails.

## 10. Phases

Each phase is one or more small pull requests with a version bump, GUI, CLI, MCP and skill in
parity, the `AGENTS.md` checks, and delivery verified on crates.io and on the server.

| Phase | Content | Risks |
| --- | --- | --- |
| 1a. Cleanup | Synthetic names in tests, docs, README, prompts, the GUI placeholder and catalogs; To-Do items carry `category`; the GUI decides by category; the task type listed with bugs comes from its guide (`todo = "bug"`); the production guide is updated as data in the same delivery. | The production To-Do stops listing that type until its guide carries the field: update it right after the install, verify the list. |
| 1b. Organization (local) | `[organization]` in the map (name, entries, source IDs); guides shown as entries; new `organization` source kind (disabled by default) and index documents; Settings › Organization replaces the current guides editor (no duplicate control); `pta organization …` with `pta guides …` kept as its guides subcommand; MCP `organization_*`; agents receive norms and architecture as untrusted input; skill, architecture and `CONTEXT.md`. Decide whether the organization travels with the account configuration (today guides do not). | Prompt size growth for agents (bound entries per run); a second place to edit guides if the old editor is not removed in the same PR. |
| 2. Storage abstraction | Repository traits per aggregate (settings and map, notes and feedback, audit history, activity records, runs, organization), the current local implementation as default, one shared contract test suite. No behavior change. | Broad refactor of hot paths: done store by store, each PR behavior-neutral and fully tested. |
| 3a. Worker and authentication | `cloud/` Worker, `Registry`, `AccountStore` schema v1 (encrypted records with clear metadata), Access service token, installation credential, health; Workers Builds; host `cloud status/connect`. | Access/Workers configuration mistakes: staging Worker first, contract tests including cross-account attempts. |
| 3b. Mirror | Outbox for append-only records, encrypted snapshots of configuration, `cloud verify`; local stays the source of truth. | Outbox growth while offline (bounded, visible in status); latency measured here. |
| 3c. Primary | Compare-and-swap configuration writes through the Worker; local replica; `portable_*` served from the account store. | Conflict handling regressions: reuse the portable rules and their tests. |
| 4a. Migration | `cloud migrate/promote/rollback/erase` with backup and read-back verification; production migrated when the owner decides. | Data loss: never delete local data; rollback rehearsed on a copy of the production profile first. |
| 4b. Shared organization | `OrganizationStore`, membership and invitations, the key or clear decision; leaving an organization clears its cache. | Key distribution complexity if encrypted; authorization bugs between accounts. |

## 11. Decisions the owner must take

1. **Cloudflare account, plan and cost.** Which account (the one that already runs the tunnel,
   Access and the landing?) and whether to stay on Free (enough for one account) or pay Workers
   Paid ($5/month minimum).
2. **Data residency.** No jurisdiction (placement near where objects are created: eastern North
   America from South America) or a fixed `us`/`eu` jurisdiction. It cannot change after creation
   without a migration.
3. **Client-side encryption versus server-side search.** Recommended: encrypt every content record
   on the client, search in the local replica. The organization store's choice (§6) can wait.
4. **Other users.** Whether accounts other than the owner's will use the cloud store and shared
   organizations; it changes the registry, invitations and the plan's limits.
5. **When to migrate production.** After phase 4a has been rehearsed on a copy.

Phases 1a, 1b and 2 depend on none of these and can start right away.

## Sources

- D1 [limits](https://developers.cloudflare.com/d1/platform/limits/),
  [pricing](https://developers.cloudflare.com/d1/platform/pricing/),
  [SQL statements and extensions](https://developers.cloudflare.com/d1/sql-api/sql-statements/),
  [import and export](https://developers.cloudflare.com/d1/best-practices/import-export-data/),
  [data location](https://developers.cloudflare.com/d1/configuration/data-location/);
  runtime bindings: [workerd discussion #3564](https://github.com/cloudflare/workerd/discussions/3564).
- Durable Objects [limits](https://developers.cloudflare.com/durable-objects/platform/limits/),
  [pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/),
  [SQLite storage API](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/),
  [data location](https://developers.cloudflare.com/durable-objects/reference/data-location/).
- [Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/) (Workers, KV,
  Vectorize), [Vectorize limits](https://developers.cloudflare.com/vectorize/platform/limits/),
  [R2 pricing](https://developers.cloudflare.com/r2/pricing/),
  [Access service tokens](https://developers.cloudflare.com/cloudflare-one/identity/service-tokens/).
