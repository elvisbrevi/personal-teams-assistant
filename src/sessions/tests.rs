use super::*;
use serde_json::json;
use std::io::Write as _;

fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}
/// Set a file's modification time `minutes` after a fixed instant, so order is known.
fn touch(path: &Path, minutes: u64) {
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_793_000_000 + minutes * 60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}
fn at(minutes: u64) -> String {
    from_millis(((1_793_000_000 + minutes * 60) * 1000) as i64).unwrap()
}

fn claude_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let path = root.join(format!("projects/-home-user-src-payments/{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"type":"summary","summary":"Payments","leafUuid":"x"}),
            json!({"type":"user","isMeta":true,"sessionId":id,"cwd":"/home/user/src/payments","message":{"role":"user","content":"<command-name>/clear</command-name>"},"timestamp":at(minutes)}),
            json!({"type":"user","sessionId":id,"cwd":"/home/user/src/payments","message":{"role":"user","content":asked},"timestamp":at(minutes)}),
            json!({"type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":"Reading the code."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/home/user/src/payments/.env"}}]},"timestamp":at(minutes)}),
            json!({"type":"user","sessionId":id,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"FILE CONTENT SHOULD NEVER APPEAR"}]},"timestamp":at(minutes)}),
            json!({"type":"assistant","isSidechain":true,"sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":"SIDECHAIN TEXT"}]},"timestamp":at(minutes)}),
            json!({"type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":answer}]},"timestamp":at(minutes)}),
        ],
    );
    touch(&path, minutes);
}
fn codex_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let path = root.join(format!(
        "sessions/2026/11/12/rollout-2026-11-12T10-00-00-{id}.jsonl"
    ));
    write_lines(
        &path,
        &[
            json!({"timestamp":at(minutes),"type":"session_meta","payload":{"id":id,"timestamp":at(minutes),"cwd":"/srv/work/billing","originator":"codex_cli_rs"}}),
            json!({"timestamp":at(minutes),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}),
            json!({"timestamp":at(minutes),"type":"event_msg","payload":{"type":"user_message","message":asked}}),
            json!({"timestamp":at(minutes),"type":"response_item","payload":{"type":"function_call_output","output":"COMMAND OUTPUT SHOULD NEVER APPEAR"}}),
            json!({"timestamp":at(minutes),"type":"event_msg","payload":{"type":"agent_message","message":answer}}),
        ],
    );
    touch(&path, minutes);
}
fn opencode_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let storage = root.join("storage");
    let ms = ((1_793_000_000 + minutes * 60) * 1000) as i64;
    let info = storage.join(format!("session/project1/{id}.json"));
    std::fs::create_dir_all(info.parent().unwrap()).unwrap();
    std::fs::write(
        &info,
        json!({"id":id,"title":"Queue","directory":"/home/user/queue","time":{"created":ms,"updated":ms}}).to_string(),
    )
    .unwrap();
    touch(&info, minutes);
    for (n, (role, text)) in [("user", asked), ("assistant", answer)].iter().enumerate() {
        let message = format!("msg_{id}_{n}");
        let dir = storage.join(format!("message/{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{message}.json")),
            json!({"id":message,"role":role,"time":{"created":ms + n as i64}}).to_string(),
        )
        .unwrap();
        let parts = storage.join(format!("part/{message}"));
        std::fs::create_dir_all(&parts).unwrap();
        std::fs::write(
            parts.join("prt_1.json"),
            json!({"id":"prt_1","type":"text","text":text}).to_string(),
        )
        .unwrap();
        std::fs::write(
            parts.join("prt_2.json"),
            json!({"id":"prt_2","type":"tool","state":{"output":"TOOL OUTPUT SHOULD NEVER APPEAR"}}).to_string(),
        )
        .unwrap();
    }
}
fn roots(dir: &Path) -> Roots {
    Roots {
        claude: Some(dir.join("claude")),
        codex: Some(dir.join("codex")),
        opencode: Some(dir.join("opencode")),
    }
}

