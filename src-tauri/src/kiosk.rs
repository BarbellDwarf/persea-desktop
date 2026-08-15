//! Kiosk mode: a locked-down, fullscreen presentation for thin-client
//! terminals. Soft kiosk only: the app locks its own surface, no
//! OS-level lockout is possible from an application.
//!
//! # Scope (locked design)
//!
//! ALLOWS: the instance's connections page, session pages (inline tabs
//! and popped session windows), the transfer window, session-ended
//! notifications.
//! BLOCKS: the server's admin, setup and account pages (see
//! [`path_blocked`]), the shell settings and pairing pages (unreachable:
//! the viewport stays on the instance), the tray (the tray must not be
//! created while kiosk is active; the tray feature consumes
//! [`is_active`]), the global hotkeys (consumed via
//! [`crate::hotkeys::set_enabled`]), window closing, resizing and
//! devtools.
//!
//! # Activation (locked design)
//!
//! Precedence: provision override (`provisioning::kiosk_enabled_override`)
//! \> per-instance user config (`instances.json` `kioskAllowed`) > off.
//! Two gates apply on top, both fail closed:
//!
//! 1. Server gate: the instance probe must report `kiosk_allowed`
//!    (`instances::capability`). No probe, no capability: the client
//!    never enters kiosk for a server that forbids it, and the config is
//!    ignored.
//! 2. Escape-hatch gate: the exit chord must actually register. A kiosk
//!    without an exit is a trap, so a conflicted chord (or an
//!    unsupported platform) keeps kiosk off with a loud warning.
//!
//! The decision targets the startup instance (default, else first
//! configured; the instance store's last-used fallback is not observable
//! through the accessors, and provisioned kiosk deployments always set a
//! default, which makes the mirror exact in practice).
//!
//! Mid-session entry runs the same gates: the tray's per-instance "Kiosk
//! mode" item and the settings Kiosk section both emit the shared
//! `kiosk-toggle` event, which [`setup`] listens for unconditionally (the
//! listener registers even when no kiosk was decided at startup). Entry
//! targets the instance from the event payload, re-registers the exit
//! chord (exit released it), navigates the viewport to that instance and
//! removes the tray icon; exit restores the tray. A provision pin of
//! `false` refuses mid-session entry too, so a pinned-off deployment
//! cannot be toggled into kiosk.
//!
//! # The exit chord
//!
//! `Ctrl+Alt+Shift+Q`, a global shortcut registered ONLY while kiosk is
//! active. Verification of the tauri 2.11.5 API surface: there is no
//! window-level key event hook (no `on_key_event` in tauri, tauri-runtime
//! or tauri-runtime-wry sources), so the chord goes through
//! tauri-plugin-global-shortcut, which fires at the OS level and keeps
//! working when the webview is frozen or dead. The confirmation is a
//! second press within [`CHORD_CONFIRM_WINDOW_SECS`] seconds: the first
//! press arms the exit, the second one within the window confirms it.
//! A single stray press changes nothing. A native dialog would need
//! tauri-plugin-dialog, which is declared in Cargo.toml but not resolved
//! in this tree and not registered by the dispatcher; the confirm step
//! is one function ([`on_chord_press`]) so a dialog-based confirm can
//! replace the second press once the dialog plugin is wired.
//!
//! The chord is registered at [`setup`] when kiosk is decided, released
//! at [`exit`], and registered again at every entry ([`enter_for`]): an
//! exit always leaves the chord unregistered, so re-entry runs the
//! escape-hatch gate again.
//!
//! Exiting kiosk restores the window (windowed, decorated, resizable,
//! maximizable), the tab strip and the hotkeys. A provisioned kiosk
//! (`locked`) re-enters on the next launch: the pin outlives the exit.
//!
//! # Wayland
//!
//! The global-shortcut plugin is X11-only and silently no-ops on
//! Wayland; the escape-hatch gate therefore refuses kiosk there. See
//! `docs/kiosk.md` for the full limitation notes.
//!
//! # Unreachable instance at boot
//!
//! Kiosk enters from the CACHED probe (fail closed on no probe). If the
//! instance is unreachable at boot, the web error page renders inside
//! the kiosk window and the exit chord still works: it is a global
//! shortcut, independent of the webview. There are no kiosk-specific
//! crash paths: every side effect is best-effort (`let _ =`).
//!
//! # Wiring for the dispatcher (do not forget; this module does not
//! register itself)
//!
//! 1. `mod kiosk;` in `lib.rs`.
//! 2. In the setup hook, AFTER `instances::setup` and `hotkeys::setup`,
//!    BEFORE the main window is built: `kiosk::setup(app)?`.
//! 3. On the main window builder: `.devtools(false)` when
//!    `kiosk::active()` (devtools are builder-only; release builds
//!    compile them out anyway, this closes the debug-build gap).
//! 4. AFTER `windows::setup` (the tab strip must exist):
//!    `if kiosk::active() { kiosk::enter(app.handle()); }`.
//! 5. In the windows.rs navigation handlers, in the `Decision::Allow`
//!    arm, consult `kiosk::navigation_blocked(url)` and block with a
//!    logged host when it returns true. No navigation-policy rebuild is
//!    needed: the consult reads live state. Apply it in
//!    `navigation_handler_for` AND `viewport_new_window_handler`.
//! 6. The tray feature must not create the tray while `kiosk::is_active()`.
//!    A mid-session entry removes the tray icon it was clicked from (the
//!    click's menu event has already fired by then); exit restores it.
//!
//! No Cargo.toml or capability changes are needed by this module.
#![allow(dead_code)] // dispatcher wiring consumes the entrypoints after landing

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcut, Shortcut, ShortcutEvent, ShortcutState};
use url::Url;

