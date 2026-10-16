---
name: save-knowledge
description: Save or update facts in the assistant's knowledge when the user asks an agent to remember, record, correct, or organize information for future answers.
---

# Save knowledge

The unit of work is a **verified note**: one fact the user confirmed, with a title, the text, the contexts (subjects such as «cobros») it belongs to and the keywords that should find it. Notes live in the app's knowledge index and reach answers through the map's notes source (`kind = "notes"`), authorized per conversation like any other source. Whole repositories are sources too (`kind = "repository"`), for documentation that already lives in Git.

Prefer the MCP tools (`notes_search`, `knowledge_search`, `contexts_list`, `note_save`, `context_save`) when the agent has the `personal-teams-assistant` server; otherwise the same operations exist in `pta`.

1. **Find what already exists.** Search before writing: `pta --json notes search "WORDS"` (and `--context ID`), `pta --json knowledge search "WORDS"` for repositories, and `pta --json contexts list`. Decide whether to correct an existing note or create one. Finish when the note to change, or the absence of one, is clear.
2. **Establish the fact.** Keep the user's exact claim and its provenance. Use a stated effective date; otherwise say when it was recorded, without treating that as the fact's effective date. For conflicting versions, make the current one and the older one's provenance clear. Ask the user when a fact is ambiguous, could be someone else's opinion, or would replace something they said before. Finish when the claim has a clear subject, scope and time, or is marked as pending confirmation.
3. **Write the note.** One self-contained fact or procedure per note, in the user's language, without secrets, emails, phone numbers or IDs (the app refuses them). Choose the contexts it belongs to (empty = every subject) and a few keywords people would use. Create with `pta notes add` (JSON on stdin: `title`, `text`, `contexts`, `keywords`, `origin` = `agent`, optional `reference` such as a work item or page URL) or update with `pta notes edit ID`. Finish when no duplicate or contradicting note remains.
4. **Check access.** Notes reach answers only through an enabled notes source with external processing and an audience: `pta --json sources list` shows it. If there is none, tell the user, and add one only if they ask (`pta sources add`, born disabled; they authorize it). A new context needs `pta contexts add` and the user's choice of its sources. Finish when it is clear which conversations can read the note.
5. **Verify.** `pta --json notes search "WORDS"` finds the note with the words a colleague would use. Report its title, contexts and which conversations can read it.

Notes are private data of the profile. Store only what answers need; never credentials. Keep private facts out of `knowledge-map.example.toml` and the repository.
