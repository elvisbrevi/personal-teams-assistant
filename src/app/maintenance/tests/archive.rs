//! Archive installations (the install scripts' kind) and unattended updating, through the host's
//! own operations, with a synthetic registry and a synthetic release (local HTTP doubles). Nothing
//! reaches the network, Cargo or the installed executables.
use super::*;
use crate::maintenance::release::{self, Source};
use crate::store::settings::SettingsStore;
use sha2::{Digest as _, Sha256};

/// The installation's recorded target: the archive format and the executable names follow it.
const TARGET: &str = if cfg!(windows) {
    "x86_64-pc-windows-msvc"
} else {
    "x86_64-unknown-linux-gnu"
};

fn archive_name(version: &str) -> String {
    release::archive_name(TARGET, &Version::parse(version).unwrap())
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The release archive of `version`: the directory with the matched pair, a licence and a readme.
/// The executables are scripts that report `reported`, so a pair is verified without building.
fn archive_of(version: &str, reported: &str) -> Vec<u8> {
    use crate::maintenance::release::tests::{Kind, tgz, zip};
    let dir = release::directory_name(TARGET, &Version::parse(version).unwrap());
    let host = format!("#!/bin/sh\necho \"personal-teams-assistant {reported}\"\n");
    let cli =
        format!("#!/bin/sh\necho '{{\"ok\":true,\"data\":{{\"cli_version\":\"{reported}\"}}}}'\n");
    let names = [
        format!("{dir}/"),
        format!("{dir}/{}", core::exe_name(core::CRATE)),
        format!("{dir}/{}", core::exe_name(core::CLI)),
        format!("{dir}/LICENSE"),
    ];
    let entries = [
        (names[0].as_str(), Kind::Dir),
        (names[1].as_str(), Kind::File(host.into_bytes())),
        (names[2].as_str(), Kind::File(cli.into_bytes())),
        (names[3].as_str(), Kind::File(b"MIT".to_vec())),
    ];
    if cfg!(windows) {
        zip(&entries)
    } else {
        tgz(&entries)
    }
}

fn asset(version: &str, file: &str) -> String {
    format!("/owner/repo/releases/download/v{version}/{file}")
}

/// A synthetic release of `version`: the archive and its `.sha256` (by default the right one).
async fn release_of(version: &str, archive: Vec<u8>, digest: Option<String>) -> MockServer {
    let server = MockServer::start().await;
    let name = archive_name(version);
    let digest = digest.unwrap_or_else(|| format!("{}  {name}\n", hex(&archive)));
    Mock::given(method("GET"))
        .and(path(asset(version, &name)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(asset(version, &format!("{name}.sha256"))))
        .respond_with(ResponseTemplate::new(200).set_body_string(digest))
        .mount(&server)
        .await;
    server
}

fn source(uri: &str) -> Source {
    Source {
        base: format!("{}/owner/repo", uri.trim_end_matches('/')),
        // The double listens on 127.0.0.1; a redirect to any other name is refused.
        hosts: vec!["127.0.0.1".into()],
        https_only: false,
    }
}

fn record(version: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "schema": 1, "kind": "archive", "package": core::CRATE, "version": version,
        "target": TARGET, "bins": [core::CRATE, core::CLI],
        "repository": "https://github.com/elvisbrevi/personal-teams-assistant",
        "archive": archive_name(version), "sha256": "0".repeat(64),
        "installed_at": "2026-11-01T00:00:00Z",
    }))
    .unwrap()
}

/// Turns the fixture into what the install scripts leave: `install.json` instead of Cargo's record.
fn archived(f: &mut Fixture, from: Option<Source>) {
    let _ = fs::remove_file(f.install.join(".crates2.json"));
    fs::write(f.install.join(core::ARCHIVE_RECORD), record("0.9.13")).unwrap();
    f.env.release = from;
    *TEST.lock().unwrap() = Some(f.env.clone());
}

fn renew(f: &mut Fixture, edit: impl FnOnce(&mut Environment)) {
    edit(&mut f.env);
    *TEST.lock().unwrap() = Some(f.env.clone());
}

fn bins(f: &Fixture) -> Vec<u8> {
    [core::CRATE, core::CLI]
        .iter()
        // As the fixture writes them: without `.exe`, also on Windows.
        .flat_map(|name| fs::read(f.install.join("bin").join(name)).unwrap())
        .collect()
}