/// The main window label, locked by the window plumbing.
const MAIN_WINDOW_LABEL: &str = "main";
/// The secret exit chord: `Ctrl+Alt+Shift+Q`.
pub const EXIT_CHORD: &str = "ctrl+alt+shift+q";
/// Seconds between the first chord press and the confirming second one.
pub const CHORD_CONFIRM_WINDOW_SECS: u64 = 3;
/// Emitted to the shell window when a kiosk toggle cannot enter kiosk
/// mode, payload `{"instanceUrl": string, "reason": string}`. The
/// settings page listens to revert its toggle and show the reason; the
/// tray has no listener and ignores it.
pub const EVENT_KIOSK_TOGGLE_FAILED: &str = "kiosk-toggle-failed";

static STATE: Mutex<Option<KioskState>> = Mutex::new(None);

/// Live kiosk state. `active` flips on entry/exit; the rest is fixed for
/// the session.
#[derive(Debug, Clone, PartialEq)]
pub struct KioskState {
    active: bool,
    /// The provision document pins kiosk on: the user setting cannot
    /// turn it off, and the next launch re-enters kiosk after an exit.
    locked: bool,
    /// The instance the kiosk runs on.
    instance: String,
    /// When the exit chord was last pressed, for the confirm window.
    armed_at: Option<Instant>,
    /// Whether the exit chord is currently registered. `exit` releases
    /// the chord, so re-entry re-registers it (registering twice would
    /// error on the X11 backend).
    chord_registered: bool,
}

/// Outcome of one exit-chord press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordAction {
    /// First press (or a press after the confirm window lapsed): arms
    /// the exit.
    Armed,
    /// A press inside the confirm window: exit confirmed.
    Confirmed,
}

// ---------------------------------------------------------------------------
// Pure decision core (unit-testable without any tauri runtime)
// ---------------------------------------------------------------------------

/// Effective kiosk decision: provision override > user config > off,
/// gated on the server capability. The escape-hatch gate (the exit
/// chord must register) is enforced by [`setup`] after this returns
/// true; see the module docs.
pub fn decide(provision: Option<bool>, user_config: bool, server_allows: bool) -> bool {
    server_allows && provision.unwrap_or(user_config)
}

/// Chord state machine: a press confirms only when the previous press is
/// inside the confirm window.
pub fn on_chord_press(armed_at: Option<Instant>, now: Instant) -> ChordAction {
    match armed_at {
        Some(at) if now.duration_since(at) <= Duration::from_secs(CHORD_CONFIRM_WINDOW_SECS) => {
            ChordAction::Confirmed
        }
        _ => ChordAction::Armed,
    }
}

