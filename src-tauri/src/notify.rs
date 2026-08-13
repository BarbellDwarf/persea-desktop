//! Desktop notifications: the quiet-defaults layer over notify-rust.
//!
//! Backend decision (spike result): notify-rust on every platform. The
//! tauri notification plugin hits the GNOME 46+ bug (notifications close
//! immediately or never render; plugins-workspace#2566, tauri#14095), and
//! firezone swapped the plugin for notify-rust in production (firezone
//! PR #11813) because notify-rust behaves there. notify-rust talks to the
//! notification daemon directly (`org.freedesktop.Notifications` over
//! D-Bus on Linux, zbus), which sidesteps the plugin layer that broke.
//! The notify-send workaround was rejected: it depends on an external
//! binary that minimal installs may lack, and it cannot carry images.
//! notify-rust also covers Windows (WinRT toasts via
//! tauri-winrt-notification) and macOS (mac-notification-sys), so the
//! shell keeps ONE notification path on all three OSes.
//!
//! Quiet defaults (locked design): notifications are OFF until the user
//! enables them in Settings. Enabling sends a test notification, which is
//! the "request on first enable" step: on macOS it triggers the OS
//! permission prompt, on Linux it sanity-checks that a notification
//! daemon answers. The default event set is the quiet one (ended, error,
//! transfer complete, update available) plus the poller's started and
//! idle-warning events; this module only renders, the poller decides
//! which events fire. The one unconditional alert is the re-login-needed
//! notice from a 401 pause: it fires even while notifications are
//! disabled, once per instance per sign-out (the poller dedupes).
//!
//! Windows caveat (documented, locked design): WinRT toasts only render
//! for apps the user can see in Settings > Notifications, which for an
//! unpackaged app means the installer registered the AUMID (`dev.persea.desktop`)
//! and the app runs from the Start Menu shortcut; a portable/dev run may
//! silently drop toasts. Thumbnails go only on Windows toasts, and only
//! as local files (the WinRT image element takes a path; the poller
//! downloads the session thumbnail URL to a temp file first). Linux and
//! macOS toasts are text-only by design: the XDG `image-path` hint is
//! ignored by GNOME's daemon for remote URLs, and macOS content images
//! need a local file too.
//!
//! macOS threading: AppKit notifications must be posted from the main
//! thread, so the macOS path hops through `run_on_main_thread`; Linux
//! (D-Bus) and Windows (WinRT) are posted from the calling thread.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// App id used for Windows toast AUMID matching; ignored elsewhere.
#[cfg(all(target_os = "windows", not(test)))]
const APP_ID: &str = "dev.persea.desktop";
/// App name shown in the notification header.
#[cfg(not(test))]
const APP_NAME: &str = "Persea Desktop";
/// Config file name in the app data dir.
const CONFIG_FILE: &str = "notifications.json";

static CONFIG: Mutex<Option<Arc<Mutex<NotifyConfig>>>> = Mutex::new(None);

/// Persisted notification settings. Quiet defaults: off.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct NotifyConfig {
    #[serde(default)]
    enabled: bool,
}

/// Load the config from the app data dir. Call once from the setup hook
/// (via `tray::setup`).
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let cfg = load(&dir.join(CONFIG_FILE));
    *CONFIG.lock().unwrap() = Some(Arc::new(Mutex::new(cfg)));
    Ok(())
}

fn load(path: &PathBuf) -> NotifyConfig {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<NotifyConfig>(&raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                let backup = path.with_extension("json.corrupt");
                let _ = std::fs::rename(path, &backup);
                eprintln!(
                    "persea-desktop: notifications.json unreadable ({e}); \
                     backed up to {}; notifications stay disabled",
                    backup.display()
                );
                NotifyConfig::default()
            }
        },
        Err(_) => NotifyConfig::default(),
    }
}

fn with_config<R>(f: impl FnOnce(&mut NotifyConfig) -> R) -> Option<R> {
    let cfg = CONFIG.lock().ok()?.clone()?;
    let mut cfg = cfg.lock().ok()?;
    Some(f(&mut cfg))
}

