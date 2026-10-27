//! Real host processes with private synthetic profiles; no production credentials or network.
#[cfg(unix)]
use personal_teams_assistant::config::Config;
use serde_json::{Value, json};
use std::{fs, path::Path};
#[cfg(unix)]
use std::{os::unix::fs::PermissionsExt, process::Command};

#[cfg(unix)]
struct Host(std::process::Child);
#[cfg(unix)]
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
async fn host(profile: &Path, home: &Path, bin: &Path) -> (Host, Value) {
    fs::create_dir_all(profile).unwrap();
    let mut config = Config::desktop_template().unwrap();
    config.server.data_dir = profile.join("data");
    config.server.cloudflare_tunnel = false;
    config.graph.user_id = uuid::Uuid::nil().to_string();
    config.knowledge_map = profile.join("knowledge-map.toml");
    if profile
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "profiles")
    {
        config.server.webhook_prefix = format!(
            "/webhooks/{}",
            profile.file_name().unwrap().to_str().unwrap()
        );
    }
    fs::create_dir_all(profile.join("data/repositories")).unwrap();
    fs::create_dir_all(profile.join("credentials/default")).unwrap();
    let state_key = profile.join("credentials/default/STATE_ENCRYPTION_KEY");
    if !state_key.exists() {
        fs::write(
            state_key,
            personal_teams_assistant::security::random_secret(),
        )
        .unwrap();
    }
    fs::write(
        profile.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let mut child = Host(
        Command::new(env!("CARGO_BIN_EXE_personal-teams-assistant"))
            .arg("--headless")
            .env_clear()
            .env("HOME", home)
            .env("PATH", bin)
            .env("TOKIO_WORKER_THREADS", "2")
            .env("PTA_PROFILE_DIR", profile)
            .env("DEEPSEEK_API_KEY", "synthetic-operator-environment-key")
            .env("AZURE_DEVOPS_TOKEN_FILE", home.join("operator-only-secret"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let endpoint = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Synthetic host exited"
            );
            if let Ok(bytes) = fs::read(profile.join("control.json"))
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    (child, endpoint)
}

#[cfg(unix)]
async fn call(endpoint: &Value, token: &str, method: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/control", endpoint["port"]))
        .bearer_auth(endpoint[token].as_str().unwrap())
        .json(&json!({"contract":1,"method":method,"args":args}))
        .send()
        .await
        .unwrap_or_else(|error| panic!("{method} via {token}: {error}"))
        .json()
        .await
        .unwrap()
}

#[cfg(unix)]
async fn agent(profile: &Path, home: &Path, bin: &Path, mcp: bool) -> Value {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pta"))
        .args(if mcp {
            vec!["mcp"]
        } else {
            vec!["--json", "tools", "call", "machine_settings"]
        })
        .env_clear()
        .env("HOME", home)
        .env("PATH", bin)
        .env("PTA_PROFILE_DIR", profile)
        .env("APPDATA", home.join("AppData"))
        .env(
            "SystemRoot",
            std::env::var_os("SystemRoot").unwrap_or_default(),
        )
        .env("TOKIO_WORKER_THREADS", "2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let input = if mcp {
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"machine_settings","arguments":{}}}).to_string()+"\n"
    } else {
        "{}".into()
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn bound_accounts_and_transports_cannot_forge_authority_or_start_hostile_processes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join("home/.config/dev.personalteams.assistant/web");
    fs::create_dir_all(&root).unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let marker = temp.path().join("forbidden-child-started");
    for name in ["codex", "claude", "opencode", "az", "gh", "cloudflared"] {
        let path = bin.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf launched >> '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let ids = [
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ];
    let mut accounts: Vec<_> = ids.iter().enumerate().map(|(i, id)| json!({
        "id":id,"username":format!("person{i}"),"version":format!("version-{i}"),
        "enabled":true,"current_profile":false,"authorized":true,
        "access_binding":{"issuer":"https://synthetic.cloudflareaccess.com",
            "idp_id":"11111111-1111-4111-8111-111111111111","provider_user_id":format!("{}",1001+i)}
    })).collect();
    fs::write(root.join("accounts.json"), json!(accounts).to_string()).unwrap();
    fs::write(
        root.join("settings.json"),
        json!({"owner_github_id":1001,"public_url":"https://synthetic.example", "access":{"issuer":"https://synthetic.cloudflareaccess.com","audience":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","account_id":"22222222222222222222222222222222","github_idp_id":"11111111-1111-4111-8111-111111111111"}}).to_string(),
    )
    .unwrap();
    let owner_dir = root.join("profiles").join(&ids[0]);
    let other_dir = root.join("profiles").join(&ids[1]);
    let (_owner, owner) = host(&owner_dir, &home, &bin).await;
    let (_other, other) = host(&other_dir, &home, &bin).await;
    assert_eq!(owner["account_binding"]["id"], ids[0]);
    assert_eq!(other["account_binding"]["id"], ids[1]);
    assert_eq!(owner["account_authority_support"], true);
    let cross_account = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/control", other["port"]))
        .bearer_auth(owner["token"].as_str().unwrap())
        .json(&json!({"contract":1,"method":"snapshot","args":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_account.status(), reqwest::StatusCode::FORBIDDEN);
    let browser_origin = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/control", owner["port"]))
        .bearer_auth(owner["token"].as_str().unwrap())
        .header("Origin", "https://synthetic.example")
        .json(&json!({"contract":1,"method":"snapshot","args":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(browser_origin.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(call(&owner, "token", "machine_settings", json!({})).await["ok"] == true);
    assert!(call(&owner, "browser_token", "machine_settings", json!({})).await["ok"] == true);
    assert_eq!(agent(&owner_dir, &home, &bin, false).await["ok"], true);
    assert_eq!(agent(&other_dir, &home, &bin, false).await["ok"], true);
    assert_eq!(
        agent(&owner_dir, &home, &bin, true).await["result"]["isError"],
        false
    );
    assert_eq!(
        agent(&other_dir, &home, &bin, true).await["result"]["isError"],
        false
    );
    assert_eq!(
        call(&owner, "machine_token", "app_quit", json!({})).await["code"],
        "authority_denied"
    );
    assert_eq!(
        call(&owner, "browser_token", "app_quit", json!({})).await["code"],
        "authority_denied"
    );
    for field in [
        "account_id",
        "account",
        "owner",
        "profile",
        "current_profile",
        "transport",
        "maintenance_authority",
    ] {
        assert_eq!(
            call(&owner, "token", "machine_settings", json!({field:ids[1]})).await["code"],
            "authority_denied"
        );
    }
    // An ordinary profile's own IPC bearer grants neither operator data nor process use.
    for method in [
        "clone_github_repository",
        "update_github_repository",
        "machine_connect",
        "test_providers",
        "github_cli_status",
        "azure_cli_status",
        "implementation_request_start",
    ] {
        let reply = call(
            &other,
            "token",
            method,
            json!({"path":owner_dir,"confirmed":true}),
        )
        .await;
        assert_eq!(reply["code"], "authority_denied", "{method}: {reply}");
    }
    let local = call(&other, "token", "machine_settings", json!({})).await["data"]["id"].clone();
    let blocked = call(
        &other,
        "token",
        "implementation_catalog",
        json!({"machine":local}),
    )
    .await;
    assert_eq!(blocked["ok"], false);
    assert!(
        blocked["message"]
            .as_str()
            .unwrap()
            .contains("personal trusted machine")
    );
    let blocked = call(
        &other,
        "token",
        "implementation_local",
        json!({"method":"implementation_catalog","args":{}}),
    )
    .await;
    assert_eq!(blocked["code"], "authority_denied");
    // A paired machine can poll its account, but cannot turn forwarding into server execution.
    let machine = uuid::Uuid::new_v4().to_string();
    let registration = call(
        &owner,
        "machine_token",
        "machine_register",
        json!({"id":machine,"name":"Personal machine","os":"linux"}),
    )
    .await;
    assert_eq!(registration["ok"], true);
    let credential = registration["data"]["token"].as_str().unwrap();
    let own_machine =
        call(&owner, "token", "machine_settings", json!({})).await["data"]["id"].clone();
    let attack = call(&owner, "machine_token", "machine_protocol", json!({"id":machine,"token":credential,
        "action":"call","method":"forward","args":{"machine":own_machine,"method":"implementation_catalog","args":{}}})).await;
    assert_eq!(attack["ok"], false);
    let stop = call(&owner, "machine_token", "machine_protocol", json!({"id":machine,"token":credential,
        "action":"call","method":"forward","args":{"machine":own_machine,"method":"machine_assistant","args":{"action":"stop"}}})).await;
    assert_eq!(
        stop["ok"], true,
        "Paired owner machines retain fixed Teams controls"
    );
    assert!(
        !marker.exists(),
        "A forbidden provider process actually started"
    );
    let own = call(&other, "token", "snapshot", json!({})).await;
    assert_eq!(own["ok"], true, "{own}");
    assert_ne!(
        own["data"]["credentials"]["DEEPSEEK_API_KEY"],
        "environment"
    );
    assert_eq!(own["data"]["account"]["local_execution"], false);
    assert_eq!(own["data"]["profile"], other_dir.to_str().unwrap());
    let catalog = call(&other, "token", "llm_providers", json!({})).await;
    assert_eq!(catalog["data"]["providers"][0]["transport"], "api");
    assert_eq!(catalog["data"]["providers"].as_array().unwrap().len(), 1);
    assert!(call(&other, "token", "activity_history", json!({})).await["ok"] == true);
    let note = call(
        &other,
        "token",
        "notes_save",
        json!({"note":{"title":"Private note", "text":"Only this account can read this note."}}),
    )
    .await;
    assert_eq!(note["ok"], true, "{note}");
    let mut config = own["data"]["config"].clone();
    config["llm"]["chain"] =
        json!([{"provider":"deepseek","model":"deepseek-flash","effort":"max","enabled":true}]);
    let mut forged = config.clone();
    forged["server"]["data_dir"] = json!(owner_dir.join("data"));
    assert_eq!(
        call(
            &other,
            "token",
            "save_settings",
            json!({"config":forged,"map":own["data"]["map"],"tunnel_config":""})
        )
        .await["code"],
        "authority_denied"
    );
    let saved = call(
        &other,
        "token",
        "save_settings",
        json!({"config":config,"map":own["data"]["map"],"tunnel_config":""}),
    )
    .await;
    assert_eq!(saved["ok"], true, "{saved}");
    // API preflight reaches only this account's missing credential, ignoring the operator environment.
    let preflight = call(&other, "token", "test_providers", json!({})).await;
    assert_ne!(preflight["code"], "authority_denied", "{preflight}");
    assert!(
        !preflight
            .to_string()
            .contains("synthetic-operator-environment-key")
    );
    let notes = call(&owner, "token", "notes_list", json!({})).await;
    assert!(!notes.to_string().contains("Private note"));
    // Concurrent OAuth callbacks retain each account's listener and PKCE transaction.
    let mut callbacks = Vec::new();
    for endpoint in [&owner, &other] {
        let pending = call(
            endpoint,
            "browser_token",
            "connect_microsoft",
            json!({"open":false}),
        )
        .await;
        assert_eq!(pending["code"], "authorization_pending", "{pending}");
        let auth = url::Url::parse(pending["data"]["authorization_url"].as_str().unwrap()).unwrap();
        let query: std::collections::HashMap<_, _> = auth.query_pairs().into_owned().collect();
        callbacks.push((query["redirect_uri"].clone(), query["state"].clone()));
    }
    assert_ne!(callbacks[0], callbacks[1]);
    let forged = format!(
        "{}?state={}&error=access_denied",
        callbacks[0].0, callbacks[0].1
    );
    let refused = call(
        &other,
        "browser_token",
        "finish_microsoft",
        json!({"redirect_url":forged}),
    )
    .await;
    assert_ne!(refused["code"], "authorization_pending");
    for (index, (redirect, state)) in callbacks.iter().enumerate() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let wrong = client
            .get(redirect)
            .query(&[
                ("state", callbacks[1 - index].1.as_str()),
                ("error", "access_denied"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), reqwest::StatusCode::BAD_REQUEST);
        let correct = client
            .get(redirect)
            .query(&[("state", state.as_str()), ("error", "access_denied")])
            .send()
            .await
            .unwrap();
        assert_eq!(correct.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    for endpoint in [&owner, &other] {
        assert_eq!(
            call(endpoint, "browser_token", "cancel_microsoft", json!({})).await["ok"],
            true
        );
        assert_eq!(
            call(endpoint, "token", "snapshot", json!({})).await["data"]["microsoft_connected"],
            false
        );
    }
    // A stale host must not write diagnostics/results into a rebound account's storage.
    accounts[0]["version"] = json!("rebound");
    fs::write(root.join("accounts.json"), json!(accounts).to_string()).unwrap();
    assert_eq!(
        call(&owner, "token", "machine_settings", json!({})).await["code"],
        "authority_denied"
    );
    assert!(!marker.exists());
    // The existing owner's standard profile remains native, without renamed paths.
    accounts[0]["current_profile"] = json!(true);
    fs::write(root.join("accounts.json"), json!(accounts).to_string()).unwrap();
    let standard = root.parent().unwrap();
    let (_native, native) = host(standard, &home, &bin).await;
    assert_eq!(native["account_binding"]["id"], ids[0]);
    assert_eq!(agent(standard, &home, &bin, false).await["ok"], true);
    assert_eq!(
        call(&native, "browser_token", "machine_settings", json!({})).await["ok"],
        true
    );
}

async fn admin(profile: &Path, home: &Path, args: &[&str], input: Value) -> Value {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pta"))
        .arg("--json")
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("PTA_PROFILE_DIR", profile)
        .env("APPDATA", home.join("AppData"))
        .env(
            "SystemRoot",
            std::env::var_os("SystemRoot").unwrap_or_default(),
        )
        .env("TOKIO_WORKER_THREADS", "2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if !input.is_null() {
        stdin.write_all(input.to_string().as_bytes()).await.unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().await.unwrap();
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("Missing CLI reply for {args:?}"))
}

#[tokio::test]
async fn operator_admission_binding_recovery_and_revocation_preserve_the_original_profile() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let profile = home.join("profile");
    fs::create_dir_all(&profile).unwrap();
    let access = json!({"issuer":"https://synthetic.cloudflareaccess.com","audience":"a".repeat(64),
        "account_id":"b".repeat(32),"github_idp_id":"11111111-1111-4111-8111-111111111111"});
    let configure = admin(
        &profile,
        &home,
        &["web", "configure"],
        json!({"owner_github_id":1001,
        "public_url":"https://synthetic.example","access":access}),
    )
    .await;
    assert_eq!(configure["ok"], true, "{configure}");
    let identity = |id| {
        json!({"id":id,"user_uuid":"33333333-3333-4333-8333-333333333333",
        "account_id":"b".repeat(32),"idp":{"id":"11111111-1111-4111-8111-111111111111","type":"github"}})
    };
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "add", "owner", "--current-profile"],
            Value::Null
        )
        .await["ok"],
        true
    );
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "bind", "owner"],
            identity(1001)
        )
        .await["ok"],
        true
    );
    let original = fs::read(profile.join("web/accounts.json")).unwrap();
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "add", "bob"],
            Value::Null
        )
        .await["ok"],
        true
    );
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "bind", "bob"],
            identity(1002)
        )
        .await["ok"],
        false
    );
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "authorize", "bob"],
            Value::Null
        )
        .await["ok"],
        true
    );
    let bound = admin(
        &profile,
        &home,
        &["web", "users", "bind", "bob"],
        identity(1002),
    )
    .await;
    assert_eq!(bound["ok"], true, "{bound}");
    let bob = std::path::PathBuf::from(bound["data"]["profile"].as_str().unwrap());
    let config = fs::read(bob.join("config.toml")).unwrap();
    fs::create_dir_all(bob.join("credentials/default")).unwrap();
    fs::write(
        bob.join("credentials/default/DEEPSEEK_API_KEY"),
        "synthetic-private-api-key",
    )
    .unwrap();
    let mut version = String::new();
    for action in ["disable", "enable", "revoke", "unbind", "bind"] {
        let reply = admin(
            &profile,
            &home,
            &["web", "users", action, "bob"],
            if action == "bind" {
                identity(1003)
            } else {
                Value::Null
            },
        )
        .await;
        assert_eq!(reply["ok"], true, "{action}: {reply}");
        let entries: Value =
            serde_json::from_slice(&fs::read(profile.join("web/accounts.json")).unwrap()).unwrap();
        let next = entries[1]["version"].as_str().unwrap();
        assert_ne!(next, version);
        version = next.into();
        let initial: Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(
            initial[0], entries[0],
            "Owner binding changed during {action}"
        );
        assert_eq!(fs::read(bob.join("config.toml")).unwrap(), config);
        assert_eq!(
            fs::read_to_string(bob.join("credentials/default/DEEPSEEK_API_KEY")).unwrap(),
            "synthetic-private-api-key"
        );
    }
    // The current operator profile can never be rebound to another person.
    assert_eq!(
        admin(
            &profile,
            &home,
            &["web", "users", "bind", "owner"],
            identity(1004)
        )
        .await["ok"],
        false
    );
    assert_eq!(
        admin(&bob, &home, &["web", "users", "list"], Value::Null).await["ok"],
        false
    );
}