#[test]
fn the_most_recent_sessions_across_the_three_clis_come_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    // Twelve sessions spread over the three CLIs, at known times.
    for n in 0..4u64 {
        claude_session(
            roots.claude.as_ref().unwrap(),
            &format!("claude-{n}"),
            n * 3,
            &format!("Fix the payments retry {n}"),
            "The retry now waits; the deploy is still pending.",
        );
        codex_session(
            roots.codex.as_ref().unwrap(),
            &format!("codex-{n}"),
            n * 3 + 1,
            &format!("Check the billing export {n}"),
            "The export works.",
        );
        opencode_session(
            roots.opencode.as_ref().unwrap(),
            &format!("ses_{n}"),
            n * 3 + 2,
            &format!("Drain the queue {n}"),
            "Drained.",
        );
    }
    let listing = local(&roots, 10, "laptop");
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    let order: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "ses_3", "codex-3", "claude-3", "ses_2", "codex-2", "claude-2", "ses_1", "codex-1",
            "claude-1", "ses_0"
        ]
    );
    let claude = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "claude-3")
        .unwrap();
    assert_eq!(claude.provider, "claude");
    assert_eq!(claude.machine, "laptop");
    assert_eq!(claude.project, "payments");
    assert_eq!(claude.asked, "Fix the payments retry 3");
    assert_eq!(
        claude.concluded,
        "The retry now waits; the deploy is still pending."
    );
    assert_eq!(
        claude.open,
        "The retry now waits; the deploy is still pending"
    );
    assert_eq!(claude.started_at, at(9));
    let codex = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "codex-3")
        .unwrap();
    assert_eq!(
        (codex.project.as_str(), codex.asked.as_str()),
        ("billing", "Check the billing export 3")
    );
    let opencode = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "ses_3")
        .unwrap();
    assert_eq!(
        (opencode.project.as_str(), opencode.concluded.as_str()),
        ("queue", "Drained.")
    );
    // Tool calls, their results and side conversations are never read.
    let text = serde_json::to_string(&listing).unwrap();
    for never in [
        "SHOULD NEVER APPEAR",
        "SIDECHAIN",
        "/home/user",
        ".env",
        "environment_context",
    ] {
        assert!(!text.contains(never), "{never}");
    }
    // The number asked for is respected.
    assert_eq!(local(&roots, 3, "laptop").sessions.len(), 3);
    assert_eq!(local(&roots, 50, "laptop").sessions.len(), 12);
}

#[test]
fn secrets_never_reach_a_summary_or_its_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let token = "tok_9fK2pQ7xL4mN8vR3sT6wY1zA5bC0dE2gH7jK";
    let connection =
        "Server=tcp:db.example.test,1433;Database=pay;User Id=app;Password=Synthetic-Pass-123;";
    let key = format!(
        "-----BEGIN {} KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\nsyntheticline\n-----END {} KEY-----",
        "PRIVATE", "PRIVATE"
    );
    let asked = format!(
        "Try the connection to the payments database from QA.\nIts string is {connection}\nUse the token {token} for the API.\n{key}\nexport DB_PASSWORD=hunter-two"
    );
    let answer = format!(
        "The QA connection times out at the firewall; I asked for the port to be opened and am waiting for it.\nI used {token}.\n```\nPassword=in-a-code-block\n```\npostgres://admin:Synthetic-Pass@db.example.test/pay"
    );
    claude_session(roots.claude.as_ref().unwrap(), "c1", 1, &asked, &answer);
    codex_session(roots.codex.as_ref().unwrap(), "x1", 2, &asked, &answer);
    opencode_session(
        roots.opencode.as_ref().unwrap(),
        "ses_1",
        3,
        &asked,
        &answer,
    );
    let listing = local(&roots, 10, "laptop");
    assert_eq!(listing.sessions.len(), 3);
    let keys: Vec<(String, &Summary)> = listing
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("s{}", i + 1), s))
        .collect();
    let stored = serde_json::to_string(&listing).unwrap();
    let sent = evidence(&keys);
    for text in [&stored, &sent] {
        for secret in [
            token,
            "Synthetic-Pass",
            "Password=",
            "MIIEvQ",
            "syntheticline",
            "PRIVATE KEY",
            "hunter-two",
            "in-a-code-block",
            "db.example.test",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    for session in &listing.sessions {
        assert_eq!(
            session.asked,
            "Try the connection to the payments database from QA."
        );
        assert!(
            session
                .concluded
                .starts_with("The QA connection times out at the firewall"),
            "{}",
            session.concluded
        );
        assert!(session.open.contains("waiting for it"), "{}", session.open);
    }
    assert!(sent.contains("Working session s1"));
    assert!(sent.contains("machine laptop"));
}

#[test]
fn private_and_empty_sessions_are_left_out_and_an_unreadable_store_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    claude_session(
        roots.claude.as_ref().unwrap(),
        "kept",
        1,
        "Review the queue",
        "Done.",
    );
    claude_session(
        roots.claude.as_ref().unwrap(),
        "hidden",
        2,
        "#private look at my notes",
        "Done.",
    );
    // An empty Codex store adds nothing.
    let codex = roots.codex.as_ref().unwrap();
    std::fs::create_dir_all(codex.join("sessions")).unwrap();
    // An OpenCode database of another layout is unreadable, never empty.
    let opencode = roots.opencode.as_ref().unwrap();
    std::fs::create_dir_all(opencode).unwrap();
    rusqlite::Connection::open(opencode.join("opencode.db"))
        .unwrap()
        .execute_batch("CREATE TABLE other(id TEXT);")
        .unwrap();
    let listing = local(&roots, 10, "laptop");
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["kept"]);
    assert_eq!(
        listing.unavailable,
        [Unavailable {
            machine: "laptop".into(),
            provider: "opencode".into(),
            reason: "unreadable".into()
        }]
    );
}

