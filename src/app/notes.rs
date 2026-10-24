//! Verified notes and the knowledge index, for the GUI, `pta` and agents: list, save and
//! remove notes, search what the assistant would read, and see or refresh what each indexed
//! source holds. Notes are data, not profile: saving one needs no revision and no restart.

use super::{DesktopState, fail, read_config};
use crate::{
    index::{Index, NOTES, Note},
    knowledge::{Access, KnowledgeMap},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

type Reply = std::result::Result<Value, String>;

fn open(
    state: &DesktopState,
) -> std::result::Result<(crate::config::Config, KnowledgeMap, Index), String> {
    let config = read_config(&state.config_path).map_err(fail)?;
    let map = KnowledgeMap::load(&state.map_path).map_err(fail)?;
    let index = Index::open(&config.server.data_dir.join(crate::index::FILE)).map_err(fail)?;
    Ok((config, map, index))
}

/// Notes, newest first: `context` keeps that context's and the general ones; `query` searches
/// them by relevance instead.
pub(super) fn list(state: &DesktopState, args: &Value) -> Reply {
    let (_, map, index) = open(state)?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 1000) as usize;
    let context = args.get("context").and_then(Value::as_str);
    if let Some(context) = context
        && !map.contexts.iter().any(|c| c.id == context)
    {
        return Err("[invalid_input] Unknown context.".into());
    }
    match args
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
    {
        Some(query) => {
            let contexts = context.map(|c| vec![c.to_owned()]);
            let hits = index
                .search(&[NOTES], query, contexts.as_deref(), None, limit)
                .map_err(fail)?;
            let notes: Vec<Note> = hits
                .iter()
                .filter_map(|h| index.note(&h.key).ok().flatten())
                .collect();
            Ok(json!(notes))
        }
        None => Ok(json!(index.notes(context, limit).map_err(fail)?)),
    }
}

/// Create (without `id`) or replace a note. Sensitive data is refused, like in an answer.
pub(super) fn save(state: &DesktopState, args: &Value) -> Reply {
    let (config, map, index) = open(state)?;
    let note: Note = serde_json::from_value(args.get("note").cloned().unwrap_or(Value::Null))
        .map_err(|_| "[invalid_input] A note needs title and text, and optionally contexts, keywords, origin and reference.".to_owned())?;
    let contexts: BTreeSet<&str> = map.contexts.iter().map(|c| c.id.as_str()).collect();
    note.validate(&contexts)
        .map_err(|e| format!("[invalid_input] {e}."))?;
    let redactor = crate::security::Redactor::new(&config.policy.sensitive_patterns, Vec::new())
        .map_err(fail)?;
    let texts = [&note.title, &note.text]
        .into_iter()
        .chain(&note.keywords)
        .chain(note.reference.as_ref());
    if texts.into_iter().any(|t| !redactor.clean(t)) {
        return Err("[invalid_input] The note contains sensitive data (a secret, an email, a phone number or an ID); remove it and save again.".into());
    }
    if !note.id.is_empty() && index.note(&note.id).map_err(fail)?.is_none() {
        return Err("[invalid_input] Unknown note.".into());
    }
    Ok(json!(index.save_note(note).map_err(fail)?))
}

pub(super) fn remove(state: &DesktopState, args: &Value) -> Reply {
    let (_, _, index) = open(state)?;
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or("[invalid_input] A note ID is required.")?;
    if !index.remove_note(id).map_err(fail)? {
        return Err("[invalid_input] Unknown note.".into());
    }
    Ok(json!({"removed": id}))
}

/// What each indexed source of the map holds, and when it was last compared with its files.
pub(super) fn status(state: &DesktopState) -> Reply {
    let (config, map, index) = open(state)?;
    let held = index.status().map_err(fail)?;
    let sources: Vec<Value> = map
        .resources
        .iter()
        .filter(|r| r.access.indexed())
        .map(|r| {
            let key = match r.access {
                Access::Notes { .. } => NOTES,
                _ => r.id.as_str(),
            };
            json!({"id": r.id, "kind": if key == NOTES { "notes" } else { "repository" }, "enabled": r.enabled, "index": held.get(key).cloned().unwrap_or_default()})
        })
        .collect();
    Ok(json!({
        "sources": sources,
        "notes": held.get(NOTES).cloned().unwrap_or_default(),
        "embeddings": config.knowledge_index.embeddings.as_ref().map(|e| json!({"url": e.url, "model": e.model})),
    }))
}

