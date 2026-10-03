---
name: save-knowledge
description: Save or update facts in the assistant's knowledge base when the user asks an agent to remember, record, correct, or organize information for future answers.
---

# Save knowledge

The unit of work is a **source**: one topical document the assistant can read for a conversation it is authorized for, and from which the app extracts useful passages. The app loads only files declared in the profile's `knowledge-map.toml`.

1. **Locate the source.** Read the app's configured map (`pta --json config get knowledge_map` gives its path; `pta --json sources list` and `pta --json repos list` its content) and the `README.md` in the checkout named by `repositories.personal`. Search the map and repository by topic, synonyms, and identifiers. Decide whether to correct an existing source or create one. Finish when the canonical document and any conflicting facts that need updating are identified.
2. **Establish the fact.** Preserve the user's exact claim and its provenance. Use a stated effective date; otherwise record the date received as the registration date without treating it as the fact's effective date. For conflicting versions, make the current version and the older version's provenance clear. Finish when every new claim has an understandable subject, scope, temporal status, and provenance, or is explicitly marked as pending confirmation.
3. **Write the document.** Apply the README's document format. Keep one stable topic per file and one self-contained fact per paragraph so `excerpt()` can retrieve complete passages. Update an existing file for the same fact; create `temas/<domain>/<subject>.md` for a new source. Finish when no active duplicates or inferred claims remain.
4. **Register access.** For a new source, add a `kind = "file"` entry with `pta sources add` (Resource JSON on stdin, as in `knowledge-map.example.toml`); it starts disabled and without audiences. Authorize it afterwards with `pta sources audience ID` and `pta sources enable ID`. Give it a stable `id`, a description of the questions it answers, `topics` matching likely user terms, and its actual relative path. Preserve existing permissions when editing a source. For a new source, use only conversations and senders explicitly approved for that content; without approval, keep `external_processing = false` and `allowed_conversations = []`, and report that the assistant cannot use it yet. Finish when the map points to the right file and its audience matches the authorization received.
5. **Verify loading.** Run `pta --json doctor --offline` to validate the profile and map. Check that the declared path exists inside the Git checkout, has a supported extension, and is under 1 MB. Finish when the configuration loads and the source's availability to the assistant is clear. Report the file, `id`, audience, and run `pta restart` if the service is running so it loads map or content changes.

The knowledge repository is private. Store only information needed for answers; keep credentials outside it. The profile map is private: keep private facts out of `knowledge-map.example.toml`.
