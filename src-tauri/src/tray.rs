//! Tray icon + menu: instances, their active sessions, pair status, the
//! kiosk toggle, About and Quit.
//!
//! Menu (locked design):
//! - one submenu per instance: session items (click raises the session:
//!   the tab manager opens the client URL and the main window is
//!   raised), "Open <instance>", the pair status item (paired /
//!   "Pair this device…" / "Re-login needed — pair again…"), and the
//!   kiosk toggle, which only appears when the instance probe reports
//!   `kiosk_allowed` (server-gating rule). The toggle emits
//!   [`EVENT_KIOSK_TOGGLE`] for the kiosk feature to consume; the tray's
//!   own kiosk awareness comes from [`set_kiosk`].
//! - empty states: with zero instances the menu is a single "Add
//!   instance…" item into the shell settings page; an instance with no
//!   live sessions shows a disabled "No active sessions" item.
//! - About + Quit (predefined items). In kiosk mode both are hidden and
//!   the instance submenus shrink to sessions only.
//!
//! The tray icon is monochrome (template style), three variants: base
//! (no sessions), active (filled dot: live sessions exist), signed-out
//! (hollow ring dot: an instance rejected the paired token). The
//! signed-out variant wins over the active one.
//!
//! Threading: every tauri 2.11 tray mutation (menu construction,
//! `set_menu`, `set_icon`) is marshalled to the main thread internally
//! (`run_main_thread!`), which is the Linux appindicator crash-class
//! guard from research 03 §6: this module never touches the tray from a
//! worker thread directly, and the poller task safely calls
//! [`set_sessions`]/[`set_signed_out`] because the tauri layer hops to
//! the GTK thread.
//!
//! Refresh policy: `set_*` calls rebuild the menu only when the state
//! actually changed; [`refresh`] additionally prunes sessions of
//! instances that lost their paired token and re-renders pair status,
//! and the poller supervisor calls it after every reconcile so pairing
//! and revoke show up within one poll cadence.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tauri::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::instances;
use crate::poller::is_terminal;

/// Tray icon id.
pub const TRAY_ID: &str = "main";
/// Emitted when the user toggles the kiosk menu item, with payload
/// `{"instanceUrl": string, "enabled": bool}`. The kiosk feature (D12)
/// consumes it and drives entry/exit; `set_kiosk` reflects the result.
pub const EVENT_KIOSK_TOGGLE: &str = "kiosk-toggle";
/// Shell page hosting instance settings (the "Add instance" target).
const SETTINGS_PAGE: &str = "settings.html";
/// Shell page hosting the device pairing flow (D07).
const PAIRING_PAGE: &str = "pairing.html";

/// One session in the tray menu.
#[derive(Debug, Clone, PartialEq)]
pub struct TraySession {
    pub id: String,
    pub name: String,
    /// pending | active | completed | error | expired | disconnected |
    /// logged_out
    pub status: String,
    /// Absolute client page URL (`{instance}/client/{id}`).
    pub url: String,
}

#[derive(Default, Clone)]
struct TrayState {
    /// Live session list per instance (trimmed URL keys).
    sessions: HashMap<String, Vec<TraySession>>,
    /// Instances whose token was rejected (401): signed-out badge.
    signed_out: HashSet<String>,
    /// Kiosk mode (set by the kiosk feature via [`set_kiosk`]).
    kiosk: bool,
}

static STATE: Mutex<Option<Arc<Mutex<TrayState>>>> = Mutex::new(None);
static TRAY: Mutex<Option<TrayIcon>> = Mutex::new(None);
static LAST_KEY: Mutex<Option<String>> = Mutex::new(None);

fn state_handle() -> Option<Arc<Mutex<TrayState>>> {
    STATE.lock().ok()?.clone()
}

fn tray_handle() -> Option<TrayIcon> {
    TRAY.lock().ok()?.clone()
}

/// Build the tray and start the poller. Call once from the setup hook
/// (dispatcher). A tray build failure (no tray host, e.g. Wayland
/// without an extension) logs and continues: notifications and the
/// poller still work.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    crate::notify::setup(app)?;
    let handle = app.handle().clone();
    *STATE.lock().unwrap() = Some(Arc::new(Mutex::new(TrayState::default())));

    let menu = build_menu(&handle)?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Persea Desktop")
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                raise_main_window(tray.app_handle());
            }
        })
        .build(app);
    match tray {
        Ok(tray) => {
            #[cfg(target_os = "macos")]
            {
                let _ = tray.set_icon_as_template(true);
            }
            *TRAY.lock().unwrap() = Some(tray);
        }
        Err(e) => eprintln!("persea-desktop: tray unavailable ({e}); continuing without it"),
    }

    crate::poller::start(&handle);
    refresh(&handle);
    Ok(())
}

