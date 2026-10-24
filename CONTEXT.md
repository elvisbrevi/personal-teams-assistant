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

**Agent session**:
The coding agent's own conversation associated with an implementation run and its work item. Its native session ID identifies it in that agent's tools and supported app links.
_Avoid_: run ID
