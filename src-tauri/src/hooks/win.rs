#![allow(dead_code, unused_imports, unused_variables)]
//! Windows WH_KEYBOARD_LL hook implementation.
//!
//! A low-level keyboard hook is installed on a dedicated message-pump
//! thread (the system invokes the proc from the installing thread's
//! message loop, so the thread must pump). The Win keys (VK_LWIN /
//! VK_RWIN, including their SYS variants) are consumed by returning 1
//! from the proc while a session window is focused, and forwarded to the
//! bridge as Meta_L; otherwise they pass through `CallNextHookEx` and the
//! Start menu behaves normally.
//!
//! ## Panic policy (read before editing the proc)
//!
//! `low_level_kb_proc` is an `extern "system"` callback: Rust does not
//! unwind across non-unwind ABIs, so a panic inside it ABORTS the
//! process. This is intentional and equivalent to building with
//! `panic = "abort"` for the hook path: a bug in the callback must never
//! tear down the OS hook chain. Keep the proc allocation-free and
//! panic-free; [`crate::hooks::dispatch`] is guaranteed non-panicking.
//!
//! ## Focus handling
//!
//! The hook stays installed for the app's lifetime; [`crate::hooks`]
//! decides consumption per event. Installing/uninstalling per focus
//! change would race with the Start menu (the key could slip through
//! between the focus event and the hook teardown).
//!
//! ## Cargo features
//!
//! The existing `windows` target dependency needs two additional
//! features: `Win32_UI_WindowsAndMessaging` (SetWindowsHookExW,
//! UnhookWindowsHookEx, CallNextHookEx, PeekMessageW, TranslateMessage,
//! DispatchMessageW, MSG, KBDLLHOOKSTRUCT, HHOOK, WH_KEYBOARD_LL, WM_*,
//! HC_ACTION, PM_REMOVE) and `Win32_UI_Input_KeyboardAndMouse`
//! (VK_LWIN, VK_RWIN). `Win32_Foundation` (WPARAM, LPARAM, LRESULT,
//! HINSTANCE) is already enabled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, PeekMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_REMOVE, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// The idle wait of the pump loop when the queue is empty. The hook proc
/// is invoked during message retrieval, so the loop only needs to wake
/// often enough to notice [`WinHook::stop`].
const PUMP_SLEEP: Duration = Duration::from_millis(2);

pub struct WinHook {
    started: AtomicBool,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl WinHook {
    pub fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        }
    }
}

impl crate::hooks::KeyboardHook for WinHook {
    fn start(&self) -> Result<(), crate::hooks::HookError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.stop.store(false, Ordering::SeqCst);
        let stop = Arc::clone(&self.stop);
        let handle = thread::Builder::new()
            .name("persea-win-hook".into())
            .spawn(move || run_pump(stop))
            .map_err(|e| crate::hooks::HookError::Failed(e.to_string()))?;
        *self.thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    fn on_session_focus(&self, _focused: bool) {
        // The hook stays installed; the per-event consume decision lives
        // in crate::hooks::dispatch.
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
        self.started.store(false, Ordering::SeqCst);
    }
}

fn run_pump(stop: Arc<AtomicBool>) {
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_kb_proc), None, 0) }
    {
        Ok(hook) => hook,
        Err(e) => {
            eprintln!("[hooks] SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
            return;
        }
    };
    eprintln!("[hooks] WH_KEYBOARD_LL hook installed");
    let mut msg = MSG::default();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if got.as_bool() {
            if msg.message == WM_QUIT {
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        } else {
            thread::sleep(PUMP_SLEEP);
        }
    }
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    eprintln!("[hooks] WH_KEYBOARD_LL hook removed");
}

unsafe extern "system" fn low_level_kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= HC_ACTION as i32 {
        let message = wparam.0 as u32;
        if matches!(message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP) {
            let kbd = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk = kbd.vkCode as u16;
            if vk == VK_LWIN.0 || vk == VK_RWIN.0 {
                let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                if crate::hooks::dispatch(crate::hooks::META_KEYSYM, down) {
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_keys_map_to_meta_l() {
        assert_eq!(VK_LWIN.0, 0x5B);
        assert_eq!(VK_RWIN.0, 0x5C);
        assert_eq!(WH_KEYBOARD_LL.0, 13);
    }
}