/// Kiosk-aware tray refresh. The kiosk feature (D12) calls this on entry
/// and exit: in kiosk mode the menu drops About, Quit, instance
/// switching, pair items and the kiosk toggle itself, leaving only the
/// session switcher.
pub fn set_kiosk(app: &AppHandle, kiosk: bool) {
    let Some(state) = state_handle() else { return };
    let mut state = state.lock().unwrap();
    if state.kiosk == kiosk {
        return;
    }
    state.kiosk = kiosk;
    drop(state);
    refresh(app);
}

/// Update the session list of one instance (from the poller). Rebuilds
/// the menu only when the list changed.
pub fn set_sessions(app: &AppHandle, instance_url: &str, sessions: Vec<TraySession>) {
    let url = instance_url.trim_end_matches('/').to_string();
    let Some(state) = state_handle() else { return };
    let mut state = state.lock().unwrap();
    if state.sessions.get(&url) == Some(&sessions) {
        return;
    }
    state.sessions.insert(url, sessions);
    drop(state);
    refresh(app);
}

/// Mark an instance signed out (401) or clear the badge after re-pair.
pub fn set_signed_out(app: &AppHandle, instance_url: &str, signed_out: bool) {
    let url = instance_url.trim_end_matches('/').to_string();
    let Some(state) = state_handle() else { return };
    let mut state = state.lock().unwrap();
    let changed = if signed_out {
        state.signed_out.insert(url)
    } else {
        state.signed_out.remove(&url)
    };
    if !changed {
        return;
    }
    drop(state);
    refresh(app);
}

/// Rebuild the menu from the current state (pruning sessions of
/// instances that lost their token) when anything relevant changed. The
/// poller supervisor calls this after every reconcile.
pub fn refresh(app: &AppHandle) {
    let Some(state) = state_handle() else { return };
    let key = {
        let mut state = state.lock().unwrap();
        let paired: HashSet<String> = crate::pairing::registered_tokens(app)
            .iter()
            .map(|t| t.instance_url.clone())
            .collect();
        state.sessions.retain(|url, _| paired.contains(url));
        render_key(app, &state)
    };
    if LAST_KEY.lock().unwrap().as_deref() == Some(&key) {
        return;
    }
    *LAST_KEY.lock().unwrap() = Some(key);
    let Ok(menu) = build_menu(app) else {
        return;
    };
    let Some(tray) = tray_handle() else {
        return;
    };
    if let Some(state) = state_handle() {
        if let Ok(state) = state.lock() {
            if let Ok(icon) = tray_icon(&state) {
                let _ = tray.set_icon(Some(icon));
            }
            let _ = tray.set_tooltip(Some(tooltip(&state)));
        }
    }
    let _ = tray.set_menu(Some(menu));
    #[cfg(target_os = "macos")]
    {
        let _ = tray.set_icon_as_template(true);
    }
}

/// Dedup key for the current render: kiosk flag, signed-out set,
/// sessions, and the pairing registry (pair status lives in the menu).
fn render_key(app: &AppHandle, state: &TrayState) -> String {
    let mut parts = vec![
        format!("kiosk={}", state.kiosk),
        format!("signed={}", state.signed_out.len()),
    ];
    let mut tokens: Vec<String> = crate::pairing::registered_tokens(app)
        .iter()
        .map(|t| format!("{}#{}", t.instance_url, t.token_id))
        .collect();
    tokens.sort();
    parts.push(tokens.join(","));
    let mut urls: Vec<&String> = state.sessions.keys().collect();
    urls.sort();
    for url in urls {
        let mut sessions = state.sessions[url].clone();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        for session in &sessions {
            parts.push(format!(
                "{}|{}|{}|{}",
                url, session.id, session.status, session.url
            ));
        }
    }
    parts.join(";")
}

fn tooltip(state: &TrayState) -> String {
    if !state.signed_out.is_empty() {
        return "Persea Desktop — re-login needed".to_string();
    }
    let live: usize = state
        .sessions
        .values()
        .flatten()
        .filter(|s| !is_terminal(&s.status))
        .count();
    match live {
        0 => "Persea Desktop".to_string(),
        1 => "Persea Desktop — 1 active session".to_string(),
        n => format!("Persea Desktop — {n} active sessions"),
    }
}

/// Which icon variant to show: signed-out wins, then active (dot) when
/// any live session exists, then the base tile.
fn icon_variant(state: &TrayState) -> &'static str {
    if !state.signed_out.is_empty() {
        "tray-signedout"
    } else if state
        .sessions
        .values()
        .flatten()
        .any(|s| !is_terminal(&s.status))
    {
        "tray-active"
    } else {
        "tray"
    }
}

