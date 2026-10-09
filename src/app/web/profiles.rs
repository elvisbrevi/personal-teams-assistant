use super::*;
use serde_json::Value;

pub(super) struct Worker {
    pub child: tokio::process::Child,
    pub profile: PathBuf,
}
impl Worker {
    pub(super) async fn shutdown(mut self) {
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(25), async {
            let _ = control::client_at(control::Request::new("app_quit"), &self.profile).await;
            let _ = self.child.wait().await;
        })
        .await;
        if stopped.is_err() {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
    }
}

pub(super) fn profile_dir(root: &Path, account: &Account) -> PathBuf {
    if account.current_profile {
        root.parent().unwrap().to_path_buf()
    } else {
        root.join("profiles").join(&account.id)
    }
}

pub(super) fn create_profile(root: &Path, account: &Account) -> Result<()> {
    let dir = profile_dir(root, account);
    let _ = init_state(&dir, &dir.join("data"))?;
    security::private_dir(&dir.join("data/repositories"))?;
    security::private_dir(&dir.join("home"))?;
    let mut config = read_config(&dir.join("config.toml"))?;
    // Providers' CLI login is private to this profile; API credentials are private too.
    config.llm.chain = vec![crate::config::LlmChoice {
        provider: "deepseek".into(),
        model: "deepseek-flash".into(),
        effort: "max".into(),
        enabled: true,
    }];
    config.server.webhook_prefix = format!("/webhooks/{}", account.id);
    let portal = settings(root)?;
    if portal.https() {
        config.server.public_url = portal.public_url;
    }
    config.server.bind = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .to_string();
    write_private(&dir.join("config.toml"), &toml::to_string_pretty(&config)?)
}