/// Path-level kiosk blocklist. Blocks the server's admin, setup and
/// account pages, including the `.html` variants the server serves
/// (`/admin.html`, `/admin/users.html`, `/account/tokens.html`).
/// Everything else passes; the navigation lockdown policy applies as in
/// normal mode.
pub fn path_blocked(path: &str) -> bool {
    const PREFIXES: [&str; 3] = ["/admin", "/setup", "/account"];
    PREFIXES.iter().any(|p| {
        path == *p
            || path
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('.'))
    })
}

/// Navigation decision for kiosk mode: consult this in the webview
/// navigation handlers AFTER the navigation policy allows a URL. Returns
/// true when the URL must be blocked. Reads live state, so kiosk entry
/// and exit take effect without rebuilding the navigation policy.
pub fn navigation_blocked(url: &Url) -> bool {
    is_active() && path_blocked(url.path())
}

// ---------------------------------------------------------------------------
// Runtime entrypoints (consumed by the dispatcher)
// ---------------------------------------------------------------------------

/// Resolve the kiosk decision at startup and, when kiosk is wanted,
/// register the exit chord. Kiosk is not entered here: the dispatcher
/// calls [`enter`] once the window manager exists (the tab strip must be
/// hideable). Never fails: unsupported or conflicted chords keep kiosk
/// off with a warning.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    register_toggle_listener(app);
    let Some(target) = startup_target() else {
        eprintln!("[kiosk] no instance configured; kiosk stays off");
        *STATE.lock().unwrap() = None;
        return Ok(());
    };
    let provision = crate::provisioning::kiosk_enabled_override();
    let user_config = target.kiosk_allowed.unwrap_or(false);
    let server_allows = crate::instances::capability(&target.url, "kiosk_allowed");
    if !decide(provision, user_config, server_allows) {
        let reason = if !server_allows {
            "the server does not advertise kiosk_allowed"
        } else if provision == Some(false) {
            "the provision document pins kiosk off"
        } else {
            "kiosk is not enabled for the instance"
        };
        eprintln!("[kiosk] kiosk stays off: {reason}");
        *STATE.lock().unwrap() = None;
        return Ok(());
    }
    match register_exit_chord(app.handle()) {
        Ok(()) => {
            *STATE.lock().unwrap() = Some(KioskState {
                active: false,
                locked: provision == Some(true),
                instance: target.url.clone(),
                armed_at: None,
                chord_registered: true,
            });
            eprintln!(
                "[kiosk] kiosk decided on {}; exit chord {}",
                target.url, EXIT_CHORD
            );
        }
        Err(e) => {
            eprintln!("[kiosk] kiosk stays off: the exit chord cannot be registered: {e}");
            *STATE.lock().unwrap() = None;
        }
    }
    Ok(())
}

/// Enter kiosk mode at startup, on the instance the startup decision
/// targeted (the dispatcher call site; see [`enter_for`] for the shared
/// implementation). Called by the dispatcher AFTER `windows::setup`, only
/// when [`is_active`] is already true from [`setup`].
pub fn enter(app: &tauri::AppHandle) {
    let instance = active_instance().or_else(|| startup_target().map(|i| i.url));
    let Some(instance) = instance else {
        return;
    };
    enter_for(app, &instance);
}

