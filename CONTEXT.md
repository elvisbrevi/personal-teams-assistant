# Personal Teams Assistant

An assistant that answers Microsoft Teams messages as the user, only from what the user authorized, and keeps daily records of the user's existing Azure DevOps tasks.

## Language

### Answering

**Source**:
One thing the assistant may read to answer, authorized per conversation: a file, a URL, a whole repository, the verified notes, the activity record or a tool such as the Wiki or Azure DevOps activity.
_Avoid_: resource (in user-facing text), knowledge base

**Audience**:
The conversations (and optionally senders) a source may be read for.
_Avoid_: permission, scope

**Context**:
A subject the user works on, such as «cobros»: the words that name it, the sources that know about it and how to answer about it. It narrows and guides; it never authorizes a source.
_Avoid_: category, topic, project (when meaning the subject)

**Verified note**:
A fact the user confirmed, kept to answer with, optionally tied to contexts.
_Avoid_: memory, knowledge entry

**Approved example**:
An answer the user rated good that guides the approach of later answers to the same question, without supplying their facts.
_Avoid_: few-shot, template, training data

**Person profile**:
How the user addresses one person who writes to them, used only when answering that person.
_Avoid_: contact, user (for the person who writes)

**Knowledge index**:
The searchable collection of question definitions, repository passages, verified notes and evaluated requests used to find relevant guidance and evidence.
_Avoid_: vector database, RAG store

**Question**:
A question the user configures with the sources its answers may use. Equivalent requests share the question while retaining the date or period actually asked about.
_Avoid_: case, intent, route, template

**Associated request**:
A message the user explicitly linked to a question during review, kept as an example of equivalent wording.
_Avoid_: training sample, alias

**Answer evaluation**:
The user's judgment of an answer to a question: a positive evaluation suggests its approach; a negative evaluation identifies an approach or mistake to avoid in later answers to that question. It provides guidance, never facts about current work.
_Avoid_: model training, automatic fact

**Recap**:
An answer about what the user did on a named day or period, from the activity record alone.

**Status round**:
A request for the user's progress, commitments, risks or blockers, answered part by part.

**Task report**:
A reply listing confirmed registered tasks for a day or period, from the activity record alone; also answers reminders to log hours.

**Activity review**:
A comparison of the user's own activity with the work registered as tasks, listing what is not registered.

### Daily work

**Activity record**:
What Activities saved by day: existing Azure tasks with verified links, descriptions, optional HU and daily hours. An empty saved day differs from an unsaved day; the app never creates or changes tasks.
_Avoid_: activity history (for the data), log

**HU**:
A work item that can hold tasks: a user story, backlog item, Basic-process issue or a bug that is not closed.
_Avoid_: story, parent

### Accounts

