//! Tauri shell: tray/menu-bar icon and the settings/chat window. Operations live in the host.
use super::*;
use tauri::{
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    image::Image,
    menu::{Menu, MenuItem},
    tray::{TrayIcon, TrayIconBuilder},
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

/// Status dot colors drawn on the tray icon (legible on light and dark menu bars).
const RUNNING_DOT: [u8; 3] = [0x34, 0xc7, 0x59];
const BUSY_DOT: [u8; 3] = [0xff, 0x9f, 0x0a];

/// The tray icon and its single start/stop item, drawn from the assistant's phase.
struct Tray {
    icon: TrayIcon,
    toggle: MenuItem<tauri::Wry>,
    stopped: Image<'static>,
    busy: Image<'static>,
    running: Image<'static>,
}

impl Tray {
    fn render(&self, phase: AssistantPhase) {
        let (text, image, status) = match phase {
            AssistantPhase::Stopped => ("Start assistant", &self.stopped, "Stopped"),
            AssistantPhase::Starting => ("Starting assistant…", &self.busy, "Starting…"),
            AssistantPhase::Running => ("Stop assistant", &self.running, "Running"),
            AssistantPhase::Stopping => ("Stopping assistant…", &self.busy, "Stopping…"),
        };
        let _ = self.toggle.set_text(text);
        let _ = self.toggle.set_enabled(matches!(
            phase,
            AssistantPhase::Stopped | AssistantPhase::Running
        ));
        let _ = self.icon.set_icon(Some(image.clone()));
        let _ = self
            .icon
            .set_tooltip(Some(format!("Personal Teams Assistant · {status}")));
    }
}

/// `base` with a status dot in its lower-right corner, cut out from the icon so it stays
/// visible over the icon's own colors.
fn with_dot(base: &Image<'_>, color: [u8; 3]) -> Image<'static> {
    let (width, height) = (base.width(), base.height());
    let size = width.min(height) as f32;
    let radius = size * 0.18;
    let gap = size * 0.06;
    let (cx, cy) = (width as f32 - radius, height as f32 - radius);
    let mut rgba = base.rgba().to_vec();
    for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (index as u32 % width) as f32 + 0.5 - cx;
        let y = (index as u32 / width) as f32 + 0.5 - cy;
        let distance = (x * x + y * y).sqrt();
        // Coverage of the cut-out ring and of the dot, antialiased over one pixel.
        let cut = (radius + gap + 0.5 - distance).clamp(0., 1.);
        let dot = (radius + 0.5 - distance).clamp(0., 1.);
        let below = f32::from(pixel[3]) / 255. * (1. - cut);
        let alpha = dot + below * (1. - dot);
        for (value, tint) in pixel[..3].iter_mut().zip(color) {
            *value = if alpha > 0. {
                ((f32::from(tint) * dot + f32::from(*value) * below * (1. - dot)) / alpha).round()
                    as u8
            } else {
                0
            };
        }
        pixel[3] = (alpha * 255.).round() as u8;
    }
    Image::new_owned(rgba, width, height)
}

/// Draws the current phase on the main thread without waiting for it: Tauri's menu and tray
/// calls from another thread block until the main thread runs them, which may be busy (e.g.
/// stopping the assistant on exit). Reading the phase there keeps the latest draw current.
fn show_phase(app: &tauri::AppHandle, failure: Option<String>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        handle
            .state::<Tray>()
            .render(*host(&handle).state.phase.borrow());
        if let Some(message) = failure {
            report_failure(&handle, &message);
        }
    });
}

/// Keeps the tray and the windows in step with the assistant's phase, whoever changes it:
/// the tray, the window, `pta` or the service ending on its own.
fn follow_phase(app: &tauri::AppHandle) {
    let mut phase = host(app).state.phase.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let current = *phase.borrow_and_update();
            show_phase(&app, None);
            let _ = app.emit("assistant-phase", current);
            if phase.changed().await.is_err() {
                break;
            }
        }
    });
}

/// The tray's single item starts a stopped assistant and stops a running one.
fn toggle_assistant(app: &tauri::AppHandle) {
    let phase = *host(app).state.phase.borrow();
    let (method, pending, failure) = match phase {
        AssistantPhase::Stopped => (
            "start_assistant",
            AssistantPhase::Starting,
            "The assistant did not start",
        ),
        AssistantPhase::Running => (
            "stop_assistant",
            AssistantPhase::Stopping,
            "The assistant did not stop",
        ),
        AssistantPhase::Starting | AssistantPhase::Stopping => return,
    };
    // Immediate feedback (this runs on the main thread), also while another operation holds
    // the dispatcher.
    app.state::<Tray>().render(pending);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let reply = control::dispatch(&host(&app), control::Request::new(method)).await;
        let failure =
            (!reply.ok).then(|| format!("{failure}: {}", reply.message.unwrap_or(reply.code)));
        show_phase(&app, failure);
    });
}

/// A failed tray operation is shown in the window when it is open, otherwise as a native
/// notification. The message is the dispatcher's sanitized one.
fn report_failure(app: &tauri::AppHandle, message: &str) {
    if app
        .get_webview_window("main")
        .is_some_and(|window| window.is_visible().unwrap_or(false))
    {
        let _ = app.emit_to("main", "assistant-error", message);
        return;
    }
    let body = message.to_owned();
    std::thread::spawn(move || {
        let shown = notify_rust::Notification::new()
            .summary("Personal Teams Assistant")
            .body(&body)
            .appname("Personal Teams Assistant")
            .timeout(15_000)
            .show();
        if shown.is_err() {
            tracing::warn!(event = "assistant_notification_unavailable");
        }
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
            let toggle =
                MenuItem::with_id(app, "assistant", "Start assistant", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &toggle, &quit])?;
            let stopped = app
                .default_window_icon()
                .context("missing app icon")?
                .clone()
                .to_owned();
            let icon = TrayIconBuilder::new()
                .icon(stopped.clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "assistant" => toggle_assistant(app),
                    "quit" => spawn_operation(app, "app_quit"),
                    _ => {}
                })
                .build(app)?;
            app.manage(Tray {
                icon,
                toggle,
                busy: with_dot(&stopped, BUSY_DOT),
                running: with_dot(&stopped, RUNNING_DOT),
                stopped,
            });
            follow_phase(app.handle());
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

    #[test]
    fn tray_dot_is_drawn_in_the_corner_and_leaves_the_rest_of_the_icon() {
        let base = tauri::image::Image::new_owned([0x17, 0x38, 0x2f, 0xff].repeat(32 * 32), 32, 32);
        let marked = super::with_dot(&base, super::RUNNING_DOT);
        let pixel = |x: u32, y: u32| {
            let at = ((y * 32 + x) * 4) as usize;
            marked.rgba()[at..at + 4].to_vec()
        };
        assert_eq!((marked.width(), marked.height()), (32, 32));
        assert_eq!(pixel(0, 0), [0x17, 0x38, 0x2f, 0xff]);
        assert_eq!(pixel(16, 16), [0x17, 0x38, 0x2f, 0xff]);
        assert_eq!(pixel(26, 26), [0x34, 0xc7, 0x59, 0xff]);
        // The ring around the dot is cut out of the icon.
        assert_eq!(pixel(19, 26)[3], 0);
    }
}
