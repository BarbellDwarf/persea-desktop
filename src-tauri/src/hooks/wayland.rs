#![allow(dead_code, unused_imports, unused_variables)]
//! Wayland keyboard capture: spike result and session probe.
//!
//! Native Wayland has no global key grab: the compositor owns the
//! keyboard and no client may register a global shortcut that the
//! compositor has not approved. The full writeup, the compositor version
//! matrix and the setup steps live in `docs/wayland-keyboard.md`; this
//! module ships the probe the dispatcher uses to detect a native Wayland
//! session and the limitation note it logs.
//!
//! What actually works, in order of fidelity:
//!
//! 1. **XWayland passive grabs** (the best-effort capture shipped in
//!    [`crate::hooks::x11`]): compositors that mediate XWayland grabs
//!    (Mutter, KWin, sway) deliver grabbed keys to the XWayland client
//!    while its window is focused. Most Wayland sessions export
//!    `DISPLAY`, so the X11 hook is attempted and usually succeeds.
//! 2. **zwp_keyboard_shortcuts_inhibit_v1** (focused-session inhibit):
//!    Mutter 49+, KWin 6.6+, sway 1.11+ let the focused client inhibit
//!    compositor shortcuts, which turns Super into an ordinary key the
//!    client receives. The webview would need to hold the inhibit on its
//!    GTK surface; Tauri 2.11 exposes no surface handle, so this path is
//!    documented, not wired.
//! 3. **evdev/uinput** (setup-step fallback): a uinput device in the
//!    `input` group can capture and re-emit keys, but it is a global
//!    interception tool with privilege and security implications. It
//!    belongs in the setup flow as an explicit opt-in, not in the app.
//! 4. **org.freedesktop.portal.RemoteDesktop** is the long-term path
//!    (compositor-sanctioned key delivery), still uneven across
//!    compositors as of the spike.
//!
//! Decision: no broken global hook ships. The XWayland best-effort plus
//! the documented limitation is the shipped state.

use std::sync::OnceLock;

/// The limitation note surfaced in logs on native Wayland sessions.
pub const LIMITATION_NOTE: &str = "native Wayland has no global key capture; \
    Super key injection works through XWayland when the compositor exposes \
    DISPLAY, otherwise only the page toolbar Win button is available \
    (see docs/wayland-keyboard.md)";

static IS_WAYLAND: OnceLock<bool> = OnceLock::new();

/// Whether this session talks to a Wayland compositor (`WAYLAND_DISPLAY`
/// set or `XDG_SESSION_TYPE` wayland, mirroring `hotkeys`' detection).
/// Probed once; the session type cannot change while the app runs.
pub fn is_wayland_session() -> bool {
    *IS_WAYLAND.get_or_init(|| {
        std::env::var("WAYLAND_DISPLAY").is_ok()
            || std::env::var("XDG_SESSION_TYPE")
                .map(|t| t == "wayland")
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limitation_note_mentions_the_fallback_paths() {
        let note = LIMITATION_NOTE;
        assert!(note.contains("XWayland"));
        assert!(note.contains("docs/wayland-keyboard.md"));
    }
}
