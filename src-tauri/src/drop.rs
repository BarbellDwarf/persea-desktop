#![allow(dead_code)]
//! Native drag-drop capture for session windows.
//!
//! # Design (locked)
//!
//! Every window that can host a persea session (`main` viewport with an
//! active inline tab, `session-<id>` pop-out windows, and the drop-zone
//! overlay itself as a Wayland fallback) gets a
//! `WebviewWindow::on_drag_drop_event` handler, attached at startup and
//! re-attached by a 1 s poll so windows created later by the tab
//! manager pick the handler up. No capability is needed for the events
//! themselves (`dragDropEnabled` stays true, which is also what keeps
//! HTML5 DnD in the remote page suppressed).
//!
//! While a drag is over a session window, a small frameless transparent
//! overlay window (`dropzone`, `shell/dropzone.html`) follows the
//! cursor as the drop-zone visual. It ignores cursor events, so the
//! drop normally lands on the session window below; on Wayland the
//! positioning is best-effort (the compositor owns window positions)
//! and a drop that lands on the overlay is still handled via the last
//! tracked target window.
//!
//! On drop the paths are handed to `transfer::handle_drop`, which does
//! the server gating, pairing check, session classification and the
//! upload flow. Rejections (transfers disabled, SSH sessions, empty
//! paths on KDE Wayland, session gone) surface as native notices.
//!
//! # Wiring for the dispatcher
//!
//! 1. `lib.rs`: `mod drop;` and `drop::setup(app)?` in the setup hook,
//!    after `transfer::setup` (the mirror poll lives here and needs the
//!    transfer registry initialized).
//! 2. Nothing else: the overlay and transfer windows are code-built
//!    here and in `transfer.rs`, and the drag events need no
//!    permissions.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use tauri::{
    AppHandle, DragDropEvent, Manager, PhysicalPosition, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use url::Url;

use crate::transfer;
use crate::windows;

/// The drop-zone overlay window label.
pub const DROPZONE_WINDOW_LABEL: &str = "dropzone";
/// Poll cadence for attaching handlers to newly created windows and
/// mirroring engine downloads.
const POLL_INTERVAL_MS: u64 = 1000;

static APP: OnceLock<AppHandle> = OnceLock::new();
/// Labels that already carry the drag handler; pruned when the window
/// disappears so a reopened window with the same label re-attaches.
static ATTACHED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// The session window a drag last entered (used when the drop lands on
/// the overlay itself, a Wayland fallback).
static LAST_TARGET: Mutex<Option<String>> = Mutex::new(None);

/// Create the hidden overlay window, attach handlers to every existing
/// window, and start the poll loop. Called once from the setup hook.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let _ = APP.set(app.handle().clone());
    let overlay = WebviewWindowBuilder::new(
        app,
        DROPZONE_WINDOW_LABEL,
        WebviewUrl::App("dropzone.html".into()),
    )
    .title("")
    .inner_size(240.0, 88.0)
    .visible(false)
    .decorations(false)
    .skip_taskbar(true)
    .always_on_top(true)
    .focused(false)
    .resizable(false);
    #[cfg(not(target_os = "macos"))]
    let overlay = overlay.transparent(true);
    let overlay = overlay.build()?;
    // Deliberately no `set_ignore_cursor_events(true)` here: on Linux the
    // request crosses into the GTK main loop before the hidden window is
    // realized and aborts the app (tao#1178). The flag is applied in
    // `position_overlay` right after the window is shown, when the
    // GdkWindow exists.

    let handle = app.handle().clone();
    refresh_attachments(&handle);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        refresh_attachments(&handle);
        transfer::mirror_engine_downloads(&handle);
    });
    Ok(())
}

/// Re-scan the window set: drop stale attach marks, attach to any
/// window that does not carry the handler yet.
fn refresh_attachments(handle: &AppHandle) {
    let current: HashSet<String> = handle.webview_windows().keys().cloned().collect();
    {
        let mut attached = ATTACHED.lock().unwrap_or_else(|p| p.into_inner());
        attached.retain(|label| current.contains(label));
    }
    for (label, win) in handle.webview_windows() {
        if is_drop_capable(&label) {
            attach(&win);
        }
    }
}

/// Attach the drag handler to a window, exactly once per label.
fn attach(win: &WebviewWindow) {
    let label = win.label().to_string();
    {
        let mut attached = ATTACHED.lock().unwrap_or_else(|p| p.into_inner());
        if !attached.insert(label.clone()) {
            return;
        }
    }
    let app = win.app_handle().clone();
    let handler_win = win.clone();
    let win = win.clone();
    win.on_window_event(move |event| {
        if let tauri::WindowEvent::DragDrop(dnd) = event {
            handle_drag_event(&app, &handler_win, dnd.clone());
        }
    });
}

