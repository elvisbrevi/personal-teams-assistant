//! Opt-in live probe using synthetic data only. Never reads chats or knowledge repositories.
use anyhow::{Result, ensure};
use personal_teams_assistant::{
    decision::{DecisionGate, Jev, Stage},
    llm::{DeepSeek, GenerationInput, LlmProvider},
    security,
};
use serde_json::json;
use std::collections::BTreeMap;
#[tokio::main]
async fn main() -> Result<()> {
    let jev_key = security::secret("TYPESAFE_API_KEY")?;
    let deepseek_key = security::secret("DEEPSEEK_API_KEY")?;
    let gate = Jev {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        endpoint: "https://api.typesafe.ai/v1/systemone".into(),
        api_key: jev_key,
        model: "jev-latest".into(),
    };
    let question = "¿Cuál es el horario de soporte?";
    let evidence = "El horario de soporte es de lunes a viernes de 09:00 a 18:00.";
    let routing = gate
        .evaluate(
            Stage::Routing,
            json!({"question":question,"sources":{"hours":"Horario de soporte"}}),
            BTreeMap::from([("hours".into(), "Horario de soporte".into())]),
        )
        .await?;
    println!(
        "Jev routing: selected={}, confidence={}",
        routing.selected, routing.confidence
    );
    let evidence_gate = gate
        .evaluate(
            Stage::Evidence,
            json!({"question":question,"evidence":evidence}),
            BTreeMap::new(),
        )
        .await?;
    println!(
        "Jev evidence: selected={}, confidence={}",
        evidence_gate.selected, evidence_gate.confidence
    );
    let llm = DeepSeek::new(
        &deepseek_key,
        "deepseek-v4-flash",
        "Español breve, una frase.",
        "https://api.deepseek.com",
    )?;
    let answer = llm.generate(GenerationInput { question, evidence }).await?;
    ensure!(!answer.is_empty(), "empty generation");
    println!(
        "Rig/DeepSeek: generated {} characters",
        answer.chars().count()
    );
    let final_gate = gate
        .evaluate(
            Stage::Final,
            json!({"question":question,"evidence":evidence,"answer":answer}),
            BTreeMap::new(),
        )
        .await?;
    println!(
        "Jev final: selected={}, confidence={}",
        final_gate.selected, final_gate.confidence
    );
    println!(
        "Synthetic pipeline would send: {}",
        routing.selected == "hours"
            && routing.allows(0.92)
            && evidence_gate.selected == "allow"
            && evidence_gate.allows(0.95)
            && final_gate.selected == "allow"
            && final_gate.allows(0.97)
    );
    Ok(())
}
