//! Win/Super-key capture with per-platform OS hooks.
//!
//! The browser never delivers the Win/Super key to the page: the OS
//! swallows it before the webview sees it. The shell therefore captures
//! the physical key at the OS level and forwards it into the LIVE session
//! over the D04 bridge as `key-inject { keysym: 0xFFE7, down }`
//! ([`crate::bridge::emit_key_inject`]). The page's existing sendKeyEvent
//! pipeline (client.html, Meta_L = 0xFFE7) consumes that event, so the
//! toolbar Win-key button keeps working unchanged on servers without the
//! bridge.
//!
//! ## Shape
//!
//! A shared [`KeyboardHook`] trait with one implementation per platform:
//!
//! - Windows: `SetWindowsHookExW(WH_KEYBOARD_LL)` with a message-pump
//!   thread; events are consumed by returning 1 from the hook proc.
//! - Linux/X11: passive `XGrabKey` grabs on the root window (and the same
//!   path is the best-effort capture on Wayland sessions that expose an
//!   XWayland display); the grab itself consumes the key.
//! - macOS: `CGEventTap` (HID) with an Input Monitoring TCC grant;
//!   command-key events are dropped (consumed) while a session window is
//!   focused.
//! - Linux/Wayland (native): no global hook exists; see
//!   [`wayland::LIMITATION_NOTE`] and `docs/wayland-keyboard.md` for the
//!   spike result.
//!
//! [`platform_hook`] selects the implementation once per process. Every
//! implementation routes events through [`dispatch`], which owns the
//! focus gate, the repeat/ghost suppression state machine, and the bridge
//! availability gate, and reports back whether the OS should consume the
//! event.
//!
//! ## Gating
//!
//! - **Session focus**: capture and injection happen only while a session
//!   window is focused. The dispatcher (window plumbing) must call
//!   [`set_session_focus`] with `true` when a `session-*` window gains
//!   focus and `false` on focus loss, window close, and app deactivation
//!   (`windows.rs` already tracks this via `Msg::WindowFocused` /
//!   `note_window_focused`). Losing focus while Meta is held flushes a
//!   synthetic release so the remote session never sees a stuck key, and
//!   the in-flight physical release is swallowed (no ghosting).
//! - **Bridge availability**: [`crate::bridge::desktop_bridge_available`]
//!   gates the injection only. With the bridge down (old server, no S07
//!   partial), the hook still captures and consumes while a session
//!   window is focused, but forwards nothing; the toolbar Win button
//!   remains the path (no regression, no errors).
//!
//! ## Wiring for the dispatcher
//!
//! 1. `lib.rs`: `mod hooks;` (plus the Cargo.toml deps listed in
//!    `docs/wayland-keyboard.md`).
//! 2. At startup, call `hooks::platform_hook()` once and
//!    `hook.start()` if present; the probe logs the Wayland limitation on
//!    Wayland-only sessions.
//! 3. From the window plumbing's focus tracking (session window label
//!    `windows::SESSION_WINDOW_PREFIX`): call `hooks::set_session_focus`
//!    on every focus gain/loss as described above.
//!
//! ## Tests
//!
//! The state machine ([`MetaKeyState`]) is pure and unit-tested; the
//! platform files are verified on their own platforms by CI.

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Meta_L keysym, matching the page's Win-key button (client.html). Every
/// physical Win/Super/Command key maps to this keysym on all platforms.
pub const META_KEYSYM: u32 = 0xFFE7;

/// Why a platform hook could not be installed.
#[derive(Debug)]
pub enum HookError {
    /// The platform has no capture mechanism (native Wayland).
    Unsupported,
    /// The OS denied the capture (macOS Input Monitoring TCC grant).
    PermissionDenied,
    /// The underlying OS call failed.
    Failed(String),
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "no keyboard hook on this platform"),
            Self::PermissionDenied => {
                write!(f, "the OS denied input monitoring; grant it in System Settings")
            }
            Self::Failed(e) => write!(f, "keyboard hook failed: {e}"),
        }
    }
}

impl std::error::Error for HookError {}

