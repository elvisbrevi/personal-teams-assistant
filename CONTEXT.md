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
A newer stable release of this package built next to the installed one and verified as a matched application and `pta` pair; it is not installed, and the installed version keeps running until the user applies it.
_Avoid_: downloaded update, installed update

**Applied update**:
A prepared update that was installed, restarted and found healthy; only the update journal's `committed` outcome says so, never the installed version alone. An unhealthy or interrupted one is rolled back to the previous matched application and `pta` pair.
_Avoid_: finished update, upgrade

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
The coding agent's own conversation associated with an implementation run and its work item. Its native session ID identifies it in that agent's tools and supported app links.
_Avoid_: run ID
