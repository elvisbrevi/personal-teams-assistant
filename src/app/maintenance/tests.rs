//! Update discovery and staging through the host's own operations, with a synthetic registry and a
//! synthetic Cargo. Nothing reaches the network, the real toolchain or the installed executables.
use super::*;
use crate::app::variables::tests::{call, host};
use authority::Transport::{Browser, Local, Machine, Window};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const RUNNING: Version = Version(0, 9, 13);

/// The tests share the process-wide environment override.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn line(version: &str, yanked: bool) -> String {
    json!({"name":core::CRATE,"vers":version,"yanked":yanked,"rust_version":"1.90"}).to_string()
}

async fn registry(lines: &[String]) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pe/rs/personal-teams-assistant"))
        .respond_with(ResponseTemplate::new(200).set_body_string(lines.join("\n")))
        .mount(&server)
        .await;
    server
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
#[cfg(not(unix))]
fn script(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    host: Arc<Host>,
    install: PathBuf,
    tools: PathBuf,
    env: Environment,
}

impl Fixture {
    fn installed(&self) -> Vec<u8> {
        [
            fs::read(self.install.join("bin/personal-teams-assistant")).unwrap(),
            fs::read(self.install.join("bin/pta")).unwrap(),
            fs::read(self.install.join(".crates2.json")).unwrap(),
        ]
        .concat()
    }
    fn arguments(&self) -> Option<String> {
        fs::read_to_string(self.tools.join("arguments.txt")).ok()
    }
    fn staged(&self) -> PathBuf {
        directory(&self.host).join("staged")
    }
    /// Cargo for the synthetic release `version`; `gate` makes it wait for a file.
    #[cfg(unix)]
    fn cargo(&self, version: &str, gate: bool) {
        let tools = self.tools.display();
        let wait = if gate {
            format!("while [ ! -f {tools}/go ]; do sleep 0.05; done\n")
        } else {
            String::new()
        };
        script(
            &self.tools.join("cargo"),
            &format!(
                r#"case "$1" in --version) echo 'cargo 1.95.0 (synthetic)'; exit 0;; esac
echo "$@" > {tools}/arguments.txt
env > {tools}/environment.txt
{wait}if [ -f {tools}/fail ]; then echo 'error: linker `cc` not found' >&2; exit 101; fi
echo '   Compiling serde v1.0.0' >&2
root=""
while [ $# -gt 0 ]; do if [ "$1" = "--root" ]; then root="$2"; fi; shift; done
mkdir -p "$root/bin"
printf '#!/bin/sh\necho "{{\\"ok\\":true,\\"data\\":{{\\"cli_version\\":\\"{version}\\"}}}}"\n' > "$root/bin/pta"
printf '#!/bin/sh\necho "personal-teams-assistant {version}"\n' > "$root/bin/personal-teams-assistant"
chmod +x "$root/bin/pta" "$root/bin/personal-teams-assistant"
printf '{{"installs":{{"personal-teams-assistant {version} (registry+https://github.com/rust-lang/crates.io-index)":{{"bins":["personal-teams-assistant","pta"],"features":[],"all_features":false,"no_default_features":true,"profile":"release","target":"x86_64-unknown-linux-gnu","rustc":"host: x86_64-unknown-linux-gnu"}}}}}}' > "$root/.crates2.json"
"#
            ),
        );
    }
}

/// An installation of 0.9.13 (headless, as `cargo install --no-default-features` leaves it).
fn fixture(registry: &MockServer) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let install = root.path().join("install");
    let tools = root.path().join("tools");
    script(&install.join("bin/personal-teams-assistant"), "echo host");
    script(&install.join("bin/pta"), "echo cli");
    fs::write(
        install.join(".crates2.json"),
        json!({"installs":{format!("{} 0.9.13 (registry+https://github.com/rust-lang/crates.io-index)", core::CRATE): {
            "bins":["personal-teams-assistant","pta"],"features":[],"all_features":false,
            "no_default_features":true,"profile":"release","target":"x86_64-unknown-linux-gnu",
            "rustc":"host: x86_64-unknown-linux-gnu"}}})
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(&tools).unwrap();
    let env = Environment {
        registry: registry.uri(),
        exe: install.join("bin/personal-teams-assistant"),
        variables: [
            (
                "PATH".to_owned(),
                format!("{}:/usr/bin:/bin", tools.display()),
            ),
            ("HOME".to_owned(), root.path().display().to_string()),
            // Account variables and credentials that must never reach a build.
            (
                "DEEPSEEK_API_KEY".to_owned(),
                "synthetic-account-secret-1".to_owned(),
            ),
            (
                "PTA_MACHINE_TOKEN".to_owned(),
                "synthetic-machine-token-2".to_owned(),
            ),
            (
                "CARGO_REGISTRY_TOKEN".to_owned(),
                "synthetic-registry-token-3".to_owned(),
            ),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .to_vec(),
        running: RUNNING,
    };
    *TEST.lock().unwrap() = Some(env.clone());
    Fixture {
        _root: root,
        host,
        install,
        tools,
        env,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        *TEST.lock().unwrap() = None;
    }
}

#[cfg(unix)]
async fn finished(f: &Fixture) -> Value {
    for _ in 0..400 {
        let status = call(&f.host, Local, "crate_update_status", json!({})).await;
        if status.data["state"] != "staging" {
            return status.data;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the build did not finish");
}

#[tokio::test]
async fn versions_are_reported_separately_and_only_a_newer_stable_release_is_offered() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[
        line("0.9.12", false),
        line("0.9.13", false),
        line("0.9.14", false),
        line("0.9.15", true),
        line("0.10.0-rc.1", false),
    ])
    .await;
    let f = fixture(&server);
    let before = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(before.data["managed"], "local");
    assert_eq!(before.data["running"], "0.9.13");
    assert_eq!(before.data["installed"], "0.9.13");
    assert_eq!(before.data["available"], Value::Null);
    assert_eq!(before.data["checked_at"], Value::Null);
    assert_eq!(before.data["install"]["supported"], true);

    let checked = call(&f.host, Window, "crate_update_check", json!({})).await;
    assert!(checked.ok, "{:?}", checked.message);
    assert_eq!(checked.data["available"], "0.9.14");
    assert_eq!(checked.data["running"], "0.9.13");
    assert_eq!(checked.data["installed"], "0.9.13");
    assert_eq!(checked.data["can_stage"], true);
    assert_eq!(checked.data["state"], "idle");
    // A check reads and remembers; it builds nothing and stages nothing.
    assert!(f.arguments().is_none());
    assert!(!f.staged().exists());
}

#[tokio::test]
async fn equal_lower_yanked_and_prerelease_versions_are_not_offered() {
    let _serial = SERIAL.lock().await;
    for lines in [
        vec![line("0.9.13", false)],
        vec![line("0.9.12", false), line("0.9.13", false)],
        vec![line("0.9.13", false), line("0.9.14", true)],
        vec![line("0.9.13", false), line("0.10.0-beta.2", false)],
    ] {
        let server = registry(&lines).await;
        let f = fixture(&server);
        let checked = call(&f.host, Local, "crate_update_check", json!({})).await;
        assert!(checked.ok, "{:?}", checked.message);
        assert_eq!(checked.data["available"], Value::Null, "{lines:?}");
        assert_eq!(checked.data["can_stage"], false);
    }
}

#[tokio::test]
async fn an_installed_but_not_yet_running_version_is_not_offered_again() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    // The executables on disk are already 0.9.14 while the host still runs 0.9.13.
    let record = fs::read_to_string(f.install.join(".crates2.json"))
        .unwrap()
        .replace("0.9.13", "0.9.14");
    fs::write(f.install.join(".crates2.json"), record).unwrap();
    let checked = call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(checked.data["installed"], "0.9.14");
    assert_eq!(checked.data["running"], "0.9.13");
    assert_eq!(checked.data["available"], Value::Null);
    assert_eq!(checked.data["restart_required"], true);
}

#[tokio::test]
async fn a_wrong_or_malformed_registry_answer_is_a_failed_check_not_an_update() {
    let _serial = SERIAL.lock().await;
    for body in [
        "<html>sorry</html>".to_owned(),
        json!({"name":"some-other-crate","vers":"9.9.9","yanked":false}).to_string(),
        String::new(),
    ] {
        let server = registry(&[body]).await;
        let f = fixture(&server);
        let checked = call(&f.host, Local, "crate_update_check", json!({})).await;
        assert!(!checked.ok);
        assert_eq!(checked.code, "dependency_or_network");
        let status = call(&f.host, Local, "crate_update_status", json!({})).await;
        assert_eq!(status.data["available"], Value::Null);
        assert!(status.data["check_error"].is_string());
    }
    // A redirect to somewhere else is a failure too.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:9/evil"),
        )
        .mount(&server)
        .await;
    let f = fixture(&server);
    assert!(
        !call(&f.host, Local, "crate_update_check", json!({}))
            .await
            .ok
    );
}