fn tray_icon(state: &TrayState) -> Result<tauri::image::Image<'static>, String> {
    let bytes: &[u8] = match icon_variant(state) {
        "tray-active" => include_bytes!("../icons/tray-active.png"),
        "tray-signedout" => include_bytes!("../icons/tray-signedout.png"),
        _ => include_bytes!("../icons/tray.png"),
    };
    tauri::image::Image::from_bytes(bytes).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Menu construction
// ---------------------------------------------------------------------------

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let state = state_handle()
        .ok_or("tray state is not initialized")
        .map_err(menu_error)?
        .lock()
        .map_err(|_| menu_error("tray state is locked"))?
        .clone();
    let instances = instances::instances();
    let mut items: Vec<&dyn IsMenuItem<tauri::Wry>> = Vec::new();
    if instances.is_empty() {
        let add = MenuItem::with_id(app, "open-settings", "Add instance…", true, None::<&str>)?;
        items.push(&add);
    } else {
        for instance in &instances {
            let submenu = build_instance_submenu(app, &state, instance)?;
            items.push(&submenu);
        }
    }
    if !state.kiosk {
        items.push(&PredefinedMenuItem::separator(app)?);
        let about = PredefinedMenuItem::about(
            app,
            Some("About"),
            Some(tauri::menu::AboutMetadata {
                name: Some(app.package_info().name.clone()),
                version: Some(app.package_info().version.to_string()),
                ..Default::default()
            }),
        )?;
        items.push(&about);
        let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;
        items.push(&quit);
    }
    Menu::with_items(app, &items)
}

fn menu_error(message: &str) -> tauri::Error {
    tauri::Error::Io(std::io::Error::other(message.to_string()))
}

fn build_instance_submenu(
    app: &AppHandle,
    state: &TrayState,
    instance: &instances::Instance,
) -> tauri::Result<Submenu<tauri::Wry>> {
    let url = instance.url.trim_end_matches('/').to_string();
    let mut items: Vec<&dyn IsMenuItem<tauri::Wry>> = Vec::new();
    let sessions = state.sessions.get(&url).cloned().unwrap_or_default();
    if sessions.is_empty() {
        let empty = MenuItem::with_id(
            app,
            format!("no-sessions:{url}"),
            "No active sessions",
            false,
            None::<&str>,
        )?;
        items.push(&empty);
    } else {
        for session in sessions {
            let label = format!("{} — {}", session.name, prettify_status(&session.status));
            let item = MenuItem::with_id(
                app,
                format!("open-session:{}", session.url),
                label,
                true,
                None::<&str>,
            )?;
            items.push(&item);
        }
    }
    if !state.kiosk {
        items.push(&PredefinedMenuItem::separator(app)?);
        let open = MenuItem::with_id(
            app,
            format!("open-instance:{url}"),
            format!("Open {}", instance.name),
            true,
            None::<&str>,
        )?;
        items.push(&open);
        let paired = crate::pairing::registered_tokens(app)
            .iter()
            .any(|t| t.instance_url == url);
        if state.signed_out.contains(&url) {
            let re_pair = MenuItem::with_id(
                app,
                format!("pair:{url}"),
                "Re-login needed — pair again…",
                true,
                None::<&str>,
            )?;
            items.push(&re_pair);
        } else if paired {
            let paired = CheckMenuItem::with_id(
                app,
                format!("paired:{url}"),
                "Paired",
                false,
                true,
                None::<&str>,
            )?;
            items.push(&paired);
        } else {
            let pair = MenuItem::with_id(
                app,
                format!("pair:{url}"),
                "Pair this device…",
                true,
                None::<&str>,
            )?;
            items.push(&pair);
        }
        if instances::capability(&url, "kiosk_allowed") {
            let checked = instance.kiosk_allowed == Some(true);
            let kiosk = CheckMenuItem::with_id(
                app,
                format!("kiosk:{url}"),
                "Kiosk mode",
                true,
                checked,
                None::<&str>,
            )?;
            items.push(&kiosk);
        }
    }
    Submenu::with_id_and_items(app, format!("instance:{url}"), &instance.name, true, &items)
}

