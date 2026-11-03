# Organization layer and per-account cloud storage

Design, 2026-11-02. Phase 1 (analysis) of the move described in §14 of the
[architecture](architecture.md#14-current-decisions-and-why). The owner's decisions are in
[§11](#11-decisions); the phases in [§10](#10-phases) follow them. Cloudflare limits and prices were read from the official documentation on
2026-11-02 ([sources](#sources)); re-read them before each implementation phase.

Phase 1b (0.10.16) implemented the local organization layer: see §8 «Organization» and §16 of the
architecture, and the skill's knowledge and account-configuration references. Phase 2 (0.10.17–0.10.22)
put every account aggregate behind its own storage trait. Phase 3a is under way: the Worker under
`cloud/` (0.10.23) and its delivery path ([delivery](delivery.md#account-store-worker-cloud)), then
the host's `pta cloud status|connect` (0.10.25).

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
| `config.toml` | `Config`: language, style, model chain, policy, `[secrets]` references, server/Graph IDs, tunnel, embeddings, message style, activity schedule | 4 KB | host (`SettingsStore`), GUI/CLI/MCP | U + R | portable part → cloud (already an allowlist in `portable.rs`); bind, URLs, paths, tunnel, Graph IDs, `[secrets]` stay local |
| `knowledge-map.toml` | sources, contexts, questions and associated requests, people, `work_item_guides`, `agent_prompts`, repository bindings, legacy cases | 36 KB | host, GUI/CLI/MCP (`SettingsStore`; organization and guides: `OrganizationStore`) | U + O | sources/contexts/questions/people/prompts → account cloud store; guides → organization layer; `repositories` bindings stay local |
| `assistant.db` › `jobs` | every Teams message handled: status and the audit JSON (question, evidence trace, proposal, answer, feedback) | ≈5.6 MB (with WAL) | pipeline worker; GUI/CLI `audit` (finished rows: `AuditStore`) | U (sensitive) | audit history → cloud; in-flight rows (`pending`/`processing`/`sending`) stay local |
| `assistant.db` › `vault` | Microsoft OAuth tokens sealed with `STATE_ENCRYPTION_KEY` | small | OAuth adapter | S | local, never uploaded |
| `assistant.db` › `subscriptions`, `outputs` | Graph subscriptions; send nonces (single-send guarantee) | small | runtime | R | local |
| `assistant.db` › `conversation_context` | last question/answer per conversation | small | pipeline | U (sensitive) | local (short-lived context, regenerable from Teams) |
| `assistant.db` › `events` | diagnostics event log (`pta logs`) | small | host | R | local |
| `assistant.db` › `activity_cache` | Azure DevOps repository/activity index cache | small | `ado` | C | local cache |
| `knowledge-index.db` › `notes`, `question_feedback`, `examples` | verified notes, answer evaluations, approved examples | 72 KB total | GUI/CLI/MCP, pipeline (`NotesStore`) | U | cloud |
| `knowledge-index.db` › `documents`, `chunks`, `chunks_fts`, embeddings | FTS5 passages of repository sources and notes, optional vectors | (same file) | index refresh | C | local derived cache, rebuilt from the cloud records and local checkouts |
| `activity-registration.db` | daily task snapshots, runs, legacy decisions and claims | 1.3 MB | activity scheduler; Activities, recaps (`ActivityStore`) | U | cloud |
| `todo-cache.db` | Azure process metadata (types, states, fields) | 1.7 MB | To-Do | C | local cache |
| `activity-cache.db` | legacy evidence cache | 0.4 MB | legacy | C | local (may be deleted later) |
| `desktop-chat.db` | local test chat and simulations | 2.3 MB | local chat | U (low value) | local by default; optional upload later |
| `implementation/runs.db` | agent runs, events, session index (coordinator copy of enrolled machines), request runs | 0.3 MB | implementation host (finished runs: `ImplementationStore`) | U + R | finished runs and request runs → cloud; in-flight intents and send ledger stay local until finished |
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
domain or Azure DevOps organization URL, but it found real repository prefixes, project and system
names, colleagues' names, work item and pull request numbers, and the organization's custom types,
fields and states in tests, doc comments, examples, a prompt example, the README, the
architecture, the skill and one GUI placeholder. Phase 1a (0.10.15) replaced them with synthetic
names (`contoso`, `example`); the concrete strings were reported to the owner privately and are
not repeated in the repository or its pull requests (the repository is public).

Two were behavior, not data, and now follow generic rules:

- **To-Do listed one specific task-category type together with bugs.** A work item guide now says
  so (`listed_with_bugs`, Azure only); without such a guide only the requirement and bug categories
  are listed, as for any process. The production guide carries the flag as data.
- **The GUI decided "this is a task" from fixed type names.** To-Do items carry their type's
  process `category` (`requirement`, `bug`, `task`) and the GUI uses it: `Microsoft.TaskCategory`
  decides.

Generic Spanish and English words the code detects without a model (deployment pages, effort
fields, workflow states) stay, because users write in both languages. The repository owner's own
name and GitHub handle (package metadata, site, README links) are not organization data and stay.

## 4. Data model

### Owners

| Owner | What it is | Owns |
| --- | --- | --- |
| **User** | A person, identified by the immutable provider identity the portal already binds (Access + GitHub subject). | One assistant account today. |
| **Account** | The assistant account (today: a portal account with its own profile, host and coordinator). The unit of tenancy and of the cloud database. | Portable settings, sources (definitions, never credentials), contexts, questions and associated requests, verified notes, answer evaluations, approved examples, person profiles, agent instructions, activity records, message audit history, finished implementation and request runs. |
| **Organization** (optional) | The employer's conventions, kept by each account for itself (organizations are not shared between accounts, §11). 0 or 1 per account. | Work item guides, norms, reference architecture, process conventions, glossary terms, templates, and the list of sources that document the organization. Default agent instructions may come from it later. |
| **Installation** | One computer's host and profile. | Paths, repository bindings, roots, tunnel, bind and public URL, Graph IDs and vault, every secret and provider session, the job queue's in-flight rows and send nonces, caches. |

An account without an organization keeps its work item guides in its own layer: the layer is
optional, the guides are not lost when it is absent. The organization is part of the account's
data: it is stored in the account's database and travels with the account configuration to every
installation signed in to the account (work item guides do not travel today). Detaching it
removes it as a whole, after a private backup, without touching the rest of the account.

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
are stored in the account's database with the rest of the organization (§11: organizations are
not shared between accounts).

### Vocabulary

The terms Organization, Organization entry, Organization source, Account store and Local replica
are defined in [`CONTEXT.md`](../CONTEXT.md) since phase 1b. "Work item guide" keeps its meaning and
is a kind of organization entry.

## 5. Cloudflare options

Verified on 2026-11-02 in the official documentation ([sources](#sources)) and again on 2026-11-03
before phases 3a, 3b, 3c, 4a and 4b: no limit or price in this section changed (the only announced
change, Workers Logs moving to Observability pricing on 2026-12-01, does not apply: the Worker logs
nothing beyond the free defaults). Two details matter to the
implementation: SQLite storage in Durable Objects is billed since January 2026 (on Free it is
the 5 GB total below), and new Access service token secrets carry a `cfast_` prefix and accept
tags, while the headers stay `CF-Access-Client-Id` / `CF-Access-Client-Secret`.

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
account, a small `Registry` object (identities, accounts and installation credentials) — plus one R2 bucket for encrypted backups and exports.
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
  trip; phase 3 measures it before anything depends on it. Measured on 2026-11-03 from the
  production server in South America (median of eight checks): `/v1/health` about 30 ms (edge and
  Worker only), `/v1/whoami` about 77 ms (the Registry object), `/v1/store` about 70 ms (the account
  store object), and a batch of up to 100 sealed records about 166 ms; the verified upload of 1,277
  records (3.3 MB) took under 7 seconds. The pipeline and the GUI read the local
  replica, so latency only affects writes and synchronization.
- **Cost on the Free plan (decided, §11).** One account uses thousands of requests and row writes
  a day at most and megabytes of storage. Free quotas belong to the Cloudflare account and are
  shared by every assistant account: 100,000 Worker requests, 100,000 Durable Object requests and
  100,000 rows written a day, 5 GB stored in total, 10 ms of CPU per Worker invocation (enough,
  because encryption happens on the client). The outbox batches records into few requests, the
  Worker keeps a per-account daily share so one account cannot exhaust the others, and an
  exhausted quota behaves like being offline (reads stay local, records wait in the outbox; §7).
  `cloud status` shows the usage. Moving to Workers Paid later changes no code.

## 6. Security and privacy

The stores hold Teams message content, answers and audits, internal documentation passages,
person profiles and activity records. Data classes:

| Class | Examples | In Cloudflare |
| --- | --- | --- |
| Secrets | variable values and keys, credentials, OAuth vault, machine tokens, provider sessions | never |
| Sensitive content | audits, answers, notes, evaluations, examples, people, activity records, run prompts and outputs, organization entries | **encrypted by the client** (§11) |
| Configuration structure | sources (URLs, tool catalogs with organization URLs and emails), contexts, questions, prompts | encrypted by the client |
| Operational metadata | blinded record IDs (a keyed hash of the natural ID), collection, revision, ciphertext digest, sizes, timestamps, schema versions | clear (needed for synchronization, never content or a natural identifier) |

**Client-side encryption.** Each record is sealed by the host with AES-256-GCM-SIV (the existing
dependency) under a per-account **content key**; the associated data binds a domain string, the
account, the key version, the collection, record ID, revision and payload schema, so ciphertext
moved to another account, record or revision fails authentication (the construction of account
variables). «The account» in the associated data is a random **content account** identifier kept
with the key (§11, decision 7), never the Worker's account ID, which no host learns. The nonce is
synthetic (a keyed hash of the associated data and the plaintext), so a repeated send carries the
same bytes; record IDs are blinded with a separate key kept in the same file. The content key is
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

**Organization.** Organizations are not shared between accounts (§11), so the organization lives
inside the account store, encrypted with the account's content key like the rest. Sharing would
need an organization key wrapped per member; it is out of scope.

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
- *Authority:* `authenticate(request) → Principal {account, installation}` is the only source
  of identity. Paths and bodies never carry an account ID; the stub is
  `AccountStore.idFromName(principal.account)`, so every user's requests reach only that user's
  database. No endpoint lists or reads across accounts. Contract tests include cross-account attempts.
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
  standing model (a new link kind, `account_store`, since 0.10.27), so the GUI and `setup_status`
  show one coherent state.
- **Coordinator.** Variables, machines, the Teams lease and the coordinator's other duties stay
  where they are. The portable configuration protocol (`portable_*`) reads from and writes to the
  account store once the cloud is primary, so every installation has the same source of truth
  without the coordinator being online.

## 8. Migration

Optional, per account, reversible, with a prior backup and no loss of history.

1. `pta cloud status` / `connect` (GUI: Settings › Advanced › Cloud storage; MCP `cloud_status`,
   `cloud_connect`) enrolls the installation and shows the mode: `off` (default), `mirror` or
   `primary`, kept in an installation-local `[cloud]` section of `Config` (`#[serde(default)]`,
   not portable). Done in phase 3a (0.10.25); see «Account store link» in the architecture.
2. `pta cloud migrate --dry-run` lists per collection what would be uploaded (counts, sizes),
   what stays local and why, and the size of the backup. Done in phase 4a (0.10.31).
3. `pta cloud migrate --confirmed` takes a **backup** first (SQLite online backup of each database
   plus the profile files into a private `backups/cloud-<time>/` with a manifest, reusing the
   data-safety copies of the updater; the newest three are kept), uploads every collection
   encrypted, then **verifies by reading back**: per collection, the record count and a digest over
   sorted `(id, revision, ciphertext digest)` computed on both sides, plus a full decryption and
   comparison of every record. Only then does the mode become `mirror`. Local stores are never
   deleted. It replaces phase 3b's `pta cloud mirror` (decision 14).
4. `mirror` keeps local as the source of truth and pushes every change. `pta cloud verify`
   compares at any time. `pta cloud promote` switches to `primary` after a clean verification and a
   new backup, and `pta cloud demote` back to `mirror` (phase 3c, 0.10.29).
5. `pta cloud rollback` backs up, pulls the configuration once when the store is primary (refused
   on a conflict or without an answer, unless `--keep-local`), verifies that the store holds nothing
   the installation lacks, switches to `off` and leaves the cloud copy untouched (decision 15).
6. `pta cloud erase`, only in `off` and with no other active installation, removes the copy from
   the Worker (`DELETE /v1/store`: the records, then the account's R2 prefix), clears the ledger and
   sets the content key aside, so a new migration uses a new key; Cloudflare's 30-day point-in-time
   recovery is the only way back (decision 16).
7. The rehearsal (`scripts/rehearse-cloud-migration.py`) runs migrate, promote, edits, rollback and
   erase on a copy of a real profile without its credentials, against staging with a temporary
   service token. Rehearsed on 2026-11-03 with a copy of the production profile (1,278 records):
   the backup held 10 files, the upload and its verification passed (verification 5.4 s), the
   edit and the note went through the store, the rollback kept every local record and the edit
   made in `primary`, and the erasure emptied the store and deleted nothing local.

Versions: the Worker exposes `/v1/…` with additive changes and reports its protocol and schema
versions; hosts advertise additive `cloud_store_support`, and a CLI or portal refuses `cloud_*`
to a host without it. The Worker keeps the previous protocol version working through a release,
and is deployed before the host release that needs it. An older host cannot load a profile with
`[cloud]` (the existing forward-only rule for profiles); it is not affected otherwise.

## 9. Deployment

- **Code.** A TypeScript Worker under `cloud/`, outside the Cargo package like `site/` (a
  `workers-rs` crate would break the single-package rule): `src/index.ts` (routing and
  `authenticate`), `access.ts` (the Access assertion), `registry.ts`, `account-store.ts`,
  `protocol.ts` (versions, collections, limits), and `wrangler.jsonc` with the Durable Object
  bindings (`new_sqlite_classes` migrations) and the R2 bucket. The organization is the
  `organization` collection of the account store, revised as one record like the host's
  `OrganizationStore`: the encrypted records need no separate object.
- **Protocol v1.** `GET /v1/health` (no credentials: environment, Worker version, deployment,
  protocols, additive `features`, schemas, whether Access is configured); `POST /v1/enroll` (Access identity only:
  issues the installation credential `ptac_…`, 256 bits, shown once); `GET /v1/whoami`,
  `GET /v1/installations`, `DELETE /v1/installations/{id|self}`; `GET /v1/store` (schema,
  per-collection counts, bytes and generations, today's usage); `POST /v1/records` (1–100 sealed
  records; the Worker checks each digest against the sealed bytes; results `created`, `updated`,
  `unchanged`, `stale` or `conflict`). Since Worker 0.2.0 (feature `cas`, phase 3c) a record may
  name the revision it replaces (`base_revision`, 0 for "no record yet"): it is written only over
  exactly that revision, otherwise `conflict`, while a repeated send of the revision already held
  stays `unchanged`; and a batch may be `atomic`, so either every record is written or none is
  (the refused ones answer `conflict`/`stale`, the others `aborted`). Hosts that send neither
  field see the earlier behavior; `GET /v1/records/{collection}?since=&limit=` and
  `GET /v1/records/{collection}/{id}`; `GET|PUT|DELETE /v1/backups/{name}` and `GET /v1/backups`
  under the account's own R2 prefix. Since Worker 0.3.0 (feature `erase`, phase 4a)
  `DELETE /v1/store` with `{"confirm": "erase"}` removes the caller's records (tombstones
  included, up to 1,000 per request, charged as rows written) and then every object under the
  account's R2 prefix, answering `{erased, backups, more}` until `more` is false; it is refused
  (`conflict`) while the account has another active installation, keeps the enrollment, the
  collection generations (so a replica never sees one go back) and the daily usage, and
  `GET /v1/store` reports `erased_at`. Since Worker 0.4.0 (phase 4b): feature `origin`, a listed
  or read record carries `own` (whether the calling installation wrote that revision; no
  installation ID is listed); feature `rebind`, `POST /v1/rebind` with `{"confirm": "rebind"}`, a
  new service token at the edge and an active installation credential of the account moves the
  account to the new token's identity (refused with `conflict` when that identity already has an
  account), keeping the account, its store and its installations; feature `plan`, `GET /v1/store`
  adds `plan {requests, rows_written, accounts, oversubscribed}`: the plan's daily quota
  (`PLAN_DAILY_*`, by default Free's 100,000), how many accounts with an active installation share
  it and whether every account at its share would exceed it. Bodies are closed schemas: an unknown
  field (an account ID, for instance) is refused. Each account has a daily share of requests and
  rows written (`ACCOUNT_DAILY_*`, or its own entry in `ACCOUNT_SHARES`, keyed by the service
  token's client ID); over it the Worker answers `quota_exhausted` (429), which the host treats
  as being offline.
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
| 1a. Cleanup (0.10.15) | Synthetic names in tests, docs, README, prompts, the GUI placeholder and catalogs; To-Do items carry `category`; the GUI decides by category; the task type listed with bugs comes from its guide (`listed_with_bugs`); the production guide is updated as data in the same delivery. | The production To-Do stops listing that type until its guide carries the field: update it right after the install, verify the list. |
| 1b. Organization (local) — done (0.10.16) | `[organization]` in the map (name, entries, source IDs); guides shown as entries; new `organization` source kind (disabled by default) and index documents; Settings › Organization replaces the current guides editor (no duplicate control); `pta organization …` with `pta guides …` kept as its guides subcommand; MCP `organization_*`; agents receive norms and architecture as untrusted input; skill, architecture and `CONTEXT.md`. The organization, guides included, joins the account configuration's allowlist and travels with the account. | Prompt size growth for agents (bound entries per run); a second place to edit guides if the old editor is not removed in the same PR. |
| 2. Storage abstraction — done (0.10.17–0.10.22) | One trait per account aggregate under the crate's `store/` (at the crate root, because the message pipeline uses them too), each with its local implementation as the default and a shared `contract::run` suite; no behavior change. `SettingsStore` (`store/settings.rs`, `LocalSettings`): `Config`, `KnowledgeMap` and the tunnel path, committed together with the existing journal. `NotesStore` (`store/notes.rs`, `LocalNotes`): verified notes (by ID, `updated_at` as revision), answer evaluations and approved examples (by Teams message), in `knowledge-index.db` with each record's search document in the same transaction. `AuditStore` (`store/audit.rs`, `LocalAudit`): the finished messages' audits (by Teams message, audit as revision) in `assistant.db` › `jobs`; the worker's in-flight rows, state machine and send nonces stay local. `ActivityStore` (`store/activity.rs`, `LocalActivity`): daily task snapshots and summaries (by day), finished runs (by ID) and the legacy record, in `activity-registration.db`; running runs, slot claims and recovery stay local. `ImplementationStore` (`store/implementation.rs`, `LocalImplementation`, crate-private): finished agent runs (by launch ID) and request runs (by ID) in `implementation/runs.db`; in-flight runs and requests, the change feed, the session index, remote-session access and the machine files stay local. `OrganizationStore` (`store/organization.rs`, `LocalOrganization`): the organization (name, entries by ID with `updated_at` as revision, documenting sources) and its work item guides (by ID) as one unit with one revision, kept in the map's `[organization]` and `[[work_item_guides]]` through the settings store's journaled commit; the settings editors still save it with the rest of the map in one commit. Search and ranking stay in the local index, which reads the stores' records; the pipeline reads the local stores and never waits on the cloud, whose store will keep the local replica current. In-flight state is joined with each store's finished records where a listing shows both (`audit`, activity runs, implementation runs and requests). | Broad refactor of hot paths: done store by store, each PR behavior-neutral and fully tested. |
| 3a. Worker and authentication — under way (Worker: 0.10.23; host link: 0.10.25) | `cloud/` Worker, `Registry`, `AccountStore` schema v1 (encrypted records with clear metadata), Access service token, installation credential, health; Workers Builds; host `cloud status/connect`. | Access/Workers configuration mistakes: staging Worker first, contract tests including cross-account attempts. |
| 3b. Mirror — done (0.10.26) | Content key, sealing and blinded IDs; the ledger as outbox for every collection (finished records and configuration snapshots); `cloud mirror` (plan, verified initial upload, then mode `mirror`; stop) and `cloud verify`; key in the operator key backup; latency in `cloud status` (0.10.26). The link as a standing kind, the `account_store` step of `setup_status`, `cloud disconnect` and the service token's expiry (0.10.27). Local stays the source of truth. | Outbox growth while offline (bounded by the local records, visible in status); latency measured here (§5). |
| 3c. Primary — done (Worker 0.2.0 with 0.10.28; host 0.10.29) | Compare-and-swap (`base_revision`, `atomic`, feature `cas`) in the Worker; mode `primary` (`cloud promote|demote`, after a clean verification): every configuration edit is written to the store first and saved locally once accepted, read-only without an answer; the loop pulls the configuration into the local replica; conflicts stay local until `cloud pull --replace`; the coordinator serves `portable_*` from the replica it pulls. Primary scope: the whole `config` (but `[secrets]`/`[cloud]`), the map, the organization and the notes (decision 10). | Conflict handling regressions: reuse the portable rules and their tests. |
| 4a. Migration — done (Worker 0.3.0 with 0.10.30; host 0.10.31) | `cloud migrate/promote/rollback/erase` with backup and read-back verification, the rehearsal script, and the rehearsal on a copy of the production profile against staging; production migrated when the owner decides. | Data loss: never delete local data; rollback rehearsed on a copy of the production profile first. |
| 4b. Other users — under way (launchers approved in the portal: 0.10.32; Worker 0.4.0 with installations, rebinding and the shared plan: 0.10.33; a store per portal account, enabled by the operator: 0.10.34; installation settings apart from the account's: 0.10.35) | Every portal account gets its own cloud account store: the operator enables it per account, each with its own content key, installation credentials and quota share; the owner's account is not special in the Worker. | Authorization bugs between accounts (cross-account contract tests); the shared Free quota (per-account shares, §5). |

## 11. Decisions

Taken by the owner on 2026-11-02:

1. **Cloudflare account and plan.** The account that already runs the tunnel, Access and the
   landing, on the **Free** plan (§5 explains how the shared quota is handled).
2. **Data residency.** No preference: no jurisdiction is set, so objects are placed near where they
   are created (eastern North America from South America, the lowest latency available). A
   jurisdiction could only be added later by migrating.
3. **Encryption.** Client-side encryption of every content record; search runs in the local
   replica (§6).
4. **Users.** Several users, each with their own data (one account store per assistant account);
   **no organizations shared between accounts**. The organization layer is part of each account
   and travels with its configuration.
5. **Production migration.** After phase 4a has been rehearsed on a copy of the production
   profile, then the owner's account first.

Taken during phase 2 (2026-11-02):

6. **The organization has its own store.** `OrganizationStore` keeps the organization and its
   work item guides as one aggregate, aligned with the Worker's planned `organization-store.ts`
   (§9), instead of leaving them inside the settings store's map. The cloud can then keep, sync
   and revise the organization apart from the rest of the configuration, while the local profile
   keeps both in `knowledge-map.toml` without changing files or formats.

Taken before phase 3b (2026-11-03):

7. **The account in the associated data.** A random content account UUID generated with the content
   key and kept in the same private file (and its backup), plus the key version; the Worker's account
   ID is never needed or sent.
8. **First upload.** All eleven collections, configuration included (without `[secrets]` and
   `[cloud]`), so verification covers everything phase 3c needs.
9. **Key delivery.** Only the operator key backup and restore in this phase; delivery at the «Trust
   this machine» confirmation waits for phase 4b, when a second installation uploads.

Taken before phase 3c (2026-11-03):

10. **What is primary.** The whole configuration (all of `config` except `[secrets]` and `[cloud]`,
    the knowledge map with people and prompts), the organization with its guides and the notes;
    history keeps being mirrored. Installation-local settings inside it are split per installation
    in phase 4b (decision 19, 0.10.35).
11. **The coordinator.** The account store is the authority; the coordinator writes through it (its
    own edits and the `portable_apply` of the account's other computers, whose protocol does not
    change) and serves `portable_*` from the replica it pulls. Without an answer from the store the
    configuration is read-only on the coordinator too.
12. **Protocol.** Compare-and-swap is an additive change of protocol v1 (`base_revision`, `atomic`,
    feature `cas`), not a v2.
13. **Production.** Stays in `mirror` until the phase 4a rehearsal; `promote` is tested against
    staging and copies.

Taken before phase 4a (2026-11-03):

14. **`migrate` replaces `mirror`.** `pta cloud migrate --dry-run|--confirmed` and `rollback` are the
    only path in the CLI, the MCP, the GUI (one «Migrate» button) and the skill; the host keeps
    accepting `cloud_mirror` for older clients, routed to the migration (with its backup) and the
    rollback.
15. **Rollback of the configuration in primary.** A last pull, so what the store accepted stays;
    refused on a conflict or without an answer, with `--keep-local` to keep the installation's
    configuration as it is. History is verified, never pulled: it is local first.
16. **Erase.** Immediate after a rollback, confirmed twice (two taps or `--confirmed`), refused while
    another installation is active; removes the records and the account's R2 prefix, keeps the
    enrollment, clears the ledger and sets the content key aside. No grace period of its own: the
    30-day point-in-time recovery is the grace, readable only with the retired key.

Taken before phase 4b (2026-11-03):

17. **Who enables an account's store.** The operator, per portal account: they create the account's
    Access service token and enable the store for it with an operator-local command; disabling
    revokes the token and leaves the account's local data as it is. Inside, the account decides
    about its own data (migrate, promote, demote, roll back, erase) from its GUI, CLI and MCP, and
    may go back to `off` and erase its copy at any time, but cannot enable itself again without the
    operator.
18. **Key delivery at «Trust this machine».** End to end: the installation that signs in generates
    an ephemeral X25519 key pair and the short code both screens show covers its public key (the
    tunnel's TLS ends at Cloudflare, so nothing travels in clear). When the account has a store and
    the person ticks «also the account store» while approving, the coordinator wraps the content
    key and the account's service token to that public key; only the verifier holder can open
    them. Revoking a machine revokes its installation in the Worker; rotating the content key
    (re-encrypting the account, seconds) is a separate, explicit «Revoke and rotate the key»,
    recommended for a lost computer, after which the other computers confirm the trust again to
    receive the new key.
19. **Installation and account settings.** A closed list is the installation's, in a record of its
    own per installation and never applied to another: `server.*` (bind, public URL, data
    directory, tunnel, webhook prefix), `graph.*` (tenant, client, user, chats, self chat), the map
    path and repository folders, `knowledge_index` (embeddings) and `policy.dry_run`. Everything else
    (language, style, model chain, policies, message style, daily activity, sources, contexts,
    questions, people, prompts, the organization, the notes) is the account's.
20. **Launchers approved in the portal.** The owner of an unshared profile may approve a launcher
    in their own portal session (`Browser` transport) as in the window: the command, arguments,
    folder and variables are shown, the digest must match, and any change needs a new approval.
    Ordinary accounts, the CLI, the MCP and machine credentials still cannot approve, and defining,
    removing and running stay on the machine itself.

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