/// What `staged/` holds: nothing at all after a failure.
fn staged_entries(f: &Fixture) -> Vec<String> {
    fs::read_dir(f.staged())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn settled(f: &Fixture) -> Value {
    for _ in 0..400 {
        let status = call(&f.host, Local, "crate_update_status", json!({})).await;
        if status.data["state"] != "staging" {
            return status.data;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the staging did not finish");
}

/// A prepared archive pair (plain bytes, as [`with_pair`]) with its manifest naming the archive.
fn with_archive_pair(f: &mut Fixture) {
    with_pair(f);
    archived(f, None);
    let base = staged_dir(&directory(&f.host), "0.9.14");
    let _ = fs::remove_file(base.join("root/.crates2.json"));
    let path = base.join("manifest.json");
    let mut manifest: Manifest = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest.no_default_features = false;
    manifest.target = Some(TARGET.into());
    manifest.archive = Some(core::ArchiveFile {
        name: archive_name("0.9.14"),
        sha256: "e".repeat(64),
        size: 1234,
    });
    fs::write(&path, serde_json::to_string(&manifest).unwrap()).unwrap();
}

fn enable(dir: &Path) {
    save(dir, |saved| {
        saved.unattended = Some(Unattended {
            enabled: true,
            changed_at: 1,
            by: "cli".into(),
        })
    })
    .unwrap();
}

fn events(f: &Fixture) -> Vec<String> {
    load(&directory(&f.host))
        .history
        .iter()
        .map(|e| e.event.clone())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn an_archive_installation_stages_its_release_archive_without_cargo() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.13", false), line("0.9.14", false)]).await;
    let releases = release_of("0.9.14", archive_of("0.9.14", "0.9.14"), None).await;
    let mut f = fixture(&server);
    archived(&mut f, Some(source(&releases.uri())));
    let before = bins(&f);
    let status = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(status.data["installed"], "0.9.13");
    assert_eq!(status.data["install"]["supported"], true);
    assert_eq!(status.data["install"]["kind"], "archive");
    assert_eq!(status.data["install"]["target"], TARGET);
    assert_eq!(
        status.data["unattended"]["enabled"], false,
        "off by default"
    );
    let checked = call(&f.host, Window, "crate_update_check", json!({})).await;
    assert_eq!(checked.data["available"], "0.9.14");
    assert_eq!(checked.data["can_stage"], true);

    let staged = call(
        &f.host,
        Window,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert!(staged.ok, "{:?}", staged.message);
    let done = settled(&f).await;
    assert_eq!(done["state"], "staged", "{done}");
    assert_eq!(done["can_apply"], true);
    assert_eq!(done["staged"]["target"], TARGET);
    // Cargo never ran, and the installed pair was not touched.
    assert!(f.arguments().is_none());
    assert_eq!(bins(&f), before);
    let base = staged_dir(&directory(&f.host), "0.9.14");
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(base.join("manifest.json")).unwrap()).unwrap();
    let archive = manifest.archive.unwrap();
    assert_eq!(archive.name, archive_name("0.9.14"));
    assert_eq!(archive.sha256, hex(&archive_of("0.9.14", "0.9.14")));
    // Only the two executables were kept: no licence, no download.
    let mut kept: Vec<String> = fs::read_dir(base.join("root/bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(kept, ["personal-teams-assistant", "pta"]);
    assert!(!base.join("download").exists());
    let text = done.to_string();
    assert!(!text.contains(&releases.uri()), "no address in the status");
}

/// A raw HTTP double that announces more (or far more) than it sends.
async fn short_server(version: &str, body: Vec<u8>, announced: u64) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let name = archive_name(version);
    let digest = format!("{}  {name}\n", hex(&body));
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (digest, body) = (digest.clone(), body.clone());
            tokio::spawn(async move {
                let mut request = vec![0u8; 4096];
                let n = socket.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]).into_owned();
                let line = request.lines().next().unwrap_or_default().to_owned();
                if line.contains(".sha256") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{digest}",
                        digest.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {announced}\r\nconnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body[..body.len() / 2]).await;
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_digest_size_or_missing_archive_failure_leaves_nothing_staged() {
    let _serial = SERIAL.lock().await;
    let good = archive_of("0.9.14", "0.9.14");
    let name = archive_name("0.9.14");
    // A release without the archive for this target (only its digest).
    let missing = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(asset("0.9.14", &format!("{name}.sha256"))))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!("{}  {name}", hex(&good))))
        .mount(&missing)
        .await;
    // The doubles stay alive for the whole test.
    let mismatch = release_of(
        "0.9.14",
        good.clone(),
        Some(format!("{}  {name}\n", hex(b"another archive"))),
    )
    .await;
    let other = release_of(
        "0.9.14",
        good.clone(),
        Some(format!("{}  other.tgz\n", hex(&good))),
    )
    .await;
    let empty = MockServer::start().await;
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "digest mismatch",
            mismatch.uri(),
            "does not match the SHA-256",
        ),
        ("digest of another file", other.uri(), "names another file"),
        ("missing archive", missing.uri(), "has no file"),
        ("missing release", empty.uri(), "has no file"),
        (
            "truncated download",
            short_server("0.9.14", good.clone(), good.len() as u64).await,
            "interrupted",
        ),
        (
            "far too large",
            short_server("0.9.14", good.clone(), release::MAX_ARCHIVE + 1).await,
            "larger than a release archive",
        ),
    ];
    for (case, uri, expected) in cases {
        let server = registry(&[line("0.9.14", false)]).await;
        let mut f = fixture(&server);
        archived(&mut f, Some(source(&uri)));
        let before = bins(&f);
        let started = call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"}),
        )
        .await;
        assert!(started.ok, "{case}: {:?}", started.message);
        let done = settled(&f).await;
        assert_eq!(done["state"], "failed", "{case}: {done}");
        let error = done["stage"]["error"].as_str().unwrap_or_default();
        assert!(error.contains(expected), "{case}: {error}");
        assert_eq!(done["can_apply"], false, "{case}");
        assert!(
            staged_entries(&f).is_empty(),
            "{case}: {:?}",
            staged_entries(&f)
        );
        assert_eq!(bins(&f), before, "{case}");
        drop(f);
    }
}

