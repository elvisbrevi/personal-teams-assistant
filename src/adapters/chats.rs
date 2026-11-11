//! Reading Teams chats for the operator and agents: a chat's history, the user's own messages
//! across chats and message search. Every read is a GET (search is Microsoft Search's read
//! POST) with the delegated `Chat.Read` scope already granted, and only chats this profile may
//! read on request (`Graph::readable_on_request`: those it receives plus the account's reading
//! scope) are read. Nothing is sent and nothing is stored.

use super::{graph::Graph, teams};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Messages one read returns by default and at most.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
/// Search results returned by default and at most.
pub const DEFAULT_SEARCH_LIMIT: usize = 25;
pub const MAX_SEARCH_LIMIT: usize = 100;
/// Days a read across chats looks back when no start is given.
pub const DEFAULT_DAYS: i64 = 30;
/// Graph pages (50 messages each) one chat's history is read for per call.
const CHAT_PAGES: usize = 20;
/// Chats a read across chats covers, and the pages it reads of each.
const SCAN_CHATS: usize = 40;
const SCAN_PAGES: usize = 10;
/// Pages of the chat list (50 chats each) read to find the recent chats.
const LIST_PAGES: usize = 10;
/// Microsoft Search requests (25 hits each) per search.
const SEARCH_PAGES: usize = 8;
/// Characters of one message's text, and of a quoted message's preview.
const TEXT: usize = 4000;
const PREVIEW: usize = 300;

/// A read this profile may not make, or input that cannot name one.
#[derive(Debug)]
pub struct ReadRefusal(pub &'static str);
impl std::fmt::Display for ReadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ReadRefusal {}

/// One message of a chat, as plain text; the author's name, never an address.
#[derive(Clone, Debug, Serialize)]
pub struct ChatLine {
    pub id: String,
    pub chat: String,
    pub resource: String,
    pub created_at: String,
    pub author: Option<String>,
    /// Written by the signed-in user.
    pub mine: bool,
    /// An answer or post this app sent (it carries the app's output marker).
    pub assistant: bool,
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    /// Messages this one replies to or quotes (`messageReference` attachments).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub replies_to: Vec<Quote>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip)]
    pub at: DateTime<Utc>,
}

/// A message another one replies to or quotes, as Teams shows it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Quote {
    pub id: String,
    pub author: Option<String>,
    pub preview: String,
}

/// Messages newest first, and how to read on.
#[derive(Debug, Default, Serialize)]
pub struct Page {
    pub messages: Vec<ChatLine>,
    /// Pass as `before` to read the older messages; absent when the window is exhausted.
    pub next_cursor: Option<String>,
    /// Some chat or message in the window was not read; `warnings` says which bound or failure.
    pub partial: bool,
    pub warnings: Vec<String>,
}

/// Messages created at or after `since` and before `until`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Window {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}
impl Window {
    /// `before` (a cursor) narrows the end; a start after the end is refused.
    pub fn new(
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
    ) -> Result<Self> {
        let until = match (until, before) {
            (Some(u), Some(b)) => Some(u.min(b)),
            (u, b) => u.or(b),
        };
        ensure!(
            since.zip(until).is_none_or(|(s, u)| s < u),
            ReadRefusal("The start of the window must be before its end.")
        );
        Ok(Self { since, until })
    }
    fn contains(&self, at: DateTime<Utc>) -> bool {
        self.since.is_none_or(|s| at >= s) && self.until.is_none_or(|u| at < u)
    }
    /// The window as RFC 3339 instants, for a reply.
    pub fn describe(&self) -> Value {
        json!({"since": self.since.map(cursor), "until": self.until.map(cursor)})
    }
    /// The same window with a start, `DEFAULT_DAYS` before its end when it has none.
    pub fn bounded(self) -> Self {
        Self {
            since: self.since.or_else(|| {
                Some(self.until.unwrap_or_else(Utc::now) - chrono::Duration::days(DEFAULT_DAYS))
            }),
            ..self
        }
    }
}