/// Enter kiosk mode for a specific instance: lock the main window
/// (fullscreen, undecorated, fixed size), hide the tab strip, disable the
/// global hotkeys, register the exit chord, block close requests, land
/// the viewport on the instance and remove the tray icon. Called by the
/// startup dispatcher (via [`enter`]) and by the `kiosk-toggle` listener
/// (tray and settings toggles). Idempotent.
pub fn enter_for(app: &tauri::AppHandle, instance_url: &str) {
    let url = instance_url.trim_end_matches('/').to_string();
    if crate::provisioning::kiosk_enabled_override() == Some(false) {
        toggle_failed(app, &url, "the provision document pins kiosk off");
        return;
    }
    if crate::instances::instance(&url).is_none() {
        toggle_failed(app, &url, "no such instance");
        return;
    }
    if !crate::instances::capability(&url, "kiosk_allowed") {
        toggle_failed(app, &url, "the server does not advertise kiosk_allowed");
        return;
    }
    {
        let mut state = match STATE.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let s = state.get_or_insert_with(|| KioskState {
            active: false,
            locked: false,
            instance: url.clone(),
            armed_at: None,
            chord_registered: false,
        });
        s.instance = url.clone();
        if s.active {
            return;
        }
        if !s.chord_registered {
            drop(state);
            if let Err(e) = register_exit_chord(app) {
                toggle_failed(
                    app,
                    &url,
                    &format!("the exit chord cannot be registered: {e}"),
                );
                return;
            }
            let mut state = match STATE.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            let Some(s) = state.as_mut() else { return };
            s.chord_registered = true;
            s.active = true;
            s.armed_at = None;
        } else {
            s.active = true;
            s.armed_at = None;
        }
    }
    crate::hotkeys::set_enabled(false);
    crate::windows::set_strip_visible(false);
    if let Some(win) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = win.set_fullscreen(true);
        let _ = win.set_decorations(false);
        let _ = win.set_resizable(false);
        let _ = win.set_maximizable(false);
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if crate::kiosk::is_active() {
                    eprintln!(
                        "[kiosk] close requests are blocked in kiosk mode; use the exit chord"
                    );
                    api.prevent_close();
                }
            }
        });
    }
    // Land the viewport on the target instance: a toggle from the
    // settings page must leave the shell page, and a tray toggle must
    // lock onto the instance it was clicked for. At startup this is
    // redundant with the dispatcher's auto-open, which navigates the
    // same instance.
    if let Err(e) = crate::instances::cmd_instances_open(app.clone(), url.clone()) {
        eprintln!("[kiosk] kiosk entered but the instance could not be opened: {e}");
    }
    crate::tray::set_kiosk(app, true);
    eprintln!("[kiosk] kiosk mode active on {url}");
}

/// Leave kiosk mode: release the exit chord, restore the hotkeys and the
/// tab strip, restore the window to windowed, decorated, resizable
/// state, and restore the tray icon (a mid-session entry removed it).
/// The confirmation lives in [`on_chord_press`]; the chord handler calls
/// this on the confirming press, and the toggle listener calls it when a
/// toggle flips kiosk off.
pub fn exit(app: &tauri::AppHandle) {
    {
        let mut state = match STATE.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let Some(s) = state.as_mut() else { return };
        if !s.active {
            return;
        }
        s.active = false;
        s.armed_at = None;
    }
    if let Err(e) = unregister_exit_chord(app) {
        eprintln!(
            "[kiosk] exit chord could not be released ({e}); it stays inert while kiosk is off"
        );
    } else if let Ok(mut state) = STATE.lock() {
        if let Some(s) = state.as_mut() {
            s.chord_registered = false;
        }
    }
    crate::hotkeys::set_enabled(true);
    crate::windows::set_strip_visible(true);
    if let Some(win) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = win.set_fullscreen(false);
        let _ = win.set_decorations(true);
        let _ = win.set_resizable(true);
        let _ = win.set_maximizable(true);
    }
    crate::tray::set_kiosk(app, false);
    eprintln!("[kiosk] kiosk mode exited; the app is back in normal mode");
}

/// Whether kiosk mode is currently active. Consumed by the dispatcher
/// (window builder flags, tray suppression) and by this module's
/// navigation and close-request checks.
pub fn is_active() -> bool {
    STATE
        .lock()
        .ok()
        .is_some_and(|s| s.as_ref().is_some_and(|s| s.active))
}

/// Whether kiosk is pinned on by the provision document (informational;
/// the settings UI consumes this to hide the kiosk toggle).
pub fn locked() -> bool {
    STATE
        .lock()
        .ok()
        .is_some_and(|s| s.as_ref().is_some_and(|s| s.locked))
}

/// The instance the kiosk runs on, when the decision is made (entered or
/// pending).
pub fn active_instance() -> Option<String> {
    STATE.lock().ok()?.as_ref().map(|s| s.instance.clone())
}

// ---------------------------------------------------------------------------
// Exit chord plumbing
// ---------------------------------------------------------------------------

/// Register the exit chord on the global-shortcut plugin. The handler
/// checks live state, so a chord left registered outside kiosk mode is
/// inert.
fn register_exit_chord(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(global) = app.try_state::<GlobalShortcut<tauri::Wry>>() else {
        return Err(
            "the global-shortcut plugin is not installed (unsupported platform)".to_string(),
        );
    };
    global
        .on_shortcut(
            EXIT_CHORD,
            |app: &tauri::AppHandle, _shortcut: &Shortcut, event: ShortcutEvent| {
                if event.state == ShortcutState::Pressed {
                    chord_pressed(app);
                }
            },
        )
        .map_err(|e| e.to_string())
}