#[tokio::test]
async fn only_the_release_hosts_own_redirect_is_followed() {
    let _serial = SERIAL.lock().await;
    let good = archive_of("0.9.14", "0.9.14");
    let name = archive_name("0.9.14");
    let target = release_of("0.9.14", good.clone(), None).await;
    let port = target.address().port();
    for (case, location, expect_staged) in [
        // The same server under another name: not one of the release host's own addresses.
        (
            "another host",
            format!("http://localhost:{port}{}", asset("0.9.14", &name)),
            false,
        ),
        (
            "the release host's own address",
            format!("http://127.0.0.1:{port}{}", asset("0.9.14", &name)),
            cfg!(unix),
        ),
    ] {
        let server = registry(&[line("0.9.14", false)]).await;
        let releases = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(asset("0.9.14", &name)))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str()))
            .mount(&releases)
            .await;
        Mock::given(method("GET"))
            .and(path(asset("0.9.14", &format!("{name}.sha256"))))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(format!("{}  {name}", hex(&good))),
            )
            .mount(&releases)
            .await;
        let mut f = fixture(&server);
        archived(&mut f, Some(source(&releases.uri())));
        call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"}),
        )
        .await;
        let done = settled(&f).await;
        if expect_staged {
            assert_eq!(done["state"], "staged", "{case}: {done}");
        } else {
            assert_eq!(done["state"], "failed", "{case}: {done}");
            if case == "another host" {
                assert!(
                    done["stage"]["error"]
                        .as_str()
                        .unwrap()
                        .contains("redirected to an unexpected address"),
                    "{done}"
                );
                assert!(staged_entries(&f).is_empty());
            }
        }
    }
    // Production refuses plain HTTP outright and lists only the release host's redirect targets.
    let official = Source::official().unwrap();
    assert!(official.https_only);
    assert!(official.base.starts_with("https://github.com/"));
}

