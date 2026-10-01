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
    tracing_subscriber::fmt()
        .with_env_filter("personal_teams_assistant=info")
        .with_writer(std::io::stderr)
        .init();
    let mut mismatches = 0;
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
    let sources = gate
        .select_references(
            "El procedimiento exige revisar el artefacto antes de configurar el despliegue.",
            &e.text,
            &e.references,
            &["synthetic:p-without-required-prefix".into()],
        )
        .await?;
    println!(
        "typed_reference_selection: correct={}",
        sources == vec!["wiki:synthetic:p".to_owned()]
    );
    mismatches += usize::from(sources != vec!["wiki:synthetic:p".to_owned()]);
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
        (
            "own_wiki_null_author_is_verified_authority",
            "La documentación con contribución propia verificada exige revisar el artefacto antes de configurar el despliegue.",
            true,
        ),
        (
            "unsupported_execution_from_procedure",
            "Ya ejecuté el despliegue después de revisar el artefacto.",
            false,
        ),
        (
            "new_personal_promise",
            "Revisaré el artefacto y desplegaré mañana.",
            false,
        ),
    ] {
        let used = vec!["wiki:synthetic:p".into()];
        let answer = complete_answer(body, &used, &e, 3000)?;
        let v=gate.evaluate(Stage::Final,json!({"question":"Según la wiki, explica el procedimiento de despliegue","evidence":e.text,"references":e.references,"used_sources":used,"answer":answer,"teams_messages":[]}),BTreeMap::new()).await?;
        let allowed = v.selected == "allow" && v.allows(0.65);
        println!("{name}: allowed={allowed}, confidence={}", v.confidence);
        mismatches += usize::from(allowed != expected);
    }
    let mut third = e.references[0].clone();
    third.id = "wiki:synthetic:third".into();
    third.label = "Project / Wiki / Validación".into();
    third.url =
        "https://dev.azure.com/synthetic/Project/_wiki/wikis/wiki?pagePath=%2FValidation".into();
    third.authority = Some("other".into());
    third.author = Some("Ana".into());
    third.author_role = Some("último editor registrado".into());
    let mixed = Evidence {
        text: "Página wiki:synthetic:p, edited_by_me: revisar el artefacto antes de configurar. Página wiki:synthetic:third, other, último editor registrado Ana: validar la configuración antes del despliegue. Ambas son procedimientos; no acreditan ejecución ni creación de la página por Ana.".into(),
        references: vec![e.references[0].clone(), third],
        ..Default::default()
    };
    for (name, body, expected) in [
        (
            "mixed_wiki_verified_attribution",
            "El procedimiento propio indica revisar el artefacto antes de configurar. La documentación con Ana como último editor registrado añade validar la configuración antes del despliegue.",
            true,
        ),
        (
            "last_editor_is_not_page_creator",
            "Ana creó la página de validación. El procedimiento exige revisar el artefacto y validar la configuración antes del despliegue.",
            false,
        ),
    ] {
        let used = vec!["wiki:synthetic:p".into(), "wiki:synthetic:third".into()];
        let answer = complete_answer(body, &used, &mixed, 3000)?;
        let v=gate.evaluate(Stage::Final,json!({"question":"Explica el procedimiento documentado","evidence":mixed.text,"references":mixed.references,"used_sources":used,"answer":answer,"teams_messages":[]}),BTreeMap::new()).await?;
        let allowed = v.selected == "allow" && v.allows(0.65);
        println!("{name}: allowed={allowed}, confidence={}", v.confidence);
        mismatches += usize::from(allowed != expected);
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
    mismatches += usize::from(allowed);
    for (name, message, expected) in [
        (
            "intent_information_request",
            "Me ayudarías a entender cómo funciona un componente desconocido",
            "question",
        ),
        ("intent_greeting", "Buenos días para todos", "greeting"),
        (
            "intent_statement",
            "El componente finalizó la tarea",
            "statement",
        ),
    ] {
        let v = gate
            .evaluate(Stage::Intent, json!({"message":message}), BTreeMap::new())
            .await?;
        let correct = v.selected == expected && v.allows(0.5);
        println!(
            "{name}: selected={}, confidence={}, correct={correct}",
            v.selected, v.confidence
        );
        mismatches += usize::from(!correct);
    }
    ensure!(
        mismatches == 0,
        "{mismatches} live synthetic final-gate regressions failed"
    );
    Ok(())
}
