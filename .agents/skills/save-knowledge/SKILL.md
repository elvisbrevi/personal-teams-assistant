---
name: save-knowledge
description: Save or update facts in Jev's knowledge base when the user asks an agent to remember, record, correct, or organize information for future answers.
---

# Save knowledge

The unit of work is a **source**: one topical document Jev can select from its descriptor and the app can extract useful passages from. The app loads only files declared in `knowledge-map.toml` at startup.

1. **Locate the source.** Read the app's configured map and the `README.md` in the checkout named by `repositories.personal` (locally, `../personal-teams-knowledge`). Search the map and repository by topic, synonyms, and identifiers. Decide whether to correct an existing source or create one. Finish when the canonical document and any conflicting facts that need updating are identified.
2. **Establish the fact.** Preserve the user's exact claim and its provenance. Use a stated effective date; otherwise record the date received as the registration date without treating it as the fact's effective date. For conflicting versions, make the current version and the older version's provenance clear. Finish when every new claim has an understandable subject, scope, temporal status, and provenance, or is explicitly marked as pending confirmation.
3. **Write the document.** Apply the README's document format. Keep one stable topic per file and one self-contained fact per paragraph so `excerpt()` can retrieve complete passages. Update an existing file for the same fact; create `temas/<domain>/<subject>.md` for a new source. Finish when no active duplicates or inferred claims remain.
4. **Register access.** For a new source, add a `kind = "file"` entry to the local `knowledge-map.toml` using the README example. Give it a stable `id`, a description of the questions it answers, `topics` matching likely user terms, and its actual relative path. Preserve existing permissions when editing a source. For a new source, use only conversations and senders explicitly approved for that content; without approval, keep `external_processing = false` and `allowed_conversations = []`, and report that Jev cannot query it yet. Finish when the map points to the right file and its audience matches the authorization received.
5. **Verify loading.** Validate the TOML and run `cargo run -- check config.toml` from the app when that configuration exists. Check that the declared path exists inside the Git checkout, has a supported extension, and is under 1 MB. Finish when the configuration loads and the source's availability to Jev is clear. Report the file, `id`, audience, and the need to restart the app to load map or content changes.

The knowledge repository is private. Store only information needed for answers; keep credentials outside it. The local map is Git-ignored: keep private facts out of `knowledge-map.example.toml`.