#[tokio::test]
async fn an_archive_update_is_applied_and_its_record_follows_only_a_healthy_commit() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    let dir = directory(&f.host);
    let status = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(status.data["state"], "staged", "{}", status.data);
    assert_eq!(status.data["can_apply"], true);
    let reply = requested(&f).await;
    assert!(reply.ok, "{:?}", reply.message);
    assert_eq!(reply.data["state"], "applying");
    let fake = Fake::new(&f);
    assert!(matches!(
        apply::drive(&dir, &fake, tune()).await,
        Outcome::Committed
    ));
    assert_eq!(installed(&f, core::CRATE), format!("{NEW} {}", core::CRATE));
    let written: Value =
        serde_json::from_str(&fs::read_to_string(f.install.join(core::ARCHIVE_RECORD)).unwrap())
            .unwrap();
    assert_eq!(written["version"], "0.9.14");
    assert_eq!(written["archive"], archive_name("0.9.14"));
    assert_eq!(written["sha256"], "e".repeat(64));
    assert_eq!(written["target"], TARGET);
    let after = call(&f.host, Local, "crate_update_status", json!({}))
        .await
        .data;
    assert_eq!(after["installed"], "0.9.14");
    assert_eq!(after["install"]["kind"], "archive");
    assert_eq!(after["apply"]["state"], "success");

    // An unhealthy new version: the previous pair and the record it had come back.
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    let dir = directory(&f.host);
    let before = fs::read_to_string(f.install.join(core::ARCHIVE_RECORD)).unwrap();
    assert!(requested(&f).await.ok);
    let mut entry = core::apply::read(&dir).unwrap();
    entry.intent.assistant = true;
    entry.intent.subscriptions = 1;
    core::apply::write(&dir, &entry).unwrap();
    let fake = Fake {
        new_unhealthy: true,
        ..Fake::new(&f)
    };
    assert!(matches!(
        apply::drive(&dir, &fake, tune()).await,
        Outcome::RolledBack
    ));
    assert_eq!(installed(&f, core::CRATE), format!("{OLD} {}", core::CRATE));
    assert_eq!(
        fs::read_to_string(f.install.join(core::ARCHIVE_RECORD)).unwrap(),
        before
    );
}

#[tokio::test]
async fn unattended_updating_is_off_by_default_and_only_the_local_owner_changes_it() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    archived(&mut f, None);
    let dir = directory(&f.host);
    let status = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(status.data["unattended"]["enabled"], false);
    assert_eq!(status.data["unattended"]["available"], true);
    // Off: a pass does nothing at all.
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Off);
    assert!(!f.staged().exists());

    // Portal sessions and machine credentials never reach it.
    for transport in [Browser, Machine] {
        let reply = call(
            &f.host,
            transport,
            "crate_update_unattended",
            json!({"enabled":true}),
        )
        .await;
        assert_eq!(reply.code, "authority_denied");
    }
    for args in [
        json!({}),
        json!({"enabled":"yes"}),
        json!({"enabled":true,"by":"agent"}),
    ] {
        let reply = call(&f.host, Local, "crate_update_unattended", args.clone()).await;
        assert_eq!(reply.code, "invalid_input", "{args}");
    }
    assert!(!unattended_on(&load(&dir)));

    // The synchronized configuration does not carry it, before or after.
    let project = |f: &Fixture| {
        let settings = &f.host.state.settings;
        serde_json::to_string(&crate::portable::Document::project(
            &settings.config().unwrap(),
            &settings.map().unwrap(),
        ))
        .unwrap()
    };
    let config = fs::read_to_string(f.host.state.settings.config_path()).unwrap();
    let portable = project(&f);
    let on = call(
        &f.host,
        Window,
        "crate_update_unattended",
        json!({"enabled":true}),
    )
    .await;
    assert!(on.ok, "{:?}", on.message);
    assert_eq!(on.data["unattended"]["enabled"], true);
    assert_eq!(on.data["unattended"]["changed_by"], "local_page");
    assert_eq!(project(&f), portable);
    assert!(!portable.contains("unattended"));
    assert_eq!(
        fs::read_to_string(f.host.state.settings.config_path()).unwrap(),
        config
    );
    // Setting the same value again changes nothing and audits nothing more.
    call(
        &f.host,
        Window,
        "crate_update_unattended",
        json!({"enabled":true}),
    )
    .await;
    let off = call(
        &f.host,
        Local,
        "crate_update_unattended",
        json!({"enabled":false}),
    )
    .await;
    assert_eq!(off.data["unattended"]["changed_by"], "cli");
    assert_eq!(events(&f), ["unattended_on", "unattended_off"]);
    let history = &off.data["unattended"]["history"];
    assert_eq!(history[0]["event"], "unattended_off");
    assert_eq!(history[1]["by"], "local_page");

    // An installation that cannot be updated from here cannot turn it on.
    renew(&mut f, |env| env.exe = PathBuf::from("/nowhere/pta"));
    let refused = call(
        &f.host,
        Local,
        "crate_update_unattended",
        json!({"enabled":true}),
    )
    .await;
    assert_eq!(refused.code, "not_ready");
}

