use super::*;
use serde_json::json;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn line(name: &str, vers: &str, yanked: bool) -> String {
    json!({"name":name,"vers":vers,"yanked":yanked,"cksum":"00","deps":[],"features":{}})
        .to_string()
}

#[test]
fn versions_are_stable_numbers_ordered_by_part() {
    assert!(v("0.10.0") > v("0.9.99"));
    assert!(v("1.0.0") > v("0.99.99"));
    assert!(v("0.9.14") > v("0.9.13"));
    assert_eq!(v("1.2.3").to_string(), "1.2.3");
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "1.2.3-rc.1",
        "1.2.3+build",
        "01.2.3",
        "1.02.3",
        "a.b.c",
        "1..3",
        " 1.2.3",
        "1.2.3 ",
        "-1.2.3",
        "99999999999999999999.0.0",
    ] {
        assert!(Version::parse(bad).is_none(), "{bad:?} must not parse");
    }
    assert_eq!(Version::parse_rust("1.95"), Some(Version(1, 95, 0)));
    assert_eq!(Version::parse_rust("1.95.2"), Some(Version(1, 95, 2)));
}

#[test]
fn sparse_index_paths_follow_cargos_layout() {
    assert_eq!(
        index_path("personal-teams-assistant"),
        "pe/rs/personal-teams-assistant"
    );
    assert_eq!(index_path("a"), "1/a");
    assert_eq!(index_path("ab"), "2/ab");
    assert_eq!(index_path("abc"), "3/a/abc");
    assert_eq!(index_path("Serde"), "se/rd/serde");
}

#[test]
fn the_newest_stable_unyanked_release_wins() {
    let body = [
        line(CRATE, "0.9.9", false),
        line(CRATE, "0.10.0", false),
        line(CRATE, "0.9.14", false),
        // Newer, but not usable.
        line(CRATE, "0.11.0", true),
        line(CRATE, "0.12.0-rc.1", false),
        line(CRATE, "0.12.0+build.5", false),
        // Another crate's lines and garbage in between.
        line("other-crate", "9.9.9", false),
        "not json".to_owned(),
        "{\"name\":3}".to_owned(),
        String::new(),
    ]
    .join("\n");
    let release = newest_stable(&body).unwrap().unwrap();
    assert_eq!(release.version, v("0.10.0"));
}

#[test]
fn the_rust_requirement_comes_from_the_chosen_release() {
    let body = [
        json!({"name":CRATE,"vers":"0.9.1","yanked":false,"rust_version":"1.80"}).to_string(),
        json!({"name":CRATE,"vers":"0.9.2","yanked":false,"rust_version":"1.95.0"}).to_string(),
    ]
    .join("\n");
    assert_eq!(
        newest_stable(&body).unwrap().unwrap().rust_version,
        Some(v("1.95.0"))
    );
}

#[test]
fn a_release_that_is_only_unusable_is_nothing_newer_not_an_error() {
    let body = [
        line(CRATE, "0.9.0", true),
        line(CRATE, "1.0.0-beta.1", false),
    ]
    .join("\n");
    assert_eq!(newest_stable(&body).unwrap(), None);
}

#[test]
fn an_answer_about_another_crate_or_malformed_is_an_error() {
    for body in [
        "",
        "<html>not the index</html>",
        "[]",
        &line("other-crate", "9.9.9", false),
        "{\"vers\":\"9.9.9\"}",
    ] {
        assert!(newest_stable(body).is_err(), "{body:?}");
    }
}

fn write_records(root: &Path, key: &str, record: serde_json::Value) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(
        root.join(".crates2.json"),
        json!({"installs": {key: record}}).to_string(),
    )
    .unwrap();
}

fn crates_io(version: &str) -> String {
    format!("{CRATE} {version} (registry+https://github.com/rust-lang/crates.io-index)")
}

fn full_record() -> serde_json::Value {
    json!({
        "version_req": null,
        "bins": ["personal-teams-assistant", "pta"],
        "features": [],
        "all_features": false,
        "no_default_features": true,
        "profile": "release",
        "target": "x86_64-unknown-linux-gnu",
        "rustc": "rustc 1.95.0\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\nrelease: 1.95.0\n"
    })
}

#[test]
fn an_installation_is_recognized_from_cargos_record() {
    let dir = tempfile::tempdir().unwrap();
    write_records(dir.path(), &crates_io("0.9.13"), full_record());
    let exe = dir.path().join("bin").join(exe_name(CLI));
    let installation = inspect(&exe).unwrap();
    assert_eq!(installation.version, v("0.9.13"));
    assert_eq!(installation.root, dir.path());
    assert!(installation.no_default_features);
    assert!(installation.features.is_empty());
    // The host's own target equals the compiler's: nothing to pass on.
    assert_eq!(installation.target, None);
}