/// Compare repository sources with their checkouts now (all, or `source`), and forget the
/// documents of sources no longer in the map.
pub(super) async fn refresh(state: &DesktopState, args: &Value) -> Reply {
    let (config, map, index) = open(state)?;
    let wanted = args.get("source").and_then(Value::as_str);
    let repositories: Vec<(String, std::path::PathBuf)> = map
        .resources
        .iter()
        .filter(|r| wanted.is_none_or(|w| w == r.id))
        .filter_map(|r| match &r.access {
            Access::Repository { repository } => map
                .repositories
                .get(repository)
                .map(|root| (r.id.clone(), root.clone())),
            _ => None,
        })
        .collect();
    if let Some(wanted) = wanted
        && repositories.is_empty()
    {
        return Err(format!(
            "[invalid_input] {wanted} is not a repository source."
        ));
    }
    let known: BTreeSet<String> = map.resources.iter().map(|r| r.id.clone()).collect();
    for source in index.status().map_err(fail)?.into_keys() {
        if !source.starts_with('@') && !known.contains(&source) {
            index.forget(&source).map_err(fail)?;
        }
    }
    drop(index);
    let path = config.server.data_dir.join(crate::index::FILE);
    let synced = tokio::task::spawn_blocking(move || {
        let index = Index::open(&path)?;
        repositories
            .into_iter()
            .map(|(id, root)| Ok((id.clone(), index.sync_repository(&id, &root)?)))
            .collect::<anyhow::Result<Vec<_>>>()
    })
    .await
    .map_err(fail)?
    .map_err(fail)?;
    Ok(json!(
        synced
            .into_iter()
            .map(|(id, sync)| json!({"id": id, "sync": sync}))
            .collect::<Vec<_>>()
    ))
}