#[cfg(unix)]
#[tokio::test]
async fn unattended_updating_prepares_and_installs_and_never_retries_a_release_that_failed() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.13", false), line("0.9.14", false)]).await;
    let releases = release_of("0.9.14", archive_of("0.9.14", "0.9.14"), None).await;
    let mut f = fixture(&server);
    archived(&mut f, Some(source(&releases.uri())));
    let dir = directory(&f.host);
    enable(&dir);
    // Nothing known yet: the pass waits for the registry check the loop makes first.
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Current);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Staging);
    assert_eq!(settled(&f).await["state"], "staged");
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Applying);
    let entry = core::apply::read(&dir).unwrap();
    assert!(entry.intent.unattended);
    assert_eq!(entry.to, "0.9.14");
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Busy);
    assert_eq!(
        crate::app::maintenance::report(&f.host)["state"],
        "applying"
    );

    // The new version never becomes healthy: rolled back, recorded, not tried again on its own.
    let mut entry = core::apply::read(&dir).unwrap();
    entry.intent.assistant = true;
    entry.intent.subscriptions = 1;
    core::apply::write(&dir, &entry).unwrap();
    let fake = Fake::new(&f);
    assert!(matches!(
        apply::drive(&dir, &fake, tune()).await,
        Outcome::RolledBack
    ));
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Skipped);
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Skipped);
    let status = call(&f.host, Local, "crate_update_status", json!({}))
        .await
        .data;
    assert_eq!(status["unattended"]["skipped"]["version"], "0.9.14");
    assert_eq!(status["unattended"]["skipped"]["reason"], "failed");
    assert_eq!(
        events(&f),
        ["unattended_stage", "unattended_apply", "unattended_result"]
    );
    let result = &load(&dir).history[2];
    assert!(
        result.detail.as_deref().unwrap().starts_with("rolled_back"),
        "{:?}",
        result.detail
    );
    let report = crate::app::maintenance::report(&f.host);
    assert_eq!(report["failed"], "0.9.14");
    assert_eq!(report["unattended"], true);
    // A person can still install it deliberately: only the unattended path skips it.
    assert!(requested(&f).await.ok);
    let _ = fs::remove_file(dir.join(core::apply::JOURNAL));

    // A newer release is tried.
    let server = registry(&[line("0.9.14", false), line("0.9.15", false)]).await;
    let releases = release_of("0.9.15", archive_of("0.9.15", "0.9.15"), None).await;
    renew(&mut f, |env| {
        env.registry = server.uri();
        env.release = Some(source(&releases.uri()));
    });
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Staging);
    assert_eq!(settled(&f).await["staged"]["version"], "0.9.15");
}

#[tokio::test]
async fn unattended_updating_waits_for_work_and_a_cancelled_request_is_not_repeated() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    let dir = directory(&f.host);
    enable(&dir);
    let data = profiles_of(&f).remove(0).data;
    fs::create_dir_all(&data).unwrap();
    jobs(&data, "processing");
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Deferred);
    let saved = load(&dir);
    let deferred = saved.deferred.as_ref().unwrap();
    assert!(deferred.unattended);
    assert_eq!(
        deferred.reasons,
        ["A Teams message is being answered or sent."]
    );
    assert!(core::apply::read(&dir).is_none(), "nothing began");
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Deferred);
    // Audited once per change of reasons, not once per look.
    assert_eq!(events(&f), ["unattended_deferred"]);
    let status = call(&f.host, Local, "crate_update_status", json!({}))
        .await
        .data;
    assert_eq!(status["deferred"]["unattended"], true);

    // Turning unattended updating off withdraws what it asked for; on again, it asks again.
    let off = call(
        &f.host,
        Local,
        "crate_update_unattended",
        json!({"enabled":false}),
    )
    .await;
    assert_eq!(off.data["state"], "staged");
    assert!(load(&dir).deferred.is_none());
    enable(&dir);
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Deferred);
    let cancelled = call(&f.host, Local, "crate_update_cancel", json!({})).await;
    assert!(cancelled.ok);
    assert_eq!(
        cancelled.data["unattended"]["skipped"]["reason"],
        "cancelled"
    );
    jobs(&data, "done");
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Skipped);
    assert_eq!(
        events(&f),
        [
            "unattended_deferred",
            "unattended_off",
            "unattended_deferred",
            "unattended_cancelled"
        ]
    );
}

/// The 2026-11-06 server finding: an assistant whose Microsoft session needs renewal still reports
/// running, and a restart would leave it stopped.
fn needs_renewal(f: &Fixture) {
    let data = profiles_of(f).remove(0).data;
    fs::create_dir_all(&data).unwrap();
    let store = crate::state::Store::open(&data.join("assistant.db")).unwrap();
    store.put_token(b"sealed synthetic session").unwrap();
    store.event("oauth", "authorized").unwrap();
    store
        .event("oauth", crate::state::Store::RENEWAL_REQUIRED)
        .unwrap();
    f.host.state.phase.send_replace(AssistantPhase::Running);
}