#[cfg(unix)]
#[tokio::test]
async fn staging_builds_the_matched_pair_beside_the_untouched_installation() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    f.cargo("0.9.14", true);
    let installed = f.installed();
    call(&f.host, Local, "crate_update_check", json!({})).await;

    let started = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert!(started.ok, "{:?}", started.message);
    assert_eq!(started.data["state"], "staging");
    // The old version stays installed, complete and unchanged while the new one is built.
    assert_eq!(f.installed(), installed);
    let mid = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(mid.data["state"], "staging");
    assert_eq!(mid.data["can_stage"], false);
    // Only one build at a time.
    let second = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(second.code, "state_conflict");

    fs::write(f.tools.join("go"), "").unwrap();
    let done = finished(&f).await;
    assert_eq!(done["state"], "staged", "{done}");
    assert_eq!(done["staged"]["version"], "0.9.14");
    assert_eq!(done["staged"]["no_default_features"], true);
    assert_eq!(done["staged"]["features"], json!([]));
    assert_eq!(
        done["staged"]["binaries"],
        json!(["personal-teams-assistant", "pta"])
    );
    assert_eq!(done["stage"]["progress"]["crates"], 1);
    assert_eq!(done["installed"], "0.9.13");
    assert_eq!(done["available"], "0.9.14");
    assert!(f.staged().join("0.9.14/root/bin/pta").is_file());
    assert!(f.staged().join("0.9.14/manifest.json").is_file());
    assert!(!f.staged().join("0.9.14.partial").exists());
    // Still nothing replaced.
    assert_eq!(f.installed(), installed);

    // Cargo got a locked, exact, headless build into the staging root and nothing else.
    let arguments = f.arguments().unwrap();
    let words: Vec<&str> = arguments.split_whitespace().collect();
    assert_eq!(&words[..4], ["install", core::CRATE, "--version", "0.9.14"]);
    assert!(words.contains(&"--locked"));
    assert!(words.contains(&"--no-default-features"));
    assert!(!words.contains(&"--features"));
    for forbidden in ["--registry", "--index", "--git", "--path", "--force"] {
        assert!(!words.contains(&forbidden), "{forbidden}");
    }
    let root = words[words.iter().position(|w| *w == "--root").unwrap() + 1];
    assert!(Path::new(root).starts_with(directory(&f.host).join("staged")));
}

