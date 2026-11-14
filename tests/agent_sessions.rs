//! The agent sessions source in the message pipeline, with synthetic session files and a fake
//! model: a message that refers back to the thread, about a subject only a working session
//! knows, is answered with the work in progress; without the source, or for a sender outside
//! its audience, the message is left as before.
mod support;
use anyhow::Result;
use async_trait::async_trait;
use personal_teams_assistant::{
    adapters::{
        MessageAdapter,
        teams::{ConversationKind, HistoryMessage, IncomingMessage},
    },
    knowledge::{Access, KnowledgeMap, Resource},
    llm::{GeneratedAnswer, GenerationInput, LlmProvider},
    pipeline::Pipeline,
    questions::Question,
    security::Redactor,
    sessions::{Listing, Reader, Roots, Unavailable},
    tools::{ReadOnlyTool, ToolSpec},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::Write as _,
    sync::{Arc, Mutex, OnceLock},
};

const TOKEN: &str = "tok_9fK2pQ7xL4mN8vR3sT6wY1zA5bC0dE2gH7jK";
const CONNECTION: &str =
    "Server=tcp:db.example.test,1433;Database=pay;User Id=app;Password=Synthetic-Pass-123;";

/// Synthetic Claude Code and Codex sessions, written once for every test of this binary, read
/// as this computer's and joined by an offline machine.
struct SyntheticMachines(tempfile::TempDir);
fn machines() -> &'static SyntheticMachines {
    static MACHINES: OnceLock<SyntheticMachines> = OnceLock::new();
    MACHINES.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let key = format!(
            "-----BEGIN {0} KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\n-----END {0} KEY-----",
            "PRIVATE"
        );
        let asked = format!(
            "Check the connection to the payments database from the QA server.\nIts string is {CONNECTION}\nThe token is {TOKEN}\n{key}"
        );
        let answer = "The QA server reaches the payments database host, but the connection times out on port 1433 at the firewall. I asked networking to open it and am waiting for their change.";
        let path = dir
            .path()
            .join("claude/projects/-home-user-src-payments/c1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        for line in [
            json!({"type":"user","sessionId":"c1","cwd":"/home/user/src/payments","message":{"role":"user","content":asked},"timestamp":"2026-11-13T09:00:00Z"}),
            json!({"type":"assistant","sessionId":"c1","message":{"role":"assistant","content":[{"type":"text","text":answer}]},"timestamp":"2026-11-13T09:40:00Z"}),
        ] {
            writeln!(file, "{line}").unwrap();
        }
        let path = dir
            .path()
            .join("codex/sessions/2026/11/12/rollout-2026-11-12T08-00-00-x1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        for line in [
            json!({"timestamp":"2026-11-12T08:00:00Z","type":"session_meta","payload":{"id":"x1","timestamp":"2026-11-12T08:00:00Z","cwd":"/srv/billing"}}),
            json!({"timestamp":"2026-11-12T08:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"Rename the billing export columns"}}),
            json!({"timestamp":"2026-11-12T08:30:00Z","type":"event_msg","payload":{"type":"agent_message","message":"Renamed and tested."}}),
        ] {
            writeln!(file, "{line}").unwrap();
        }
        personal_teams_assistant::sessions::install(Some(Arc::new(FakeFleet)));
        SyntheticMachines(dir)
    })
}
/// The account's machines: this computer's synthetic stores and an offline one.
struct FakeFleet;
#[async_trait]
impl Reader for FakeFleet {
    async fn recent(&self, limit: usize) -> Result<Listing> {
        let root = machines().0.path();
        let mut listing = personal_teams_assistant::sessions::local(
            &Roots {
                claude: Some(root.join("claude")),
                codex: Some(root.join("codex")),
                opencode: None,
            },
            limit,
            "laptop",
        );
        listing.unavailable.push(Unavailable {
            machine: "desk".into(),
            provider: String::new(),
            reason: "offline".into(),
        });
        Ok(listing)
    }
}

