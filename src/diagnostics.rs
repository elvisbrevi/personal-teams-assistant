//! Installed provider probes use only synthetic facts, never local repositories or Teams.
use crate::{
    config::Config,
    llm::{self, Failure, GenerationInput, LlmProvider},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

/// Why a probe failed: the provider's failure class or a check on its answer. Never the
/// provider's text.
#[derive(Debug)]
struct ProbeFailure(&'static str);
impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for ProbeFailure {}

fn failure(error: &anyhow::Error) -> &'static str {
    if let Some(probe) = error.downcast_ref::<ProbeFailure>() {
        return probe.0;
    }
    llm::failure_class(error).map_or(Failure::Failed.as_str(), Failure::as_str)
}

/// Probe each enabled provider on its own, so a fallback cannot hide a broken default. A
/// provider passes when it answers both synthetic questions with a non-empty answer in the
/// JSON contract and classifies a synthetic message. Whether it chose the detailed mode and
/// the intent it returned are reported, not required: the pipeline never depends on them.
pub async fn providers(config: &Config) -> Result<Value> {
    let evidence = "Synthetic test data: support Monday to Friday from 09:00 to 18:00. Maintenance on Sunday from 02:00 to 03:00. The maintenance window is independent of support.";
    let mut checks = Vec::new();
    for choice in config.llm.active() {
        let label = format!("{}:{}:{}", choice.provider, choice.model, choice.effort);
        let probe = async {
            let model = llm::model_for(&choice, &config.llm, &mut Vec::new())?;
            let mut answers = Vec::new();
            for (question, detail_requested) in [
                ("What are the support hours? Answer briefly.", false),
                (
                    "Explain in detail the difference between support and maintenance, with their hours and how they relate.",
                    true,
                ),
            ] {
                let generated = model
                    .generate_response(GenerationInput {
                        question,
                        evidence,
                        detail_requested,
                        history: "",
                        review: false,
                        proposing: false,
                    })
                    .await?;
                let chars = generated.answer.trim().chars().count();
                ensure!(chars > 0, ProbeFailure("empty_answer"));
                answers.push(json!({
                    "detail_requested": detail_requested,
                    "detailed": generated.detailed,
                    "chars": chars,
                }));
            }
            let intent = model
                .classify_intent("Could you help me understand the support hours", "")
                .await?;
            Ok::<_, anyhow::Error>(json!({
                "answers": answers,
                "intent": intent.intent.as_str(),
                "intent_confidence": intent.confidence,
            }))
        };
        checks.push(match probe.await {
            Ok(result) => json!({"provider":label,"ok":true,"checks":result}),
            Err(error) => json!({"provider":label,"ok":false,"failure":failure(&error)}),
        });
    }
    let passed = checks.iter().filter(|c| c["ok"] == true).count();
    Ok(json!({
        "synthetic_only": true,
        "graph_send": false,
        "passed": passed,
        "all_passed": passed == checks.len(),
        "provider_checks": checks,
    }))
}

/// One line for a run where no provider passed: each provider with its failure class.
pub fn failed_summary(report: &Value) -> Option<String> {
    if report["passed"].as_u64() != Some(0) {
        return None;
    }
    let failures: Vec<String> = report["provider_checks"]
        .as_array()?
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                c["provider"].as_str().unwrap_or("provider"),
                c["failure"].as_str().unwrap_or("failed")
            )
        })
        .collect();
    Some(format!(
        "No language model passed the synthetic checks ({}).",
        failures.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_classes_never_carry_provider_text() {
        let error = anyhow::Error::new(ProbeFailure("empty_answer")).context("probe");
        assert_eq!(failure(&error), "empty_answer");
        let error = anyhow::Error::new(llm::Unavailable(Failure::UsageLimit))
            .context("You've hit your usage limit");
        assert_eq!(failure(&error), "usage_limit");
        assert_eq!(failure(&anyhow::anyhow!("secret provider text")), "failed");
    }

    #[test]
    fn a_run_without_passing_providers_names_each_failure() {
        let report = json!({"passed":0,"provider_checks":[
            {"provider":"codex:gpt-6.1-sol:medium","ok":false,"failure":"usage_limit"},
            {"provider":"claude:claude-opus-5-5:medium","ok":false,"failure":"invalid_answer"},
        ]});
        assert_eq!(
            failed_summary(&report).unwrap(),
            "No language model passed the synthetic checks (codex:gpt-6.1-sol:medium: usage_limit; claude:claude-opus-5-5:medium: invalid_answer)."
        );
        assert!(failed_summary(&json!({"passed":1,"provider_checks":[]})).is_none());
    }
}