#[test]
fn the_feature_and_target_choices_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut record = full_record();
    record["no_default_features"] = json!(false);
    record["features"] = json!(["gui"]);
    record["target"] = json!("aarch64-apple-darwin");
    record["bins"] = json!(["personal-teams-assistant.exe", "pta.exe"]);
    write_records(dir.path(), &crates_io("0.9.13"), record);
    let installation = inspect(&dir.path().join("bin").join("pta")).unwrap();
    assert_eq!(installation.features, ["gui"]);
    assert_eq!(installation.target.as_deref(), Some("aarch64-apple-darwin"));
    let args: Vec<String> = install_args(
        &installation,
        &v("0.9.14"),
        Path::new("/stage/root"),
        Path::new("/cache"),
    )
    .iter()
    .map(|a| a.to_string_lossy().into_owned())
    .collect();
    assert_eq!(
        args,
        [
            "install",
            CRATE,
            "--version",
            "0.9.14",
            "--locked",
            "--root",
            "/stage/root",
            "--target-dir",
            "/cache",
            "--color",
            "never",
            "--features",
            "gui",
            "--target",
            "aarch64-apple-darwin"
        ]
    );
}

#[test]
fn the_build_cache_size_counts_nested_files() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(tree_size(&dir.path().join("missing")), 0);
    std::fs::create_dir_all(dir.path().join("release/deps")).unwrap();
    std::fs::write(dir.path().join("a"), [0u8; 3]).unwrap();
    std::fs::write(dir.path().join("release/deps/b"), [0u8; 5]).unwrap();
    assert_eq!(tree_size(dir.path()), 8);
}

#[test]
fn a_headless_install_is_rebuilt_headless() {
    let dir = tempfile::tempdir().unwrap();
    write_records(dir.path(), &crates_io("0.9.13"), full_record());
    let installation = inspect(&dir.path().join("bin").join(exe_name(CLI))).unwrap();
    let args = install_args(
        &installation,
        &v("0.9.14"),
        Path::new("/r"),
        Path::new("/t"),
    );
    assert!(args.iter().any(|a| a == "--no-default-features"));
    assert!(
        !args
            .iter()
            .any(|a| a == "--features" || a == "--all-features")
    );
    assert!(args.iter().any(|a| a == "--locked"));
}

