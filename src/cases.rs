//! The kinds of request the assistant tells apart (cases) and what the user may change in each:
//! how the model recognizes it, extra words code recognizes it by, the sources it reads and
//! how it is answered. The map keeps only what the user changed (`[[cases]]`); a case not
//! listed behaves as built. Code still decides the clear cases, every source must still be
//! authorized for the conversation, and the rules on evidence, links, identifiers and
//! sensitive data are not part of any case.

use crate::llm::prompts;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// What the user changed in one built-in case.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    /// Off: the case is never chosen (its messages follow the general route).
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub enabled: bool,
    /// How the model recognizes it; replaces the default description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Words or phrases code recognizes it by, besides the built-in ones; matched whole,
    /// ignoring case and accents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The sources it reads, among those authorized for the conversation; empty = as built.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// How to answer it; replaces the default instructions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}
fn on() -> bool {
    true
}
fn is_on(value: &bool) -> bool {
    *value
}
impl Default for Case {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            description: String::new(),
            keywords: Vec::new(),
            sources: Vec::new(),
            instructions: String::new(),
        }
    }
}

/// A built-in case: what it is and which parts the user may change, with their defaults.
#[derive(Clone, Debug, Serialize)]
pub struct BuiltIn {
    pub id: &'static str,
    pub name: &'static str,
    /// What it covers, for the user.
    pub summary: &'static str,
    /// The default description the model recognizes it by; `None` when code alone decides.
    pub description: Option<&'static str>,
    /// The default instructions; `None` when it is answered with a fixed text or not at all.
    pub instructions: Option<&'static str>,
    /// Whether extra keywords apply.
    pub keywords: bool,
    /// Whether its sources can be chosen, and which kinds of sources it reads.
    pub sources: Option<&'static str>,
    /// Whether it can be turned off.
    pub optional: bool,
}

pub const BUILT_IN: [BuiltIn; 7] = [
    BuiltIn {
        id: "general",
        name: "Every answer",
        summary: "How every answer sounds, and how the model recognizes a question.",
        description: Some(prompts::CLASSIFY_QUESTION),
        instructions: Some(prompts::GENERAL),
        keywords: false,
        sources: None,
        optional: false,
    },
    BuiltIn {
        id: "status_round",
        name: "Status round",
        summary: "Progress, commitments, risks or blockers of your work («avance», «compromiso», «bloqueos», a named HU).",
        description: None,
        instructions: Some(prompts::STATUS_ROUND),
        keywords: true,
        sources: Some(
            "Azure DevOps activity, Wiki (your own recent pages) and the activity record",
        ),
        optional: true,
    },
    BuiltIn {
        id: "recap",
        name: "What you did",
        summary: "What you did on a day or period («¿qué hice ayer?», «la semana pasada»), from the activity record alone.",
        description: None,
        instructions: Some(prompts::RECAP),
        keywords: true,
        sources: None,
        optional: true,
    },
    BuiltIn {
        id: "activity_review",
        name: "Activity review",
        summary: "Your work that no task records yet, compared with your work items.",
        description: Some(prompts::CLASSIFY_ACTIVITY_REVIEW),
        instructions: Some(prompts::ACTIVITY_REVIEW),
        keywords: true,
        sources: Some(
            "Azure DevOps activity, Wiki edits, your Teams messages and the activity record",
        ),
        optional: true,
    },
    BuiltIn {
        id: "documentation",
        name: "Documentation",
        summary: "How something works or is used («según la wiki», endpoints, procedures), from the Wiki.",
        description: None,
        instructions: Some(prompts::DOCUMENTATION),
        keywords: true,
        sources: Some("Wiki"),
        optional: true,
    },
    BuiltIn {
        id: "personal_request",
        name: "Request for you",
        summary: "A call, a meeting or your availability: left for you to answer, outside your personal chat.",
        description: Some(prompts::CLASSIFY_PERSONAL),
        instructions: None,
        keywords: true,
        sources: None,
        optional: true,
    },
    BuiltIn {
        id: "statement",
        name: "Informational message",
        summary: "A message that asks for no answer: left unanswered, and replied to only when you ask for an answer in Messages.",
        description: Some(prompts::CLASSIFY_STATEMENT),
        instructions: Some(prompts::STATEMENT),
        keywords: false,
        sources: None,
        optional: false,
    },
];