#[test]
fn an_opencode_database_is_read_without_its_tool_parts_or_sub_agents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    let db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT, parent_id TEXT, directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         INSERT INTO session VALUES('ses_a', NULL, '/home/user/gateway', 'Gateway', 1793000000000, 1793000600000);
         INSERT INTO session VALUES('ses_child', 'ses_a', '/home/user/gateway', 'Sub', 1793000000000, 1793000900000);
         INSERT INTO message VALUES('m1', 'ses_a', 1793000000000, '{\"role\":\"user\"}');
         INSERT INTO message VALUES('m2', 'ses_a', 1793000500000, '{\"role\":\"assistant\"}');
         INSERT INTO part VALUES('p1', 'm1', 'ses_a', 1, '{\"type\":\"text\",\"text\":\"Why does the gateway return 502?\"}');
         INSERT INTO part VALUES('p2', 'm2', 'ses_a', 2, '{\"type\":\"tool\",\"state\":{\"output\":\"TOOL OUTPUT\"}}');
         INSERT INTO part VALUES('p3', 'm2', 'ses_a', 3, '{\"type\":\"text\",\"text\":\"The upstream pool is exhausted.\"}');",
    )
    .unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "server",
    );
    assert_eq!(listing.sessions.len(), 1);
    let session = &listing.sessions[0];
    assert_eq!(session.session_id, "ses_a");
    assert_eq!(session.project, "gateway");
    assert_eq!(session.asked, "Why does the gateway return 502?");
    assert_eq!(session.concluded, "The upstream pool is exhausted.");
    assert_eq!(session.updated_at, from_millis(1_793_000_600_000).unwrap());
}

#[test]
fn listings_merge_newest_first_and_name_the_runs_pta_started() {
    let summary = |id: &str, at: &str| Summary {
        provider: "claude".into(),
        session_id: id.into(),
        updated_at: at.into(),
        ..Default::default()
    };
    let mut merged = Listing::merge(
        [
            Listing {
                sessions: vec![summary("a", "2026-11-12T10:00:00Z")],
                unavailable: vec![],
            },
            Listing {
                sessions: vec![
                    summary("b", "2026-11-12T11:00:00Z"),
                    summary("a", "2026-11-12T10:00:00Z"),
                ],
                unavailable: vec![Unavailable {
                    machine: "desk".into(),
                    provider: String::new(),
                    reason: "offline".into(),
                }],
            },
        ],
        10,
    );
    assert_eq!(
        merged
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(merged.unavailable[0].reason, "offline");
    merged.attribute(&[(
        "a".into(),
        Started {
            run: "run-1".into(),
            work_item_url: "https://dev.azure.com/org/proj/_workitems/edit/7".into(),
            work_item_title: "Retry payments".into(),
            state: "running".into(),
        },
    )]);
    assert_eq!(merged.sessions[1].started_by.as_ref().unwrap().run, "run-1");
    let text = evidence(&[("s1".into(), &merged.sessions[1])]);
    assert!(text.contains("«Retry payments» (https://dev.azure.com/org/proj/_workitems/edit/7)"));
}

#[test]
fn a_long_session_is_read_from_its_start_and_its_end_and_long_lines_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude/projects/p/long.jsonl");
    let mut lines = vec![
        json!({"type":"user","sessionId":"long","cwd":"/w/p","message":{"role":"user","content":"First request"},"timestamp":at(0)}),
    ];
    let image = "A".repeat(400_000);
    for _ in 0..12 {
        lines.push(json!({"type":"user","sessionId":"long","message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"image","source":{"data":image}}]}]},"timestamp":at(1)}));
    }
    lines.push(json!({"type":"assistant","sessionId":"long","message":{"role":"assistant","content":[{"type":"text","text":"Final answer"}]},"timestamp":at(5)}));
    write_lines(&path, &lines);
    assert!(std::fs::metadata(&path).unwrap().len() > HEAD_BYTES + TAIL_BYTES);
    let listing = local(
        &Roots {
            claude: Some(dir.path().join("claude")),
            ..Default::default()
        },
        10,
        "",
    );
    let session = &listing.sessions[0];
    assert_eq!(
        (session.asked.as_str(), session.concluded.as_str()),
        ("First request", "Final answer")
    );
    assert_eq!(session.updated_at, at(5));
}