#[cfg(unix)]
#[tokio::test]
async fn the_build_never_sees_account_variables_or_credentials() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    f.cargo("0.9.14", false);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    let started = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert!(started.ok, "{:?}", started.message);
    let done = finished(&f).await;
    assert_eq!(done["state"], "staged", "{done}");
    let seen = fs::read_to_string(f.tools.join("environment.txt")).unwrap();
    for secret in [
        "synthetic-account-secret-1",
        "synthetic-machine-token-2",
        "synthetic-registry-token-3",
        "DEEPSEEK_API_KEY",
        "PTA_MACHINE_TOKEN",
        "CARGO_REGISTRY_TOKEN",
    ] {
        assert!(!seen.contains(secret), "{secret} reached the build");
    }
    assert!(seen.contains("PATH="));
    // Neither the status nor the saved state carries a value.
    let everything = serde_json::to_string(&done).unwrap()
        + &fs::read_to_string(directory(&f.host).join("status.json")).unwrap();
    assert!(!everything.contains("synthetic-"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_forged_version_source_or_argument_stages_nothing() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    f.cargo("0.9.14", false);
    // A version that is not the registry's newest.
    for version in ["0.9.15", "0.9.13", "1.0.0"] {
        let reply = call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":version}),
        )
        .await;
        assert!(!reply.ok, "{version}");
        assert_eq!(reply.code, "state_conflict", "{version}");
    }
    // Malformed versions and arguments that try to choose a registry, crate, URL, path or command.
    for args in [
        json!({"version":"latest"}),
        json!({"version":"0.9.14-rc.1"}),
        json!({"version":"0.9.14","registry":"http://127.0.0.1:1"}),
        json!({"version":"0.9.14","crate":"other"}),
        json!({"version":"0.9.14","url":"https://example.invalid"}),
        json!({"version":"0.9.14","root":"/tmp/x"}),
        json!({"version":"0.9.14","command":"sh -c id"}),
        json!({"version":"0.9.14","features":["x"]}),
        json!({}),
        json!({"version":14}),
    ] {
        let reply = call(&f.host, Local, "crate_update_stage", args.clone()).await;
        assert!(!reply.ok, "{args}");
        assert_eq!(reply.code, "invalid_input", "{args}");
    }
    // Authority cannot be supplied as an argument.
    for key in [
        "account",
        "profile",
        "owner",
        "transport",
        "maintenance_authority",
        "authority",
    ] {
        let reply = call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14", key: "x"}),
        )
        .await;
        assert_eq!(reply.code, "authority_denied", "{key}");
    }
    assert!(f.arguments().is_none(), "cargo must not have run");
    assert!(!f.staged().exists());
}

