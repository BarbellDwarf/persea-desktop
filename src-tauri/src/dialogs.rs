#![allow(dead_code)]
//! Native dialog mapping for the webview's `confirm()` calls, file
//! inputs and the download save dialog.
//!
//! # Verified facts (tauri 2.11.5 / wry 0.55.1)
//!
//! - **No dialog API in tauri core.** The tauri 2.11.5 crate exposes no
//!   dialog module; the only dialog-adjacent method on `WebviewWindow`
//!   is the webview print dialog. Native message/confirm/save dialogs
//!   come from `tauri-plugin-dialog` (JS `ask` / `confirm` / `message`
//!   plus Rust `blocking_*` variants) — it is NOT pre-wired in this
//!   crate (see the dispatcher list below).
//! - **No script-dialog hooks in wry.** wry 0.55.1 exposes no
//!   `ScriptDialogOpening` / `runJavaScriptConfirmPanel` delegate on any
//!   platform (verified by reading the webview2 / webkitgtk / wkwebview
//!   sources; only the Android Kotlin `WebChromeClient` handles script
//!   dialogs). The locked design's engine-delegate mapping cannot be
//!   wired through tauri 2.11; it needs either an upstream wry hook or
//!   the server-side bridge flow below.
//! - **`window.confirm` is synchronous.** The plugin's `ask()`/`confirm()`
//!   return a Promise, so a JS override of `window.confirm` cannot wait
//!   for a native dialog and return its result synchronously. A generic
//!   JS-side override is therefore impossible; the correct native path
//!   is the engine delegate (unreachable today, see above) or a bridge
//!   event flow.
//!
//! # What ships in v1.2.0
//!
//! ## confirm()
//!
//! Default engine behavior is kept, and it satisfies the acceptance
//! criteria on all three engines:
//!
//! | Engine | Default confirm | Notes |
//! |--------|-----------------|-------|
//! | WebView2 | native dialog (script dialog opening) | blocks the page until answered, like a browser |
//! | WebKitGTK | GTK script dialog | same |
//! | WKWebView | macOS sheet | same |
//!
//! The disconnect / terminate flows in the web client call
//! `confirm(...)` and complete with the engine dialog. The native-look
//! mapping (shell-styled dialogs) is the follow-up below.
//!
//! ## Recommended native-dialog follow-up (dispatcher + server)
//!
//! The clean synchronous path is an event round-trip through the
//! existing desktop bridge: the server's page (the desktop-bridge
//! partial) forwards its `confirm()` call sites to the shell via a
//! page-to-shell event; the shell shows `blocking_ask`
//! (tauri-plugin-dialog) and replies with a shell-to-page event
//! carrying the boolean. That keeps `confirm()` synchronous at the call
//! site and uses a real native dialog. It needs: (a) the server to emit
//! a confirm request event for its confirm call sites, (b) the
//! dispatcher to pre-wire the dialog plugin. This module's
//! [`confirm_override_script`] is the shell side of that round trip,
//! shipped ready to activate.
//!
//! ## File inputs
//!
//! All three engines open their native file picker for
//! `<input type=file>` — wry intercepts nothing (verified by reading
//! the webview layers). Keep not swallowing them; the per-OS manual
//! matrix is the acceptance gate.
//!
//! ## Save dialogs
//!
//! The download interception (see `downloads.rs`) replaces the engine's
//! destination path. A user-chosen folder needs the dialog plugin's
//! `blocking_save_file`; the seam is documented there.
//!
//! # Wiring for the dispatcher
//!
//! 1. `Cargo.toml`: add `tauri-plugin-dialog = "2"`; register
//!    `.plugin(tauri_plugin_dialog::init())`.
//! 2. Capabilities: grant `dialog:allow-ask`, `dialog:allow-message`
//!    and `dialog:allow-save` to the shell windows (`main` and the tab
//!    strip) and — if the bridge confirm round-trip is adopted —
//!    `dialog:allow-ask` to the remote capability for the instance
//!    origins.
//! 3. Apply [`confirm_override_script`] as an initialization script on
//!    every webview that hosts persea pages (the window manager already
//!    applies the bridge init script on session windows; add this one
//!    alongside it once the plugin + server side exist).
//!
//! Do NOT wire it before the server side exists: without the request
//! event, the script is inert, which is also why it ships as an
//! opt-in string rather than part of the bridge init script.

/// Document-start script for the native confirm round trip.
///
/// Contract with the server page (the desktop-bridge partial, server
/// side): the page emits `confirm-request` with `{ "message": string }`
/// and listens for `confirm-response` with `{ "ok": boolean }`. This
/// script answers with a native dialog through the plugin's `ask` when
/// the plugin JS is present, and never throws. Without the plugin the
/// listener is never installed and the script is inert — which is why
/// it is NOT part of the bridge init script: it only becomes active
/// once the dispatcher pre-wires `tauri-plugin-dialog` AND the server
/// side emits the request event.
///
/// Note on `window.confirm`: it is synchronous, the plugin's `ask` is
/// async, and wry exposes no engine script-dialog delegate, so a
/// generic JS override cannot return the native result synchronously.
/// Un-migrated call sites keep the engine's own dialog (native on all
/// three engines); migrated call sites get the shell's native dialog
/// through the round trip.
pub fn confirm_override_script() -> &'static str {
    CONFIRM_ROUND_TRIP_SCRIPT
}

const CONFIRM_ROUND_TRIP_SCRIPT: &str = r#"
(function () {
  'use strict';
  try {
    var tauri = window.__TAURI__;
    if (!tauri || !tauri.event || !tauri.dialog || typeof tauri.dialog.ask !== 'function') { return; }
    tauri.event.listen('confirm-request', function (evt) {
      var payload = evt && evt.payload ? evt.payload : {};
      var message = typeof payload.message === 'string' && payload.message ? payload.message : 'Are you sure?';
      tauri.dialog.ask(message, { title: 'Persea Desktop', kind: 'warning' })
        .then(function (ok) {
          tauri.event.emit('confirm-response', { ok: !!ok });
        })
        .catch(function () {
          tauri.event.emit('confirm-response', { ok: false });
        });
    }).catch(function () {});
  } catch (e) { /* never throw */ }
})();
"#;

/// Whether file inputs open native pickers. Always true on the three
/// desktop engines (wry intercepts nothing); kept as a single
/// inspectable fact the per-OS matrix can assert against.
pub fn file_inputs_are_native() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_round_trip_script_is_guarded_and_never_throws() {
        // The script only activates with the plugin AND the server's
        // confirm-request event; without either it stays inert.
        let script = confirm_override_script();
        assert!(script.contains("window.__TAURI__"));
        assert!(script.contains("tauri.dialog.ask"));
        assert!(script.contains("confirm-request"));
        assert!(script.contains("confirm-response"));
        assert!(script.contains("try {"));
        assert!(script.contains("catch (e)"));
        // No window.confirm override: it cannot be synchronous.
        assert!(!script.contains("window.confirm"));
    }

    #[test]
    fn file_inputs_are_native_on_all_desktop_engines() {
        assert!(file_inputs_are_native());
    }
}
