//! Installed provider probes use only synthetic facts, never local repositories or Teams.
use crate::{
    config::Config,
    decision::{DecisionGate, Jev, Stage},
    llm::{DeepSeek, GenerationInput, LlmProvider},
    security,
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::collections::BTreeMap;
pub async fn providers(config: &Config) -> Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let gate = Jev {
        client,
        endpoint: "https://api.typesafe.ai/v1/systemone".into(),
        api_key: security::secret("TYPESAFE_API_KEY")?,
        model: config.jev.model.clone(),
    };
    let llm = DeepSeek::new(
        &security::secret("DEEPSEEK_API_KEY")?,
        &config.llm.model,
        &config.llm.style,
        "https://api.deepseek.com",
    )?;
    let evidence = "Datos ficticios de prueba: soporte lunes a viernes de 09:00 a 18:00. Mantenimiento domingo de 02:00 a 03:00. La ventana de mantenimiento es independiente del soporte.";
    let mut results = Vec::new();
    for (question, expect_detail) in [
        (
            "¿Cuál es el horario de soporte? Responde brevemente.",
            false,
        ),
        (
            "Explica detalladamente la diferencia entre soporte y mantenimiento, con sus horarios y relación.",
            true,
        ),
    ] {
        let generated = llm
            .generate_response(GenerationInput {
                question,
                evidence,
                detail_requested: false,
                max_answer_chars: config.policy.max_answer_chars,
                max_detailed_answer_chars: config.policy.max_detailed_answer_chars,
            })
            .await?;
        let limit = if generated.detailed {
            config.policy.max_detailed_answer_chars
        } else {
            config.policy.max_answer_chars
        };
        let chars = generated.answer.chars().count();
        ensure!(
            chars > 0 && chars <= limit,
            "provider exceeded the selected character limit"
        );
        ensure!(
            generated.detailed == expect_detail,
            "provider selected an unexpected answer mode"
        );
        let verdict = gate
            .evaluate(
                Stage::Final,
                json!({"question":question,"evidence":evidence,"answer":generated.answer}),
                BTreeMap::new(),
            )
            .await?;
        ensure!(
            verdict.selected == "allow" && verdict.allows(config.jev.final_threshold),
            "synthetic final check did not pass"
        );
        results.push(
            json!({"detailed":generated.detailed,"chars":chars,"limit":limit,"final_allowed":true}),
        );
    }
    Ok(json!({"synthetic_only":true,"graph_send":false,"provider_checks":results}))
}