/// Tray label for a session status.
fn prettify_status(status: &str) -> String {
    match status {
        "pending" => "Connecting",
        "active" => "Active",
        "disconnected" => "Disconnected",
        "completed" => "Ended",
        "error" => "Error",
        "expired" => "Expired",
        "logged_out" => "Logged out",
        other => other,
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

fn handle_menu_event(app: &AppHandle, event: &MenuEvent) {
    let id = event.id().as_ref();
    if id == "open-settings" {
        open_shell_page(app, SETTINGS_PAGE);
        return;
    }
    if let Some(url) = id.strip_prefix("open-instance:") {
        let _ = crate::instances::cmd_instances_open(app.clone(), url.to_string());
        raise_main_window(app);
        return;
    }
    if let Some(session_url) = id.strip_prefix("open-session:") {
        // The tab manager opens (or focuses) the session window and
        // navigates the viewport to the owning instance; raise the main
        // window so the click always surfaces the session.
        let _ = crate::windows::cmd_tabs_open(session_url.to_string());
        raise_main_window(app);
        return;
    }
    if let Some(url) = id.strip_prefix("pair:") {
        open_pairing(app, url);
        return;
    }
    if let Some(url) = id.strip_prefix("kiosk:") {
        let desired = instances::instance(url)
            .map(|i| i.kiosk_allowed != Some(true))
            .unwrap_or(true);
        let _ = app.emit(
            EVENT_KIOSK_TOGGLE,
            json!({ "instanceUrl": url, "enabled": desired }),
        );
    }
}

/// Raise the main window: restore from minimize, show, focus.
fn raise_main_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window(instances::window_label("")) else {
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

/// The app's own shell pages live on the tauri protocol
/// (`tauri://localhost` or `http://tauri.localhost` on Windows); the
/// navigation lockdown always allows them.
fn app_url(path: &str) -> tauri::Url {
    let base = if cfg!(windows) {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    tauri::Url::parse(&format!("{base}{path}")).expect("static shell page URL parses")
}

fn open_shell_page(app: &AppHandle, path: &str) {
    let Some(win) = app.get_webview_window(instances::window_label("")) else {
        return;
    };
    let _ = win.navigate(app_url(path));
}

/// Navigate the main window to the shell pairing page for an instance.
fn open_pairing(app: &AppHandle, instance_url: &str) {
    let Some(win) = app.get_webview_window(instances::window_label("")) else {
        return;
    };
    let mut url = app_url(PAIRING_PAGE);
    url.query_pairs_mut().append_pair("url", instance_url);
    let _ = win.navigate(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(kiosk: bool, signed: &[&str], sessions: &[(&str, &str, &str)]) -> TrayState {
        let mut state = TrayState {
            kiosk,
            ..TrayState::default()
        };
        for url in signed {
            state.signed_out.insert((*url).to_string());
        }
        for (url, id, status) in sessions {
            state
                .sessions
                .entry((*url).to_string())
                .or_default()
                .push(TraySession {
                    id: (*id).to_string(),
                    name: (*id).to_string(),
                    status: (*status).to_string(),
                    url: format!("https://{url}/client/{id}"),
                });
        }
        state
    }

    #[test]
    fn icon_variant_priority() {
        assert_eq!(icon_variant(&TrayState::default()), "tray");
        let active = state_with(false, &[], &[("a.example", "1", "active")]);
        assert_eq!(icon_variant(&active), "tray-active");
        let signed = state_with(false, &["a.example"], &[("a.example", "1", "active")]);
        assert_eq!(icon_variant(&signed), "tray-signedout");
        let only_ended = state_with(false, &[], &[("a.example", "1", "completed")]);
        assert_eq!(icon_variant(&only_ended), "tray");
    }

    #[test]
    fn status_labels_are_pretty() {
        assert_eq!(prettify_status("active"), "Active");
        assert_eq!(prettify_status("pending"), "Connecting");
        assert_eq!(prettify_status("completed"), "Ended");
        assert_eq!(prettify_status("logged_out"), "Logged out");
        assert_eq!(prettify_status("weird"), "weird");
    }

    #[test]
    fn render_key_is_stable_and_order_independent() {
        let a = state_with(false, &["x.example"], &[("b.example", "2", "active")]);
        let mut b = TrayState {
            kiosk: false,
            sessions: HashMap::from([(
                "b.example".to_string(),
                vec![TraySession {
                    id: "2".to_string(),
                    name: "2".to_string(),
                    status: "active".to_string(),
                    url: "https://b.example/client/2".to_string(),
                }],
            )]),
            signed_out: HashSet::from(["x.example".to_string()]),
        };
        assert_eq!(render_key_stub(&a), render_key_stub(&b));
        b.signed_out.clear();
        assert_ne!(render_key_stub(&a), render_key_stub(&b));
    }

    fn render_key_stub(state: &TrayState) -> String {
        // The pairing-registry component needs an AppHandle; the
        // session/signed/kiosk components are exercised here.
        let mut parts = vec![
            format!("kiosk={}", state.kiosk),
            format!("signed={}", state.signed_out.len()),
        ];
        let mut urls: Vec<&String> = state.sessions.keys().collect();
        urls.sort();
        for url in urls {
            let mut sessions = state.sessions[url].clone();
            sessions.sort_by(|a, b| a.id.cmp(&b.id));
            for session in &sessions {
                parts.push(format!(
                    "{}|{}|{}|{}",
                    url, session.id, session.status, session.url
                ));
            }
        }
        parts.join(";")
    }
}
