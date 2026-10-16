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
        if source != NOTES && !known.contains(&source) {
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