/// A day (`YYYY-MM-DD`, in this computer's time zone) or an RFC 3339 instant. As an end, a day
/// means the end of that day.
pub fn instant(text: &str, end_of_day: bool) -> Result<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&Utc));
    }
    let day = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
        ReadRefusal("Dates are YYYY-MM-DD or an RFC 3339 instant (2026-10-01T09:00:00Z).")
    })?;
    let day = if end_of_day {
        day.succ_opt().context("date out of range")?
    } else {
        day
    };
    let midnight = day.and_hms_opt(0, 0, 0).context("date out of range")?;
    Ok(midnight
        .and_local_timezone(chrono::Local)
        .earliest()
        .context("date out of range")?
        .with_timezone(&Utc))
}

fn cursor(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Keep `limit` of `lines` (newest first) and say where the next page starts. Messages created
/// in the same millisecond as the first one left out are left out with it, so a next page read
/// `before` the cursor misses none of them.
fn cut(mut lines: Vec<ChatLine>, limit: usize) -> (Vec<ChatLine>, Option<String>) {
    if lines.len() <= limit {
        return (lines, None);
    }
    let first_left = lines[limit].at;
    lines.truncate(limit);
    while lines.len() > 1 && lines.last().is_some_and(|l| l.at == first_left) {
        lines.pop();
    }
    let next = lines.last().map(|l| cursor(l.at));
    (lines, next)
}

/// Accent- and case-folded words of `query`; a message matches when it holds all of them.
fn terms(query: &str) -> Vec<String> {
    crate::index::fold(query)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}
fn matches(text: &str, wanted: &[String]) -> bool {
    let words = terms(text);
    wanted.iter().all(|t| words.iter().any(|w| w == t))
}
fn named(author: Option<&str>, from: Option<&str>) -> bool {
    from.is_none_or(|from| {
        author.is_some_and(|a| crate::index::fold(a).contains(&crate::index::fold(from)))
    })
}

/// The scan of one chat: what it kept, and whether it stopped at its page bound first.
struct Scan {
    lines: Vec<ChatLine>,
    bounded: bool,
    /// The oldest message read, kept or not, when the scan stopped at its bound.
    oldest: Option<DateTime<Utc>>,
}

/// What a message search may return.
#[derive(Clone, Debug, Default)]
pub struct SearchFilter {
    /// Chat IDs (or `self`) to keep; empty keeps every readable chat.
    pub chats: Vec<String>,
    /// Part of the author's name.
    pub from: Option<String>,
    pub window: Window,
}

/// Search results and how they were found.
#[derive(Debug, Default, Serialize)]
pub struct SearchPage {
    #[serde(flatten)]
    pub page: Page,
    /// `microsoft_search`, or `chat_history` when Microsoft Search could not be used and the
    /// chats' history was searched directly.
    pub method: &'static str,
}

impl Graph {
    /// The chat ID a read names: an ID from `teams chats`, or `self`. Only chats this profile
    /// may read are accepted.
    pub fn readable_chat(&self, chat: &str) -> Result<String> {
        let id = if chat == "self" {
            self.config
                .graph
                .self_chat
                .as_ref()
                .map(|c| c.id.clone())
                .unwrap_or_else(|| "48:notes".into())
        } else {
            chat.to_owned()
        };
        ensure!(
            teams::canonical_resource(&format!("chats/{id}/messages/0")).is_ok(),
            ReadRefusal("Name the chat: an ID from teams chats, or self.")
        );
        ensure!(
            self.readable_on_request(&format!("chats/{id}/messages")),
            ReadRefusal(
                "This profile may not read that chat: neither this computer (graph.discover_all_chats, graph.allowed_chats, the personal chat) nor the account (teams_reading) lets it read that chat."
            )
        );
        Ok(id)
    }

    /// One message as the operator reads it, or `None` for system events and deleted ones.
    fn line(&self, chat: &str, value: &Value, at: DateTime<Utc>) -> Option<ChatLine> {
        if value["messageType"].as_str() != Some("message") || !value["deletedDateTime"].is_null() {
            return None;
        }
        let id = value["id"].as_str().filter(|id| {
            !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
        })?;
        let raw = value["body"]["content"].as_str().unwrap_or("");
        let assistant = raw.contains("https://personalteams.invalid/output/");
        // Inline tags leave runs of spaces («estado  10 »): one space between words, lines kept.
        let mut text = super::card::message_text(value)
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        if assistant {
            text = text.trim_end_matches("PTA").trim_end().to_owned();
        }
        let truncated = text.chars().count() > TEXT;
        if truncated {
            text = text.chars().take(TEXT).collect();
        }
        let from = &value["from"];
        let author = from["user"]["displayName"]
            .as_str()
            .or_else(|| from["application"]["displayName"].as_str())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| n.chars().take(120).collect());
        Some(ChatLine {
            id: id.to_owned(),
            chat: chat.to_owned(),
            resource: format!("chats/{chat}/messages/{id}"),
            created_at: cursor(at),
            author,
            mine: from["user"]["id"].as_str() == Some(self.config.graph.user_id.as_str()),
            assistant,
            text,
            truncated,
            edited: !value["lastEditedDateTime"].is_null(),
            replies_to: quotes(value),
            url: value["webUrl"]
                .as_str()
                .filter(|u| u.starts_with("https://teams.microsoft.com/l/message/"))
                .map(str::to_owned),
            at,
        })
    }

    /// Messages of one chat in `window`, newest first, kept by `keep`, until `limit` + 1 are
    /// kept, the window's start is passed or `pages` Graph pages were read.
    async fn scan(
        &self,
        id: &str,
        window: Window,
        limit: usize,
        pages: usize,
        keep: &(dyn Fn(&ChatLine) -> bool + Send + Sync),
    ) -> Result<Scan> {
        // Notes are readable only through /me, as in personal polling.
        let path = if id == "48:notes" {
            "me/chats/48:notes/messages".to_owned()
        } else {
            format!("chats/{id}/messages")
        };
        let mut url = self.url(&path)?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("$top", "50")
                .append_pair("$orderby", "createdDateTime desc");
            // Graph filters creation only with `lt`, and only ordered by it.
            if let Some(until) = window.until {
                query.append_pair("$filter", &format!("createdDateTime lt {}", cursor(until)));
            }
        }
        let (origin, expected) = (url.origin(), url.path().to_owned());
        let mut lines = Vec::new();
        let mut oldest = None;
        for _ in 0..pages {
            let page = self
                .request_url(Method::GET, url.clone(), None, true)
                .await?;
            for value in page["value"]
                .as_array()
                .context("invalid Graph chat messages")?
            {
                let Some(at) = value["createdDateTime"]
                    .as_str()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc))
                else {
                    continue;
                };
                oldest = Some(at);
                if window.since.is_some_and(|s| at < s) {
                    return Ok(Scan {
                        lines,
                        bounded: false,
                        oldest: None,
                    });
                }
                if !window.contains(at) {
                    continue;
                }
                if let Some(line) = self.line(id, value, at)
                    && keep(&line)
                {
                    lines.push(line);
                    if lines.len() > limit {
                        return Ok(Scan {
                            lines,
                            bounded: false,
                            oldest: None,
                        });
                    }
                }
            }
            match page["@odata.nextLink"].as_str() {
                Some(next) => {
                    url = url::Url::parse(next)?;
                    ensure!(
                        url.origin() == origin
                            && url.path() == expected
                            && url.username().is_empty()
                            && url.password().is_none(),
                        "untrusted Graph pagination URL"
                    );
                }
                None => {
                    return Ok(Scan {
                        lines,
                        bounded: false,
                        oldest: None,
                    });
                }
            }
        }
        Ok(Scan {
            lines,
            bounded: true,
            oldest,
        })
    }

    /// A chat's messages in `window`, newest first, from any author.
    pub async fn chat_messages(&self, chat: &str, window: Window, limit: usize) -> Result<Page> {
        let id = self.readable_chat(chat)?;
        let scan = self.scan(&id, window, limit, CHAT_PAGES, &|_| true).await?;
        let (messages, mut next_cursor) = cut(scan.lines, limit);
        let mut warnings = Vec::new();
        if next_cursor.is_none() && scan.bounded {
            next_cursor = scan.oldest.map(cursor);
            warnings.push(format!(
                "Stopped after {} messages of this chat; read on with next_cursor.",
                CHAT_PAGES * 50
            ));
        }
        Ok(Page {
            messages,
            next_cursor,
            partial: false,
            warnings,
        })
    }

    /// One message by its resource (`chats/{chat}/messages/{id}`), in a chat this profile may
    /// read: the text of a message the audit lists without it.
    pub async fn chat_message(&self, resource: &str) -> Result<Page> {
        let resource = teams::canonical_resource(resource)
            .map_err(|_| ReadRefusal("A message resource looks like chats/CHAT/messages/ID."))?;
        let parts: Vec<&str> = resource.split('/').collect();
        let chat = self.readable_chat(parts[1])?;
        let value = self.request(Method::GET, &resource, None, true).await?;
        ensure!(
            value["id"].as_str() == Some(parts[3]),
            "Graph returned another message"
        );
        let at = value["createdDateTime"]
            .as_str()
            .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
            .context("invalid Graph message")?
            .with_timezone(&Utc);
        let messages: Vec<ChatLine> = self.line(&chat, &value, at).into_iter().collect();
        let warnings = if messages.is_empty() {
            vec!["The message was deleted or is a system event.".to_owned()]
        } else {
            Vec::new()
        };
        Ok(Page {
            messages,
            next_cursor: None,
            partial: false,
            warnings,
        })
    }

    /// Chats with a message since `since`, newest first: readable 1:1, group and meeting chats,
    /// the personal chat left out. Says whether every such chat was listed.
    async fn recent_chats(&self, since: DateTime<Utc>) -> Result<(Vec<String>, bool)> {
        let mut url = self.url("me/chats")?;
        url.query_pairs_mut()
            .append_pair("$expand", "lastMessagePreview")
            .append_pair("$orderby", "lastMessagePreview/createdDateTime desc")
            .append_pair("$top", "50");
        let (origin, expected) = (url.origin(), url.path().to_owned());
        let personal = self.config.graph.self_chat.as_ref().map(|c| c.id.as_str());
        let mut chats = Vec::new();
        for _ in 0..LIST_PAGES {
            let page = self
                .request_url(Method::GET, url.clone(), None, true)
                .await?;
            for chat in page["value"].as_array().context("invalid Graph chats")? {
                let last = chat["lastMessagePreview"]["createdDateTime"]
                    .as_str()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok());
                let Some(last) = last else { continue };
                if last.with_timezone(&Utc) < since {
                    return Ok((chats, true));
                }
                let Some(id) = chat["id"].as_str() else {
                    continue;
                };
                if !matches!(
                    chat["chatType"].as_str(),
                    Some("oneOnOne" | "group" | "meeting")
                ) || personal == Some(id)
                    || id == "48:notes"
                    || self.readable_chat(id).is_err()
                {
                    continue;
                }
                if chats.len() >= SCAN_CHATS {
                    return Ok((chats, false));
                }
                chats.push(id.to_owned());
            }
            match page["@odata.nextLink"].as_str() {
                Some(next) => {
                    url = url::Url::parse(next)?;
                    ensure!(
                        url.origin() == origin && url.path() == expected,
                        "untrusted Graph pagination URL"
                    );
                }
                None => return Ok((chats, true)),
            }
        }
        Ok((chats, false))
    }

    /// Messages kept by `keep` in several chats, merged newest first.
    async fn scan_chats(
        &self,
        chats: Vec<String>,
        window: Window,
        limit: usize,
        keep: Arc<dyn Fn(&ChatLine) -> bool + Send + Sync>,
    ) -> Page {
        let mut jobs = tokio::task::JoinSet::new();
        let permits = Arc::new(tokio::sync::Semaphore::new(6));
        for id in chats {
            let (graph, permits, keep) = (self.clone(), permits.clone(), keep.clone());
            jobs.spawn(async move {
                let _permit = permits.acquire().await?;
                graph.scan(&id, window, limit, SCAN_PAGES, &*keep).await
            });
        }
        let mut lines = Vec::new();
        let (mut failed, mut bounded) = (0, 0);
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(scan)) => {
                    bounded += usize::from(scan.bounded);
                    lines.extend(scan.lines);
                }
                _ => failed += 1,
            }
        }
        lines.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
        let (messages, next_cursor) = cut(lines, limit);
        let mut warnings = Vec::new();
        if failed > 0 {
            warnings.push(format!("{failed} chat(s) could not be read."));
        }
        if bounded > 0 {
            warnings.push(format!(
                "{bounded} chat(s) have more than {} messages in the window; their older ones were not read. Narrow the window.",
                SCAN_PAGES * 50
            ));
        }
        Page {
            messages,
            next_cursor,
            partial: failed > 0 || bounded > 0,
            warnings,
        }
    }

    /// The user's own messages in the chats this profile may read, newest first. Without a
    /// start the window is the last `DEFAULT_DAYS` days; the personal chat is left out.
    pub async fn own_chat_messages(&self, window: Window, limit: usize) -> Result<Page> {
        let window = window.bounded();
        let (chats, complete) = self
            .recent_chats(window.since.unwrap_or_else(Utc::now))
            .await?;
        let mut page = self
            .scan_chats(chats, window, limit, Arc::new(|line: &ChatLine| line.mine))
            .await;
        if !complete {
            page.partial = true;
            page.warnings.push(format!(
                "Only the {SCAN_CHATS} chats with the latest messages were read. Narrow the window."
            ));
        }
        Ok(page)
    }

    /// Messages matching `query` in the chats this profile may read: Microsoft Search when it
    /// answers, otherwise the chats' history read directly (bounded, `partial` when cut).
    pub async fn search_messages(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<SearchPage> {
        let query = query.trim();
        ensure!(
            !query.is_empty()
                && query.chars().count() <= 200
                && !query.chars().any(char::is_control)
                && !terms(query).is_empty(),
            ReadRefusal("Search for some words (at most 200 characters).")
        );
        let chats = filter
            .chats
            .iter()
            .map(|c| self.readable_chat(c))
            .collect::<Result<Vec<_>>>()?;
        match self.microsoft_search(query, &chats, filter, limit).await {
            Ok(page) => Ok(SearchPage {
                page,
                method: "microsoft_search",
            }),
            Err(error)
                if error
                    .downcast_ref::<super::oauth::AuthorizationRequired>()
                    .is_some() =>
            {
                Err(error)
            }
            Err(error) => {
                let reason = super::graph::http_status(&error)
                    .map(|status| format!("HTTP {status}"))
                    .unwrap_or_else(|| "no usable answer".into());
                let window = filter.window.bounded();
                let (chats, complete) = if chats.is_empty() {
                    self.recent_chats(window.since.unwrap_or_else(Utc::now))
                        .await?
                } else {
                    (chats, true)
                };
                let count = chats.len();
                let (terms, from) = (terms(query), filter.from.clone());
                let mut page = self
                    .scan_chats(
                        chats,
                        window,
                        limit,
                        Arc::new(move |line: &ChatLine| {
                            matches(&line.text, &terms)
                                && named(line.author.as_deref(), from.as_deref())
                        }),
                    )
                    .await;
                page.warnings.insert(
                    0,
                    format!(
                        "Microsoft Search could not be used ({reason}); the history of {count} chat(s) since {} was searched directly.",
                        window.since.map(cursor).unwrap_or_default()
                    ),
                );
                if !complete {
                    page.partial = true;
                    page.warnings.push(format!(
                        "Only the {SCAN_CHATS} chats with the latest messages were searched. Narrow the window or name the chats."
                    ));
                }
                Ok(SearchPage {
                    page,
                    method: "chat_history",
                })
            }
        }
    }

    /// Microsoft Search hits for `kql` in readable chats that pass the filter, in Search's
    /// order: up to `limit` + 1, and whether Search had more.
    async fn search_hits(
        &self,
        kql: &str,
        chats: &[String],
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<(Vec<(String, String)>, bool)> {
        let mut found: Vec<(String, String)> = Vec::new();
        let mut more = false;
        for page in 0..SEARCH_PAGES {
            let body = json!({"requests": [{"entityTypes": ["chatMessage"], "query": {"queryString": kql},
                "from": page * 25, "size": 25}]});
            let reply = self
                .request(Method::POST, "search/query", Some(body), true)
                .await?;
            ensure!(reply["value"].is_array(), "invalid Microsoft Search answer");
            let container = &reply["value"][0]["hitsContainers"][0];
            for hit in container["hits"].as_array().into_iter().flatten() {
                let resource = &hit["resource"];
                let (Some(chat), Some(id)) = (resource["chatId"].as_str(), resource["id"].as_str())
                else {
                    continue;
                };
                // Channel posts are not chats this app reads.
                if resource["channelIdentity"]["channelId"].is_string()
                    || self.readable_chat(chat).is_err()
                    || (!chats.is_empty() && !chats.iter().any(|c| c == chat))
                    || found.iter().any(|(c, i)| c == chat && i == id)
                {
                    continue;
                }
                let at = resource["createdDateTime"]
                    .as_str()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc));
                if at.is_some_and(|at| !filter.window.contains(at))
                    || !named(
                        resource["from"]["emailAddress"]["name"].as_str(),
                        filter.from.as_deref(),
                    )
                {
                    continue;
                }
                found.push((chat.to_owned(), id.to_owned()));
            }
            more = container["moreResultsAvailable"].as_bool() == Some(true);
            if found.len() > limit || !more {
                break;
            }
        }
        Ok((found, more))
    }

    async fn microsoft_search(
        &self,
        query: &str,
        chats: &[String],
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Page> {
        let mut dated = query.to_owned();
        if let Some(since) = filter.window.since {
            dated.push_str(&format!(
                " sent>={}",
                since.with_timezone(&chrono::Local).format("%Y-%m-%d")
            ));
        }
        if let Some(until) = filter.window.until {
            dated.push_str(&format!(
                " sent<={}",
                until.with_timezone(&chrono::Local).format("%Y-%m-%d")
            ));
        }
        let mut warnings = Vec::new();
        let (mut found, mut more) = self.search_hits(&dated, chats, filter, limit).await?;
        // The date terms narrow the index, but a tenant that reads them differently finds
        // nothing: search the words alone then and keep the window here.
        if found.is_empty() && dated != query {
            (found, more) = self.search_hits(query, chats, filter, limit).await?;
            if !found.is_empty() {
                warnings.push(
                    "Microsoft Search found nothing with the dates in the query; the words alone were searched and the window applied here.".to_owned(),
                );
            }
        }
        let cut_short = found.len() > limit;
        found.truncate(limit);
        let mut jobs = tokio::task::JoinSet::new();
        let permits = Arc::new(tokio::sync::Semaphore::new(6));
        for (chat, id) in found {
            let (graph, permits) = (self.clone(), permits.clone());
            jobs.spawn(async move {
                let _permit = permits.acquire().await?;
                let value = graph
                    .request(
                        Method::GET,
                        &format!("chats/{chat}/messages/{id}"),
                        None,
                        true,
                    )
                    .await?;
                let at = value["createdDateTime"]
                    .as_str()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .context("invalid Graph message")?
                    .with_timezone(&Utc);
                Ok::<_, anyhow::Error>(graph.line(&chat, &value, at))
            });
        }
        let mut messages = Vec::new();
        let mut failed = 0;
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(Some(line))) => messages.push(line),
                Ok(Ok(None)) => {}
                _ => failed += 1,
            }
        }
        messages.sort_by_key(|m| std::cmp::Reverse(m.at));
        if failed > 0 {
            warnings.push(format!("{failed} matching message(s) could not be read."));
        }
        if cut_short || more {
            warnings.push(
                "More messages match: raise --limit, narrow the window or name the chats.".into(),
            );
        }
        Ok(Page {
            messages,
            next_cursor: None,
            partial: failed > 0 || cut_short || more,
            warnings,
        })
    }
}

