//! The host process on every operating system: no window. Its owner's GUI is the local page in
//! the system browser (`page.rs`), and `pta`, `pta mcp` and the portal operate it through the
//! same contract. Runs until `pta app quit`, SIGTERM or Ctrl-C, which stop the service and its
//! tunnel first.
use super::*;

struct HeadlessShell {
    quit: watch::Sender<bool>,
    /// A desktop session this host can open the system browser in (`page::browser_session`).
    browser: bool,
}

impl Shell for HeadlessShell {
    fn open_url(&self, url: &str) -> Result<()> {
        if self.browser {
            page::system_browser(url)
        } else {
            // The URL is returned to the caller, who opens it on any device.
            Ok(())
        }
    }
    fn browser(&self) -> bool {
        self.browser
    }
    fn exit(&self) {
        let _ = self.quit.send(true);
    }
}

/// Runs the host. `open_page`: a person launched the application, so its page opens in the
/// system browser once the host is ready (when this computer has a browser session).
/// `restore`: the service manager started it at login, so the assistant runs again only if the
/// person left it running and this computer still holds the Teams selection and lease.
pub(crate) fn run(
    host_lock: fs::File,
    start_service: bool,
    open_page: bool,
    restore: bool,
) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let config_dir = control::profile_dir()?;
        configure_credentials(&config_dir)?;
        let (quit, mut quit_requested) = watch::channel(false);
        let host = Arc::new(Host {
            state: init_state(&config_dir, &control::default_data_dir()?)?,
            shell: Box::new(HeadlessShell {
                quit,
                browser: page::browser_session(),
            }),
        });
        consumers::install(&host);
        // Agent sessions sources read every machine of the account through this host.
        crate::sessions::install(Some(Arc::new(
            super::implementation::sessions::FleetReader::new(&host),
        )));
        tokio::spawn(cloud::run(host.clone()));
        if let Some(page) = page::launch(host.clone()) {
            tokio::spawn(page);
        }
        tokio::spawn(control::launch(host.clone(), host_lock)?);
        tracing::info!(event = "headless_host_ready");
        if open_page && host.shell.browser() && page::open(&host).is_err() {
            tracing::warn!(event = "local_page_not_opened");
        }
        if restore {
            tokio::spawn(service::restore(host.clone(), service::restore_pause()));
        }
        if start_service {
            // Keep the host alive on failure so `pta status/doctor/audit` can diagnose it.
            let reply = control::dispatch(&host, control::Request::new("start_assistant")).await;
            if !reply.ok {
                tracing::warn!(event = "headless_autostart_failed", code = %reply.code);
            }
        }
        tokio::select! {
            _ = quit_requested.changed() => {}
            _ = shutdown_signal() => {}
        }
        if let Some(task) = host.state.microsoft_login.lock().await.take() {
            task.abort();
        }
        if let Some(task) = host.state.github_finish.lock().await.take() {
            task.abort();
        }
        activity::shutdown(&host.state).await;
        stop(&host.state).await
    })
}

pub(super) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_without_a_browser_session_explains_how_to_open_its_page_and_quits() {
        let dir = tempfile::tempdir().unwrap();
        let (quit, mut quit_requested) = watch::channel(false);
        let host = Arc::new(Host {
            state: init_state(&dir.path().join("profile"), &dir.path().join("data")).unwrap(),
            shell: Box::new(HeadlessShell {
                quit,
                browser: false,
            }),
        });
        let reply = control::dispatch(&host, control::Request::new("app_open")).await;
        assert_eq!((reply.ok, reply.exit_code), (false, 3));
        assert!(reply.message.unwrap().contains("pta"));
        let logs = control::dispatch(&host, control::Request::new("logs")).await;
        assert!(logs.ok);
        assert_eq!(logs.data[0]["kind"], "app_operation_failed");
        let detail: serde_json::Value =
            serde_json::from_str(logs.data[0]["detail"].as_str().unwrap()).unwrap();
        assert_eq!(detail["operation"], "app_open");
        assert_eq!(detail["code"], "not_ready");
        assert_eq!(detail["causes"][0]["chain"][0]["type"], "precondition");
        // The activities open in the same page, so they need a browser too.
        let reply = control::dispatch(&host, control::Request::new("activity_open")).await;
        assert_eq!((reply.ok, reply.code.as_str()), (false, "not_ready"));
        let reply = control::dispatch(&host, control::Request::new("app_quit")).await;
        assert!(reply.ok);
        tokio::time::timeout(std::time::Duration::from_secs(2), quit_requested.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*quit_requested.borrow());
    }

    #[tokio::test]
    async fn redirect_from_another_machine_must_target_the_pending_loopback_login() {
        let dir = tempfile::tempdir().unwrap();
        let state = init_state(&dir.path().join("profile"), &dir.path().join("data")).unwrap();
        let pending = forward_microsoft_redirect(&state, "http://localhost:1/?code=c&state=s");
        assert!(pending.await.unwrap_err().starts_with("[not_ready]"));
        *state.microsoft_redirect.lock().await = Some("http://localhost:45678/".into());
        for pasted in [
            "http://localhost:45679/?code=c&state=s",
            "https://localhost:45678/?code=c&state=s",
            "http://evil.example:45678/?code=c&state=s",
            "http://localhost:45678/other?code=c&state=s",
            "http://localhost:45678/",
            "not a url",
        ] {
            let error = forward_microsoft_redirect(&state, pasted)
                .await
                .unwrap_err();
            assert!(error.starts_with("[invalid_input]"), "{pasted}");
        }
    }
}