pub fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_IN.iter().find(|b| b.id == id)
}

/// Check the user's changes against what each built-in case allows.
pub fn validate(cases: &[Case], source_exists: impl Fn(&str) -> bool) -> Result<()> {
    let plain = |text: &str, max: usize| {
        text.chars().count() <= max
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
    };
    let mut seen = std::collections::BTreeSet::new();
    for case in cases {
        let built = built_in(&case.id).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown case {}; cases are {}",
                case.id,
                BUILT_IN.map(|b| b.id).join(", ")
            )
        })?;
        ensure!(seen.insert(case.id.as_str()), "duplicate case {}", case.id);
        ensure!(
            case.enabled || built.optional,
            "the {} case cannot be turned off",
            case.id
        );
        ensure!(
            case.description.is_empty() || built.description.is_some(),
            "the {} case has no description: code recognizes it",
            case.id
        );
        ensure!(
            case.instructions.is_empty() || built.instructions.is_some(),
            "the {} case has no instructions",
            case.id
        );
        ensure!(
            case.keywords.is_empty() || built.keywords,
            "the {} case takes no keywords",
            case.id
        );
        ensure!(
            case.sources.is_empty() || built.sources.is_some(),
            "the {} case reads no sources of its own",
            case.id
        );
        ensure!(
            plain(&case.description, 2000) && plain(&case.instructions, 8000),
            "a case's description has at most 2000 characters and its instructions 8000"
        );
        ensure!(
            case.keywords.len() <= 100
                && case
                    .keywords
                    .iter()
                    .all(|k| !k.trim().is_empty() && plain(k, 80)),
            "a case has at most 100 keywords of at most 80 characters"
        );
        ensure!(
            case.sources.len() <= 50 && case.sources.iter().all(|id| source_exists(id)),
            "a case's sources must exist in the knowledge map"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_offers_only_what_it_can_use() {
        let exists = |id: &str| id == "wiki";
        let case = |id: &str| Case {
            id: id.into(),
            ..Default::default()
        };
        assert!(validate(&[case("status_round")], exists).is_ok());
        assert!(validate(&[case("unknown")], exists).is_err());
        assert!(validate(&[case("recap"), case("recap")], exists).is_err());
        // The general case is always on; recap is recognized by code, not described.
        assert!(
            validate(
                &[Case {
                    enabled: false,
                    ..case("general")
                }],
                exists
            )
            .is_err()
        );
        assert!(
            validate(
                &[Case {
                    enabled: false,
                    ..case("recap")
                }],
                exists
            )
            .is_ok()
        );
        assert!(
            validate(
                &[Case {
                    description: "x".into(),
                    ..case("recap")
                }],
                exists
            )
            .is_err()
        );
        assert!(
            validate(
                &[Case {
                    keywords: vec!["x".into()],
                    ..case("general")
                }],
                exists
            )
            .is_err()
        );
        assert!(
            validate(
                &[Case {
                    instructions: "x".into(),
                    ..case("personal_request")
                }],
                exists
            )
            .is_err()
        );
        assert!(
            validate(
                &[Case {
                    sources: vec!["wiki".into()],
                    ..case("documentation")
                }],
                exists
            )
            .is_ok()
        );
        assert!(
            validate(
                &[Case {
                    sources: vec!["gone".into()],
                    ..case("documentation")
                }],
                exists
            )
            .is_err()
        );
        assert!(
            validate(
                &[Case {
                    sources: vec!["wiki".into()],
                    ..case("recap")
                }],
                exists
            )
            .is_err()
        );
        // Saved, a case keeps only what changed.
        let saved = toml::to_string(&Case {
            keywords: vec!["entrega".into()],
            ..case("status_round")
        })
        .unwrap();
        assert_eq!(saved, "id = \"status_round\"\nkeywords = [\"entrega\"]\n");
    }

    #[test]
    fn every_default_text_is_present() {
        for built in BUILT_IN {
            for text in built.description.iter().chain(built.instructions.iter()) {
                assert!(text.len() > 40, "{}", built.id);
            }
        }
    }
}