/// The messages a message replies to or quotes, from its `messageReference` attachments.
fn quotes(value: &Value) -> Vec<Quote> {
    value["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["contentType"].as_str() == Some("messageReference"))
        .filter_map(|a| {
            let content: Value = serde_json::from_str(a["content"].as_str()?).ok()?;
            let id = content["messageId"]
                .as_str()
                .or_else(|| a["id"].as_str())?
                .to_owned();
            let sender = &content["messageSender"];
            Some(Quote {
                id,
                author: sender["user"]["displayName"]
                    .as_str()
                    .or_else(|| sender["application"]["displayName"].as_str())
                    .map(str::to_owned),
                preview: teams::plain_text(content["messagePreview"].as_str().unwrap_or(""))
                    .chars()
                    .take(PREVIEW)
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, at: &str) -> ChatLine {
        let at = DateTime::parse_from_rfc3339(at)
            .unwrap()
            .with_timezone(&Utc);
        ChatLine {
            id: id.into(),
            chat: "chat1".into(),
            resource: format!("chats/chat1/messages/{id}"),
            created_at: cursor(at),
            author: None,
            mine: false,
            assistant: false,
            text: String::new(),
            truncated: false,
            edited: false,
            replies_to: Vec::new(),
            url: None,
            at,
        }
    }

    #[test]
    fn a_page_is_cut_on_a_millisecond_boundary_so_the_cursor_misses_nothing() {
        let lines = vec![
            line("4", "2026-10-04T10:00:00.000Z"),
            line("3", "2026-10-03T10:00:00.000Z"),
            line("2", "2026-10-03T10:00:00.000Z"),
            line("1", "2026-10-01T10:00:00.000Z"),
        ];
        let (kept, next) = cut(lines.clone(), 4);
        assert_eq!(kept.len(), 4);
        assert!(next.is_none());
        // «2» shares its millisecond with «3»: both wait for the next page.
        let (kept, next) = cut(lines.clone(), 2);
        assert_eq!(
            kept.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["4"]
        );
        assert_eq!(next.as_deref(), Some("2026-10-04T10:00:00.000Z"));
        let (kept, next) = cut(lines, 3);
        assert_eq!(kept.len(), 3);
        assert_eq!(next.as_deref(), Some("2026-10-03T10:00:00.000Z"));
    }

    #[test]
    fn dates_are_days_or_instants_and_a_window_needs_its_start_first() {
        let day = instant("2026-10-01", false).unwrap();
        let end = instant("2026-10-01", true).unwrap();
        assert_eq!(end - day, chrono::Duration::days(1));
        assert_eq!(
            instant("2026-10-01T09:00:00Z", true).unwrap(),
            DateTime::parse_from_rfc3339("2026-10-01T09:00:00Z").unwrap()
        );
        for wrong in ["01/10/2026", "yesterday", ""] {
            assert!(
                instant(wrong, false)
                    .unwrap_err()
                    .downcast_ref::<ReadRefusal>()
                    .is_some()
            );
        }
        let window =
            Window::new(Some(day), Some(end), Some(day + chrono::Duration::hours(1))).unwrap();
        assert_eq!(window.until, Some(day + chrono::Duration::hours(1)));
        assert!(window.contains(day) && !window.contains(end));
        assert!(Window::new(Some(end), Some(day), None).is_err());
        let open = Window::new(None, Some(end), None).unwrap().bounded();
        assert_eq!(open.since, Some(end - chrono::Duration::days(DEFAULT_DAYS)));
    }

    #[test]
    fn a_match_needs_every_folded_word_and_a_name_part() {
        let wanted = terms("Estado 10");
        assert!(matches("La SPS pasa a estado `10` (pendiente).", &wanted));
        assert!(matches("ESTADO: 10", &wanted));
        assert!(!matches("estado 100", &wanted));
        assert!(!matches("solo el estado", &wanted));
        assert!(named(Some("Miguel Antúnez"), Some("antunez")));
        assert!(named(None, None));
        assert!(!named(None, Some("ana")));
    }

    #[test]
    fn quotes_come_from_message_references() {
        let value = json!({"attachments": [
            {"id": "1", "contentType": "messageReference", "content": json!({"messageId": "1",
                "messagePreview": "<p>¿Qué es el <b>estado</b> 10?</p>",
                "messageSender": {"user": {"displayName": "Ana"}}}).to_string()},
            {"id": "2", "contentType": "reference", "content": "{}"}]});
        assert_eq!(
            quotes(&value),
            [Quote {
                id: "1".into(),
                author: Some("Ana".into()),
                preview: teams::plain_text("<p>¿Qué es el <b>estado</b> 10?</p>"),
            }]
        );
    }

    #[test]
    fn the_running_service_sends_with_the_saved_message_style() {
        struct NoToken;
        #[async_trait::async_trait]
        impl super::super::oauth::AccessToken for NoToken {
            async fn access_token(&self) -> Result<String> {
                anyhow::bail!("no Graph session")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::state::Store::open(&dir.path().join("cache.db")).unwrap());
        let started = crate::config::Config::desktop_template().unwrap();
        let path = dir.path().join("config.toml");
        let mut graph = Graph {
            client: reqwest::Client::new(),
            token: Arc::new(NoToken),
            config: Arc::new(started.clone()),
            store,
            base_url: "https://graph.microsoft.com/v1.0".into(),
            client_state: String::new(),
            config_path: None,
        };
        assert_eq!(graph.message_style().0, started.message_style);
        // A style the account synced after the service started.
        let mut saved = started.clone();
        saved.message_style.enabled = true;
        saved.message_style.label = "Synthetic label".into();
        saved.llm.language = crate::config::Language::En;
        std::fs::write(&path, toml::to_string(&saved).unwrap()).unwrap();
        graph.config_path = Some(path.to_string_lossy().into_owned());
        let (style, language) = graph.message_style();
        assert_eq!(style, saved.message_style);
        assert_eq!(language, crate::config::Language::En);
        // An unreadable profile keeps the style the service started with.
        std::fs::write(&path, "not a profile").unwrap();
        assert_eq!(graph.message_style().0, started.message_style);
    }

    #[test]
    fn the_accounts_reading_scope_widens_reads_on_request_and_never_reception() {
        struct NoToken;
        #[async_trait::async_trait]
        impl super::super::oauth::AccessToken for NoToken {
            async fn access_token(&self) -> Result<String> {
                anyhow::bail!("no Graph session")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::state::Store::open(&dir.path().join("cache.db")).unwrap());
        let graph = |reading: crate::config::TeamsReading| {
            let mut config = crate::config::Config::desktop_template().unwrap();
            config.teams_reading = reading;
            Graph {
                client: reqwest::Client::new(),
                token: Arc::new(NoToken),
                config: Arc::new(config),
                store: store.clone(),
                base_url: "https://graph.microsoft.com/v1.0".into(),
                client_state: String::new(),
                config_path: None,
            }
        };
        // A computer that receives nothing reads nothing on request.
        let local = graph(Default::default());
        assert!(local.readable_chat("self").is_err());
        assert!(local.readable_chat("19:shared@thread.v2").is_err());

        let listed = graph(crate::config::TeamsReading {
            personal_chat: true,
            chats: vec!["19:shared@thread.v2".into()],
            ..Default::default()
        });
        assert_eq!(listed.readable_chat("self").unwrap(), "48:notes");
        assert!(listed.readable_chat("19:shared@thread.v2").is_ok());
        assert!(listed.readable_chat("19:other@thread.v2").is_err());
        let all = graph(crate::config::TeamsReading {
            all_chats: true,
            ..Default::default()
        });
        assert!(
            all.readable_chat("self").is_ok() && all.readable_chat("19:other@thread.v2").is_ok()
        );
        // Receiving, recovering and sending keep this computer's own scope.
        for graph in [&listed, &all] {
            assert!(!graph.allowed_collection("chats/19:shared@thread.v2/messages"));
            assert!(!graph.allowed_collection("chats/48:notes/messages"));
            assert!(!graph.allowed_collection(&graph.user_messages_resource()));
        }
    }
}
