#![allow(dead_code, unused_imports, unused_variables)]
//! macOS CGEventTap implementation.
//!
//! A HID-level event tap captures the Command keys (the macOS Win/Super
//! equivalent). While a session window is focused, Command press/release
//! events are consumed (the callback returns NULL, dropping the event
//! from the queue) and forwarded to the bridge as Meta_L; every other
//! event passes through untouched, and with no session focus the tap is a
//! no-op passthrough, so other apps are never affected.
//!
//! ## Why the tap is created with raw FFI
//!
//! The published core-graphics versions (0.23.2, 0.24.0) cannot drop
//! events: their internal shim turns a callback `None` into "keep the
//! original event", so consumption is impossible. The `Drop` variant
//! exists only on the unreleased main branch. The tap is therefore
//! created here with direct `CGEventTapCreate` declarations against the
//! CoreGraphics framework (the approach used by rdev/enigo); the
//! core-graphics crate still supplies the event types.
//!
//! ## TCC permission
//!
//! HID event taps require the **Input Monitoring** privacy grant. The
//! tap is preflighted with `CGPreflightListenEventAccess`; when it is not
//! granted, `CGRequestListenEventAccess` raises the system prompt and
//! [`start`](MacHook::start) fails with
//! [`PermissionDenied`](crate::hooks::HookError::PermissionDenied) if the
//! user declines. Call `start` from the main thread so the prompt is
//! presented correctly. Accessibility (`AXIsProcessTrusted`) is only
//! needed to POST events, which this implementation never does; it is
//! documented here because a future re-synthesis path would need it.
//!
//! ## Consume semantics
//!
//! Only the modifier FlagsChanged events for the two Command keycodes are
//! consumed (modifiers produce FlagsChanged, never KeyDown/KeyUp). The
//! keys that complete a combo are delivered to the webview normally; the
//! page's existing Meta_L handling (client.html) composes them with the
//! bridge-injected Meta state. Because the tap drops the Command press
//! before the WindowServer sees it, the app switcher and system Command
//! shortcuts stay off while a session window is focused, and return the
//! instant focus leaves (the dispatcher's `set_session_focus(false)`).
//!
//! ## Cargo features
//!
//! Two target deps: `core-graphics = "=0.24.0"` (CGEventTapLocation,
//! CGEventTapPlacement, CGEventTapOptions, CGEventType, EventField,
//! KeyCode, CGEventFlags) and `core-foundation = "0.10"` (CFMachPort,
//! CFRunLoop, kCFRunLoopCommonModes, TCFType). The tap functions
//! themselves are declared here against the CoreGraphics framework.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    EventField, KeyCode,
};

/// Opaque CoreGraphics event reference (kCFNull-droppable by the tap).
/// c_void keeps the extern declarations clippy-FFI-clean.
type CGEventRef = *const c_void;

/// Opaque tap proxy passed to the callback.
type CGEventTapProxy = *const c_void;

/// The C callback type: return NULL to consume the event, the event ref
/// to pass it through.
type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    etype: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOptions,
        events_of_interest: u64,
        callback: Option<CGEventTapCallBack>,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    /// Whether the app may listen for input events (Input Monitoring TCC).
    fn CGPreflightListenEventAccess() -> bool;
    /// Prompt the user for the Input Monitoring grant. Returns the grant
    /// state after the prompt.
    fn CGRequestListenEventAccess() -> bool;
}

/// The tap callback runs on the run-loop thread; this struct is the
/// shared state between the hook and the thread.
struct MacRuntime {
    runloop: Mutex<Option<CFRunLoop>>,
}

pub struct MacHook {
    started: AtomicBool,
    runtime: Arc<MacRuntime>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl MacHook {
    pub fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            runtime: Arc::new(MacRuntime {
                runloop: Mutex::new(None),
            }),
            thread: Mutex::new(None),
        }
    }
}

