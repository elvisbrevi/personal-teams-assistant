//! References are captured at retrieval, never reconstructed from model prose.
use crate::security::Redactor;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Evidence {
    pub text: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub references: Vec<Reference>,
    #[serde(default)]
    pub teams: Vec<TeamsMessage>,
    #[serde(default)]
    pub partial: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub text: String,
    pub source_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reference {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub url: String,
    pub organization: String,
    pub project: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub parent: Option<String>,
    pub revision: Option<String>,
    pub authority: Option<String>,
    pub author: Option<String>,
    pub author_role: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamsMessage {
    pub conversation: String,
    pub message: String,
    pub sender: String,
    pub name: Option<String>,
    pub mine: bool,
    pub date: String,
    pub text: String,
}
impl Evidence {
    pub fn sanitize(&mut self, redactor: &Redactor) {
        self.text = redactor.redact(&self.text);
        for block in &mut self.blocks {
            block.text = redactor.redact(&block.text);
        }
        for r in &mut self.references {
            r.label = redactor.redact(&r.label);
            r.aliases = r.aliases.iter().map(|a| redactor.redact(a)).collect();
            r.author = r.author.take().filter(|a| redactor.clean(a));
        }
        for m in &mut self.teams {
            m.text = redactor.redact(&m.text);
            m.name = m.name.take().filter(|n| redactor.clean(n));
        }
    }
    /// Keep each message and each source header together through passage selection.
    pub fn context(&mut self, _question: &str, budget: usize) -> String {
        let blocks = if self.blocks.is_empty() {
            vec![Block {
                text: self.text.clone(),
                source_ids: self.references.iter().map(|r| r.id.clone()).collect(),
            }]
        } else {
            self.blocks.clone()
        };
        let mut out = String::new();
        let teams_reserve = self
            .teams
            .iter()
            .map(|m| serde_json::to_string(m).unwrap().chars().count() + 20)
            .sum::<usize>()
            .min(budget / 3);
        let facts_budget = budget.saturating_sub(teams_reserve);
        let mut included = BTreeSet::new();
        for block in blocks {
            let refs: Vec<_> = self
                .references
                .iter()
                .filter(|r| block.source_ids.contains(&r.id) && !included.contains(&r.id))
                .collect();
            let metadata = serde_json::to_string(&refs).unwrap();
            let part = format!("\nReferencias autorizadas: {metadata}\n{}\n", block.text);
            if out.chars().count() + part.chars().count() <= facts_budget {
                included.extend(block.source_ids);
                out.push_str(&part);
            } else {
                self.partial = true;
            }
        }
        let mut teams = Vec::new();
        for message in &self.teams {
            let refs: Vec<_> = self
                .references
                .iter()
                .filter(|r| {
                    r.aliases.iter().any(|a| {
                        !a.is_empty() && message.text.to_lowercase().contains(&a.to_lowercase())
                    })
                })
                .collect();
            let block = format!(
                "\nMensaje Teams: {}\nReferencias de ese mensaje: {}\n",
                serde_json::to_string(message).unwrap(),
                serde_json::to_string(&refs).unwrap()
            );
            if out.chars().count() + block.chars().count() <= budget {
                out.push_str(&block);
                included.extend(refs.iter().map(|r| r.id.clone()));
                teams.push(message.clone());
            } else {
                self.partial = true;
            }
        }
        self.references.retain(|r| included.contains(&r.id));
        self.teams = teams;
        if self.partial {
            let notice = "\nCobertura parcial: se aplicaron límites de recuperación/contexto; no inferir ausencia de hechos.\n";
            if out.chars().count() + notice.chars().count() <= budget {
                out.push_str(notice);
            }
        }
        out
    }
}
impl Reference {
    pub fn validate(&self) -> Result<()> {
        let u = url::Url::parse(&self.url)?;
        let parts: Vec<_> = u
            .path_segments()
            .ok_or_else(|| anyhow::anyhow!("invalid reference URL"))?
            .collect();
        ensure!(
            u.scheme() == "https"
                && u.host_str() == Some("dev.azure.com")
                && u.port_or_known_default() == Some(443)
                && u.username().is_empty()
                && u.password().is_none()
                && parts.len() >= 3,
            "invalid reference origin"
        );
        let expected = url::Url::parse(&self.organization)?;
        ensure!(
            expected.scheme() == "https" && expected.host_str() == Some("dev.azure.com"),
            "invalid reference organization"
        );
        ensure!(
            parts[0] == expected.path().trim_matches('/'),
            "reference outside organization"
        );
        // Project is encoded by Url::path_segments_mut in every captured reference.
        let mut project_url = expected.clone();
        project_url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid reference organization"))?
            .pop_if_empty()
            .push(&self.project);
        ensure!(
            u.path()
                .starts_with(&format!("{}/", project_url.path().trim_end_matches('/'))),
            "reference outside project"
        );
        ensure!(
            !self.id.is_empty() && !self.label.is_empty(),
            "invalid reference identity"
        );
        Ok(())
    }
    fn citation(&self) -> String {
        let label = self.label.replace(['[', ']', '\n', '\r'], " ");
        let link = format!("[{label}]({})", self.url);
        if self.kind == "wiki" {
            match self.authority.as_deref() {
                Some("created_by_me" | "edited_by_me") => {
                    format!("Fuente: {link} (documentación con contribución propia verificada).")
                }
                _ => match &self.author {
                    Some(name) => format!(
                        "Fuente: {link}, wiki del proyecto {}; {}: {}.",
                        self.label.split(" / ").next().unwrap_or(&self.project),
                        self.author_role
                            .as_deref()
                            .unwrap_or("último editor registrado"),
                        name
                    ),
                    None => format!(
                        "Fuente: {link}, wiki del proyecto {}; no se pudo verificar quién la documentó.",
                        self.label.split(" / ").next().unwrap_or(&self.project)
                    ),
                },
            }
        } else {
            format!("Referencia: {link}.")
        }
    }
}
/// Runs before privacy and Jev's final gate, against the complete rendered answer.
pub fn complete_answer(
    body: &str,
    used: &[String],
    evidence: &Evidence,
    limit: usize,
) -> Result<String> {
    let selected: BTreeSet<_> = used.iter().collect();
    ensure!(selected.len() == used.len(), "duplicate used source");
    for id in &selected {
        ensure!(
            evidence.references.iter().any(|r| &r.id == *id),
            "invented used source"
        );
    }
    if evidence.references.iter().any(|r| r.kind == "wiki") {
        ensure!(
            evidence
                .references
                .iter()
                .any(|r| r.kind == "wiki" && selected.contains(&r.id)),
            "wiki answer missing used sources"
        );
    }
    let lower = body.to_lowercase();
    for r in &evidence.references {
        if r.kind != "wiki"
            && r.aliases
                .iter()
                .any(|a| !a.is_empty() && lower.contains(&a.to_lowercase()))
        {
            ensure!(
                selected.contains(&r.id)
                    || evidence
                        .references
                        .iter()
                        .any(|other| selected.contains(&other.id)
                            && (other.kind == r.kind
                                || (other.kind.starts_with("stage_")
                                    && r.kind.starts_with("stage_"))
                                || (other.kind.starts_with("pipeline_")
                                    && r.kind.starts_with("pipeline_")))
                            && other.aliases.iter().any(|a| !a.is_empty()
                                && lower.contains(&a.to_lowercase())
                                && r.aliases.contains(a))),
                "mentioned entity missing used source"
            );
        }
    }
    let concrete = regex::Regex::new(r"(?i)(?:#|\b(?:HU|work item|build)\s+)\d+")?;
    for entity in concrete.find_iter(body) {
        let mention = entity.as_str().to_lowercase();
        let required_kind = if mention.starts_with("hu") || mention.starts_with("work item") {
            Some("work_item")
        } else if mention.starts_with("build") {
            Some("pipeline_run")
        } else {
            None
        };
        let id = entity
            .as_str()
            .split(|c: char| !c.is_ascii_digit())
            .rfind(|s| !s.is_empty())
            .unwrap_or("");
        let exact_id = regex::Regex::new(&format!(r"#{id}\b"))?;
        ensure!(
            evidence.references.iter().any(|r| r.kind != "wiki"
                && required_kind.is_none_or(|kind| r.kind == kind)
                && selected.contains(&r.id)
                && exact_id.is_match(&r.label)),
            "concrete entity without verified reference"
        );
    }
    // Model-supplied URLs must match verified references. Do not let prose override the registry.
    let urls = regex::Regex::new(r"https?://[^\s<>)\]]+")?;
    for hit in urls.find_iter(body) {
        ensure!(
            evidence
                .references
                .iter()
                .any(|r| r.url == hit.as_str() && selected.contains(&r.id)),
            "unverified answer URL"
        );
    }
    let mut answer = body.to_owned();
    let mut linked = BTreeSet::new();
    for r in &evidence.references {
        if selected.contains(&r.id) {
            r.validate()?;
            if linked.insert((r.url.clone(), r.label.clone())) {
                answer.push_str(&format!("\n\n{}", r.citation()));
            }
        }
    }
    ensure!(
        answer.chars().count() <= limit,
        "complete answer exceeds character budget for references"
    );
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(id: &str, kind: &str) -> Reference {
        Reference {
            id: id.into(),
            kind: kind.into(),
            label: if kind == "wiki" {
                "Project / Wiki / Procedure".into()
            } else {
                "Work item #42".into()
            },
            url: if kind == "wiki" {
                "https://dev.azure.com/test/Project/_wiki/wikis/wiki?pagePath=%2FProcedure".into()
            } else {
                "https://dev.azure.com/test/Project/_workitems/edit/42".into()
            },
            organization: "https://dev.azure.com/test".into(),
            project: "Project".into(),
            aliases: vec!["#42".into()],
            parent: None,
            revision: Some("revision".into()),
            authority: Some("edited_by_me".into()),
            author: None,
            author_role: None,
        }
    }
    #[test]
    fn all_wiki_authority_modes_include_verified_source_and_precise_attribution() {
        for mode in ["created_by_me", "edited_by_me", "other", "unknown"] {
            let mut r = reference("p", "wiki");
            r.authority = Some(mode.into());
            if mode == "other" {
                r.author = Some("Ana".into());
                r.author_role = Some("último editor registrado".into());
            }
            let e = Evidence {
                references: vec![r],
                ..Default::default()
            };
            let a = complete_answer(
                "El procedimiento documentado exige revisar.",
                &["p".into()],
                &e,
                3000,
            )
            .unwrap();
            assert!(a.contains("pagePath=%2FProcedure"));
            if mode == "other" {
                assert!(a.contains("último editor registrado: Ana"));
                assert!(!a.contains("creador"));
            }
            if mode == "unknown" {
                assert!(a.contains("no se pudo verificar quién"));
            }
        }
    }
    #[test]
    fn missing_invented_out_of_scope_sources_and_insufficient_citation_space_fail_closed() {
        let e = Evidence {
            references: vec![reference("p", "wiki"), reference("w", "work_item")],
            ..Default::default()
        };
        assert!(complete_answer("Procedimiento", &[], &e, 3000).is_err());
        assert!(complete_answer("Procedimiento", &["invented".into()], &e, 3000).is_err());
        assert!(complete_answer("Según #42", &["p".into()], &e, 3000).is_err());
        assert!(complete_answer("Según #99", &["p".into()], &e, 3000).is_err());
        assert!(complete_answer("Procedimiento", &["p".into()], &e, 20).is_err());
        assert!(
            complete_answer(
                "https://dev.azure.com/other/Project/_workitems/edit/42",
                &["p".into()],
                &e,
                3000
            )
            .is_err()
        );
        let a = complete_answer("Revisar #42", &["p".into(), "w".into()], &e, 3000).unwrap();
        assert!(a.contains("_workitems/edit/42"));
        assert!(a.contains("pagePath"));
        assert!(complete_answer("Concreta el tema", &[], &Evidence::default(), 100).is_ok());
        let mut bad = e.clone();
        bad.references[0].url =
            "https://dev.azure.com/other/Project/_wiki/wikis/wiki?pagePath=%2FProcedure".into();
        assert!(complete_answer("Procedimiento", &["p".into()], &bad, 3000).is_err());
        bad.references[0] = reference("p", "wiki");
        bad.references[0].organization = "mailto:invalid".into();
        assert!(complete_answer("Procedimiento", &["p".into()], &bad, 3000).is_err());
        bad.references[1].kind = "pipeline_run".into();
        assert!(complete_answer("Work item 42", &["p".into(), "w".into()], &bad, 3000).is_err());
        bad.references[0] = reference("p", "wiki");
        bad.references[1].label = "Build #420".into();
        bad.references[1].aliases.clear();
        assert!(complete_answer("Build 42", &["p".into(), "w".into()], &bad, 3000).is_err());
    }
    #[test]
    fn homonymous_stages_preserve_execution_definition_and_parent_identity() {
        let mut run = reference("stage:run:40", "stage_run");
        run.label = "Stage Deploy, ejecución #40 que lo contiene".into();
        run.aliases = vec!["Deploy".into()];
        run.url = "https://dev.azure.com/test/Project/_build/results?buildId=40".into();
        run.parent = Some("run:40".into());
        let mut config = run.clone();
        config.id = "stage:config:5:2".into();
        config.kind = "stage_configuration".into();
        config.label = "Stage Deploy, configuración en release #5".into();
        config.url =
            "https://dev.azure.com/test/Project/_release?definitionId=5&_a=definition-tasks".into();
        config.parent = Some("definition:5".into());
        let e = Evidence {
            references: vec![run, config],
            ..Default::default()
        };
        let a = complete_answer(
            "Stage Deploy ejecutado en #40",
            &["stage:run:40".into()],
            &e,
            3000,
        )
        .unwrap();
        assert!(a.contains("buildId=40"));
        assert!(!a.contains("definitionId"));
        let b = complete_answer(
            "Stage Deploy configurado",
            &["stage:config:5:2".into()],
            &e,
            3000,
        )
        .unwrap();
        assert!(b.contains("configuración en release #5"));
    }
    #[test]
    fn redacted_names_become_unavailable_and_message_authors_stay_attached() {
        let mut e = Evidence {
            references: vec![reference("p", "wiki")],
            teams: vec![
                TeamsMessage {
                    conversation: "chat".into(),
                    message: "1".into(),
                    sender: "a".into(),
                    name: Some("Ana".into()),
                    mine: false,
                    date: "date".into(),
                    text: "Luis debe revisar".into(),
                },
                TeamsMessage {
                    conversation: "chat".into(),
                    message: "2".into(),
                    sender: "l".into(),
                    name: Some("Luis".into()),
                    mine: false,
                    date: "date".into(),
                    text: "Falta validar".into(),
                },
            ],
            ..Default::default()
        };
        e.references[0].author = Some("Ana".into());
        e.references[0].authority = Some("other".into());
        let r = Redactor::new(&["Ana".into()], vec![]).unwrap();
        e.sanitize(&r);
        assert!(e.references[0].author.is_none());
        assert!(e.teams[0].name.is_none());
        assert_eq!(e.teams[1].name.as_deref(), Some("Luis"));
        assert_eq!(e.teams[1].text, "Falta validar");
    }
}
