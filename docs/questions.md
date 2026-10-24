# Questions and answer evaluations

Knowledge › Questions replaces the built-in Cases editor. A question contains an ID, its text,
selected source IDs, and optional requests the owner associated during review. The question
specifies an information need; the incoming message still specifies the actual date or period.

Loading a map without `questions` seeds exactly one question, **What did you do today?**, with
the existing activity-record source. When that source is missing, it is added disabled, without
an audience or external processing. Existing sources retain their settings. Saving an empty
`questions = []` keeps the list empty. Legacy `cases` data remains readable for compatibility;
loaded profiles use Questions and ignore those former route customizations.

## Recognition and evidence

An exact question or owner-associated request matches directly after case/accent normalization.
Other wording goes through the existing knowledge index (`@questions`): optional embeddings
rank candidate questions; the tool-less model confirms one supplied ID by meaning, with
confidence at least 0.85. Without embeddings the model considers all configured questions.
Missing, uncertain or unavailable recognition leaves the message unanswered for review.
For example, a summary of today's activities and a status request for a specific day can match
the initial question, while retaining their respective dates.

A match reads only the intersection of its selected sources and those enabled, allowed external
processing, and authorized for the conversation and sender. Explicit local test sources are
also intersected with the question. No automatic Wiki/status/review sources are added. History
records `identified_question` (ID and text), `available_sources`, `selected_sources`, the answer,
references, coverage and the existing trace. Sources selected for reading differ from references
actually cited in the answer; both remain visible.

The activity record is read for the requested days, defaulting to the day the message was written
in the activity schedule's time zone. Unsaved days do not prove that no work happened.
Repository and notes sources use FTS5 plus optional embeddings, as before. Exact URL groups
(`kind = "urls"`, 1–10 HTTPS URLs) read public text/HTML/JSON with the existing DNS, IP, redirect
and response-size restrictions. Authenticated Azure Wiki pages use a Wiki source's existing
catalog and credential, with `tool.page_urls`: an explicit nonempty list of exact web page URLs.
The source reads only those pages through the native Wiki API, preserving canonical references
and authorship, without Search or repository discovery. Page names/IDs must resolve inside the
catalog, team projects, allowed Wiki IDs and published versions. New URL sources start disabled
and must be authorized before a question can use them.

## Review and evaluations

An unmatched message offers **Associate question** in Messages and Test chat. Choose an existing
question or create one and select its sources. The host saves the message as an associated request
in the same profile map shown in Questions, records its question in history, and never sends a
reply as part of the association. Review/regeneration and sending remain separate actions.

Good and bad answer ratings with optional comments are saved in the message audit and in
`knowledge-index.db` (`question_feedback`, search source `@feedback:QUESTION_ID`). Later answers
retrieve evaluations for that same question, only where every original source remains authorized.
Optional embeddings rank evaluated requests; recent judgments also apply when wording differs
and no embeddings endpoint is configured. Up to eight evaluations guide generation: good
suggests structure/tone, bad instructs the model to avoid the rejected approach and follow the
owner's correction. Evaluations never supply current facts, dates, names, IDs or URLs. Those
come from fresh source evidence. Code blocks a generated answer that repeats an identical
rejected answer; avoiding broader mistakes depends on the model following the owner's feedback.
Changing a rating replaces its guidance; clearing it removes the indexed evaluation. Ratings do
not retrain model weights or change source audiences.

## CLI and tools

- `pta questions list`, `show ID`, `add`, `edit ID`, `remove ID`; add/edit take JSON
  `{ "id": "daily", "text": "What did you do today?", "sources": ["activity-record"] }`.
- `pta questions associate RESOURCE` takes `question_id` (existing) or `text` (new), plus `sources`.
- `pta answer feedback RESOURCE` takes `rating` (`good`, `bad`, or null) and optional `comment`.
- Shared tools/MCP expose `questions_list`, `question_save`, `question_remove`,
  `question_associate` and `answer_rate`, using the same host operations and profile revision checks.

The registry allows 100 questions, 1000 characters per question or associated request, 50 source
IDs and 50 associated requests per question. IDs and source references are validated in code.