/// A per-platform OS-level keyboard capture.
///
/// Implementations are singletons: [`start`](Self::start) is idempotent,
/// [`stop`](Self::stop) tears the capture down, and every captured event
/// flows into [`dispatch`] via the shared [`META_KEYSYM`] pipeline.
pub trait KeyboardHook: Send + Sync {
    /// Install the OS-level capture. Idempotent; safe to call again after
    /// [`stop`](Self::stop).
    fn start(&self) -> Result<(), HookError>;
    /// Session-window focus changed: `true` when a session window gained
    /// focus, `false` on focus loss. The implementation reacts only when
    /// it must (X11 arms/disarms grabs); Windows and macOS decide per
    /// event inside [`dispatch`] and do nothing here.
    fn on_session_focus(&self, focused: bool);
    /// Stop the capture. On X11 the event thread stays blocked in
    /// `wait_for_event` until the process exits (the X protocol has no
    /// wakeup); the disconnect is harmless at teardown.
    fn stop(&self);
}

static SESSION_FOCUS: AtomicBool = AtomicBool::new(false);
static KEY_STATE: Mutex<MetaKeyState> = Mutex::new(MetaKeyState::new());
static PLATFORM_HOOK: OnceLock<Option<Arc<dyn KeyboardHook>>> = OnceLock::new();

/// The per-platform hook singleton, probed once per process. `None` on
/// platforms without a capture mechanism (native Wayland, no display).
///
/// The probe is cheap: Windows/macOS construct a state holder, Linux
/// opens the X11 connection and resolves the Super keycodes. Call it once
/// at startup so the Wayland limitation note is logged early.
pub fn platform_hook() -> Option<Arc<dyn KeyboardHook>> {
    PLATFORM_HOOK.get_or_init(platform_hook_impl).clone()
}

/// Tracks whether a session window currently holds focus. The dispatcher
/// wires this from the window plumbing's focus events (see the module
/// docs); on every transition the platform hook is notified and a held
/// Meta key is flushed so the remote session never sticks.
pub fn set_session_focus(focused: bool) {
    let was_focused = SESSION_FOCUS.swap(focused, Ordering::Relaxed);
    if !focused && was_focused {
        let mut state = KEY_STATE.lock().unwrap();
        if state.focus_lost() && crate::bridge::desktop_bridge_available() {
            let _ = crate::bridge::emit_key_inject(META_KEYSYM, false);
        }
    }
    if let Some(hook) = platform_hook() {
        hook.on_session_focus(focused);
    }
}

/// Whether a session window currently holds focus.
pub fn is_session_focused() -> bool {
    SESSION_FOCUS.load(Ordering::Relaxed)
}

/// Routes a captured key transition through the focus gate and the
/// ghost-suppression state machine, then injects into the live session
/// when the bridge is available.
///
/// Returns whether the OS should consume the event (suppress the Start
/// menu, the compositor action, the command-key combos): `true` only when
/// a session window is focused and the transition is real (not a repeat,
/// not a stray release). With the bridge down the event is still consumed
/// while focused, but nothing is forwarded (the toolbar Win button is the
/// path in that deployment).
pub fn dispatch(keysym: u32, down: bool) -> bool {
    let focused = SESSION_FOCUS.load(Ordering::Relaxed);
    let mut state = KEY_STATE.lock().unwrap();
    if !state.note(down, focused) {
        return false;
    }
    if crate::bridge::desktop_bridge_available() {
        let _ = crate::bridge::emit_key_inject(keysym, down);
    }
    true
}

/// Ghost-suppression state machine for the Meta key.
///
/// Tracks the logical down/up state so auto-repeat presses and stray
/// releases (focus changed mid-press, X11 grab handoff) never produce a
/// second injection, and focus loss flushes exactly one release.
#[derive(Debug, Default)]
struct MetaKeyState {
    down: bool,
}

impl MetaKeyState {
    const fn new() -> Self {
        Self { down: false }
    }

