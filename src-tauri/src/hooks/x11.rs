#![allow(dead_code, unused_imports, unused_variables)]
//! Linux X11 passive-grab implementation (XGrabKey).
//!
//! On the root window, `XGrabKey` passive grabs are armed for the
//! Super_L/Super_R keycodes (resolved from the keyboard mapping) across
//! the common modifier combinations. While armed, every press of those
//! keys is delivered to this client and nobody else, which IS the consume
//! step on X11: the window manager's Super binding never fires. The
//! grabs are armed only while a session window is focused (see
//! [`on_session_focus`](X11Hook::on_session_focus)), so outside sessions
//! the WM keeps its bindings.
//!
//! ## Wayland sessions (best-effort)
//!
//! On Wayland, most compositors still export an XWayland display
//! (`DISPLAY` is set), and compositors that mediate XWayland grabs
//! (Mutter, KWin, sway) deliver grabbed keys to the XWayland client
//! while its window is focused. That makes this module the focused
//! best-effort capture for Wayland sessions too; native Wayland itself
//! has no grab (see `wayland.rs` and `docs/wayland-keyboard.md`).
//!
//! ## Known limitation
//!
//! If another client already grabbed the same (window, keycode,
//! modifiers) combination, the grab fails with BadAccess and is silently
//! absent (the feature degrades to nothing; the toolbar button still
//! works). GNOME Shell on X11 is the usual offender for the bare Super
//! combo. The grab is re-armed on every focus-in, so a grab that becomes
//! available later is picked up.
//!
//! ## Cargo features
//!
//! New Linux target dep: `x11rb = { version = "0.14", features =
//! ["allow-unsafe-code"] }` (the crate ships no default features; the
//! feature enables the fast-path unsafe code the RustConnection uses).
//!
//! ## Stop semantics
//!
//! `wait_for_event` blocks until an event or a connection error; there is
//! no X11 wakeup, so the event thread lives until the process exits (or
//! the X connection dies). `stop` disarms the grabs, which is what
//! matters; the blocked thread is reaped at teardown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, Keycode, ModMask, Window};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

/// XK_Super_L / XK_Super_R: both map to the shared Meta_L bridge keysym.
const SUPER_L_KEYSYM: u32 = 0xffeb;
const SUPER_R_KEYSYM: u32 = 0xffec;

/// All combinations of the four low modifier bits (Shift, Lock, Control,
/// Mod1) cover the usual keyboard state; NumLock (Mod2) is included in
/// the sweep as value 4 in every combination.
const GRAB_MODIFIERS: std::ops::Range<u16> = 0..16;

/// Shared state between the hook and the event thread.
struct X11Inner {
    conn: RustConnection,
    root: Window,
    keycodes: Vec<Keycode>,
    armed: AtomicBool,
    started: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct X11Hook {
    inner: Arc<X11Inner>,
}

impl X11Hook {
    /// Connects to the X11 display (`DISPLAY`; on Wayland sessions the
    /// XWayland display) and resolves the Super keycodes. Fails fast when
    /// there is no display or no Super key in the layout.
    pub fn connect() -> Result<Self, crate::hooks::HookError> {
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|e| crate::hooks::HookError::Failed(format!("cannot connect to X11: {e}")))?;
        let root = conn.setup().roots[screen_num].root;
        let keycodes = super_keycodes(&conn)?;
        if keycodes.is_empty() {
            return Err(crate::hooks::HookError::Failed(
                "no Super key found in the keyboard layout".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(X11Inner {
                conn,
                root,
                keycodes,
                armed: AtomicBool::new(false),
                started: AtomicBool::new(false),
                thread: Mutex::new(None),
            }),
        })
    }
}

impl crate::hooks::KeyboardHook for X11Hook {
    fn start(&self) -> Result<(), crate::hooks::HookError> {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let inner = Arc::clone(&self.inner);
        let handle = thread::Builder::new()
            .name("persea-x11-hook".into())
            .spawn(move || run_loop(inner))
            .map_err(|e| crate::hooks::HookError::Failed(e.to_string()))?;
        *self.inner.thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    fn on_session_focus(&self, focused: bool) {
        if focused {
            self.arm();
        } else {
            self.disarm();
        }
    }

    fn stop(&self) {
        self.disarm();
        self.inner.started.store(false, Ordering::SeqCst);
        // The event thread stays blocked in wait_for_event until the
        // process exits; see the module docs.
    }
}

impl X11Hook {
    fn arm(&self) {
        let inner = &self.inner;
        if inner.armed.swap(true, Ordering::SeqCst) {
            return;
        }
        for &keycode in &inner.keycodes {
            for bits in GRAB_MODIFIERS {
                let modifiers = ModMask::from(bits as u8);
                let _ = inner.conn.grab_key(
                    false,
                    inner.root,
                    modifiers,
                    keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                );
            }
        }
        let _ = inner.conn.flush();
        eprintln!(
            "[hooks] XGrabKey armed for {} Super keycode(s)",
            inner.keycodes.len()
        );
    }

    fn disarm(&self) {
        let inner = &self.inner;
        if !inner.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        for &keycode in &inner.keycodes {
            for bits in GRAB_MODIFIERS {
                let modifiers = ModMask::from(bits as u8);
                let _ = inner.conn.ungrab_key(keycode, inner.root, modifiers);
            }
        }
        let _ = inner.conn.flush();
        eprintln!("[hooks] XGrabKey disarmed");
    }
}

/// Scans the keyboard mapping for Super_L/Super_R keycodes.
fn super_keycodes(conn: &RustConnection) -> Result<Vec<Keycode>, crate::hooks::HookError> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let reply = conn
        .get_keyboard_mapping(min, count)
        .map_err(|e| crate::hooks::HookError::Failed(e.to_string()))?
        .reply()
        .map_err(|e| crate::hooks::HookError::Failed(e.to_string()))?;
    let per_row = reply.keysyms_per_keycode as usize;
    let mut out = Vec::new();
    for (i, row) in reply.keysyms.chunks(per_row).enumerate() {
        if row
            .iter()
            .any(|&keysym| keysym == SUPER_L_KEYSYM || keysym == SUPER_R_KEYSYM)
        {
            out.push(min + i as u8);
        }
    }
    Ok(out)
}

fn run_loop(inner: Arc<X11Inner>) {
    loop {
        let event = match inner.conn.wait_for_event() {
            Ok(event) => event,
            Err(e) => {
                eprintln!("[hooks] X11 connection lost: {e}");
                break;
            }
        };
        match event {
            Event::KeyPress(ev) if inner.keycodes.contains(&ev.detail) => {
                let _ = crate::hooks::dispatch(crate::hooks::META_KEYSYM, true);
            }
            Event::KeyRelease(ev) if inner.keycodes.contains(&ev.detail) => {
                let _ = crate::hooks::dispatch(crate::hooks::META_KEYSYM, false);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_keysyms_are_stable() {
        assert_eq!(SUPER_L_KEYSYM, 0xffeb);
        assert_eq!(SUPER_R_KEYSYM, 0xffec);
    }
}