fn is_drop_capable(label: &str) -> bool {
    label == DROPZONE_WINDOW_LABEL
        || label == windows::MAIN_WINDOW_LABEL
        || label.starts_with(windows::SESSION_WINDOW_PREFIX)
}

fn handle_drag_event(app: &AppHandle, win: &WebviewWindow, event: DragDropEvent) {
    match event {
        DragDropEvent::Enter { position, .. } => {
            remember_target(win);
            position_overlay(win, position);
        }
        DragDropEvent::Over { position } => position_overlay(win, position),
        DragDropEvent::Drop { paths, position } => {
            remember_target(win);
            hide_overlay();
            on_drop(app, win, paths, position);
        }
        DragDropEvent::Leave => hide_overlay(),
        _ => {}
    }
}

/// Remember which window the drag is over. Drops that land on the
/// overlay itself (Wayland, where `set_ignore_cursor_events` is
/// best-effort) resolve their session from this target.
fn remember_target(win: &WebviewWindow) {
    let label = win.label().to_string();
    if let Ok(mut target) = LAST_TARGET.lock() {
        *target = Some(label);
    }
}

fn on_drop(
    app: &AppHandle,
    win: &WebviewWindow,
    paths: Vec<PathBuf>,
    _position: PhysicalPosition<f64>,
) {
    let app = app.clone();
    let label = if win.label() == DROPZONE_WINDOW_LABEL {
        LAST_TARGET
            .lock()
            .ok()
            .and_then(|t| t.clone())
            .unwrap_or_default()
    } else {
        win.label().to_string()
    };
    tauri::async_runtime::spawn(async move {
        if paths.is_empty() {
            transfer::notice(
                &app,
                "Empty drop",
                "No file paths were received (a known quirk on KDE Wayland). \
                 Try the drag again.",
            );
            return;
        }
        let Some((instance, session_id)) = resolve_session(&label) else {
            transfer::notice(
                &app,
                "No session",
                "Drop a file onto an active session window to send it.",
            );
            return;
        };
        transfer::handle_drop(app, instance, session_id, paths).await;
    });
}

/// Resolve the instance URL and session id behind a window label: the
/// viewport (`main`) shows the active inline tab; `session-<id>` labels
/// map to their tab.
fn resolve_session(label: &str) -> Option<(String, String)> {
    let tabs = windows::cmd_tabs_list().ok()?;
    let tab = if label == windows::MAIN_WINDOW_LABEL {
        tabs.iter().find(|t| t.active && t.mode == "inline")
    } else {
        tabs.iter()
            .find(|t| windows::session_window_label(&t.id) == label)
    }?;
    let url = Url::parse(&tab.url).ok()?;
    let session_id = windows::session_id_from_url(&url)?;
    let instance = url.origin().ascii_serialization();
    Some((instance, session_id))
}

/// Position the overlay centered on the cursor, clamped into the target
/// window, and show it. All window calls run on the main thread.
fn position_overlay(win: &WebviewWindow, position: PhysicalPosition<f64>) {
    let app = win.app_handle().clone();
    let target_label = win.label().to_string();
    let thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(overlay) = thread_app.get_webview_window(DROPZONE_WINDOW_LABEL) else {
            return;
        };
        let Some(target) = thread_app.get_webview_window(&target_label) else {
            return;
        };
        let Ok(win_pos) = target.outer_position() else {
            return;
        };
        let Ok(win_size) = target.outer_size() else {
            return;
        };
        let Ok(overlay_size) = overlay.outer_size() else {
            return;
        };
        // Clamp so the overlay stays fully inside the target window; a
        // window smaller than the overlay pins it to the top-left.
        let max_x = (win_pos.x + win_size.width as i32 - overlay_size.width as i32).max(win_pos.x);
        let max_y =
            (win_pos.y + win_size.height as i32 - overlay_size.height as i32).max(win_pos.y);
        let x = (win_pos.x + position.x as i32 - (overlay_size.width / 2) as i32)
            .clamp(win_pos.x, max_x);
        let y = (win_pos.y + position.y as i32 - (overlay_size.height / 2) as i32)
            .clamp(win_pos.y, max_y);
        let _ = overlay.set_position(PhysicalPosition::new(x, y));
        if !overlay.is_visible().unwrap_or(false) {
            let _ = overlay.show();
            // Window is realized now; safe to ignore cursor events (see
            // the setup comment about tao#1178).
            let _ = overlay.set_ignore_cursor_events(true);
        }
    });
}

fn hide_overlay() {
    let Some(app) = APP.get() else {
        return;
    };
    let app = app.clone();
    let thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(overlay) = thread_app.get_webview_window(DROPZONE_WINDOW_LABEL) else {
            return;
        };
        if overlay.is_visible().unwrap_or(false) {
            let _ = overlay.hide();
        }
    });
}
