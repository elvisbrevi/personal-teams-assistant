//! Host without a window, for Linux servers and remote use. Same profile, contract and
//! operations as the GUI; `pta` is the only interface. Runs in the foreground until
//! `pta app quit`, SIGTERM or Ctrl-C, which stop the service and its tunnel first.
use super::*;

struct HeadlessShell(watch::Sender<bool>);

impl Shell for HeadlessShell {
    fn headless(&self) -> bool {
        true
    }
    fn show(&self) -> Result<()> {
        Err(SetupError("This host runs without a window; operate it with pta.").into())
    }
    fn hide(&self) -> Result<()> {
        self.show()
    }
    fn open_url(&self, _url: &str) -> Result<()> {
        // The URL is returned to the caller, who opens it on any device.
        Ok(())
    }
    fn exit(&self) {
        let _ = self.0.send(true);
    }
}

pub(crate) fn run(host_lock: fs::File, start_service: bool) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let config_dir = control::profile_dir()?;
        configure_credentials(&config_dir)?;
        let (quit, mut quit_requested) = watch::channel(false);
        let host = Arc::new(Host {
            state: init_state(&config_dir, &control::default_data_dir()?)?,
            shell: Box::new(HeadlessShell(quit)),
        });
        tokio::spawn(control::launch(host.clone(), host_lock)?);
        tracing::info!(event = "headless_host_ready");
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
    async fn headless_host_has_no_window_and_quit_requests_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let (quit, mut quit_requested) = watch::channel(false);
        let host = Arc::new(Host {
            state: init_state(&dir.path().join("profile"), &dir.path().join("data")).unwrap(),
            shell: Box::new(HeadlessShell(quit)),
        });
        let reply = control::dispatch(&host, control::Request::new("app_open")).await;
        assert_eq!((reply.ok, reply.exit_code), (false, 3));
        assert!(reply.message.unwrap().contains("pta"));
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
