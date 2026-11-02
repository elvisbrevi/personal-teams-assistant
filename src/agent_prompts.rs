//! The instructions the implementation agents start from: planning children, implementing a
//! work item and writing a new request. The map keeps only what the user changed
//! (`[[agent_prompts]]`); a prompt not listed uses its default. What the agents may not do
//! (untrusted evidence, no provider writes, the JSON contract) is added by code to every prompt
//! and cannot be replaced.

use crate::llm::prompts;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// What the user changed in one built-in prompt.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub id: String,
    /// Replaces the default instructions.
    pub instructions: String,
}

/// A built-in prompt: what it is for and its default instructions.
#[derive(Clone, Debug, Serialize)]
pub struct BuiltIn {
    pub id: &'static str,
    pub name: &'static str,
    /// When the agent receives it, for the user.
    pub summary: &'static str,
    pub instructions: &'static str,
}

pub const PLAN: &str = "plan";
pub const CODE: &str = "code";
pub const REQUEST: &str = "request";

pub const BUILT_IN: [BuiltIn; 3] = [
    BuiltIn {
        id: PLAN,
        name: "Plan children",
        summary: "Implement on an HU, a bug or a GitHub epic: the agent proposes child tasks for you to review before creating them.",
        instructions: prompts::IMPLEMENTATION_PLAN,
    },
    BuiltIn {
        id: CODE,
        name: "Implement",
        summary: "Implement on a task, a development bug or a GitHub issue: the agent changes the code in the selected directories.",
        instructions: prompts::IMPLEMENTATION_CODE,
    },
    BuiltIn {
        id: REQUEST,
        name: "Write a request",
        summary: "Create request in To-Do: the agent writes the new HU, bug or GitHub epic from your context.",
        instructions: prompts::IMPLEMENTATION_REQUEST,
    },
];

/// The instructions in effect for a built-in prompt.
pub fn resolve(changes: &[Prompt], id: &str) -> String {
    changes
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.instructions.clone())
        .or_else(|| {
            BUILT_IN
                .iter()
                .find(|b| b.id == id)
                .map(|b| b.instructions.to_owned())
        })
        .unwrap_or_default()
}

pub fn validate(changes: &[Prompt]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for change in changes {
        ensure!(
            BUILT_IN.iter().any(|b| b.id == change.id),
            "unknown agent prompt {}; prompts are {}",
            change.id,
            BUILT_IN.map(|b| b.id).join(", ")
        );
        ensure!(
            seen.insert(change.id.as_str()),
            "duplicate agent prompt {}",
            change.id
        );
        ensure!(
            !change.instructions.trim().is_empty()
                && change.instructions.chars().count() <= 8000
                && !change
                    .instructions
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "an agent prompt needs instructions of at most 8000 characters"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_replaces_only_its_prompt() {
        let changes = [Prompt {
            id: CODE.into(),
            instructions: "Follow the repository's AGENTS.md.".into(),
        }];
        assert!(validate(&changes).is_ok());
        assert_eq!(
            resolve(&changes, CODE),
            "Follow the repository's AGENTS.md."
        );
        assert_eq!(resolve(&changes, PLAN), prompts::IMPLEMENTATION_PLAN);
        let change = |id: &str, text: &str| Prompt {
            id: id.into(),
            instructions: text.into(),
        };
        assert!(validate(&[change("unknown", "x")]).is_err());
        assert!(validate(&[change(PLAN, " ")]).is_err());
        assert!(validate(&[change(PLAN, "a"), change(PLAN, "b")]).is_err());
        assert!(validate(&[change(REQUEST, &"x".repeat(8001))]).is_err());
    }
}