**Account configuration**:
The preferences, model choices, sources with their audiences, contexts, questions, verified notes, the organization and its work item guides that follow an assistant account to every installation signed in to it. Folders, credentials, sign-ins and tunnels are never part of it.
_Avoid_: profile (the profile is one computer's whole set of files), settings sync

**Account store**:
Where an account's configuration, knowledge and history are kept: the local profile, or the account's cloud database with the local profile as its replica.
_Avoid_: cloud sync, backup

**Content key**:
The account's random key that seals every record on an installation before it goes to the account store; Cloudflare never receives it. Losing it makes the cloud copy unreadable, not the local data.
_Avoid_: encryption password, master key (the variables' own keys)

**Mirror**:
The mode in which the account store holds an encrypted copy of an installation's records, kept current by an outbox, while the local profile stays the source of truth.
_Avoid_: backup, cloud sync

**Primary**:
The mode in which the account store is the source of truth of the account's configuration: a change is written there first under compare-and-swap and saved locally only once accepted, and the local profile is its replica. Without an answer from the store the configuration is read-only; history keeps being mirrored.
_Avoid_: cloud mode, online mode

**Conflict** (account store):
A configuration record changed on an installation without reaching the account store while the store accepted another version. It stays as it is locally and cannot be edited until the user adopts the store's version or rolls back keeping this installation's.
_Avoid_: merge

**Migration** (account store):
Moving an installation's records into the account store: a private backup of the profile first, then the encrypted upload, verified by reading it back, after which the mode is mirror.
_Avoid_: sync, import

**Rollback** (account store):
Making the local profile the only source again: a backup, a last pull of the configuration when the store was primary, a verification, then mode off. Nothing local is deleted and the cloud copy stays until it is erased.
_Avoid_: restore, undo

**Erase** (account store):
Removing an account's copy from the account store after a rollback and setting its content key aside, so a later migration uses a new key. Local data is untouched.
_Avoid_: delete account, reset

**Local replica**:
An installation's readable copy of the account store, plus the caches derived from it; it answers while offline.
_Avoid_: offline mode

**Account standing**:
Whether an installation signed in to an account may still act for it, from its latest exchange with the coordinator: live, offline, signed out or revoked. Cached configuration stays readable in every state; a new use of an account variable needs a live answer.
_Avoid_: connection status, login state

**Readiness**:
How far an installation is from being a working member of its account, told step by step, each step belonging to the account or to this computer alone, and separating what is configured from what is running, received or sent. It only reports; the person or the owning operation performs each step.
_Avoid_: setup wizard, health check, status (the assistant's status is the host's)

**Unavailable source**:
A source of the account that this computer cannot read because its repository has no folder here; it keeps its meaning for the account and is never reinterpreted.
_Avoid_: broken source, missing source

**Prepared update**:
A newer stable release of this package built (or, for an archive installation, downloaded from the release and checked against its published digest) next to the installed one and verified as a matched application and `pta` pair; it is not installed, and the installed version keeps running until it is applied.
_Avoid_: downloaded update, installed update

**Applied update**:
A prepared update that was installed, restarted and found healthy; only the update journal's `committed` outcome says so, never the installed version alone. An unhealthy or interrupted one is rolled back to the previous matched application and `pta` pair.
_Avoid_: finished update, upgrade

**Archive installation**:
An installation the one-command installer made from the release's prebuilt archive, recorded in `install.json`; it updates from that release's archive for its own target, without Cargo.
_Avoid_: binary install, portable install

**Unattended updating**:
A per-computer choice, off unless the local owner turns it on, under which the application prepares and applies a newer release by itself when nothing would be interrupted; a release that was rolled back is not tried again by itself.
_Avoid_: auto-update, automatic upgrade

### Organization

**Organization**:
The optional conventions of the organization the user works for — work item guides, norms, architecture, conventions, glossary and templates — kept as data of the user's own account (never shared with another account) and detachable from it.
_Avoid_: company, tenant (Microsoft's), workspace

**Organization entry**:
One stored item of the organization: a norm, an architecture description, a convention, a glossary term or a template, optionally limited to some projects; a work item guide is one too. It informs answers and agents and grants nothing.
_Avoid_: rule, policy (code enforces policy, not entries)

**Organization source**:
A source the user marked as documenting the organization (a Wiki, a repository, a file or a URL); its access stays that of the source.
_Avoid_: provider (in user-facing text)

### Implementation

**Execution machine**:
A computer connected to the user's assistant account that can run a coding agent in directories the user selected.
_Avoid_: worker node, resource

**Coordinator**:
The user's main assistant installation, where their connected execution machines are listed and the machine responsible for Teams is selected.

**Implementation plan**:
A proposal of children for an HU, bug or GitHub epic, reviewed and editable by the user before they create the children.
_Avoid_: activity plan, registration proposal

**Implementation run**:
One attempt to plan children or implement a selected task, development bug or GitHub issue, using the user's context and chosen coding agent on an execution machine.
_Avoid_: activity run

**Work item guide**:
How the user's organization uses one work item type: when it is used, how it is written and what each field is for, with its allowed values and whether the agents may propose it. A kind of organization entry. The implementation agents follow it; it never authorizes a destination, a type or a value.
_Avoid_: template, schema

**Agent instructions**:
The editable text an implementation agent starts from when it plans children, implements an item or writes a request. Code always adds the rules it cannot change.
_Avoid_: system prompt (in user-facing text)

**Agent session**:
A coding agent's own conversation (Claude Code, Codex or OpenCode): one the user started on a machine of their account, or one an implementation run started for its work item. Its native session ID identifies it in that agent's tools and supported app links. The agent sessions source reads the most recent ones as summaries; one whose requests carry `#private` is never read.
_Avoid_: run ID

**Workflow**:
A saved recipe for implementing the items of one Azure DevOps project: the repositories it works on, the execution machine and the checkout of each repository there, the coding agent, its instructions and an ordered list of workflow blocks. It travels with the account and changes only on the coordinator. It grants nothing: every directory, repository, agent, chat and launcher is checked when it is saved and again when it is used. The one exception is its `allow`, which the user confirms when saving it: the fixed rules its own coding runs may break (work item writes, Teams messages, merging, publishing and deploying).
_Avoid_: pipeline (Azure DevOps' own), automation, recipe (in user-facing text)

**Workflow block**:
A reusable part of a workflow: an instruction the agents read, a Teams notification posted to one chat at one stage, or an approved launcher of the coordinator run at one stage. A block that acts is authorized when it is saved (a notification with the user's confirmation of its chat and text, a launcher by the owner at the GUI), not when it runs; the engine then runs it once at each occurrence of its stage and records its outcome, and it never reaches a Teams channel.
_Avoid_: step, plugin

**Workflow director**:
The model-backed selector and summarizer of a project's items: it chooses which enabled workflow fits an item, or none, and writes progress summaries; code checks its choice and it decides nothing else. A project's director is its own when Settings › Workflows sets one, otherwise the machine and agent its workflows share, otherwise the account's general director. Distinct from the coordinator, which stays the user's main installation.
_Avoid_: coordinator, orchestrator, dispatcher

**Workflow supervisor**:
The model-backed agent on the coordinator that looks after the account's workflow runs: on events (a run stopped, waited too long, lost its machine or disagrees with its session) or on request, it reads what the runs' agents did, checks their final reports against facts code read, drafts handover notes, work items and summaries, and proposes actions the user confirms. When every run of a batch has ended it writes an audit. By itself it changes no code and writes nothing outside the run's record, except moving the run it looks at ahead in the queue and what the user pre-approved for a workflow (a restart after a cut session, a post of its summary to a chat the user named). Distinct from the workflow director, which only chooses a workflow and summarizes.
_Avoid_: orchestrator, monitor, watchdog

**Workflow run**:
One execution of a workflow for one work item, kept on the coordinator, advancing through stages (planning, the plan-review gate, child creation, coding, pull requests, blocks, done) by the existing implementation operations, never past the user's review of the plan. An item has at most one active run; a run that stops (needs attention, interrupted, failed) waits for the user to resume it, and a resume never repeats a write or a started coding run. A restart ends a run and starts a new one of the same workflow for the same item, which records the run it continues; a pause holds every run, or one project's, so nothing new starts until it ends.
_Avoid_: implementation run (one of its parts), job

**Handover note**:
The user's text given to a restarted run's coding runs: what the previous run did, not to be repeated, and what is left. Stored with the new run and posted nowhere by the engine; the agent may write it elsewhere only where its workflow allows.
_Avoid_: comment, handoff comment

**Ready state**:
A workflow state of a project's process, among those of its requirement, bug and task types, that the user chose in the workflow settings as «ready to develop»: an assigned item that reaches it can start its workflow. Chosen from the states the process has, never typed.
_Avoid_: trigger state, start state

**Workflow watcher**:
The coordinator's task that, every few minutes, reads the items assigned to the user in the ready states and starts a run for each it has not seen in that state (a seen ledger), only through a workflow that starts by itself. It only starts runs: the plan-review gate stands, and it never runs on a worker or a shared account.
_Avoid_: poller, trigger
