# Personal Teams Assistant

An assistant that answers Microsoft Teams messages as the user, only from what the user authorized, and registers the user's work in Azure DevOps.

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

**Knowledge index**:
The searchable copy of repository sources and verified notes, split into sections.
_Avoid_: vector database, RAG store

**Case**:
A kind of request the assistant tells apart (a status round, a recap, an activity review, documentation, a request for the person, an informational message, any answer), with how it is recognized and answered.
_Avoid_: intent (for the configurable kind), route, template

**Recap**:
An answer about what the user did on a named day or period, from the activity record alone.

**Status round**:
A request for the user's progress, commitments, risks or blockers, answered part by part.

**Activity review**:
A comparison of the user's own activity with the work registered as tasks, listing what is not registered.

### Registering work

**Activity record**:
What the Activities section recorded by day: the tasks the user approved, with their HU and hours, and verified work not registered yet.
_Avoid_: activity history (for the data), log

**HU**:
A work item that can hold tasks: a user story, backlog item, Basic-process issue or a bug that is not closed.
_Avoid_: story, parent