#[tokio::test]
async fn only_the_local_owner_can_check_or_stage_a_portal_session_machine_or_other_account_cannot()
{
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    for (transport, name) in [(Browser, "browser"), (Machine, "machine")] {
        for (op, args) in [
            ("crate_update_check", json!({})),
            ("crate_update_stage", json!({"version":"0.9.14"})),
            ("crate_update_dismiss", json!({"version":"0.9.14"})),
        ] {
            let reply = call(&f.host, transport, op, args).await;
            assert!(!reply.ok, "{name} {op}");
            assert_eq!(reply.code, "authority_denied", "{name} {op}");
        }
    }
    // A portal session sees only that the server is managed. (The test profile has no portal
    // account, so reach the operation itself instead of the browser allowlist in front of it.)
    let mut request = control::Request::new("crate_update_status");
    request.args = json!({});
    let seen = authority::within(
        authority::Invocation {
            scope: f.host.state.authority.clone(),
            transport: Browser,
        },
        operate(&f.host, &request),
    )
    .await;
    assert!(seen.ok, "{:?}", seen.message);
    assert_eq!(seen.data["managed"], "server");
    assert_eq!(seen.data["can_stage"], false);
    assert!(seen.data["message"].as_str().unwrap().contains("operator"));
    for hidden in [
        "installed",
        "available",
        "install",
        "root",
        "tail",
        "staged",
    ] {
        assert!(seen.data.get(hidden).is_none(), "{hidden}");
    }
    // A machine credential never reaches it.
    assert!(
        !call(&f.host, Machine, "crate_update_status", json!({}))
            .await
            .ok
    );
    assert!(f.arguments().is_none());
    assert!(!f.staged().exists());

    // The decision is the same for every kind of caller of a shared or non-owner profile.
    let scope = |owner: bool, shared: bool| authority::Scope {
        profile: PathBuf::from("/profile"),
        account: None,
        owner,
        shared,
        admitted: true,
    };
    assert!(is_local_owner(&scope(true, false), Local));
    assert!(is_local_owner(&scope(true, false), Window));
    assert!(!is_local_owner(&scope(true, false), Browser));
    assert!(!is_local_owner(&scope(true, false), Machine));
    assert!(!is_local_owner(&scope(false, true), Local));
    assert!(!is_local_owner(&scope(false, true), Window));
    assert!(!is_local_owner(&scope(true, true), Local));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_build_reports_a_fix_and_leaves_everything_as_it_was() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    f.cargo("0.9.14", false);
    fs::write(f.tools.join("fail"), "").unwrap();
    let installed = f.installed();
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert!(
        call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"})
        )
        .await
        .ok
    );
    let done = finished(&f).await;
    assert_eq!(done["state"], "failed");
    let error = done["stage"]["error"].as_str().unwrap();
    assert!(error.contains("C compiler"), "{error}");
    assert!(done["stage"]["tail"].as_array().unwrap().len() <= core::TAIL_LINES);
    assert_eq!(f.installed(), installed);
    assert!(!f.staged().join("0.9.14.partial").exists());
    assert!(!f.staged().join("0.9.14").exists());
    // The update stays on offer, and fixing the problem lets a retry succeed.
    assert_eq!(done["available"], "0.9.14");
    assert_eq!(done["can_stage"], true);
    fs::remove_file(f.tools.join("fail")).unwrap();
    assert!(
        call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"})
        )
        .await
        .ok
    );
    assert_eq!(finished(&f).await["state"], "staged");
}

