//! Optional live Jev regression, synthetic facts only. Never constructs a Graph client.
use anyhow::{Result, ensure};
use personal_teams_assistant::{
    decision::{DecisionGate, Jev, Stage},
    evidence::{Evidence, Reference, TeamsMessage, complete_answer},
    security,
};
use serde_json::json;
use std::collections::BTreeMap;
#[tokio::main]
async fn main() -> Result<()> {
    let gate = Jev {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()?,
        endpoint: "https://api.typesafe.ai/v1/systemone".into(),
        api_key: security::secret("TYPESAFE_API_KEY")?,
        model: "jev-latest".into(),
    };
    let r = Reference {
        id: "wiki:synthetic:p".into(),
        kind: "wiki".into(),
        label: "Project / Wiki / Procedure".into(),
        url: "https://dev.azure.com/synthetic/Project/_wiki/wikis/wiki?pagePath=%2FProcedure"
            .into(),
        organization: "https://dev.azure.com/synthetic".into(),
        project: "Project".into(),
        aliases: vec![],
        parent: None,
        revision: Some("synthetic-revision".into()),
        authority: Some("edited_by_me".into()),
        author: None,
        author_role: None,
    };
    let e=Evidence{text:"Página wiki:synthetic:p, procedimiento documentado: revisar el artefacto antes de configurar el despliegue. También documenta un stage llamado Deploy; no se recuperó una referencia verificable al stage. La documentación no prueba ejecución.".into(),references:vec![r],..Default::default()};
    for (name, body, expected) in [
        (
            "wiki_generic_procedure",
            "El procedimiento documentado consiste en revisar el artefacto antes de configurar el despliegue. La wiki no acredita una ejecución.",
            true,
        ),
        (
            "named_stage_without_reference",
            "El procedimiento exige configurar el stage Deploy.",
            false,
        ),
    ] {
        let used = vec!["wiki:synthetic:p".into()];
        let answer = complete_answer(body, &used, &e, 3000)?;
        let v=gate.evaluate(Stage::Final,json!({"question":"Según la wiki, explica el procedimiento de despliegue","evidence":e.text,"references":e.references,"used_sources":used,"answer":answer,"teams_messages":[]}),BTreeMap::new()).await?;
        let allowed = v.selected == "allow" && v.allows(0.65);
        println!("{name}: allowed={allowed}, confidence={}", v.confidence);
        ensure!(
            allowed == expected,
            "live synthetic final-gate regression failed: {name}"
        );
    }
    let messages: Vec<_> = [
        ("a", "Ana", "Pidió revisar el procedimiento"),
        ("l", "Luis", "Indicó que faltaba validar"),
    ]
    .into_iter()
    .map(|(id, name, text)| TeamsMessage {
        conversation: "synthetic".into(),
        message: id.into(),
        sender: id.into(),
        name: Some(name.into()),
        mine: false,
        date: "2026-09-30".into(),
        text: text.into(),
    })
    .collect();
    let v=gate.evaluate(Stage::Final,json!({"question":"Resume quién pidió revisar y quién indicó que faltaba validar","evidence":serde_json::to_string(&messages)?,"teams_messages":messages,"references":[],"used_sources":[],"answer":"Luis pidió revisar el procedimiento y Ana indicó que faltaba validar."}),BTreeMap::new()).await?;
    let allowed = v.selected == "allow" && v.allows(0.65);
    println!(
        "teams_swapped_authors: allowed={allowed}, confidence={}",
        v.confidence
    );
    ensure!(!allowed, "live Teams attribution regression failed");
    Ok(())
}
