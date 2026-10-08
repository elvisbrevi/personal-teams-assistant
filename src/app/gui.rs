//! Tauri shell: tray/menu-bar icon and the settings/chat window. Operations live in the host.
use super::*;
use tauri::{
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_opener::OpenerExt;

struct TauriShell(tauri::AppHandle);

impl Shell for TauriShell {
    fn headless(&self) -> bool {
        false
    }
    fn show(&self) -> Result<()> {
        show_main(&self.0);
        Ok(())
    }
    fn hide(&self) -> Result<()> {
        if let Some(window) = self.0.get_webview_window("main") {
            window.hide()?;
        }
        Ok(())
    }
    fn open_url(&self, url: &str) -> Result<()> {
        self.0.opener().open_url(url, None::<&str>)?;
        Ok(())
    }
    fn exit(&self) {
        self.0.exit(0);
    }
    fn open_activity(&self, entry: Option<&str>) -> Result<()> {
        open_activity(&self.0, entry)
    }
    fn notify_activity(&self, title: &str, body: &str, entry: Option<&str>) -> Result<()> {
        let _ = self.0.emit("activity-registration", ());
        let app = self.0.clone();
        let title = title.to_owned();
        let body = body.to_owned();
        let entry = entry.map(str::to_owned);
        std::thread::spawn(move || {
            let result = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .appname("Personal Teams Assistant")
                .action("default", "Review activities")
                .timeout(15_000)
                .show();
            if let Ok(handle) = result {
                handle.wait_for_action(|action| {
                    if action != "__closed" {
                        let target = app.clone();
                        let _ = app.run_on_main_thread(move || {
                            let _ = open_activity(&target, entry.as_deref());
                        });
                    }
                });
            } else {
                tracing::warn!(event = "activity_notification_unavailable");
            }
        });
        Ok(())
    }
}

fn open_activity(app: &tauri::AppHandle, entry: Option<&str>) -> Result<()> {
    if let Some(window) = app.get_webview_window("activity-registration") {
        window.emit("activity-open", serde_json::json!({"id":entry}))?;
        window.show()?;
        window.set_focus()?;
    } else {
        let query = entry.unwrap_or("pending");
        WebviewWindowBuilder::new(
            app,
            "activity-registration",
            WebviewUrl::App(format!("index.html?activity={query}").into()),
        )
        .title("Activity registration — Personal Teams Assistant")
        .inner_size(1000., 760.)
        .min_inner_size(640., 520.)
        .build()?;
    }
    Ok(())
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn host(app: &tauri::AppHandle) -> Arc<Host> {
    app.state::<Arc<Host>>().inner().clone()
}

/// The WebView reaches the host only through this command, with the same contract as `pta`.
#[tauri::command]
async fn command(app: tauri::AppHandle, request: control::Request) -> control::Reply {
    control::dispatch(&host(&app), request).await
}

fn spawn_operation(app: &tauri::AppHandle, method: &'static str) {
    let host = host(app);
    tauri::async_runtime::spawn(async move {
        let _ = control::dispatch(&host, control::Request::new(method)).await;
    });
}

fn start_host(app: &tauri::App, host_lock: fs::File) -> Result<Arc<Host>> {
    let config_dir = control::profile_dir()?;
    ensure!(
        app.path().app_config_dir()? == config_dir,
        "profile path differs between GUI and CLI"
    );
    configure_credentials(&config_dir)?;
    let host = Arc::new(Host {
        state: init_state(&config_dir, &app.path().app_data_dir()?)?,
        shell: Box::new(TauriShell(app.handle().clone())),
    });
    tauri::async_runtime::spawn(control::launch(host.clone(), host_lock)?);
    Ok(host)
}

/// Window configuration and UI assets, embedded from `desktop/` (generated once per binary).
fn context() -> tauri::Context<tauri::Wry> {
    tauri::generate_context!("desktop/tauri.conf.json")
}

pub(crate) fn run(host_lock: fs::File, restart_offer: bool) {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![command])
        .setup(move |app| {
            let host = start_host(app, host_lock).map_err(Box::<dyn std::error::Error>::from)?;
            // Replaced an outdated host whose assistant was running: the window asks.
            host.state
                .restart_offer
                .store(restart_offer, std::sync::atomic::Ordering::Relaxed);
            app.manage(host);
            let open =
                MenuItem::with_id(app, "open", "Open settings and chat", true, None::<&str>)?;
            let start_item =
                MenuItem::with_id(app, "start", "Start assistant", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "Stop assistant", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &start_item, &stop_item, &quit])?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .context("missing app icon")?
                        .clone(),
                )
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "start" => spawn_operation(app, "start_assistant"),
                    "stop" => spawn_operation(app, "stop_assistant"),
                    "quit" => spawn_operation(app, "app_quit"),
                    _ => {}
                })
                .build(app)?;
            if !std::env::args().any(|a| a == "--host") {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(context())
        .expect("cannot build desktop application")
        .run(|app, event| match event {
            // Native macOS Quit/termination may bypass ExitRequested and deliver Exit directly.
            // Await cleanup on the main event thread while Tokio keeps servicing background tasks.
            tauri::RunEvent::Exit => {
                tauri::async_runtime::block_on(activity::shutdown(&host(app).state));
                let _ = tauri::async_runtime::block_on(stop(&host(app).state));
            }
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                api.prevent_exit();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = control::dispatch(&host(&app), control::Request::new("stop_assistant"))
                        .await;
                    app.exit(0);
                });
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show_main(app),
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_assets_are_embedded_from_the_desktop_directory() {
        let context = super::context();
        for asset in ["index.html", "app.js", "style.css"] {
            assert!(
                context.assets().get(&asset.into()).is_some(),
                "{asset} is not embedded"
            );
        }
        assert_eq!(context.config().identifier, "dev.personalteams.assistant");
    }
}