fn unregister_exit_chord(app: &tauri::AppHandle) -> Result<(), String> {
    match app.try_state::<GlobalShortcut<tauri::Wry>>() {
        Some(global) => global.unregister(EXIT_CHORD).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// The chord handler: arm on the first press, confirm (and exit) on the
/// second press inside the confirm window. Shell-level: fires from the
/// OS regardless of webview state.
fn chord_pressed(app: &tauri::AppHandle) {
    let action = {
        let mut state = match STATE.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let Some(s) = state.as_mut() else { return };
        if !s.active {
            return;
        }
        let now = Instant::now();
        let action = on_chord_press(s.armed_at, now);
        s.armed_at = if action == ChordAction::Armed {
            Some(now)
        } else {
            None
        };
        action
    };
    match action {
        ChordAction::Armed => eprintln!(
            "[kiosk] exit chord pressed; press it again within {CHORD_CONFIRM_WINDOW_SECS}s to leave kiosk mode"
        ),
        ChordAction::Confirmed => exit(app),
    }
}

/// The startup target the kiosk decision applies to: the default
/// instance, else the first configured one. Mirrors the instance store's
/// startup selection; the last-used fallback is not observable through
/// the instance accessors (see the module docs).
fn startup_target() -> Option<crate::instances::Instance> {
    let all = crate::instances::instances();
    all.iter()
        .find(|i| i.default)
        .cloned()
        .or_else(|| all.first().cloned())
}

// ---------------------------------------------------------------------------
// Kiosk toggle (tray menu item + settings section)
// ---------------------------------------------------------------------------

/// Payload of the `kiosk-toggle` event both toggles emit, mirroring the
/// tray's `{"instanceUrl", "enabled"}` schema.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KioskToggle {
    instance_url: String,
    enabled: bool,
}

/// Register the `kiosk-toggle` listener. Unconditional: the tray and
/// settings toggles must work even when no kiosk was decided at startup.
fn register_toggle_listener(app: &mut tauri::App) {
    let handle = app.handle().clone();
    app.listen_any(crate::tray::EVENT_KIOSK_TOGGLE, move |event| {
        let Some(toggle) = serde_json::from_str::<KioskToggle>(event.payload()).ok() else {
            eprintln!(
                "[kiosk] malformed kiosk-toggle payload: {}",
                event.payload()
            );
            return;
        };
        on_toggle(&handle, &toggle);
    });
}

fn on_toggle(app: &tauri::AppHandle, toggle: &KioskToggle) {
    if toggle.enabled {
        enter_for(app, &toggle.instance_url);
    } else {
        exit(app);
    }
}

/// Log a refused entry and tell the settings page why (it reverts its
/// toggle and shows the reason).
fn toggle_failed(app: &tauri::AppHandle, instance_url: &str, reason: &str) {
    eprintln!("[kiosk] kiosk not entered for {instance_url}: {reason}");
    let _ = app.emit(
        EVENT_KIOSK_TOGGLE_FAILED,
        serde_json::json!({ "instanceUrl": instance_url, "reason": reason }),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn state(active: bool, locked: bool, instance: &str) -> KioskState {
        KioskState {
            active,
            locked,
            instance: instance.to_string(),
            armed_at: None,
            chord_registered: false,
        }
    }

    fn set_state_for_tests(state: Option<KioskState>) {
        *STATE.lock().unwrap() = state;
    }

    #[test]
    fn decide_follows_provision_over_user_config_over_off() {
        // Provision pins on: user config is irrelevant.
        assert!(decide(Some(true), false, true));
        assert!(decide(Some(true), true, true));
        // No provision: the user config decides.
        assert!(decide(None, true, true));
        assert!(!decide(None, false, true));
        // Provision pins off: user config cannot turn it on.
        assert!(!decide(Some(false), true, true));
        assert!(!decide(Some(false), false, true));
    }

    #[test]
    fn decide_fails_closed_on_the_server_gate() {
        // The server gate wins over everything, including a provision pin.
        assert!(!decide(Some(true), true, false));
        assert!(!decide(None, true, false));
        assert!(!decide(Some(false), true, false));
        assert!(!decide(None, false, false));
    }

    #[test]
    fn path_blocking_covers_admin_setup_and_account() {
        let blocked = [
            "/admin",
            "/admin/",
            "/admin.html",
            "/admin/users.html",
            "/admin/settings.html",
            "/setup",
            "/setup/",
            "/setup.html",
            "/account",
            "/account/",
            "/account/tokens.html",
            "/account/profile.html",
        ];
        for p in blocked {
            assert!(path_blocked(p), "{p} must be blocked in kiosk");
        }
        let allowed = [
            "/",
            "/connections",
            "/client/abc123?token=secret",
            "/sessions",
            "/recordings",
            "/login",
            "/auth/login",
            "/api/auth/status",
            "/docs",
            // Prefix lookalikes are not blocked.
            "/administrator",
            "/accounting",
            "/setup-guide",
        ];
        for p in allowed {
            assert!(!path_blocked(p), "{p} must not be blocked");
        }
    }

    #[test]
    fn navigation_blocking_applies_only_while_active() {
        set_state_for_tests(None);
        let admin = Url::parse("https://persea.example.com/admin/users.html").unwrap();
        let client = Url::parse("https://persea.example.com/client/abc123").unwrap();
        assert!(!navigation_blocked(&admin), "no kiosk, no blocking");
        assert!(!navigation_blocked(&client));

        set_state_for_tests(Some(state(true, false, "https://persea.example.com")));
        assert!(navigation_blocked(&admin));
        assert!(!navigation_blocked(&client));
        // Other paths on the instance stay reachable.
        let home = Url::parse("https://persea.example.com/").unwrap();
        assert!(!navigation_blocked(&home));
        // Non-instance URLs are the navigation policy's business.
        let external = Url::parse("https://idp.example.com/login").unwrap();
        assert!(!navigation_blocked(&external));

        set_state_for_tests(Some(state(false, false, "https://persea.example.com")));
        assert!(!navigation_blocked(&admin), "kiosk exited: blocking lifts");
        set_state_for_tests(None);
    }

    #[test]
    fn chord_state_machine_arms_then_confirms_within_the_window() {
        let t0 = Instant::now();
        assert_eq!(on_chord_press(None, t0), ChordAction::Armed);
        assert_eq!(
            on_chord_press(Some(t0), t0 + Duration::from_millis(500)),
            ChordAction::Confirmed
        );
        assert_eq!(
            on_chord_press(
                Some(t0),
                t0 + Duration::from_secs(CHORD_CONFIRM_WINDOW_SECS + 1)
            ),
            ChordAction::Armed,
            "a stale press re-arms instead of confirming"
        );
        assert_eq!(
            on_chord_press(
                Some(t0),
                t0 + Duration::from_secs(CHORD_CONFIRM_WINDOW_SECS)
            ),
            ChordAction::Confirmed,
            "a press exactly at the window edge confirms"
        );
    }

    #[test]
    fn toggle_payload_parses_camel_case() {
        let raw = r#"{"instanceUrl": "https://persea.example.com", "enabled": true}"#;
        let toggle: KioskToggle = serde_json::from_str(raw).unwrap();
        assert_eq!(toggle.instance_url, "https://persea.example.com");
        assert!(toggle.enabled);

        let raw = r#"{"instanceUrl": "https://persea.example.com", "enabled": false}"#;
        let toggle: KioskToggle = serde_json::from_str(raw).unwrap();
        assert!(!toggle.enabled);
    }

    #[test]
    fn accessors_reflect_the_stored_state() {
        set_state_for_tests(None);
        assert!(!is_active());
        assert!(!locked());
        assert_eq!(active_instance(), None);

        set_state_for_tests(Some(state(false, true, "https://persea.example.com")));
        assert!(!is_active(), "decided but not entered");
        assert!(locked());
        assert_eq!(
            active_instance().as_deref(),
            Some("https://persea.example.com")
        );

        set_state_for_tests(Some(state(true, true, "https://persea.example.com")));
        assert!(is_active());
        assert!(locked());
        set_state_for_tests(None);
    }
}