#[cfg(unix)]
#[tokio::test]
async fn a_pair_that_reports_another_version_is_not_staged() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    // Cargo "builds" a different version than the one asked for.
    f.cargo("0.9.99", false);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert!(
        call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"})
        )
        .await
        .ok
    );
    let done = finished(&f).await;
    assert_eq!(done["state"], "failed");
    assert!(done["stage"]["error"].as_str().unwrap().contains("0.9.14"));
    assert!(!f.staged().join("0.9.14").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn missing_or_too_old_tooling_is_explained_before_anything_is_built() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[
        json!({"name":core::CRATE,"vers":"0.9.14","yanked":false,"rust_version":"1.99"})
            .to_string(),
    ])
    .await;
    let f = fixture(&server);
    call(&f.host, Local, "crate_update_check", json!({})).await;

    let missing = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(missing.code, "not_ready");
    assert!(missing.message.unwrap().contains("rustup"));

    f.cargo("0.9.14", false);
    let old = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(old.code, "not_ready");
    let message = old.message.unwrap();
    assert!(
        message.contains("1.99") && message.contains("1.95.0"),
        "{message}"
    );
    assert!(f.arguments().is_none());
    // The refusal does not leave the profile marked as building.
    assert_eq!(
        call(&f.host, Local, "crate_update_status", json!({}))
            .await
            .data["state"],
        "idle"
    );
}

#[tokio::test]
async fn custom_installations_get_a_diagnostic_not_an_update() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let mut f = fixture(&server);
    // A development build: not in a Cargo `bin` directory.
    f.env.exe = f.install.join("target/debug/personal-teams-assistant");
    *TEST.lock().unwrap() = Some(f.env.clone());
    let status = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(status.data["install"]["supported"], false);
    assert!(
        status.data["install"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("development build")
    );
    assert_eq!(status.data["installed"], Value::Null);
    let checked = call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(checked.data["available"], "0.9.14");
    assert_eq!(checked.data["can_stage"], false);
    let staged = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(staged.code, "not_ready");

    // A path install.
    f.env.exe = f.install.join("bin/personal-teams-assistant");
    *TEST.lock().unwrap() = Some(f.env.clone());
    let record = fs::read_to_string(f.install.join(".crates2.json"))
        .unwrap()
        .replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            "path+file:///src/x#0.9.13",
        );
    fs::write(f.install.join(".crates2.json"), record).unwrap();
    let staged = call(
        &f.host,
        Local,
        "crate_update_stage",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(staged.code, "not_ready");
    assert!(staged.message.unwrap().contains("crates.io"));
}

