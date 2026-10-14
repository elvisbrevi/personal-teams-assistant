//! References are captured at retrieval, never reconstructed from model prose.
use crate::{config::Language, security::Redactor};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

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
    /// Activity review only: what the user may ask to link afterwards in the personal chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<crate::ado::link::LinkOffer>,
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
/// A reference a drafted answer uses, with the words of the answer that name it (empty when
/// the answer uses it without naming it). Code links those words if they are in the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Used {
    pub id: String,
    pub mention: String,
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
        if let Some(offer) = &mut self.links {
            for activity in &mut offer.activities {
                activity.label = redactor.redact(&activity.label);
                activity.short = redactor.redact(&activity.short);
            }
            for item in &mut offer.work_items {
                item.title = redactor.redact(&item.title);
            }
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
            let part = format!("\nAuthorized references: {metadata}\n{}\n", block.text);
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
                    r.aliases
                        .iter()
                        .any(|a| names(&message.text.to_lowercase(), a))
                })
                .collect();
            let block = format!(
                "\nTeams message: {}\nReferences in that message: {}\n",
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
            let notice = "\nPartial coverage: retrieval or context limits applied; do not infer that facts are missing.\n";
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
        if self.kind == "teams_message" {
            // A message link Graph returned for one of the user's own messages.
            ensure!(
                u.scheme() == "https"
                    && u.host_str() == Some("teams.microsoft.com")
                    && u.port_or_known_default() == Some(443)
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.path().starts_with("/l/message/")
                    && !self.id.is_empty()
                    && !self.label.is_empty(),
                "invalid Teams message link"
            );
            return Ok(());
        }
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
    /// The title a Wiki link shows: the page, without its project and wiki.
    fn title(&self) -> &str {
        self.label
            .rsplit(" / ")
            .next()
            .unwrap_or(&self.label)
            .trim()
    }
    /// Who documented a third-party Wiki page, said right after its link («última edición de
    /// Ana»). Own pages and pages without a known author add nothing: the link says where the
    /// documentation is, and missing authorship is never invented.
    fn attribution(&self, language: Language) -> Option<String> {
        if self.kind != "wiki"
            || matches!(
                self.authority.as_deref(),
                Some("created_by_me" | "edited_by_me")
            )
        {
            return None;
        }
        let name = self.author.as_deref()?;
        // The Wiki reader records "last recorded editor" as the only author role; audits
        // before 0.6.1 hold its Spanish form. Either reads as who last edited the page.
        Some(match (language, self.author_role.as_deref()) {
            (Language::Es, None | Some("last recorded editor" | "último editor registrado")) => {
                format!("última edición de {name}")
            }
            (Language::En, None | Some("last recorded editor" | "último editor registrado")) => {
                format!("last edited by {name}")
            }
            (_, Some(role)) => format!("{role}: {name}"),
        })
    }
}
/// Split `total` characters among parts: short parts keep everything, the rest share what is
/// left evenly.
pub fn shares(lengths: &[usize], total: usize) -> Vec<usize> {
    let mut caps = vec![0; lengths.len()];
    let mut open: Vec<usize> = (0..lengths.len()).collect();
    let mut remaining = total;
    while !open.is_empty() {
        let share = remaining / open.len();
        let (fits, rest): (Vec<usize>, Vec<usize>) =
            open.iter().partition(|&&i| lengths[i] <= share);
        if fits.is_empty() {
            for i in rest {
                caps[i] = share;
            }
            break;
        }
        for i in fits {
            caps[i] = lengths[i];
            remaining -= lengths[i];
        }
        open = rest;
    }
    caps
}
/// References the code sees named in the answer: aliases or `#id`, and Wiki pages by a
/// distinctive title. Added to the model's selection, and the whole selection when no model
/// can choose, so a named work item, pipeline or stage always gets its verified link.
pub fn named_references(body: &str, evidence: &Evidence) -> Vec<String> {
    let lower = body.to_lowercase();
    let ids: Vec<String> = regex::Regex::new(r"#(\d+)\b")
        .map(|re| {
            re.captures_iter(body)
                .map(|c| format!("#{}", &c[1]))
                .collect()
        })
        .unwrap_or_default();
    evidence
        .references
        .iter()
        .filter(|r| {
            if r.kind == "wiki" {
                return distinctive(r.title()) && names(&lower, r.title());
            }
            r.aliases.iter().any(|a| names(&lower, a))
                || ids.iter().any(|id| {
                    regex::Regex::new(&format!(r"{id}\b")).is_ok_and(|re| re.is_match(&r.label))
                })
        })
        .map(|r| r.id.clone())
        .collect()
}
/// Whether `lower_text` names `alias` as whole words: «Notificador» does not name the entity
/// inside «notificadorimport», nor `#12` inside `#123`.
fn names(lower_text: &str, alias: &str) -> bool {
    word_position(lower_text, &alias.to_lowercase()).is_some()
}
fn word_position(lower_text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    lower_text.match_indices(name).find_map(|(at, _)| {
        let before = lower_text[..at].chars().next_back();
        let after = lower_text[at + name.len()..].chars().next();
        (!before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric))
            .then_some(at)
    })
}
/// Where the answer first names a reference: an alias, its `#id` or a Wiki page title.
/// References the text does not name sort last.
fn first_mention(lower_body: &str, reference: &Reference) -> usize {
    spoken_names(reference)
        .iter()
        .filter_map(|name| word_position(lower_body, &name.to_lowercase()))
        .min()
        .unwrap_or(usize::MAX)
}
/// The names a reference goes by in prose: a Wiki page's title (its aliases are API URLs), or
/// an entity's aliases and the `#id` of its label.
fn spoken_names(reference: &Reference) -> Vec<String> {
    if reference.kind == "wiki" {
        return vec![reference.title().to_owned()];
    }
    let mut names: Vec<String> = reference
        .aliases
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(number) = regex::Regex::new(r"#\d+")
        .ok()
        .and_then(|re| re.find(&reference.label).map(|m| m.as_str().to_owned()))
    {
        names.push(number);
    }
    names
}
/// A name that identifies its entity on its own: one with a digit or a symbol (`#26477`,
/// `PR !4848`, `sag.portalpagos.ms.solicitud`) or of several words. A single plain word
/// («Desarrollo», «QA») is too common to link without saying what it is.
fn distinctive(name: &str) -> bool {
    name.chars()
        .any(|c| c.is_ascii_digit() || matches!(c, '#' | '!' | '.' | '/' | '_' | '-'))
        || name.split_whitespace().count() > 1
}
/// Words that say what the single-word name after them is: «stage QA», «la página Inicio»,
/// «commit cccccccc».
const KIND_WORDS: [&str; 16] = [
    "commit",
    "stage",
    "etapa",
    "ambiente",
    "entorno",
    "environment",
    "pipeline",
    "release",
    "build",
    "repositorio",
    "repository",
    "repo",
    "página",
    "pagina",
    "page",
    "wiki",
];
/// The longest name the model may mark as naming a reference.
const MENTION_CHARS: usize = 120;
/// Byte ranges no link may touch: code, links already written and bare URLs.
fn protected(body: &str) -> Vec<Range<usize>> {
    [
        r"(?s)```.*?(?:```|\z)",
        r"`[^`\n]*`",
        r"\[[^\]\n]*\]\((?:[^()\s]|\([^()\s]*\))*\)",
        r"https?://[^\s<>)\]]+",
    ]
    .iter()
    .filter_map(|pattern| regex::Regex::new(pattern).ok())
    .flat_map(|re| re.find_iter(body).map(|m| m.range()).collect::<Vec<_>>())
    .collect()
}
fn free(span: &Range<usize>, taken: &[Range<usize>]) -> bool {
    !taken
        .iter()
        .any(|t| t.start < span.end && span.start < t.end)
}
/// Whole-word, case-insensitive occurrences of `name` in `text`, as byte ranges of `text`.
fn occurrences(text: &str, name: &str) -> Vec<Range<usize>> {
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(name))) else {
        return Vec::new();
    };
    re.find_iter(text)
        .filter(|m| {
            !text[..m.start()]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && !text[m.end()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        })
        .map(|m| m.range())
        .collect()
}
/// Where code links `reference` in `body` without a model: the earliest distinctive name of it
/// outside `taken` (or a single word right after a word saying what it is, «stage QA»), joined
/// with the names of it that follow, so «#26477 — PR Pipeline Transversal» is one link.
fn code_span(body: &str, reference: &Reference, taken: &[Range<usize>]) -> Option<Range<usize>> {
    let names = spoken_names(reference);
    let mut best: Option<Range<usize>> = None;
    for name in &names {
        for mut span in occurrences(body, name) {
            if !distinctive(name) {
                let head = &body[..span.start];
                let trimmed = head.trim_end_matches(' ');
                let word = trimmed
                    .char_indices()
                    .rev()
                    .take_while(|(_, c)| c.is_alphanumeric())
                    .last()
                    .map(|(at, _)| at);
                let Some(word) = word.filter(|_| trimmed.len() < head.len()) else {
                    continue;
                };
                if !KIND_WORDS.contains(&trimmed[word..].to_lowercase().as_str()) {
                    continue;
                }
                span.start = word;
            }
            if free(&span, taken)
                && best.as_ref().is_none_or(|b| {
                    span.start < b.start || (span.start == b.start && span.end > b.end)
                })
            {
                best = Some(span);
            }
        }
    }
    let mut span = best?;
    loop {
        let rest = &body[span.end..];
        let at = span.end + rest.len()
            - rest
                .trim_start_matches([' ', '—', '–', '-', ':', '«', '"'])
                .len();
        if at == span.end || at - span.end > 8 {
            break;
        }
        let Some(mut end) = names
            .iter()
            .filter_map(|n| {
                occurrences(&body[at..], n)
                    .first()
                    .filter(|o| o.start == 0)
                    .map(|o| at + o.end)
            })
            .max()
        else {
            break;
        };
        // A quoted name keeps its closing quote inside the link, or is left out.
        match body[span.end..at].chars().next_back() {
            Some('«') if body[end..].starts_with('»') => end += '»'.len_utf8(),
            Some('"') if body[end..].starts_with('"') => end += 1,
            Some('«' | '"') => break,
            _ => {}
        }
        let joined = span.start..end;
        if !free(&joined, taken) {
            break;
        }
        span = joined;
    }
    Some(span)
}
/// Where the words the model marked as naming a reference appear in `body`, outside `taken`.
fn mention_span(body: &str, mention: &str, taken: &[Range<usize>]) -> Option<Range<usize>> {
    let mention = mention.trim().trim_matches(['*', '_']).trim();
    if mention.chars().count() > MENTION_CHARS
        || mention.contains(['\n', '\r', '[', ']', '`', '*', '~', '{', '}', '|'])
    {
        return None;
    }
    occurrences(body, mention)
        .into_iter()
        .find(|span| free(span, taken))
}
/// References code links where the answer names them, without a model's help.
pub fn code_linked(body: &str, evidence: &Evidence) -> BTreeSet<String> {
    let taken = protected(body);
    evidence
        .references
        .iter()
        .filter(|r| code_span(body, r, &taken).is_some())
        .map(|r| r.id.clone())
        .collect()
}
/// The answer with each link reduced to its text. Saved as conversation context: copied
/// URLs would fail the next answer's URL check.
pub fn unlinked(answer: &str) -> String {
    regex::Regex::new(r"\[([^\]\n]+)\]\((?:[^()\s]|\([^()\s]*\))*\)")
        .map(|re| re.replace_all(answer, "$1").into_owned())
        .unwrap_or_else(|_| answer.to_owned())
}
/// Runs before the privacy checks. Every reference selected as used is verified; each one the
/// answer names becomes a link on those very words («[#26477 — PR Pipeline Transversal](…)»),
/// first where code sees a distinctive name, then on the words a model marked in `mentions`.
/// A Wiki page the text uses without naming it is linked in one closing sentence, so a
/// Wiki-based answer always links its pages; third-party pages say who last edited them. No
/// sources section is appended: the full list stays in the audit. URLs in the body must be
/// verified references or appear literally in `evidence_text`.
pub fn complete_answer(
    body: &str,
    used: &[String],
    mentions: &BTreeMap<String, String>,
    evidence: &Evidence,
    evidence_text: &str,
    language: Language,
) -> Result<String> {
    let selected: BTreeSet<_> = used.iter().collect();
    ensure!(selected.len() == used.len(), "duplicate used source");
    for id in &selected {
        ensure!(
            evidence.references.iter().any(|r| &r.id == *id),
            "invented used source"
        );
    }
    let consulted_only = evidence.references.iter().any(|r| r.kind == "wiki")
        && !evidence
            .references
            .iter()
            .any(|r| r.kind == "wiki" && selected.contains(&r.id));
    let lower = body.to_lowercase();
    for r in &evidence.references {
        if r.kind != "wiki" && r.aliases.iter().any(|a| names(&lower, a)) {
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
                                && names(&lower, a)
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
    // Model-supplied URLs must be verified references or be copied from the evidence (a
    // documented endpoint). Prose cannot introduce a new destination.
    let urls = regex::Regex::new(r"https?://[^\s<>)\]]+")?;
    for hit in urls.find_iter(body) {
        let url = hit
            .as_str()
            .trim_end_matches(['`', '.', ',', ';', ':', '"', '\'', '*', '~', '|', '}']);
        ensure!(
            evidence.references.iter().any(|r| r.url == url
                && (selected.contains(&r.id) || (consulted_only && r.kind == "wiki")))
                || evidence_text.contains(url),
            "unverified answer URL"
        );
    }
    // Links follow the answer: the first thing it names is linked first.
    let mut cited: Vec<&Reference> = evidence
        .references
        .iter()
        .filter(|r| selected.contains(&r.id))
        .collect();
    cited.sort_by_key(|r| first_mention(&lower, r));
    for r in &cited {
        r.validate()?;
    }
    // The same entity captured twice (another URL form of its organization) is linked once;
    // stages of one release share its URL but are different entities.
    let same = |a: &Reference, b: &Reference| {
        a.url == b.url
            && a.kind == b.kind
            && (a.kind == "wiki" || a.aliases.iter().any(|x| b.aliases.contains(x)))
    };
    let mut taken = protected(body);
    let mut placed: Vec<&Reference> = cited
        .iter()
        .copied()
        .filter(|r| body.contains(&format!("]({})", r.url)))
        .collect();
    let mut links: Vec<(Range<usize>, &Reference)> = Vec::new();
    for by_model in [false, true] {
        for r in &cited {
            if placed.iter().any(|p| same(p, r)) {
                continue;
            }
            let span = if by_model {
                mentions
                    .get(&r.id)
                    .and_then(|m| mention_span(body, m, &taken))
            } else {
                code_span(body, r, &taken)
            };
            if let Some(span) = span.filter(|s| !body[s.clone()].contains(['[', ']', '\n'])) {
                taken.push(span.clone());
                links.push((span, r));
                placed.push(r);
            }
        }
    }
    let attribution = |r: &Reference| {
        r.attribution(language)
            .filter(|_| {
                r.author
                    .as_deref()
                    .is_none_or(|name| !lower.contains(&name.to_lowercase()))
            })
            .map(|a| format!(" ({a})"))
            .unwrap_or_default()
    };
    links.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut answer = body.to_owned();
    for (span, r) in links {
        let link = format!("[{}]({}){}", &body[span.clone()], r.url, attribution(r));
        answer.replace_range(span, &link);
    }
    let mut answer = answer.trim_end().to_owned();
    let mut pages: Vec<&Reference> = Vec::new();
    for r in cited.iter().filter(|r| r.kind == "wiki") {
        if !placed.iter().chain(&pages).any(|p| same(p, r)) {
            pages.push(r);
        }
    }
    let pages: Vec<String> = pages
        .iter()
        .map(|r| {
            let title = r.title().replace(['[', ']', '\n', '\r'], " ");
            format!("[{}]({}){}", title.trim(), r.url, attribution(r))
        })
        .collect();
    if let Some((last, first)) = pages.split_last() {
        let (lead, and) = match language {
            Language::Es => ("Más detalle en", "y"),
            Language::En => ("More detail in", "and"),
        };
        let list = if first.is_empty() {
            last.clone()
        } else {
            format!("{} {and} {last}", first.join(", "))
        };
        answer.push_str(&format!("\n\n{lead} {list}."));
    }
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
    fn none() -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    const PAGE: &str = "https://dev.azure.com/test/Project/_wiki/wikis/wiki?pagePath=%2FProcedure";
    #[test]
    fn wiki_pages_are_linked_where_named_or_in_one_closing_sentence() {
        for mode in ["created_by_me", "edited_by_me", "other", "unknown"] {
            let mut r = reference("p", "wiki");
            r.authority = Some(mode.into());
            if mode == "other" {
                r.author = Some("Ana".into());
                r.author_role = Some("last recorded editor".into());
            }
            let e = Evidence {
                references: vec![r],
                ..Default::default()
            };
            let a = complete_answer(
                "El procedimiento documentado exige revisar.",
                &["p".into()],
                &none(),
                &e,
                "",
                Language::Es,
            )
            .unwrap();
            // No sources section: one closing sentence links the page by its title, and only
            // a third-party page says who last edited it.
            let attribution = if mode == "other" {
                " (última edición de Ana)"
            } else {
                ""
            };
            assert_eq!(
                a,
                format!(
                    "El procedimiento documentado exige revisar.\n\nMás detalle en [Procedure]({PAGE}){attribution}."
                )
            );
            assert!(!a.contains("Fuentes") && !a.contains("autoría"));
            // Named by the answer, the page is linked on those words instead.
            let a = complete_answer(
                "Según la página Procedure, hay que revisar.",
                &["p".into()],
                &none(),
                &e,
                "",
                Language::Es,
            )
            .unwrap();
            assert_eq!(
                a,
                format!("Según la [página Procedure]({PAGE}){attribution}, hay que revisar.")
            );
            let marked = BTreeMap::from([("p".to_string(), "procedimiento documentado".into())]);
            let a = complete_answer(
                "El procedimiento documentado exige revisar.",
                &["p".into()],
                &marked,
                &e,
                "",
                Language::Es,
            )
            .unwrap();
            assert_eq!(
                a,
                format!("El [procedimiento documentado]({PAGE}){attribution} exige revisar.")
            );
        }
        // An answer that already names the editor does not repeat it.
        let mut r = reference("p", "wiki");
        r.authority = Some("other".into());
        r.author = Some("Ana".into());
        let e = Evidence {
            references: vec![r],
            ..Default::default()
        };
        let a = complete_answer(
            "Ana documentó que hay que revisar.",
            &["p".into()],
            &none(),
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert!(
            a.ends_with(&format!("Más detalle en [Procedure]({PAGE}).")),
            "{a}"
        );
    }
    #[test]
    fn named_entities_become_links_on_their_own_words() {
        let mut item = reference("w26477", "work_item");
        item.label = "#26477 PR Pipeline Transversal — New".into();
        item.url = "https://dev.azure.com/test/Project/_workitems/edit/26477".into();
        item.aliases = vec![
            "#26477".into(),
            "HU 26477".into(),
            "work item 26477".into(),
            "PR Pipeline Transversal".into(),
        ];
        let mut copy = item.clone();
        copy.id = "w26477-other-organization-form".into();
        copy.aliases = vec!["#26477".into(), "work item 26477".into()];
        let mut stage = reference("stage", "stage_configuration");
        stage.label = "Stage Desarrollo, configured in release #29".into();
        stage.url =
            "https://dev.azure.com/test/Project/_release?definitionId=29&_a=definition-tasks"
                .into();
        stage.aliases = vec!["Desarrollo".into()];
        let mut pr = reference("pr4848", "pull_request");
        pr.label = "PR #4848 Template dotnet10 (templates.pipelines)".into();
        pr.project = "Soluciones Transversales".into();
        pr.url = "https://dev.azure.com/test/Soluciones%20Transversales/_git/templates.pipelines/pullrequest/4848".into();
        pr.aliases = vec![
            "PR #4848".into(),
            "PR !4848".into(),
            "PR 4848".into(),
            "pull request 4848".into(),
            "!4848".into(),
        ];
        let page = reference("pap", "wiki");
        let e = Evidence {
            references: vec![page, item, copy, stage, pr],
            ..Default::default()
        };
        let body = "Tengo identificado el **#26477 — PR Pipeline Transversal**, pero está en **Cobro Pago y Tarifas**. No tengo el ticket en **Sección Desarrollo / Apoyo Coor-Dev** con el enlace al PR !4848.";
        let used = named_references(body, &e);
        assert_eq!(
            used,
            [
                "w26477",
                "w26477-other-organization-form",
                "stage",
                "pr4848"
            ]
        );
        // «Sección Desarrollo» is a project, not the stage: a single common word is linked
        // only with a model's mark or right after a word saying what it is.
        assert_eq!(
            code_linked(body, &e),
            BTreeSet::from([
                "w26477".to_string(),
                "w26477-other-organization-form".into(),
                "pr4848".into()
            ])
        );
        let a = complete_answer(body, &used, &none(), &e, "", Language::Es).unwrap();
        assert_eq!(
            a,
            "Tengo identificado el **[#26477 — PR Pipeline Transversal](https://dev.azure.com/test/Project/_workitems/edit/26477)**, pero está en **Cobro Pago y Tarifas**. No tengo el ticket en **Sección Desarrollo / Apoyo Coor-Dev** con el enlace al [PR !4848](https://dev.azure.com/test/Soluciones%20Transversales/_git/templates.pipelines/pullrequest/4848)."
        );
        let html = crate::adapters::teams::html(&a);
        assert!(
            html.contains("<b><a href=\"https://dev.azure.com/test/Project/_workitems/edit/26477\">#26477 — PR Pipeline Transversal</a></b>"),
            "{html}"
        );
        // The consulted page that no one selected stays in the audit only.
        let a = complete_answer(body, &used, &none(), &e, "", Language::Es).unwrap();
        assert!(!a.contains("pagePath") && !a.contains("Páginas consultadas"));
        // A stage named as such is linked on its name.
        let a = complete_answer(
            "Falta el stage Desarrollo de #26477.",
            &["stage".into(), "w26477".into()],
            &none(),
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert!(a.starts_with("Falta el [stage Desarrollo](https://dev.azure.com/test/Project/_release?definitionId=29&_a=definition-tasks) de [#26477]("), "{a}");
    }
    #[test]
    fn model_marks_never_link_inside_code_or_existing_links() {
        let e = Evidence {
            references: vec![reference("w", "work_item")],
            ..Default::default()
        };
        let marked = BTreeMap::from([("w".to_string(), "**la tarea**".into())]);
        let a = complete_answer(
            "Revisa `#42` y la tarea pendiente.",
            &["w".into()],
            &marked,
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert_eq!(
            a,
            "Revisa `#42` y [la tarea](https://dev.azure.com/test/Project/_workitems/edit/42) pendiente."
        );
        // A link the answer already has is kept, and words missing from it are ignored.
        let body = "Ver [#42](https://dev.azure.com/test/Project/_workitems/edit/42).";
        let marked = BTreeMap::from([("w".to_string(), "otra cosa".into())]);
        assert_eq!(
            complete_answer(body, &["w".into()], &marked, &e, "", Language::Es).unwrap(),
            body
        );
        assert_eq!(
            unlinked("Ver [#42](https://x.test/a_(b)) y [A](https://b.test)."),
            "Ver #42 y A."
        );
        assert!(distinctive("#26477") && distinctive("PR !4848"));
        assert!(distinctive("sag.portalpagos.ms.solicitud") && distinctive("Portal Recaudación"));
        assert!(!distinctive("Desarrollo") && !distinctive("QA"));
    }
    #[test]
    fn aliases_match_whole_words_only() {
        assert!(names("ejecuté el run #12 de notificador.", "Notificador"));
        assert!(!names("ejecuté notificadorimport", "Notificador"));
        assert!(!names("ver #123", "#12"));
        assert!(names("ver #12, listo", "#12"));
        assert!(names("pr #5 y (pr #6)", "PR #6"));
        assert!(!names("algo", ""));
    }

    #[test]
    fn each_named_reference_is_linked_where_the_answer_names_it() {
        let mut first = reference("w12", "work_item");
        first.label = "#12 First task — Active".into();
        first.url = "https://dev.azure.com/test/Project/_workitems/edit/12".into();
        first.aliases = vec!["#12".into()];
        let mut pr = reference("pr7", "pull_request");
        pr.label = "PR #7 Change (repo)".into();
        pr.url = "https://dev.azure.com/test/Project/_git/repo/pullrequest/7".into();
        pr.aliases = vec!["PR #7".into()];
        let mut unnamed = reference("w123", "work_item");
        unnamed.label = "#123 Other task — Done".into();
        unnamed.url = "https://dev.azure.com/test/Project/_workitems/edit/123".into();
        unnamed.aliases = vec![];
        let e = Evidence {
            references: vec![unnamed.clone(), first.clone(), pr.clone()],
            ..Default::default()
        };
        let a = complete_answer(
            "I merged PR #7, which belongs in #12.",
            &["w123".into(), "w12".into(), "pr7".into()],
            &none(),
            &e,
            "",
            Language::En,
        )
        .unwrap();
        // A reference the text uses without naming it stays in the audit only.
        assert_eq!(
            a,
            "I merged [PR #7](https://dev.azure.com/test/Project/_git/repo/pullrequest/7), which belongs in [#12](https://dev.azure.com/test/Project/_workitems/edit/12)."
        );
    }

    #[test]
    fn english_answers_get_english_links_and_attribution() {
        let mut other = reference("p", "wiki");
        other.authority = Some("other".into());
        other.author = Some("Ana".into());
        other.author_role = Some("last recorded editor".into());
        let mut second = reference("q", "wiki");
        second.label = "Project / Wiki / Deploy guide".into();
        second.url =
            "https://dev.azure.com/test/Project/_wiki/wikis/wiki?pagePath=%2FDeploy".into();
        second.authority = Some("unknown".into());
        let e = Evidence {
            references: vec![other, second],
            ..Default::default()
        };
        let a = complete_answer(
            "The procedure says so.",
            &["p".into(), "q".into()],
            &none(),
            &e,
            "",
            Language::En,
        )
        .unwrap();
        assert_eq!(
            a,
            format!(
                "The procedure says so.\n\nMore detail in [Procedure]({PAGE}) (last edited by Ana) and [Deploy guide](https://dev.azure.com/test/Project/_wiki/wikis/wiki?pagePath=%2FDeploy)."
            )
        );
        // Consulted pages that the answer does not use are not listed.
        let a = complete_answer("Not documented.", &[], &none(), &e, "", Language::En).unwrap();
        assert_eq!(a, "Not documented.");
    }
    #[test]
    fn missing_invented_and_out_of_scope_sources_fail_closed() {
        let e = Evidence {
            references: vec![reference("p", "wiki"), reference("w", "work_item")],
            ..Default::default()
        };
        // No page selected as used: the answer goes as written, without consulted pages.
        assert_eq!(
            complete_answer("No está documentado.", &[], &none(), &e, "", Language::Es).unwrap(),
            "No está documentado."
        );
        assert!(
            complete_answer(
                "Procedimiento",
                &["invented".into()],
                &none(),
                &e,
                "",
                Language::Es
            )
            .is_err()
        );
        assert!(
            complete_answer("Según #42", &["p".into()], &none(), &e, "", Language::Es).is_err()
        );
        assert!(
            complete_answer("Según #99", &["p".into()], &none(), &e, "", Language::Es).is_err()
        );
        assert_eq!(named_references("Según #42", &e), vec!["w".to_string()]);
        assert!(named_references("Según #99", &e).is_empty());
        assert!(
            complete_answer(
                "https://dev.azure.com/other/Project/_workitems/edit/42",
                &["p".into()],
                &none(),
                &e,
                "",
                Language::Es
            )
            .is_err()
        );
        // A documented endpoint copied from the evidence is allowed; an invented one is not.
        let evidence_text = "Ambiente test: https://api-test.example.cl/api/v1/solicitudes/crear";
        let a = complete_answer(
            "En test: `https://api-test.example.cl/api/v1/solicitudes/crear`.",
            &["p".into()],
            &none(),
            &e,
            evidence_text,
            Language::Es,
        )
        .unwrap();
        assert!(a.contains("api-test.example.cl"));
        // Formatting right after the URL (a table cell, a color) is not part of it.
        let a = complete_answer(
            "| Ambiente | URL |\n|---|---|\n| test |https://api-test.example.cl/api/v1/solicitudes/crear|\n\n{blue:https://api-test.example.cl/api/v1/solicitudes/crear}",
            &["p".into()], &none(),
            &e,
            evidence_text,
            Language::Es,
        )
        .unwrap();
        assert!(a.contains("{blue:"));
        assert!(
            complete_answer(
                "En test: https://api-prod.example.cl/api/v1/solicitudes/crear",
                &["p".into()],
                &none(),
                &e,
                evidence_text,
                Language::Es
            )
            .is_err()
        );
        let a = complete_answer(
            "Revisar #42",
            &["p".into(), "w".into()],
            &none(),
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert_eq!(
            a,
            format!(
                "Revisar [#42](https://dev.azure.com/test/Project/_workitems/edit/42)\n\nMás detalle en [Procedure]({PAGE})."
            )
        );
        assert!(
            complete_answer(
                "Concreta el tema",
                &[],
                &none(),
                &Evidence::default(),
                "",
                Language::Es
            )
            .is_ok()
        );
        let mut bad = e.clone();
        bad.references[0].url =
            "https://dev.azure.com/other/Project/_wiki/wikis/wiki?pagePath=%2FProcedure".into();
        assert!(
            complete_answer(
                "Procedimiento",
                &["p".into()],
                &none(),
                &bad,
                "",
                Language::Es
            )
            .is_err()
        );
        // A reference the answer does not use never reaches it.
        assert!(complete_answer("Procedimiento", &[], &none(), &bad, "", Language::Es).is_ok());
        bad.references[0] = reference("p", "wiki");
        bad.references[0].organization = "mailto:invalid".into();
        assert!(
            complete_answer(
                "Procedimiento",
                &["p".into()],
                &none(),
                &bad,
                "",
                Language::Es
            )
            .is_err()
        );
        bad.references[1].kind = "pipeline_run".into();
        assert!(
            complete_answer(
                "Work item 42",
                &["p".into(), "w".into()],
                &none(),
                &bad,
                "",
                Language::Es
            )
            .is_err()
        );
        bad.references[0] = reference("p", "wiki");
        bad.references[1].label = "Build #420".into();
        bad.references[1].aliases.clear();
        assert!(
            complete_answer(
                "Build 42",
                &["p".into(), "w".into()],
                &none(),
                &bad,
                "",
                Language::Es
            )
            .is_err()
        );
    }
    #[test]
    fn homonymous_stages_preserve_execution_definition_and_parent_identity() {
        let mut run = reference("stage:run:40", "stage_run");
        run.label = "Stage Deploy, in run #40".into();
        run.aliases = vec!["Deploy".into()];
        run.url = "https://dev.azure.com/test/Project/_build/results?buildId=40".into();
        run.parent = Some("run:40".into());
        let mut config = run.clone();
        config.id = "stage:config:5:2".into();
        config.kind = "stage_configuration".into();
        config.label = "Stage Deploy, configured in release #5".into();
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
            &none(),
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert_eq!(
            a,
            "[Stage Deploy](https://dev.azure.com/test/Project/_build/results?buildId=40) ejecutado en #40"
        );
        let b = complete_answer(
            "Stage Deploy configurado",
            &["stage:config:5:2".into()],
            &none(),
            &e,
            "",
            Language::Es,
        )
        .unwrap();
        assert_eq!(
            b,
            "[Stage Deploy](https://dev.azure.com/test/Project/_release?definitionId=5&_a=definition-tasks) configurado"
        );
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