/// The sections the assistant would read for `query` from the indexed sources (all, or those
/// in `sources`), without a model: to check what a repository or the notes answer. Vectors are
/// not used here, so nothing leaves the computer.
pub(super) fn search(state: &DesktopState, args: &Value) -> Reply {
    let (_, map, index) = open(state)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty() && q.chars().count() <= 2000)
        .ok_or("[invalid_input] Write what to search for (at most 2000 characters).")?;
    let wanted: Option<Vec<String>> = args.get("sources").and_then(Value::as_array).map(|ids| {
        ids.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    });
    let names: Vec<&str> = map
        .resources
        .iter()
        .filter(|r| r.access.indexed())
        .filter(|r| wanted.as_ref().is_none_or(|w| w.contains(&r.id)))
        .map(|r| match r.access {
            Access::Notes { .. } => NOTES,
            _ => r.id.as_str(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let context = args
        .get("context")
        .and_then(Value::as_str)
        .map(|c| vec![c.to_owned()]);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 50) as usize;
    Ok(json!(
        index
            .search(&names, query, context.as_deref(), None, limit)
            .map_err(fail)?
    ))
}

/// The user's judgment of an answer: `rating` `good` or `bad` (null clears it), an optional
/// `comment`, and `example` (a good answer guides later answers to similar requests, within
/// `context` when given). Sensitive data in the comment is refused.
pub(super) fn feedback(state: &DesktopState, args: &Value) -> Reply {
    let resource = rating_resource(args)?;
    let (config, map, index) = open(state)?;
    let rating = match args.get("rating") {
        None | Some(Value::Null) => None,
        Some(Value::String(r)) if r == "good" || r == "bad" => Some(r.clone()),
        _ => {
            return Err(
                "[invalid_input] Rate an answer good or bad, or clear the rating with null.".into(),
            );
        }
    };
    let comment = args
        .get("comment")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let example = args
        .get("example")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = args
        .get("context")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    if comment.chars().count() > 2000 || comment.chars().any(|c| c.is_control() && c != '\n') {
        return Err("[invalid_input] A comment has at most 2000 characters.".into());
    }
    let redactor = crate::security::Redactor::new(&config.policy.sensitive_patterns, Vec::new())
        .map_err(fail)?;
    if !redactor.clean(&comment) {
        return Err(
            "[invalid_input] The comment contains sensitive data; remove it and save again.".into(),
        );
    }
    if let Some(context) = &context
        && !map.contexts.iter().any(|c| &c.id == context)
    {
        return Err("[invalid_input] Unknown context.".into());
    }
    if example && rating.as_deref() != Some("good") {
        return Err("[invalid_input] Only an answer rated good can be an example.".into());
    }
    let db = message_database(&config, &resource);
    let row = crate::state::Store::inspect(&db, false, 1, Some(&resource), true)
        .map_err(fail)?
        .into_iter()
        .next()
        .ok_or("[invalid_input] Unknown message.")?;
    let audit: crate::state::Audit = serde_json::from_value(row["audit"].clone()).map_err(fail)?;
    let answer = audit
        .sent
        .clone()
        .or(audit.proposed.clone())
        .filter(|a| !a.trim().is_empty());
    if rating.is_some() && answer.is_none() {
        return Err("[invalid_input] This message has no answer to rate.".into());
    }
    let feedback = rating.map(|rating| crate::state::Feedback {
        example: example || (audit.identified_question.is_some() && rating == "good"),
        rating,
        comment,
        context: context.clone(),
        at: chrono::Utc::now().timestamp(),
    });
    if !crate::state::Store::set_feedback(&db, &resource, feedback.as_ref()).map_err(fail)? {
        return Err("[conflict] This message is being processed; rate it when it ends.".into());
    }
    let request = audit
        .resolved_question
        .clone()
        .or(audit.question.clone())
        .unwrap_or_default();
    match (&feedback, &answer, &audit.identified_question) {
        (Some(f), Some(answer), Some(q)) => index
            .save_evaluation(&crate::questions::AnswerEvaluation {
                resource: resource.clone(),
                question_id: q.id.clone(),
                request: request.clone(),
                answer: crate::evidence::unlinked(answer),
                rating: f.rating.clone(),
                comment: f.comment.clone(),
                sources: if audit.selected_sources.is_empty() {
                    audit
                        .source
                        .as_deref()
                        .unwrap_or("")
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                } else {
                    audit.selected_sources.clone()
                },
            })
            .map_err(fail)?,
        _ => index.remove_evaluation(&resource).map_err(fail)?,
    }
    match (&feedback, answer) {
        (Some(f), Some(answer))
            if audit.identified_question.is_none() && f.example && !request.is_empty() =>
        {
            index
                .save_example(
                    &resource,
                    &request,
                    &crate::evidence::unlinked(&answer),
                    context.as_deref(),
                )
                .map_err(fail)?
        }
        _ => index.remove_example(&resource).map_err(fail)?,
    }
    Ok(json!({"resource": resource, "feedback": feedback}))
}

fn rating_resource(args: &Value) -> std::result::Result<String, String> {
    args.get("resource")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty() && r.len() <= 512 && !r.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or("[invalid_input] Invalid message.".into())
}
fn message_database(config: &crate::config::Config, resource: &str) -> std::path::PathBuf {
    config
        .server
        .data_dir
        .join(if resource.starts_with("simulation:") {
            "desktop-chat.db"
        } else {
            "assistant.db"
        })
}

/// Associate an unanswered message with a question, or create one, and keep the owner's
/// example in the same settings map shown by the Questions editor. Never sends a reply.
pub(super) async fn associate(state: &DesktopState, args: &Value) -> Reply {
    let resource = rating_resource(args)?;
    let (config, mut map, _) = open(state)?;
    let db = message_database(&config, &resource);
    let row = crate::state::Store::inspect(&db, false, 1, Some(&resource), true)
        .map_err(fail)?
        .into_iter()
        .next()
        .ok_or("[invalid_input] Unknown message.")?;
    let audit: crate::state::Audit = serde_json::from_value(row["audit"].clone()).map_err(fail)?;
    if audit.sent.is_some()
        || !["ignored", "dry_run"].contains(&row["status"].as_str().unwrap_or(""))
    {
        return Err("[conflict] Associate a message when it is left for review.".into());
    }
    let request = audit
        .resolved_question
        .or(audit.question)
        .filter(|s| crate::questions::valid_text(s))
        .ok_or("[invalid_input] This message has no usable question text.")?;
    let redactor = crate::security::Redactor::new(&config.policy.sensitive_patterns, Vec::new())
        .map_err(fail)?;
    if !redactor.clean(&request) {
        return Err("[invalid_input] The message contains sensitive data.".into());
    }
    let questions = map.questions.get_or_insert_with(Vec::new);
    let id = if let Some(id) = args.get("question_id").and_then(Value::as_str) {
        if !questions.iter().any(|q| q.id == id) {
            return Err("[invalid_input] Unknown question.".into());
        }
        id.to_owned()
    } else {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or(&request)
            .trim();
        if !crate::questions::valid_text(text) || !redactor.clean(text) {
            return Err("[invalid_input] Write a question without sensitive data (at most 1000 characters).".into());
        }
        let id = format!("question-{}", uuid::Uuid::new_v4().simple());
        questions.push(crate::questions::Question {
            id: id.clone(),
            text: text.into(),
            sources: vec![],
            examples: vec![],
        });
        id
    };
    // A reviewed request belongs to one question. Re-associating it removes the old alias.
    let normalized = crate::questions::normalize(&request);
    for q in questions.iter_mut() {
        q.examples
            .retain(|s| crate::questions::normalize(s) != normalized);
    }
    let q = questions.iter_mut().find(|q| q.id == id).unwrap();
    if crate::questions::normalize(&q.text) != normalized {
        q.examples.push(request);
    }
    if let Some(sources) = args.get("sources") {
        q.sources = serde_json::from_value(sources.clone())
            .map_err(|_| "[invalid_input] Select the question sources.".to_owned())?;
    }
    let identified = crate::questions::IdentifiedQuestion {
        id: q.id.clone(),
        text: q.text.clone(),
    };
    let tunnel = std::fs::read_to_string(&state.tunnel_path).unwrap_or_default();
    super::save_settings(state, config, map, tunnel).await?;
    crate::state::Store::associate_question(&db, &resource, &identified).map_err(fail)?;
    Ok(json!({"resource":resource,"question":identified}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn review_associates_creates_and_rates_questions_in_the_shared_profile() {
        let dir = tempfile::tempdir().unwrap();
        let state = super::super::init_state(&dir.path().join("profile"), &dir.path().join("data"))
            .unwrap();
        let (config, map, _) = open(&state).unwrap();
        let source = map.questions.as_ref().unwrap()[0].sources[0].clone();
        let db = config.server.data_dir.join("desktop-chat.db");
        let store = crate::state::Store::open(&db).unwrap();
        let resource = "simulation:review:one";
        store.begin_simulation(resource).unwrap();
        store
            .record(
                resource,
                &crate::state::Audit {
                    status: "ignored".into(),
                    reason: "no_matching_question".into(),
                    question: Some("Summary of your work\non Tuesday".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        associate(
            &state,
            &json!({"resource":resource,"question_id":"daily-activities","sources":[source]}),
        )
        .await
        .unwrap();
        let (_, saved, _) = open(&state).unwrap();
        assert_eq!(saved.questions.as_ref().unwrap().len(), 1);
        assert_eq!(
            saved.questions.as_ref().unwrap()[0].examples,
            vec!["Summary of your work\non Tuesday"]
        );
        assert_eq!(
            store
                .audit(resource)
                .unwrap()
                .unwrap()
                .identified_question
                .unwrap()
                .id,
            "daily-activities"
        );
        assert!(store.audit(resource).unwrap().unwrap().sent.is_none());
        // Re-association creates a question and removes the former association.
        let result = associate(&state, &json!({"resource":resource,"text":"What work was completed on Tuesday?","sources":[source]})).await.unwrap();
        let (_, saved, index) = open(&state).unwrap();
        assert_eq!(saved.questions.as_ref().unwrap().len(), 2);
        assert!(saved.questions.as_ref().unwrap()[0].examples.is_empty());
        let identified = serde_json::from_value(result["question"].clone()).unwrap();
        store
            .record(
                resource,
                &crate::state::Audit {
                    status: "dry_run".into(),
                    question: Some("Summary of your work\non Tuesday".into()),
                    identified_question: Some(identified),
                    proposed: Some("I completed the recorded tasks.".into()),
                    selected_sources: vec![source.clone()],
                    ..Default::default()
                },
            )
            .unwrap();
        feedback(
            &state,
            &json!({"resource":resource,"rating":"bad","comment":"List the tasks and their hours"}),
        )
        .unwrap();
        let id = result["question"]["id"].as_str().unwrap();
        assert_eq!(
            index
                .evaluations(id, "status", None, std::slice::from_ref(&source))
                .unwrap()[0]
                .rating,
            "bad"
        );
        feedback(&state, &json!({"resource":resource,"rating":null})).unwrap();
        assert!(
            index
                .evaluations(id, "status", None, &[source])
                .unwrap()
                .is_empty()
        );
    }
}
