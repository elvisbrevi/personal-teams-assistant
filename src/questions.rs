//! Owner-configured questions. A question groups equivalent requests and fixes the sources
//! they may read; matching never grants an audience or reuses an earlier answer's facts.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub sources: Vec<String>,
    /// Requests the owner explicitly associated with this question during review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct IdentifiedQuestion {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AnswerEvaluation {
    pub resource: String,
    pub question_id: String,
    pub request: String,
    pub answer: String,
    pub rating: String,
    pub comment: String,
    /// Only reused where every source of the evaluated answer is still authorized.
    pub sources: Vec<String>,
}

pub fn validate(questions: &[Question], source_exists: impl Fn(&str) -> bool) -> Result<()> {
    ensure!(questions.len() <= 100, "at most 100 questions are allowed");
    let pattern = regex::Regex::new(r"^[a-z][a-z0-9_-]{0,63}$")?;
    let mut ids = BTreeSet::new();
    let mut texts = BTreeSet::new();
    let mut owners = BTreeMap::new();
    for q in questions {
        ensure!(
            pattern.is_match(&q.id) && ids.insert(&q.id),
            "invalid or duplicate question ID"
        );
        ensure!(
            valid_text(&q.text) && texts.insert(normalize(&q.text)),
            "a question needs unique text of at most 1000 characters"
        );
        ensure!(
            !q.sources.is_empty()
                && q.sources.len() <= 50
                && q.sources.iter().all(|s| source_exists(s)),
            "a question needs at least one existing source"
        );
        ensure!(
            q.examples.len() <= 50 && q.examples.iter().all(|s| valid_text(s)),
            "at most 50 associated requests of 1000 characters are allowed"
        );
        for text in std::iter::once(&q.text).chain(&q.examples) {
            let previous = owners.insert(normalize(text), &q.id);
            ensure!(
                previous.is_none_or(|id| id == &q.id),
                "a request cannot belong to two different questions"
            );
        }
    }
    Ok(())
}

pub fn normalize(text: &str) -> String {
    crate::index::fold(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn valid_text(text: &str) -> bool {
    !normalize(text).is_empty()
        && text.chars().count() <= 1000
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{index::Index, knowledge::KnowledgeMap};

    #[test]
    fn migration_seeds_once_and_question_feedback_stays_with_its_authorized_sources() {
        let mut map = KnowledgeMap::parse("").unwrap();
        map.initialize_questions();
        assert_eq!(map.questions.as_ref().unwrap().len(), 1);
        assert!(!map.resources[0].enabled);
        assert!(!map.resources[0].external_processing);
        assert!(map.resources[0].allowed_conversations.is_empty());
        let original = map.questions.clone();
        map.initialize_questions();
        assert_eq!(map.questions, original);
        map.questions = Some(vec![]);
        map.initialize_questions();
        assert!(map.questions.as_ref().unwrap().is_empty());
        let encoded = toml::to_string(&map).unwrap();
        assert_eq!(
            KnowledgeMap::parse(&encoded).unwrap().questions,
            Some(vec![])
        );

        let dir = tempfile::tempdir().unwrap();
        let index = Index::open(&dir.path().join("index.db")).unwrap();
        let allowed = vec!["activities".to_owned()];
        let mut value = AnswerEvaluation {
            resource: "m1".into(),
            question_id: "daily".into(),
            request: "What did you do today?".into(),
            answer: "Old answer".into(),
            rating: "bad".into(),
            comment: "Use a concise list".into(),
            sources: allowed.clone(),
        };
        index.save_evaluation(&value).unwrap();
        // A differently worded request benefits even without an embeddings server.
        let found = index
            .evaluations("daily", "Resumen de actividades", None, &allowed)
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rating, "bad");
        assert!(
            index
                .evaluations("unrelated", "today", None, &allowed)
                .unwrap()
                .is_empty()
        );
        assert!(
            index
                .evaluations("daily", "today", None, &[])
                .unwrap()
                .is_empty()
        );
        value.rating = "good".into();
        index.save_evaluation(&value).unwrap();
        assert_eq!(
            index.evaluations("daily", "today", None, &allowed).unwrap()[0].rating,
            "good"
        );
        index.remove_evaluation("m1").unwrap();
        assert!(
            index
                .evaluations("daily", "today", None, &allowed)
                .unwrap()
                .is_empty()
        );
    }
}
