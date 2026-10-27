//! Real host processes with private synthetic profiles; no production credentials or network.
#![cfg(unix)]
use personal_teams_assistant::config::Config;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

struct Host(std::process::Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn host(profile: &Path, home: &Path, bin: &Path) -> (Host, Value) {
    fs::create_dir_all(profile).unwrap();
    let mut config = Config::desktop_template().unwrap();
    config.server.data_dir = profile.join("data");
    config.knowledge_map = profile.join("knowledge-map.toml");
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
        "enabled":true,"current_profile":false,
        "access_binding":{"issuer":"https://synthetic.cloudflareaccess.com",
            "idp_id":"11111111-1111-4111-8111-111111111111","provider_user_id":format!("{}",1001+i)}
    })).collect();
    fs::write(root.join("accounts.json"), json!(accounts).to_string()).unwrap();
    fs::write(
        root.join("settings.json"),
        json!({"owner_github_id":1001}).to_string(),
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
    let refused = agent(&other_dir, &home, &bin, false).await;
    assert_eq!(refused["ok"], false);
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("personal trusted machine")
    );
    assert_eq!(
        agent(&owner_dir, &home, &bin, true).await["result"]["isError"],
        false
    );
    assert_eq!(
        agent(&other_dir, &home, &bin, true).await["result"]["isError"],
        true
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
        "app_quit",
        "snapshot",
        "notes_list",
        "notes_save",
        "index_refresh",
        "activity_run",
        "machine_save",
        "machine_browse",
        "llm_providers",
        "test_providers",
        "github_cli_status",
        "azure_cli_status",
        "implementation_request_start",
        "implementation_start",
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
    assert!(!other_dir.join("data/knowledge-index.db").exists());
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