fn save(app: &AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(CONFIG_FILE);
    let tmp = path.with_extension("json.tmp");
    let cfg = with_config(|c| c.clone()).ok_or("notification config is not initialized")?;
    let data = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether notifications are enabled (quiet defaults: off).
pub fn enabled() -> bool {
    with_config(|c| c.enabled).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Commands (registered by the dispatcher in lib.rs)
// ---------------------------------------------------------------------------

/// Current notification setting for the Settings page.
#[tauri::command]
pub fn notifications_get_enabled() -> bool {
    enabled()
}

/// Enable or disable notifications. Enabling persists the setting and
/// sends a test notification: on macOS this is what triggers the OS
/// permission prompt (first enable), on Linux it verifies a daemon
/// answers. Returns the new state.
#[tauri::command]
pub fn notifications_set_enabled(app: AppHandle, enabled: bool) -> Result<bool, String> {
    with_config(|c| c.enabled = enabled).ok_or("notification config is not initialized")?;
    save(&app)?;
    if enabled {
        notify(
            &app,
            "Notifications enabled",
            "Session alerts will show up here.",
            None,
        );
    }
    Ok(enabled)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// One toast. Returns the notification daemon's verdict as a log line;
/// failures are never fatal.
fn notify(_app: &AppHandle, summary: &str, body: &str, image_path: Option<&str>) {
    if !enabled() {
        return;
    }
    let summary = summary.to_string();
    let body = body.to_string();
    let image_path = image_path.map(str::to_string);
    #[cfg(target_os = "macos")]
    {
        // AppKit requires notifications from the main thread.
        let app = _app.clone();
        let _ = app.run_on_main_thread(move || {
            post(&summary, &body, image_path.as_deref());
        });
    }
    #[cfg(not(target_os = "macos"))]
    post(&summary, &body, image_path.as_deref());
}

#[cfg(not(test))]
fn post(summary: &str, body: &str, image_path: Option<&str>) {
    let mut notification = notify_rust::Notification::new();
    notification.summary(summary);
    notification.body(body);
    notification.appname(APP_NAME);
    #[cfg(target_os = "windows")]
    notification.app_id(APP_ID);
    if let Some(path) = image_path {
        notification.image_path(path);
    }
    if let Err(e) = notification.show() {
        eprintln!("persea-desktop: notification failed: {e}");
    }
}

#[cfg(test)]
fn post(_summary: &str, _body: &str, _image_path: Option<&str>) {}

/// The unconditional alert: fires even while notifications are disabled.
/// The poller calls this once per instance per sign-out.
pub fn relogin_needed(_app: &AppHandle, instance_name: &str) {
    let summary = "Re-login needed".to_string();
    let body = format!(
        "{instance_name} rejected the session token. Pair this device again to resume alerts."
    );
    #[cfg(target_os = "macos")]
    {
        let app = _app.clone();
        let _ = app.run_on_main_thread(move || {
            post(&summary, &body, None);
        });
    }
    #[cfg(not(target_os = "macos"))]
    post(&summary, &body, None);
}

/// Session started (own sessions). Name is the entry display name,
/// user@host or the hostname.
pub fn session_started(app: &AppHandle, name: &str, session_type: &str) {
    notify(
        app,
        &format!("Session started · {name}"),
        &format!("{session_type} session {name} is now running."),
        None,
    );
}

/// Session ended normally (completed/expired/logged_out). `thumbnail` is
/// a local file path, Windows toasts only.
pub fn session_ended(app: &AppHandle, name: &str, status: &str, thumbnail: Option<&str>) {
    notify(
        app,
        &format!("Session ended · {name}"),
        &format!("The {name} session ended ({status})."),
        thumbnail,
    );
}

/// Session ended with an error. `thumbnail` is a local file path,
/// Windows toasts only.
pub fn session_error(app: &AppHandle, name: &str, thumbnail: Option<&str>) {
    notify(
        app,
        &format!("Session error · {name}"),
        &format!("The {name} session stopped with an error."),
        thumbnail,
    );
}

/// Idle warning: the session will be reaped soon (approximation from the
/// last-activity fields; see poller.rs).
pub fn session_idle_warning(app: &AppHandle, name: &str) {
    notify(
        app,
        &format!("Session idle · {name}"),
        &format!("{name} will be closed soon because it has been idle."),
        None,
    );
}

/// Transfer-complete placeholder (drag-drop uploads, D11): the hook the
/// transfer feature calls when a native upload finishes.
#[allow(dead_code)] // wired by the transfer feature (D11); placeholder until then
pub fn transfer_complete(app: &AppHandle, name: &str) {
    notify(
        app,
        &format!("Transfer complete · {name}"),
        &format!("Files for {name} finished uploading."),
        None,
    );
}

/// Update-available placeholder (auto-update, D13): the hook the updater
/// calls when a new version is ready.
#[allow(dead_code)] // wired by the updater (D13); placeholder until then
pub fn update_available(app: &AppHandle, version: &str) {
    notify(
        app,
        "Update available",
        &format!("Persea Desktop {version} is ready to install."),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "persea-desktop-notify-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cfg() -> NotifyConfig {
        NotifyConfig::default()
    }

    #[test]
    fn defaults_are_quiet() {
        assert!(!cfg().enabled);
    }

    #[test]
    fn config_round_trip_preserves_enabled() {
        let path = tmp_path("roundtrip");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = NotifyConfig { enabled: true };
        std::fs::write(&path, serde_json::to_string(&c).unwrap()).unwrap();
        assert!(load(&path).enabled);
        c.enabled = false;
        std::fs::write(&path, serde_json::to_string(&c).unwrap()).unwrap();
        assert!(!load(&path).enabled);
    }

    #[test]
    fn corrupt_config_falls_back_to_quiet() {
        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ nope").unwrap();
        assert!(!load(&path).enabled);
        assert!(path.with_extension("json.corrupt").exists());
    }

    #[test]
    fn missing_config_is_quiet() {
        assert!(!load(&tmp_path("missing")).enabled);
    }
}
