use anyhow::Result;
use personal_teams_assistant::{
    ado,
    config::Config,
    decision::{DecisionGate, Jev, Stage},
    llm::{DeepSeek, GenerationInput, LlmProvider},
    security::Redactor,
    state::Store,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::var("AZURE_DEVOPS_TOKEN")?;
    let catalog_path = std::env::var("AZURE_DEVOPS_CATALOG")?;
    let catalog = std::fs::read_to_string(catalog_path)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "¿Qué hice esta semana y qué impedimentos tengo?".into());
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(&dir.path().join("activity.db"))?);
    let evidence = ado::status(&client, &key, &catalog, &question, store).await?;
    println!(
        "Azure DevOps read succeeded: {} characters, {} active projects, {} work items, {} commits, {} pipelines, {} stages, {} release definitions, {} releases; partial coverage: {}",
        evidence.chars().count(),
        evidence.matches("Proyecto: ").count(),
        evidence.matches("Work item ").count(),
        evidence.matches("Commit personal").count(),
        evidence.matches("Pipeline ").count(),
        evidence.matches("Etapa ").count(),
        evidence.matches("Definición de release ").count(),
        evidence.matches("Release ").count(),
        evidence.contains("Cobertura parcial")
    );
    if std::env::var_os("ADO_SMOKE_GENERATE").is_some() {
        let config = Config::load("config.toml")?;
        let llm_key = std::env::var("DEEPSEEK_API_KEY")?;
        let redactor = Redactor::new(
            &config.policy.sensitive_patterns,
            vec![key, llm_key.clone()],
        )?;
        let sanitized = redactor.redact(&evidence);
        let llm = DeepSeek::new(
            &llm_key,
            &config.llm.model,
            &config.llm.style,
            "https://api.deepseek.com",
        )?;
        let gate = Jev {
            client: client.clone(),
            endpoint: "https://api.typesafe.ai/v1/systemone".into(),
            api_key: std::env::var("TYPESAFE_API_KEY")?,
            model: config.jev.model,
        };
        let evidence_verdict = gate
            .evaluate(
                Stage::Evidence,
                json!({"question":question,"evidence":sanitized,"source":"azure-devops-status"}),
                BTreeMap::new(),
            )
            .await?;
        println!(
            "Evidence gate: {}, confidence {}, passes {}",
            evidence_verdict.selected,
            evidence_verdict.confidence,
            evidence_verdict.allows(config.jev.evidence_threshold)
        );
        let answer = llm
            .generate(GenerationInput {
                question: &question,
                evidence: &sanitized,
                detail_requested: false,
                max_answer_chars: 3000,
                max_detailed_answer_chars: 8000,
            })
            .await?;
        println!(
            "Generated answer ({} / {} characters):\n{answer}",
            answer.chars().count(),
            config.policy.max_answer_chars
        );
        let verdict = gate
            .evaluate(
                Stage::Final,
                json!({"question":question,"evidence":sanitized,"answer":answer}),
                BTreeMap::new(),
            )
            .await?;
        println!(
            "Final gate: {}, confidence {}, passes {}",
            verdict.selected,
            verdict.confidence,
            verdict.allows(config.jev.final_threshold)
        );
    }
    Ok(())
}
