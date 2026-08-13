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
//! > per-instance user config (`instances.json` `kioskAllowed`) > off.
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
//!
//! No Cargo.toml or capability changes are needed by this module.
#![allow(dead_code)] // dispatcher wiring consumes the entrypoints after landing

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcut, Shortcut, ShortcutEvent, ShortcutState};
use url::Url;

/// The main window label, locked by the window plumbing.
const MAIN_WINDOW_LABEL: &str = "main";
/// The secret exit chord: `Ctrl+Alt+Shift+Q`.
pub const EXIT_CHORD: &str = "ctrl+alt+shift+q";
/// Seconds between the first chord press and the confirming second one.
pub const CHORD_CONFIRM_WINDOW_SECS: u64 = 3;

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

/// Enter kiosk mode: lock the main window (fullscreen, undecorated,
/// fixed size), hide the tab strip, disable the global hotkeys and block
/// close requests. Called by the dispatcher AFTER `windows::setup`, only
/// when [`is_active`] is already true from [`setup`]. Idempotent.
pub fn enter(app: &tauri::AppHandle) {
    {
        let mut state = match STATE.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let Some(s) = state.as_mut() else { return };
        if s.active {
            return;
        }
        s.active = true;
        s.armed_at = None;
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
    let instance = active_instance().unwrap_or_default();
    eprintln!("[kiosk] kiosk mode active on {instance}");
}

/// Leave kiosk mode: release the exit chord, restore the hotkeys and the
/// tab strip, and restore the window to windowed, decorated, resizable
/// state. The confirmation lives in [`on_chord_press`]; the chord
/// handler calls this on the confirming press.
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
    }
    crate::hotkeys::set_enabled(true);
    crate::windows::set_strip_visible(true);
    if let Some(win) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = win.set_fullscreen(false);
        let _ = win.set_decorations(true);
        let _ = win.set_resizable(true);
        let _ = win.set_maximizable(true);
    }
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
            "[kiosk] exit chord pressed; press it again within {}s to leave kiosk mode",
            CHORD_CONFIRM_WINDOW_SECS
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