#[tokio::test]
async fn unattended_updating_defers_while_the_microsoft_session_needs_renewal() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    let dir = directory(&f.host);
    enable(&dir);
    let renewal = |f: &Fixture| {
        let host = f.host.clone();
        async move {
            call(&host, Local, "snapshot", json!({})).await.data["microsoft_session_needs_renewal"]
                .clone()
        }
    };
    assert_eq!(renewal(&f).await, json!(false));
    needs_renewal(&f);
    // `pta --json status` says so, for the server's updater (scripts/vps-update.py).
    assert_eq!(renewal(&f).await, json!(true));
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Deferred);
    let saved = load(&dir);
    assert_eq!(saved.deferred.as_ref().unwrap().reasons, [RENEWAL]);
    assert!(core::apply::read(&dir).is_none(), "nothing was stopped");
    assert_eq!(installed(&f, core::CRATE), format!("{OLD} {}", core::CRATE));

    // Signed in again: the waiting request goes ahead.
    let data = profiles_of(&f).remove(0).data;
    crate::state::Store::open(&data.join("assistant.db"))
        .unwrap()
        .event("oauth", "authorized")
        .unwrap();
    assert_eq!(renewal(&f).await, json!(false));
    let reply = apply::request(&f.host, &f.env, "0.9.14", true).await;
    assert_eq!(reply.data["state"], "applying", "{:?}", reply.message);
    assert!(!core::apply::read(&dir).unwrap().intent.degraded);
}

#[tokio::test]
async fn a_persons_update_counts_an_assistant_that_needed_renewal_honestly() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    let dir = directory(&f.host);
    needs_renewal(&f);
    // The person asked: it proceeds, and the journal says the assistant was not answering.
    let reply = requested(&f).await;
    assert_eq!(reply.data["state"], "applying", "{:?}", reply.message);
    let entry = core::apply::read(&dir).unwrap();
    assert!(entry.intent.assistant && entry.intent.degraded);
    // The assistant cannot start on the new version either (same session): that is not the
    // release's fault, so the update is kept, with a note to sign in again.
    let fake = Fake {
        assistant_fails: true,
        ..Fake::new(&f)
    };
    assert!(matches!(
        apply::drive(&dir, &fake, tune()).await,
        Outcome::Committed
    ));
    assert_eq!(installed(&f, core::CRATE), format!("{NEW} {}", core::CRATE));
    assert!(fake.logged().contains(&"start_assistant".to_owned()));
    let status = call(&f.host, Local, "crate_update_status", json!({}))
        .await
        .data;
    let notes = status["apply"]["notes"].to_string();
    assert!(notes.contains("needs renewal"), "{notes}");
}

#[tokio::test]
async fn on_a_mac_with_keychain_credentials_the_unattended_update_waits_for_the_person() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    with_archive_pair(&mut f);
    renew(&mut f, |env| env.keychain = Some(true));
    let dir = directory(&f.host);
    enable(&dir);
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Interactive);
    assert_eq!(unattended(&f.host, &f.env).await, Pass::Interactive);
    assert!(core::apply::read(&dir).is_none());
    let status = call(&f.host, Local, "crate_update_status", json!({}))
        .await
        .data;
    assert_eq!(status["state"], "staged");
    assert_eq!(status["can_apply"], true, "the person can install it");
    assert!(
        status["unattended"]["waiting"]
            .as_str()
            .unwrap()
            .contains("Keychain")
    );
    assert_eq!(events(&f), ["unattended_waiting"]);
    assert_eq!(requested(&f).await.data["state"], "applying");
}

#[tokio::test]
async fn the_fleet_report_carries_only_versions_and_fixed_states() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    archived(&mut f, None);
    let report = crate::app::maintenance::report(&f.host);
    assert_eq!(
        report,
        json!({"state":"current","pending":null,"failed":null,"unattended":false})
    );
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(
        crate::app::maintenance::report(&f.host)["state"],
        "available"
    );
    with_archive_pair(&mut f);
    enable(&directory(&f.host));
    let report = crate::app::maintenance::report(&f.host);
    assert_eq!(
        report,
        json!({"state":"staged","pending":"0.9.14","failed":null,"unattended":true})
    );
    assert!(
        !report
            .to_string()
            .contains(&f.install.display().to_string())
    );
}