#[test]
fn unsupported_installations_say_why() {
    let reason = |dir: &Path| inspect(&dir.join("bin").join(exe_name(CLI))).unwrap_err();

    // Not in a `bin` directory: a development build.
    let dev = tempfile::tempdir().unwrap();
    assert!(
        inspect(&dev.path().join("target").join("debug").join("pta"))
            .unwrap_err()
            .contains("development build")
    );

    // No record at all.
    let bare = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(bare.path().join("bin")).unwrap();
    assert!(reason(bare.path()).contains("cargo install"));

    // Installed from a path, git or another registry.
    for source in [
        "path+file:///home/me/src/personal-teams-assistant#0.9.13",
        "git+https://example.invalid/x.git#abc",
        "registry+https://example.invalid/index",
        "sparse+https://index.example.invalid/",
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_records(
            dir.path(),
            &format!("{CRATE} 0.9.13 ({source})"),
            full_record(),
        );
        assert!(reason(dir.path()).contains("crates.io"), "{source}");
    }

    // The sparse crates.io source is the same registry.
    let sparse = tempfile::tempdir().unwrap();
    write_records(
        sparse.path(),
        &format!("{CRATE} 0.9.13 (sparse+https://index.crates.io/)"),
        full_record(),
    );
    assert!(inspect(&sparse.path().join("bin").join(exe_name(CLI))).is_ok());

    // A custom feature, a missing binary, a debug profile and a hostile target.
    for edit in [
        |r: &mut serde_json::Value| r["features"] = json!(["something-else"]),
        |r: &mut serde_json::Value| r["bins"] = json!(["pta"]),
        |r: &mut serde_json::Value| r["profile"] = json!("dev"),
        |r: &mut serde_json::Value| r["target"] = json!("x; rm -rf /"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut record = full_record();
        edit(&mut record);
        write_records(dir.path(), &crates_io("0.9.13"), record);
        assert!(inspect(&dir.path().join("bin").join(exe_name(CLI))).is_err());
    }

    // A prerelease is not a version this updater compares.
    let pre = tempfile::tempdir().unwrap();
    write_records(pre.path(), &crates_io("0.9.13-rc.1"), full_record());
    assert!(reason(pre.path()).contains("stable"));
}

#[test]
fn the_build_environment_keeps_tool_settings_and_drops_everything_else() {
    let source = [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/me"),
        ("CARGO_HOME", "/home/me/.cargo"),
        ("HTTPS_PROXY", "http://proxy.invalid:3128"),
        ("SSL_CERT_FILE", "/etc/ssl/ca.pem"),
        // Everything below must never reach the build.
        ("CARGO_REGISTRY_TOKEN", "registry-secret"),
        ("CARGO_REGISTRIES_PRIVATE_TOKEN", "registry-secret"),
        ("DEEPSEEK_API_KEY", "account-secret"),
        ("STATE_ENCRYPTION_KEY", "state-secret"),
        ("PTA_MACHINE_TOKEN", "machine-secret"),
        ("PTA_PROFILE_DIR", "/profile"),
        ("GITHUB_TOKEN", "github-secret"),
        ("RUSTFLAGS", "-C link-arg=-fuse-ld=evil"),
        ("CARGO_TARGET_DIR", "/elsewhere"),
        ("CARGO_BUILD_TARGET", "wasm32-unknown-unknown"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let kept = build_environment(source);
    let names: Vec<&str> = kept.iter().map(|(k, _)| k.to_str().unwrap()).collect();
    for expected in ["PATH", "HOME", "CARGO_HOME", "HTTPS_PROXY", "SSL_CERT_FILE"] {
        assert!(names.contains(&expected), "{expected}");
    }
    for forbidden in [
        "CARGO_REGISTRY_TOKEN",
        "CARGO_REGISTRIES_PRIVATE_TOKEN",
        "DEEPSEEK_API_KEY",
        "STATE_ENCRYPTION_KEY",
        "PTA_MACHINE_TOKEN",
        "PTA_PROFILE_DIR",
        "GITHUB_TOKEN",
        "RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
    ] {
        assert!(!names.contains(&forbidden), "{forbidden}");
    }
    assert!(
        kept.iter()
            .all(|(_, v)| !v.to_string_lossy().contains("secret"))
    );
    assert!(kept.contains(&("CARGO_INCREMENTAL".into(), "0".into())));
}

#[test]
fn failures_get_actionable_hints() {
    let tail = vec!["error: linker `cc` not found".to_owned()];
    assert!(hint(&tail).unwrap().contains("C compiler"));
    assert!(
        hint(&["error: No space left on device".to_owned()])
            .unwrap()
            .contains("disk")
    );
    assert!(hint(&["something else".to_owned()]).is_none());
}

#[test]
fn programs_are_found_only_on_the_given_path() {
    let dir = tempfile::tempdir().unwrap();
    let name = exe_name("cargo");
    std::fs::write(dir.path().join(&name), "x").unwrap();
    assert_eq!(
        find_program("cargo", Some(dir.path().as_os_str())),
        Some(dir.path().join(&name))
    );
    assert_eq!(find_program("cargo", None), None);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(find_program("cargo", Some(empty.path().as_os_str())), None);
}

#[cfg(unix)]
mod processes {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub fn script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A pair of programs that report `version`, with Cargo's record for them.
    pub fn staged(root: &Path, version: &str, features: serde_json::Value, no_default: bool) {
        script(
            &root.join("bin").join(CLI),
            &format!("echo '{{\"ok\":true,\"data\":{{\"cli_version\":\"{version}\"}}}}'"),
        );
        script(
            &root.join("bin").join(CRATE),
            &format!("echo '{CRATE} {version}'"),
        );
        let mut record = full_record();
        record["features"] = features;
        record["no_default_features"] = json!(no_default);
        write_records(root, &crates_io(version), record);
    }

    pub fn wanted() -> Installation {
        Installation {
            root: PathBuf::from("/installed"),
            version: Version(0, 9, 13),
            features: Vec::new(),
            all_features: false,
            no_default_features: true,
            target: None,
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_matched_pair_is_verified_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    processes::staged(dir.path(), "0.9.14", json!([]), true);
    let manifest = verify(dir.path(), &v("0.9.14"), &processes::wanted(), &[], 77)
        .await
        .unwrap();
    assert_eq!(manifest.version, "0.9.14");
    assert_eq!(manifest.staged_at, 77);
    assert_eq!(manifest.binaries.len(), 2);
    assert!(
        manifest
            .binaries
            .values()
            .all(|d| d.sha256.len() == 64 && d.size > 0)
    );
    assert!(unchanged(dir.path(), &manifest));
    // A swapped executable no longer matches what was verified.
    processes::script(&dir.path().join("bin").join(CLI), "echo changed");
    assert!(!unchanged(dir.path(), &manifest));
}

#[cfg(unix)]
#[tokio::test]
async fn a_mismatched_or_incomplete_pair_is_refused() {
    let wanted = processes::wanted();
    let check = |dir: &Path, version: &'static str| {
        let dir = dir.to_path_buf();
        let wanted = wanted.clone();
        async move { verify(&dir, &v(version), &wanted, &[], 0).await }
    };

    // The record names another version.
    let other = tempfile::tempdir().unwrap();
    processes::staged(other.path(), "0.9.15", json!([]), true);
    assert!(check(other.path(), "0.9.14").await.is_err());

    // The CLI reports a different version than the record claims.
    let cli = tempfile::tempdir().unwrap();
    processes::staged(cli.path(), "0.9.14", json!([]), true);
    processes::script(
        &cli.path().join("bin").join(CLI),
        "echo '{\"ok\":true,\"data\":{\"cli_version\":\"0.9.13\"}}'",
    );
    assert!(
        check(cli.path(), "0.9.14")
            .await
            .unwrap_err()
            .to_string()
            .contains("pta")
    );

    // The application reports a different version.
    let host = tempfile::tempdir().unwrap();
    processes::staged(host.path(), "0.9.14", json!([]), true);
    processes::script(
        &host.path().join("bin").join(CRATE),
        &format!("echo '{CRATE} 0.9.13'"),
    );
    assert!(
        check(host.path(), "0.9.14")
            .await
            .unwrap_err()
            .to_string()
            .contains("application")
    );

    // One of the two executables is missing.
    let missing = tempfile::tempdir().unwrap();
    processes::staged(missing.path(), "0.9.14", json!([]), true);
    std::fs::remove_file(missing.path().join("bin").join(CRATE)).unwrap();
    assert!(check(missing.path(), "0.9.14").await.is_err());

    // A different feature choice than the installed one.
    let features = tempfile::tempdir().unwrap();
    processes::staged(features.path(), "0.9.14", json!(["gui"]), false);
    assert!(
        check(features.path(), "0.9.14")
            .await
            .unwrap_err()
            .to_string()
            .contains("feature")
    );

    // No record: not something Cargo installed.
    let bare = tempfile::tempdir().unwrap();
    assert!(check(bare.path(), "0.9.14").await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn a_build_reports_progress_and_never_sees_the_callers_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("cargo");
    let seen = dir.path().join("seen.txt");
    processes::script(
        &cargo,
        &format!(
            "env > {seen:?}\necho '   Compiling serde v1.0.0' >&2\necho '   Compiling tokio v1.0.0' >&2\nexit 0"
        ),
    );
    // The process the test runs in holds a secret; the build must not receive it.
    let secret = [
        ("PTA_TEST_ACCOUNT_SECRET", "account-secret-value"),
        ("PATH", "/usr/bin:/bin"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let env = build_environment(secret);
    let mut last = Progress::default();
    build(
        &cargo,
        &[],
        &env,
        dir.path(),
        Duration::from_secs(30),
        |p| last = p.clone(),
    )
    .await
    .unwrap();
    assert_eq!(last.crates, 2);
    assert_eq!(last.current.as_deref(), Some("tokio"));
    let environment = std::fs::read_to_string(&seen).unwrap();
    assert!(!environment.contains("account-secret-value"));
    assert!(!environment.contains("PTA_TEST_ACCOUNT_SECRET"));
    assert!(environment.contains("CARGO_TERM_COLOR=never"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_build_keeps_a_bounded_redacted_tail_and_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("cargo");
    processes::script(
        &cargo,
        "i=0; while [ $i -lt 60 ]; do echo \"progress line $i\" >&2; i=$((i+1)); done\necho 'token = sk-abcdefghijklmnop' >&2\necho 'error: linker `cc` not found' >&2\nexit 101",
    );
    let failure = build(
        &cargo,
        &[],
        &[],
        dir.path(),
        Duration::from_secs(30),
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(failure.tail.len() <= TAIL_LINES);
    assert!(failure.reason.contains("C compiler"));
    let tail = failure.tail.join("\n");
    assert!(!tail.contains("sk-abcdefghijklmnop"));
    assert!(tail.contains("linker `cc` not found"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_build_that_hangs_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("cargo");
    processes::script(&cargo, "sleep 30");
    let failure = build(
        &cargo,
        &[],
        &[],
        dir.path(),
        Duration::from_millis(300),
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(failure.reason.contains("too long"));
}

#[cfg(unix)]
#[tokio::test]
async fn cargo_prerequisites_are_probed() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("cargo");
    processes::script(&cargo, "echo 'cargo 1.95.1 (abc 2026-01-01)'");
    assert_eq!(cargo_version(&cargo, &[]).await.unwrap(), v("1.95.1"));
    processes::script(&cargo, "exit 1");
    assert!(
        cargo_version(&cargo, &[])
            .await
            .unwrap_err()
            .contains("rustup")
    );
    processes::script(&cargo, "echo garbage");
    assert!(cargo_version(&cargo, &[]).await.is_err());
}