#[tokio::test]
async fn dismissing_hides_that_release_only() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    let checked = call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(checked.data["dismissed"], false);
    let dismissed = call(
        &f.host,
        Local,
        "crate_update_dismiss",
        json!({"version":"0.9.14"}),
    )
    .await;
    assert_eq!(dismissed.data["dismissed"], true);
    // It can still be prepared explicitly.
    assert_eq!(dismissed.data["can_stage"], true);
    assert!(
        !call(
            &f.host,
            Local,
            "crate_update_dismiss",
            json!({"version":"soon"})
        )
        .await
        .ok
    );
    // A newer release shows the notice again.
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string([line("0.9.14", false), line("0.9.15", false)].join("\n")),
        )
        .mount(&server)
        .await;
    let later = call(&f.host, Local, "crate_update_check", json!({})).await;
    assert_eq!(later.data["available"], "0.9.15");
    assert_eq!(later.data["dismissed"], false);
}

#[tokio::test]
async fn a_build_interrupted_by_a_restart_is_reported_as_failed() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    save(&directory(&f.host), |saved| {
        saved.stage = Some(Stage {
            version: "0.9.14".into(),
            state: "staging".into(),
            phase: "building".into(),
            started_at: 1,
            finished_at: None,
            crates: 40,
            current: Some("tokio".into()),
            error: None,
            tail: Vec::new(),
        })
    })
    .unwrap();
    let status = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(status.data["state"], "failed");
    assert!(
        status.data["stage"]["error"]
            .as_str()
            .unwrap()
            .contains("stopped")
    );
    assert_eq!(status.data["can_stage"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn a_staged_pair_that_changed_or_was_installed_is_no_longer_reported_staged() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    f.cargo("0.9.14", false);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    assert!(
        call(
            &f.host,
            Local,
            "crate_update_stage",
            json!({"version":"0.9.14"})
        )
        .await
        .ok
    );
    assert_eq!(finished(&f).await["state"], "staged");

    // A file replaced after verification is not a verified pair.
    let cli = f.staged().join("0.9.14/root/bin/pta");
    let original = fs::read(&cli).unwrap();
    fs::write(&cli, "x").unwrap();
    assert_ne!(
        call(&f.host, Local, "crate_update_status", json!({}))
            .await
            .data["state"],
        "staged"
    );
    fs::write(&cli, original).unwrap();
    assert_eq!(
        call(&f.host, Local, "crate_update_status", json!({}))
            .await
            .data["state"],
        "staged"
    );

    // Once that version is installed there is nothing left to stage.
    let record = fs::read_to_string(f.install.join(".crates2.json"))
        .unwrap()
        .replace("0.9.13", "0.9.14");
    fs::write(f.install.join(".crates2.json"), record).unwrap();
    let after = call(&f.host, Local, "crate_update_status", json!({})).await;
    assert_eq!(after.data["state"], "idle");
    assert_eq!(after.data["available"], Value::Null);
}

#[tokio::test]
async fn saved_state_holds_no_installation_path_or_secret() {
    let _serial = SERIAL.lock().await;
    let server = registry(&[line("0.9.14", false)]).await;
    let f = fixture(&server);
    call(&f.host, Local, "crate_update_check", json!({})).await;
    let saved = fs::read_to_string(directory(&f.host).join("status.json")).unwrap();
    assert!(saved.contains("0.9.14"));
    assert!(!saved.contains(&f.install.display().to_string()));
}