pub(super) fn worker_command(binary: &Path, dir: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(binary);
    command.arg("--headless").env_clear();
    // Keep networking and native loader setup, but never the operator's app secrets or
    // provider login variables. Each child owns its credential store and provider home.
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "LANG",
        "LC_ALL",
        "TZ",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let home = dir.join("home");
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env("PTA_PROFILE_DIR", dir)
        .env("PTA_HEADLESS", "1")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

impl Portal {
    pub(super) async fn worker(&self, account: &Account) -> Result<PathBuf> {
        let dir = profile_dir(&self.root, account);
        let mut workers = self.workers.lock().await;
        if control::existing_host_at(&dir).is_ok() {
            control::require_web_access_host_at(&dir)?;
            if !account.current_profile {
                let mut config = read_config(&dir.join("config.toml"))?;
                let public_url = if self.settings.https() {
                    self.settings.public_url.clone()
                } else {
                    "https://assistant.example.com".into()
                };
                if config.server.public_url != public_url || config.server.cloudflare_tunnel {
                    let previous =
                        control::client_at(control::Request::new("snapshot"), &dir).await?;
                    config.server.public_url = public_url;
                    config.server.cloudflare_tunnel = false;
                    let mut request = control::Request::new("save_settings");
                    request.revision = previous.revision;
                    request.args =
                        json!({"config":config,"map":previous.data["map"],"tunnel_config":""});
                    ensure!(
                        control::client_at(request, &dir).await?.ok,
                        "profile hosting update did not complete"
                    );
                }
            }
            return Ok(dir);
        }
        workers.remove(&account.id);
        let mut command = if account.current_profile {
            let mut command = tokio::process::Command::new(&self.binary);
            command
                .arg("--headless")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            command
        } else {
            let mut config = read_config(&dir.join("config.toml"))?;
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            config.server.bind = listener.local_addr()?.to_string();
            config.server.public_url = if self.settings.https() {
                self.settings.public_url.clone()
            } else {
                "https://assistant.example.com".into()
            };
            config.server.cloudflare_tunnel = false;
            config.server.webhook_prefix = format!("/webhooks/{}", account.id);
            write_private(&dir.join("config.toml"), &toml::to_string_pretty(&config)?)?;
            drop(listener);
            worker_command(&self.binary, &dir)
        };
        let child = command.spawn()?;
        workers.insert(
            account.id.clone(),
            Worker {
                child,
                profile: dir.clone(),
            },
        );
        for _ in 0..100 {
            if control::existing_host_at(&dir).is_ok() {
                return Ok(dir);
            }
            if workers
                .get_mut(&account.id)
                .unwrap()
                .child
                .try_wait()?
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        workers.remove(&account.id);
        anyhow::bail!("web profile host did not become ready")
    }

    pub(super) fn resume(&self, account: &Account, running: bool) -> Result<()> {
        security::private_dir(&self.root.join("resume"))?;
        write_private(
            &self.root.join("resume").join(&account.id),
            if running { "1" } else { "0" },
        )
    }

    pub(super) async fn reconcile(&self) {
        let Ok(entries) = accounts(&self.root) else {
            return;
        };
        for entry in &entries {
            if !entry.enabled {
                let worker = self.workers.lock().await.remove(&entry.id);
                if let Some(worker) = worker {
                    worker.shutdown().await;
                }
            }
        }
    }

    /// A different profile must never answer as the same Teams account, even with a
    /// different Entra client. Called under the portal's identity mutex.
    pub(super) fn unique_identity(&self, account: &Account, config: &Config) -> Result<()> {
        if config.graph.user_id == uuid::Uuid::nil().to_string() {
            return Ok(());
        }
        let mut dirs: Vec<_> = accounts(&self.root)?
            .into_iter()
            .filter(|e| e.id != account.id)
            .map(|e| profile_dir(&self.root, &e))
            .collect();
        let desktop = self.root.parent().unwrap().to_path_buf();
        if desktop != profile_dir(&self.root, account) {
            dirs.push(desktop);
        }
        for dir in dirs {
            if let Ok(other) = read_config(&dir.join("config.toml")) {
                ensure!(
                    !other
                        .graph
                        .tenant_id
                        .eq_ignore_ascii_case(&config.graph.tenant_id)
                        || !other
                            .graph
                            .user_id
                            .eq_ignore_ascii_case(&config.graph.user_id),
                    "This Teams account already belongs to another profile. Use a different account."
                );
            }
        }
        Ok(())
    }

    pub(super) fn check_request(
        &self,
        account: &Account,
        request: &control::Request,
    ) -> Result<()> {
        const METHODS: &[&str] = &[
            "snapshot",
            "chat",
            "github_repositories",
            "self_chat_status",
            "llm_providers",
            "audit",
            "activity_status",
            "activity_history",
            "activity_pending",
            "activity_run",
            "activity_resolve",
            "activity_open",
            "start_assistant",
            "stop_assistant",
            "restart",
            "dismiss_restart_offer",
            "validate",
            "save_settings",
            "import_existing",
            "set_credential",
            "delete_credential",
            "begin_github_login",
            "open_github_login",
            "finish_github_login",
            "cancel_github",
            "disconnect_github",
            "clone_github_repository",
            "update_github_repository",
            "connect_microsoft",
            "finish_microsoft",
            "cancel_microsoft",
            "microsoft_logout",
            "self_chat_enable",
            "self_chat_disable",
            "test_self_chat",
            "test_providers",
        ];
        ensure!(
            METHODS.contains(&request.method.as_str()),
            "Operation is unavailable in the web interface."
        );
        let dir = profile_dir(&self.root, account);
        let saved = read_config(&dir.join("config.toml"))?;
        if matches!(request.method.as_str(), "save_settings" | "validate") {
            let config: Config = serde_json::from_value(request.args["config"].clone())?;
            let map: KnowledgeMap = serde_json::from_value(request.args["map"].clone())?;
            self.check_paths(account, &saved, &config, &map)?;
            ensure!(
                request
                    .args
                    .get("tunnel_config")
                    .or_else(|| request.args.get("tunnelConfig"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    == fs::read_to_string(dir.join("cloudflared-path.txt"))
                        .unwrap_or_default()
                        .trim(),
                "The portal operator manages tunnel configuration."
            );
            self.unique_identity(account, &config)?;
        }
        if request.method == "import_existing" {
            let path = PathBuf::from(
                request.args["path"]
                    .as_str()
                    .context("invalid import path")?,
            );
            ensure!(
                confined(&path, &dir),
                "Import files must be inside your profile directory."
            );
            let config = read_config(&path)?;
            ensure!(
                config.knowledge_map == saved.knowledge_map,
                "The portal operator manages profile and data paths."
            );
            let map = KnowledgeMap::load(&config.knowledge_map)?;
            self.check_paths(account, &saved, &config, &map)?;
            self.unique_identity(account, &config)?;
        }
        Ok(())
    }

    fn check_paths(
        &self,
        account: &Account,
        saved: &Config,
        config: &Config,
        map: &KnowledgeMap,
    ) -> Result<()> {
        ensure!(
            config.server.data_dir == saved.server.data_dir
                && config.knowledge_map == saved.knowledge_map,
            "The portal operator manages profile and data paths."
        );
        ensure!(
            config.server.bind == saved.server.bind
                && config.server.public_url == saved.server.public_url
                && config.server.webhook_prefix == saved.server.webhook_prefix
                && config.server.cloudflare_tunnel == saved.server.cloudflare_tunnel,
            "The portal operator manages the public URL, listener and tunnel."
        );
        let old_map = KnowledgeMap::load(&saved.knowledge_map)?;
        for (alias, path) in &map.repositories {
            let existing = account.current_profile && old_map.repositories.get(alias) == Some(path);
            ensure!(
                existing || confined(path, &saved.server.data_dir.join("repositories")),
                "Repository paths must be inside your profile's data/repositories directory."
            );
        }
        Ok(())
    }
}

fn confined(path: &Path, root: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && path
            .canonicalize()
            .ok()
            .zip(root.canonicalize().ok())
            .is_some_and(|(p, r)| p.starts_with(r))
}
