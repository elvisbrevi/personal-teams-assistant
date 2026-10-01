//! Installed provider probes use only synthetic facts, never local repositories or Teams.
use crate::{
    config::Config,
    decision::{DecisionGate, Jev, Stage},
    llm::{self, GenerationInput, LlmProvider},
    security,
};
use anyhow::{Result, ensure};
use serde_json::json;
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
    let evidence = "Datos ficticios de prueba: soporte lunes a viernes de 09:00 a 18:00. Mantenimiento domingo de 02:00 a 03:00. La ventana de mantenimiento es independiente del soporte.";
    // Each enabled provider is probed on its own, so a fallback cannot hide a broken default.
    let mut checks = Vec::new();
    for choice in config.llm.active() {
        let label = format!("{}:{}:{}", choice.provider, choice.model, choice.effort);
        let probe = async {
            let model = llm::model_for(&choice, &config.llm.style, &mut Vec::new())?;
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
                let generated = model
                    .generate_response(GenerationInput {
                        question,
                        evidence,
                        detail_requested: false,
                        history: "",
                    })
                    .await?;
                let chars = generated.answer.chars().count();
                ensure!(chars > 0, "provider returned an empty answer");
                ensure!(
                    generated.detailed == expect_detail,
                    "provider selected an unexpected answer mode"
                );
                let verdict = gate
                    .evaluate(
                        Stage::Final,
                        json!({"question":question,"evidence":evidence,"answer":generated.answer}),
                    )
                    .await?;
                ensure!(
                    verdict.selected == "allow" && verdict.allows(config.jev.final_threshold),
                    "synthetic final check did not pass"
                );
                results.push(
                    json!({"detailed":generated.detailed,"chars":chars,"final_allowed":true}),
                );
            }
            Ok::<_, anyhow::Error>(results)
        };
        checks.push(match probe.await {
            Ok(results) => json!({"provider":label,"ok":true,"checks":results}),
            Err(error) => {
                let failure = llm::failure_class(&error).map_or("check_failed", |f| f.as_str());
                json!({"provider":label,"ok":false,"failure":failure})
            }
        });
    }
    let passed = checks.iter().filter(|c| c["ok"] == true).count();
    ensure!(passed > 0, "no LLM provider passed the synthetic checks");
    Ok(
        json!({"synthetic_only":true,"graph_send":false,"all_passed":passed == checks.len(),"provider_checks":checks}),
    )
}
