use anyhow::Result;
use personal_teams_assistant::{
    ado,
    config::Config,
    decision::{DecisionGate, Jev, Stage},
    llm::{DeepSeek, GenerationInput, LlmProvider},
    security::Redactor,
};
use serde_json::json;
use std::collections::BTreeMap;

#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::var("AZURE_DEVOPS_TOKEN")?;
    let catalog = std::fs::read_to_string("../personal-teams-knowledge/azure-devops.toml")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "¿Qué hice esta semana y qué impedimentos tengo?".into());
    let evidence = ado::status(&client, &key, &catalog, &question).await?;
    println!(
        "Azure DevOps read succeeded: {} characters, {} active projects, {} work items, {} commits, {} pipelines, {} stages, {} releases; RPF included: {}; partial coverage: {}",
        evidence.chars().count(),
        evidence.matches("Proyecto: ").count(),
        evidence.matches("Work item ").count(),
        evidence.matches("Commit personal").count(),
        evidence.matches("Pipeline ").count(),
        evidence.matches("Etapa ").count(),
        evidence.matches("Release ").count(),
        evidence.contains("Proyecto: Sistema de Red Pronostico Fitosanitario (RPF)"),
        evidence.contains("Cobertura parcial")
    );
    if let Some(rpf) = evidence
        .split("Proyecto: Sistema de Red Pronostico Fitosanitario (RPF).")
        .nth(1)
        .and_then(|rest| rest.split("\nProyecto:").next())
    {
        println!(
            "RPF: {} work items, {} commits, {} pipelines",
            rpf.matches("Work item ").count(),
            rpf.matches("Commit personal").count(),
            rpf.matches("Pipeline ").count()
        );
    }
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