/// A chat of `total` messages: an early request about the QA connection, then other talk; the
/// current message only refers back to it.
struct Thread {
    total: usize,
    sender: String,
    asked_limit: Mutex<Vec<usize>>,
    sent: Mutex<Vec<String>>,
}
impl Thread {
    fn messages(&self) -> Vec<HistoryMessage> {
        (0..self.total)
            .map(|n| HistoryMessage {
                author: if n % 2 == 0 {
                    "Ana".into()
                } else {
                    "me".into()
                },
                at: format!("2026-11-13 {:02}:{:02}", 8 + n / 60, n % 60),
                text: if n == self.total - 4 {
                    "¿Puedes revisar la conexión a la base de pagos desde el servidor de QA?".into()
                } else {
                    format!("Mensaje {n} sobre otra cosa")
                },
            })
            .collect()
    }
}
#[async_trait]
impl MessageAdapter for Thread {
    async fn fetch(&self, resource: &str) -> Result<IncomingMessage> {
        Ok(IncomingMessage {
            resource: resource.into(),
            conversation: "chats/chat1".into(),
            sender: self.sender.clone(),
            sender_name: "Ana".into(),
            kind: ConversationKind::Direct,
            mentions: vec![],
            text: "prueba la conexión que te mencioné hace unos mensajes".into(),
            created_at: chrono::Utc::now().timestamp(),
            created_at_millis: chrono::Utc::now().timestamp_millis(),
            is_user_message: true,
        })
    }
    async fn send(&self, _: &IncomingMessage, text: &str) -> Result<String> {
        self.sent.lock().unwrap().push(text.into());
        Ok("sent-1".into())
    }
    async fn history(&self, _: &IncomingMessage, limit: usize) -> Result<Vec<HistoryMessage>> {
        self.asked_limit.lock().unwrap().push(limit);
        let all = self.messages();
        Ok(all[all.len().saturating_sub(limit)..].to_vec())
    }
}