    /// Records a transition and reports whether the platform should
    /// consume it. A press while already down is auto-repeat (ignored); a
    /// release that was not preceded by a press is stray (ignored, and it
    /// clears any stale state so later presses work again).
    fn note(&mut self, down: bool, focused: bool) -> bool {
        if !focused {
            if !down {
                self.down = false;
            }
            return false;
        }
        if down {
            if self.down {
                return false;
            }
            self.down = true;
        } else {
            if !self.down {
                return false;
            }
            self.down = false;
        }
        true
    }

    /// Focus was lost while the key might be held. Clears the state and
    /// reports whether a release must be flushed to the remote session.
    fn focus_lost(&mut self) -> bool {
        let was = self.down;
        self.down = false;
        was
    }
}

#[cfg(target_os = "windows")]
fn platform_hook_impl() -> Option<Arc<dyn KeyboardHook>> {
    Some(Arc::new(win::WinHook::new()))
}

#[cfg(target_os = "macos")]
fn platform_hook_impl() -> Option<Arc<dyn KeyboardHook>> {
    Some(Arc::new(macos::MacHook::new()))
}

#[cfg(target_os = "linux")]
fn platform_hook_impl() -> Option<Arc<dyn KeyboardHook>> {
    match x11::X11Hook::connect() {
        Ok(hook) => {
            eprintln!("[hooks] X11 keyboard hook ready (Super key capture)");
            Some(Arc::new(hook))
        }
        Err(e) => {
            if wayland::is_wayland_session() {
                eprintln!(
                    "[hooks] {}; native Wayland has no global hook: {}",
                    e,
                    wayland::LIMITATION_NOTE
                );
            } else {
                eprintln!("[hooks] {e}");
            }
            None
        }
    }
}

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod wayland;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_is_meta_l() {
        assert_eq!(META_KEYSYM, 0xFFE7);
    }

    #[test]
    fn ignores_everything_without_focus() {
        let mut state = MetaKeyState::new();
        assert!(!state.note(true, false));
        assert!(!state.note(false, false));
        assert!(!state.down);
    }

    #[test]
    fn forwards_down_then_up_while_focused() {
        let mut state = MetaKeyState::new();
        assert!(state.note(true, true));
        assert!(state.down);
        assert!(state.note(false, true));
        assert!(!state.down);
    }

    #[test]
    fn deduplicates_auto_repeat_press() {
        let mut state = MetaKeyState::new();
        assert!(state.note(true, true));
        assert!(!state.note(true, true));
        assert!(state.note(false, true));
        assert!(!state.down);
    }

    #[test]
    fn ignores_stray_release() {
        let mut state = MetaKeyState::new();
        assert!(!state.note(false, true));
        assert!(!state.down);
    }

    #[test]
    fn focus_loss_flushes_held_meta_once() {
        let mut state = MetaKeyState::new();
        assert!(state.note(true, true));
        assert!(state.focus_lost());
        assert!(!state.down);
        assert!(!state.focus_lost());
    }

    #[test]
    fn in_flight_release_after_focus_loss_is_swallowed() {
        let mut state = MetaKeyState::new();
        assert!(state.note(true, true));
        assert!(state.focus_lost());
        assert!(!state.note(false, false));
        assert!(!state.down);
    }

    #[test]
    fn fresh_press_after_focus_restore_works() {
        let mut state = MetaKeyState::new();
        assert!(state.note(true, true));
        assert!(state.focus_lost());
        assert!(state.note(true, true));
        assert!(state.note(false, true));
    }

    #[test]
    fn dispatch_requires_session_focus() {
        // Touch the focus static directly: set_session_focus probes
        // platform_hook, which would arm real grabs on a live display.
        SESSION_FOCUS.store(false, Ordering::Relaxed);
        assert!(!dispatch(META_KEYSYM, true));
        SESSION_FOCUS.store(true, Ordering::Relaxed);
        assert!(dispatch(META_KEYSYM, true));
        assert!(dispatch(META_KEYSYM, false));
        SESSION_FOCUS.store(false, Ordering::Relaxed);
        assert!(!dispatch(META_KEYSYM, false));
    }
}
