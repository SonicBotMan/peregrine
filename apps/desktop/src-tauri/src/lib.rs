//! Peregrine desktop shell (Tauri 2).
//!
//! Responsibilities — the shell is a THIN layer; all download logic
//! lives in the daemon (sidecar) and the web UI:
//! 1. Spawn/manage the `peregrined` sidecar on loopback TCP (the
//!    webview cannot speak UDS).
//! 2. Tray icon: close-to-tray, show, quit (kills the daemon).
//! 3. Single instance: second launch focuses the existing window.
//!
//! Notifications are sent from the WEB layer (plugin-notification
//! invoked when a task completes) — the shell never touches task
//! state, keeping the ownership rule: engine → daemon → any client.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

/// Box<dyn Error> matches `Builder::setup`'s error contract and
/// gives us `From<String>` for the spawn/tray failure paths.
type ShellResult = Result<(), Box<dyn std::error::Error>>;

/// TCP port the daemon sidecar listens on. Fixed on purpose: the
/// webview client and this shell must agree without discovery, and
/// a fixed loopback port is guarded by the daemon's host check.
pub const DAEMON_PORT: u16 = 8420;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Second launch: surface the running window instead.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .setup(|app| {
            spawn_daemon(app.handle())?;
            build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Close-to-tray: the daemon keeps downloading; the window
            // just hides. Real exit goes through the tray menu.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running peregrine shell");
}

/// Spawn the daemon sidecar and log its output. The sidecar binary
/// is provided by `bundle.externalBin` (CI copies the release build
/// to `binaries/peregrined-$TRIPLE`). TCP-only (`--tcp`, no UDS):
/// the webview speaks HTTP/WS; an app-bound UDS buys nothing and a
/// second daemon instance from the CLI would fight over the socket.
fn spawn_daemon(app: &tauri::AppHandle) -> ShellResult {
    let cmd = app
        .shell()
        .sidecar("peregrined")
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!(
                "sidecar `peregrined` not bundled (expected binaries/peregrined-<triple>): {e}"
            )
            .into()
        })?
        .args([
            "--tcp",
            &DAEMON_PORT.to_string(),
        ])
        .spawn();

    match cmd {
        Ok((mut rx, _child)) => {
            // Sidecar lifecycle: pump stdout/stderr into our logs and
            // surface an unexpected death to the UI (the web layer
            // already reconnects its WS; this is for the human).
            tauri::async_runtime::spawn(async move {
                while let Some(evt) = rx.recv().await {
                    match evt {
                        CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                            let text = String::from_utf8_lossy(&line);
                            print!("[peregrined] {text}");
                        }
                        CommandEvent::Terminated(p) => {
                            eprintln!("[peregrined] terminated: {p:?}");
                            break;
                        }
                        _ => {}
                    }
                }
            });
            Ok(())
        }
        Err(e) => {
            eprintln!("failed to spawn peregrined sidecar: {e}");
            Err(format!("failed to spawn peregrined: {e}").into())
        }
    }
}

/// Tray with Show/Quit. Quit is the ONLY exit path (window close
/// hides). tauri-plugin-shell kills spawned children on app exit,
/// so app exit → daemon exit.
fn build_tray(app: &tauri::AppHandle) -> ShellResult {
    let show = MenuItem::with_id(app, "show", "Show Peregrine", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops downloads)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    // NOTE: no `trayIcon` block in tauri.conf.json — config-defined
    // trays auto-register during Builder::build() with the SAME
    // default id ("main") we use here, which would stack a second,
    // inert OS tray icon (register() doesn't dedupe ids). The manual
    // builder is the sole owner of the tray.
    let tray = TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .expect("default window icon missing from config")
                .clone(),
        )
        .tooltip("Peregrine")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "quit" => {
                // No pre-exit web event: emit→exit races the event
                // loop teardown and the listener would likely never
                // run. When a flush ack is ever needed, add an
                // ack/timeout handshake here.
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    // Click (not menu) → toggle window, IDM-style.
    tray.on_tray_icon_event(|tray, event| {
        if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, button_state: tauri::tray::MouseButtonState::Up, .. } = event
        {
            let app = tray.app_handle();
            if let Some(win) = app.get_webview_window("main") {
                if win.is_visible().unwrap_or(false) {
                    let _ = win.hide();
                } else {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        }
    });
    Ok(())
}