impl crate::hooks::KeyboardHook for MacHook {
    fn start(&self) -> Result<(), crate::hooks::HookError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if !unsafe { CGPreflightListenEventAccess() } && !unsafe { CGRequestListenEventAccess() } {
            self.started.store(false, Ordering::SeqCst);
            return Err(crate::hooks::HookError::PermissionDenied);
        }
        let runtime = Arc::clone(&self.runtime);
        let handle = thread::Builder::new()
            .name("persea-macos-hook".into())
            .spawn(move || run_tap(runtime))
            .map_err(|e| crate::hooks::HookError::Failed(e.to_string()))?;
        *self.thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    fn on_session_focus(&self, _focused: bool) {
        // The tap stays enabled; the per-event consume decision lives in
        // crate::hooks::dispatch. Enabling/disabling per focus change
        // would race with the app switcher.
    }

    fn stop(&self) {
        if let Some(runloop) = self.runtime.runloop.lock().unwrap().take() {
            runloop.stop();
            // Join only when the run loop was observed: if stop raced
            // ahead of the thread's slot store, joining could hang on a
            // run loop that never stops (one detached thread at exit is
            // harmless).
            if let Some(handle) = self.thread.lock().unwrap().take() {
                let _ = handle.join();
            }
        }
        self.started.store(false, Ordering::SeqCst);
    }
}

fn run_tap(runtime: Arc<MacRuntime>) {
    // kCGEventFlagCommand; Events.h: command key bit of the event flags.
    let event_mask: u64 = 1 << (CGEventType::FlagsChanged as u64);
    let port = unsafe {
        CGEventTapCreate(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            event_mask,
            Some(tap_callback),
            std::ptr::null_mut(),
        )
    };
    let port = match std::ptr::NonNull::new(port) {
        Some(port) => port,
        None => {
            eprintln!("[hooks] CGEventTapCreate failed; check Input Monitoring in System Settings");
            return;
        }
    };
    // SAFETY: CGEventTapCreate returned a new reference; the port is
    // released when this CFMachPort drops at thread exit.
    let port: CFMachPort = unsafe { CFMachPort::wrap_under_create_rule(port.as_ptr()) };
    let source = match port.create_runloop_source(0) {
        Ok(source) => source,
        Err(()) => {
            eprintln!("[hooks] cannot create the tap run-loop source");
            return;
        }
    };
    let runloop = CFRunLoop::get_current();
    runloop.add_source(&source, unsafe { kCFRunLoopCommonModes });
    unsafe { CGEventTapEnable(port.as_concrete_TypeRef(), true) };
    eprintln!("[hooks] CGEventTap active (Command key capture)");
    *runtime.runloop.lock().unwrap() = Some(runloop);
    CFRunLoop::run_current();
    eprintln!("[hooks] CGEventTap run loop stopped");
}

/// Consumes Command-key FlagsChanged events while a session window is
/// focused and forwards them as Meta_L; passes everything else through.
unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if !matches!(etype, CGEventType::FlagsChanged) {
        return event;
    }
    let keycode = CGEventGetIntegerValueField(event, EventField::KEYBOARD_EVENT_KEYCODE) as u16;
    if !matches!(keycode, KeyCode::COMMAND | KeyCode::RIGHT_COMMAND) {
        return event;
    }
    let flags = CGEventFlags::from_bits_truncate(CGEventGetFlags(event));
    let down = flags.contains(CGEventFlags::CGEventFlagCommand);
    if crate::hooks::dispatch(crate::hooks::META_KEYSYM, down) {
        std::ptr::null_mut()
    } else {
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_keycodes_are_known() {
        assert_eq!(KeyCode::COMMAND, 0x37);
        assert_eq!(KeyCode::RIGHT_COMMAND, 0x36);
    }

    #[test]
    fn flags_changed_mask_is_the_command_bit() {
        assert_eq!(1u64 << (CGEventType::FlagsChanged as u64), 1 << 12);
    }
}