/// No configured question matches; it chooses the session about the QA connection when the
/// thread it receives mentions it.
#[derive(Default)]
struct Model {
    selections: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    generated: Mutex<Vec<(String, bool, String)>>,
}
#[async_trait]
impl LlmProvider for Model {
    fn name(&self) -> &str {
        "fake"
    }
    async fn identify_question(
        &self,
        _: &str,
        _: &str,
        _: &BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    async fn select_sessions(
        &self,
        _: &str,
        history: &str,
        sessions: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        self.selections
            .lock()
            .unwrap()
            .push((history.into(), sessions.clone()));
        Ok(if history.contains("base de pagos") {
            sessions
                .iter()
                .filter(|(_, s)| s.contains("payments database"))
                .map(|(k, _)| k.clone())
                .collect()
        } else {
            Vec::new()
        })
    }
    async fn generate(&self, _: GenerationInput<'_>) -> Result<String> {
        unreachable!()
    }
    async fn generate_response(&self, input: GenerationInput<'_>) -> Result<GeneratedAnswer> {
        self.generated.lock().unwrap().push((
            input.evidence.into(),
            input.in_progress,
            input.history.into(),
        ));
        Ok(GeneratedAnswer {
            answer: "Ya lo estoy revisando en una sesión de trabajo: hasta ahora el servidor de QA llega a la base de pagos, pero la conexión se corta en el firewall y estoy esperando que redes abra el puerto.".into(),
            detailed: false,
            used_sources: vec![],
            provider: Some("fake:model:default".into()),
            fallbacks: vec![],
        })
    }
}
struct NoTools;
#[async_trait]
impl ReadOnlyTool for NoTools {
    async fn execute(&self, _: &ToolSpec, _: &str, _: &str) -> Result<String> {
        panic!("no tool is read: the sessions answer reads only its source")
    }
}

fn map(enabled: bool, senders: Vec<String>) -> KnowledgeMap {
    let mut map = KnowledgeMap::parse("").unwrap();
    map.resources = vec![
        Resource {
            id: "hours".into(),
            description: "Support hours".into(),
            topics: vec![],
            enabled: true,
            external_processing: true,
            allowed_conversations: vec!["chats/chat1".into()],
            allowed_senders: vec![],
            access: Access::Urls {
                urls: vec!["https://example.com/hours".into()],
            },
        },
        Resource {
            id: "agent-sessions".into(),
            description: "What my coding agents are working on".into(),
            topics: vec![],
            enabled,
            external_processing: true,
            allowed_conversations: vec!["chats/chat1".into()],
            allowed_senders: senders,
            access: Access::Tool {
                tool: ToolSpec::AgentSessions { sessions: 10 },
            },
        },
    ];
    map.questions = Some(vec![Question {
        id: "support-hours".into(),
        text: "When is support open?".into(),
        sources: vec!["hours".into()],
        examples: vec![],
    }]);
    map
}
struct Run {
    store: Arc<personal_teams_assistant::state::Store>,
    thread: Arc<Thread>,
    model: Arc<Model>,
    audit: personal_teams_assistant::state::Audit,
    _dir: tempfile::TempDir,
}
async fn run(knowledge: KnowledgeMap, history_messages: Option<usize>, sender: &str) -> Run {
    machines();
    let dir = tempfile::tempdir().unwrap();
    let store = support::store(&dir);
    let mut config = support::config();
    config.server.data_dir = dir.path().into();
    if let Some(n) = history_messages {
        config.policy.history_messages = n;
    }
    config.validate().unwrap();
    let thread = Arc::new(Thread {
        total: 30,
        sender: sender.into(),
        asked_limit: Mutex::default(),
        sent: Mutex::default(),
    });
    let model = Arc::new(Model::default());
    let pipeline = Pipeline {
        config: Arc::new(config),
        store: store.clone(),
        adapter: thread.clone(),
        knowledge,
        llm: model.clone(),
        tools: Arc::new(NoTools),
        redactor: Arc::new(Redactor::new(&[], vec![]).unwrap()),
    };
    let resource = "chats/chat1/messages/123";
    store.enqueue(resource).unwrap();
    pipeline.process(resource).await.unwrap();
    let audit = store.audit(resource).unwrap().unwrap();
    Run {
        store,
        thread,
        model,
        audit,
        _dir: dir,
    }
}

#[tokio::test]
async fn a_message_that_refers_back_is_answered_with_the_work_in_progress() {
    let run = run(map(true, vec![]), None, "colleague").await;
    let audit = &run.audit;
    assert_eq!(audit.status, "sent", "{:?}", audit.trace);
    assert_eq!(audit.reason, "agent_session_answer");
    assert_eq!(audit.intent.as_deref(), Some("question"));
    // The audit names the source and the session it used.
    assert_eq!(audit.used_sources, ["agent-sessions"]);
    assert_eq!(audit.agent_sessions.len(), 1);
    let used = &audit.agent_sessions[0];
    assert_eq!(
        (
            used.source.as_str(),
            used.provider.as_str(),
            used.session_id.as_str(),
            used.machine.as_str()
        ),
        ("agent-sessions", "claude", "c1", "laptop")
    );
    assert!(audit.tools.contains(&"read_agent_sessions".to_string()));
    // The offline machine's sessions are unknown, not absent.
    assert!(
        audit
            .coverage_warnings
            .iter()
            .any(|w| w.contains("machine desk") && w.contains("offline")),
        "{:?}",
        audit.coverage_warnings
    );
    // The thread's last ten messages were read before deciding (the default of 10, out of 30).
    assert_eq!(*run.thread.asked_limit.lock().unwrap(), [10]);
    assert_eq!(audit.history_messages, 10);
    let selections = run.model.selections.lock().unwrap();
    let (history, offered) = &selections[0];
    assert_eq!(
        history.lines().count(),
        11,
        "ten messages and the current time"
    );
    assert!(history.contains("Mensaje 20") && !history.contains("Mensaje 19 "));
    assert_eq!(offered.len(), 2, "both sessions are offered by key");
    // The answer comes from the session alone and says the work is in progress.
    let generated = run.model.generated.lock().unwrap();
    let (evidence, in_progress, _) = &generated[0];
    assert!(*in_progress);
    assert!(evidence.contains("times out on port 1433 at the firewall"));
    assert!(!evidence.contains("billing"), "only the chosen session");
    let sent = run.thread.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].starts_with("Ya lo estoy revisando"));
    // No secret of the session reaches the model, the record or Teams.
    let stored = serde_json::to_string(audit).unwrap();
    let offered = serde_json::to_string(offered).unwrap();
    for text in [
        evidence.as_str(),
        stored.as_str(),
        sent[0].as_str(),
        &offered,
    ] {
        for secret in [
            TOKEN,
            "Synthetic-Pass",
            "Password=",
            "MIIEvQ",
            "PRIVATE KEY",
            "/home/user",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    drop((selections, generated, sent));
    let _ = &run.store;
}

#[tokio::test]
async fn without_the_source_or_outside_its_audience_the_message_is_left_as_before() {
    for (knowledge, sender) in [
        (map(false, vec![]), "colleague"),
        (map(true, vec!["someone-else".into()]), "colleague"),
    ] {
        let run = run(knowledge, None, sender).await;
        assert_eq!(run.audit.reason, "no_matching_question");
        assert_eq!(run.audit.status, "ignored");
        assert!(run.model.selections.lock().unwrap().is_empty());
        assert!(run.model.generated.lock().unwrap().is_empty());
        assert!(run.thread.sent.lock().unwrap().is_empty());
        assert!(run.audit.agent_sessions.is_empty());
    }
}

#[tokio::test]
async fn a_session_about_something_else_leaves_the_message_for_review() {
    // With only the last three messages the request about the QA connection is out of view.
    let run = run(map(true, vec![]), Some(3), "colleague").await;
    assert_eq!(*run.thread.asked_limit.lock().unwrap(), [3]);
    assert_eq!(run.audit.history_messages, 3);
    assert_eq!(run.audit.reason, "no_matching_question");
    assert_eq!(run.model.selections.lock().unwrap().len(), 1);
    assert!(run.model.generated.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_thread_context_setting_reads_that_many_earlier_messages() {
    let run = run(map(true, vec![]), Some(25), "colleague").await;
    assert_eq!(*run.thread.asked_limit.lock().unwrap(), [25]);
    assert_eq!(run.audit.history_messages, 25);
    let selections = run.model.selections.lock().unwrap();
    assert_eq!(selections[0].0.lines().count(), 26);
    assert!(selections[0].0.contains("Mensaje 5 "));
}
