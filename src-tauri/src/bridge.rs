//! Scoped remote-origin IPC bridge for persea pages (wayfinder/v1.2.0/D04).
//!
//! Tauri 2 gates remote-origin IPC by capability only: a capability with
//! `remote.urls` matching the page origin grants its permissions to that
//! origin, and nothing else is exposed (tauri-2.11.5 `Webview::on_message`
//! rejects any command from a remote origin without a matching remote
//! capability). The v1 `dangerousRemoteDomainIpcAccess` config key does not
//! exist in the v2 config schema (tauri-utils 2.9.3 `SecurityConfig` is
//! `deny_unknown_fields`), so the capability file is the whole mechanism.
//!
//! Capabilities are compiled into the binary at build time, so the instance
//! origins are build-time data: D02's instance provisioning writes them into
//! `src-tauri/capabilities/remote.json`, and [`register`] validates every
//! runtime-configured instance origin against that baked allowlist, failing
//! closed (unlisted origins get no bridge).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{App, AppHandle, Emitter, EventTarget, Listener, Manager};
use url::Url;

/// The baked remote capability file. `include_str!` keeps validation in sync
/// with what `tauri-build` compiled into the binary: both read this file.
const REMOTE_CAPABILITY_JSON: &str = include_str!("../capabilities/remote.json");

/// Shell-to-page event names (locked schema, mirrored by the server's S07
/// partial: `templates/partials/desktop_bridge.html` binds these).
pub const EVENT_KEY_INJECT: &str = "key-inject";
pub const EVENT_FILE_DROP: &str = "file-drop";
pub const EVENT_SESSION_COMMAND: &str = "session-command";
pub const EVENT_DESKTOP_MODE: &str = "desktop-mode";

/// Page-to-shell event names (locked schema).
pub const EVENT_SESSION_READY: &str = "session-ready";
pub const EVENT_DRIVE_BROWSER_OPEN: &str = "drive-browser-open";
pub const EVENT_SESSION_ENDED: &str = "session-ended";

const PAGE_EVENT_NAMES: [&str; 3] = [
  EVENT_SESSION_READY,
  EVENT_DRIVE_BROWSER_OPEN,
  EVENT_SESSION_ENDED,
];

/// The window label that hosts persea pages.
const SHELL_WINDOW_LABEL: &str = "main";

/// Upper bound on the buffered page-to-shell events.
const PAGE_EVENT_QUEUE_CAP: usize = 256;

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
static ALLOWED_ORIGINS: OnceLock<Vec<String>> = OnceLock::new();
static BRIDGE_AVAILABLE: OnceLock<bool> = OnceLock::new();
static PAGE_EVENT_QUEUE: OnceLock<Mutex<VecDeque<PageEvent>>> = OnceLock::new();

/// A page-to-shell event captured by [`register`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageEvent {
  /// Event name, one of [`EVENT_SESSION_READY`], [`EVENT_DRIVE_BROWSER_OPEN`],
  /// [`EVENT_SESSION_ENDED`].
  pub name: String,
  /// Raw JSON payload (`null` when the page sent no payload).
  pub payload: serde_json::Value,
}

/// Commands the shell can send to a session page (event
/// [`EVENT_SESSION_COMMAND`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionCommandKind {
  /// Enter or leave the in-session fullscreen toggle.
  Fullscreen,
  /// Close the current session.
  Close,
  /// Navigate the session to a new address.
  Navigate,
}

/// Errors from the emit helpers.
#[derive(Debug)]
pub enum BridgeError {
  /// [`register`] has not been called yet.
  NotRegistered,
  /// The bridge is disabled (no allowlisted instance origin, or the server
  /// capability probe reported the bridge as unavailable).
  Unavailable,
  /// The underlying Tauri emit failed.
  Emit(tauri::Error),
}

impl std::fmt::Display for BridgeError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::NotRegistered => write!(f, "bridge not registered"),
      Self::Unavailable => write!(f, "desktop bridge unavailable"),
      Self::Emit(e) => write!(f, "emit failed: {e}"),
    }
  }
}

impl std::error::Error for BridgeError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Emit(e) => Some(e),
      _ => None,
    }
  }
}

/// Registers the bridge: validates the configured instance origins against
/// the baked allowlist (fail closed), installs the page-to-shell event
/// listeners, and stores the app handle used by the emit helpers.
///
/// Returns the origins that passed validation; an empty list means the bridge
/// stays disabled. The caller (D02's probe flow) decides whether to report
/// [`desktop_bridge_available`] as true via [`set_bridge_available`].
///
/// The init script (see [`init_script`]) cannot be attached to an existing
/// webview: Tauri 2.11 applies initialization scripts only at webview
/// creation, and the main window is created before the setup hook runs. The
/// window plumbing (D05) must apply it to every webview that hosts persea
/// pages via `WebviewWindowBuilder::initialization_script(bridge::init_script())`.
pub fn register(app: &mut App, instance_origins: Vec<String>) -> Vec<String> {
  let _ = APP_HANDLE.set(app.handle().clone());

  let allowed = validate_origins(&instance_origins);
  let _ = ALLOWED_ORIGINS.set(allowed.clone());

  if allowed.is_empty() {
    eprintln!(
      "[bridge] no instance origin is allowlisted in src-tauri/capabilities/remote.json; \
       the desktop bridge stays disabled (fail closed)"
    );
  } else {
    eprintln!(
      "[bridge] allowlisted instance origins: {}",
      allowed.join(", ")
    );
    eprintln!(
      "[bridge] init script handoff: window plumbing (D05) must apply \
       bridge::init_script() to every webview hosting persea pages via \
       WebviewWindowBuilder::initialization_script"
    );
  }

  for name in PAGE_EVENT_NAMES {
    app.listen_any(name, move |event| {
      let payload = serde_json::from_str::<serde_json::Value>(event.payload())
        .unwrap_or(serde_json::Value::Null);
      let mut queue = PAGE_EVENT_QUEUE
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap();
      if queue.len() >= PAGE_EVENT_QUEUE_CAP {
        queue.pop_front();
      }
      queue.push_back(PageEvent {
        name: name.to_string(),
        payload,
      });
    });
  }

  allowed
}

/// The origins that passed validation. Empty until [`register`] runs.
pub fn allowed_origins() -> &'static [String] {
  ALLOWED_ORIGINS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// Sets whether the bridge is available. The dispatcher sets this from D02's
/// capability probe (the server advertises `desktop_bridge`), mirroring the
/// server's `init_allow_bridge` pattern. False until set.
pub fn set_bridge_available(available: bool) {
  let _ = BRIDGE_AVAILABLE.set(available);
}

/// Whether the bridge is available for the current deployment. False until
/// [`set_bridge_available`] runs, which matches the disabled default.
pub fn desktop_bridge_available() -> bool {
  BRIDGE_AVAILABLE.get().copied().unwrap_or(false)
}

/// The document-start script that installs the page-side listener plumbing.
///
/// It defines `window.perseaShell` (`on(name, handler)` for shell-to-page
/// events, `emit(name, payload)` for page-to-shell events) only when
/// `window.__TAURI__` is present (injected via `withGlobalTauri`, which the
/// shell enables), and stays completely inert otherwise. Every path is
/// defensive; it never throws. The server's own S07 partial
/// (`templates/partials/desktop_bridge.html`) binds the shell-to-page events
/// through its `window.perseaDesktop` API; both sides tolerate the other
/// being absent.
pub fn init_script() -> &'static str {
  INIT_SCRIPT
}

/// Validates the configured instance origins against the baked remote
/// capability allowlist. Returns the subset that matches, logging a warning
/// for every origin that is not covered (fail closed).
pub fn validate_origins(instance_origins: &[String]) -> Vec<String> {
  let capability: RemoteCapabilityFile = match serde_json::from_str(REMOTE_CAPABILITY_JSON) {
    Ok(c) => c,
    Err(e) => {
      eprintln!(
        "[bridge] cannot parse src-tauri/capabilities/remote.json ({e}); \
         refusing the bridge for all origins"
      );
      return Vec::new();
    }
  };

  let patterns = capability.remote.map(|r| r.urls).unwrap_or_default();
  if patterns.is_empty() {
    eprintln!(
      "[bridge] src-tauri/capabilities/remote.json lists no remote urls; \
       refusing the bridge for all origins"
    );
    return Vec::new();
  }

  let mut allowed = Vec::new();
  for origin in instance_origins {
    if let Ok(origin_url) = Url::parse(origin) {
      if patterns.iter().any(|p| pattern_matches_origin(p, &origin_url)) {
        allowed.push(origin.clone());
        continue;
      }
    }
    eprintln!(
      "[bridge] instance origin {origin:?} is not allowlisted in \
       src-tauri/capabilities/remote.json; refusing bridge features for it"
    );
  }
  allowed
}

/// Pure validation of origins against a pattern list (test seam; see
/// [`validate_origins`] for the baked-file wrapper).
fn validate_origins_against(patterns: &[String], instance_origins: &[String]) -> Vec<String> {
  let mut allowed = Vec::new();
  for origin in instance_origins {
    if let Ok(origin_url) = Url::parse(origin) {
      if patterns.iter().any(|p| pattern_matches_origin(p, &origin_url)) {
        allowed.push(origin.clone());
      }
    }
  }
  allowed
}

/// Whether a remote capability URL pattern covers the origin. Patterns are
/// expected in `scheme://host[:port]` form; a leading `*.` on the host allows
/// subdomains. A pattern without a port covers any port, matching URLPattern
/// semantics. Anything else (wildcard schemes, malformed URLs) is a miss, so
/// validation fails closed.
fn pattern_matches_origin(pattern: &str, origin: &Url) -> bool {
  let Ok(pattern_url) = Url::parse(pattern) else {
    eprintln!("[bridge] malformed remote urls entry {pattern:?}; ignoring it");
    return false;
  };
  if pattern_url.scheme() != origin.scheme() {
    return false;
  }
  let Some(pattern_host) = pattern_url.host_str() else {
    return false;
  };
  let Some(origin_host) = origin.host_str() else {
    return false;
  };
  if let Some(suffix) = pattern_host.strip_prefix("*.") {
    if origin_host != suffix && !origin_host.ends_with(&format!(".{suffix}")) {
      return false;
    }
  } else if pattern_host != origin_host {
    return false;
  }
  if let Some(pattern_port) = pattern_url.port() {
    if origin.port() != Some(pattern_port) {
      return false;
    }
  }
  true
}

/// Emits `key-inject` to the shell window (Win-key injection).
pub fn emit_key_inject(keysym: u32, down: bool) -> Result<(), BridgeError> {
  emit_to_main(
    EVENT_KEY_INJECT,
    KeyInjectPayload { keysym, down },
  )
}

/// Emits `file-drop` to the shell window (drag-drop transfer).
pub fn emit_file_drop(paths: Vec<String>) -> Result<(), BridgeError> {
  emit_to_main(EVENT_FILE_DROP, FileDropPayload { paths })
}

/// Emits `session-command` to the shell window (fullscreen/close/navigate).
pub fn emit_session_command(
  cmd: SessionCommandKind,
  arg: Option<String>,
) -> Result<(), BridgeError> {
  emit_to_main(
    EVENT_SESSION_COMMAND,
    SessionCommandPayload {
      cmd,
      arg,
    },
  )
}

/// Emits `desktop-mode` to the shell window (the page hides its V03 tab bar
/// when on; D05 relies on this).
pub fn emit_desktop_mode(on: bool) -> Result<(), BridgeError> {
  emit_to_main(EVENT_DESKTOP_MODE, DesktopModePayload { on })
}

/// Drains the buffered page-to-shell events.
pub fn drain_page_events() -> Vec<PageEvent> {
  PAGE_EVENT_QUEUE
    .get()
    .map(|queue| {
      let mut queue = queue.lock().unwrap();
      queue.drain(..).collect()
    })
    .unwrap_or_default()
}

fn emit_to_main(
  event: &str,
  payload: impl Serialize + Clone,
) -> Result<(), BridgeError> {
  if !desktop_bridge_available() {
    return Err(BridgeError::Unavailable);
  }
  let Some(handle) = APP_HANDLE.get() else {
    return Err(BridgeError::NotRegistered);
  };
  handle
    .emit_to(EventTarget::webview_window(SHELL_WINDOW_LABEL), event, payload)
    .map_err(BridgeError::Emit)
}

#[derive(Debug, Clone, Serialize)]
struct KeyInjectPayload {
  keysym: u32,
  down: bool,
}

#[derive(Debug, Clone, Serialize)]
struct FileDropPayload {
  paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionCommandPayload {
  cmd: SessionCommandKind,
  #[serde(skip_serializing_if = "Option::is_none")]
  arg: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DesktopModePayload {
  on: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteCapabilityFile {
  identifier: String,
  #[serde(default)]
  windows: Vec<String>,
  remote: Option<CapabilityRemote>,
  #[serde(default)]
  permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CapabilityRemote {
  urls: Vec<String>,
}

const INIT_SCRIPT: &str = r#"
(function () {
  'use strict';
  try {
    var tauri = window.__TAURI__;
    if (!tauri || !tauri.event) { return; }
    var api = window.perseaShell;
    if (!api) {
      api = {};
      try {
        Object.defineProperty(window, 'perseaShell', { value: api, configurable: true });
      } catch (e) {
        window.perseaShell = api;
      }
    }
    if (typeof api.on !== 'function') {
      api.on = function (name, handler) {
        if (typeof handler !== 'function') { return function () {}; }
        try {
          return tauri.event.listen(name, function (evt) {
            try {
              handler(evt && evt.payload !== undefined ? evt.payload : evt);
            } catch (e) { /* handler errors must not break the page */ }
          }).catch(function () { return function () {}; });
        } catch (e) {
          return function () {};
        }
      };
    }
    if (typeof api.emit !== 'function') {
      api.emit = function (name, payload) {
        try {
          return tauri.event.emit(name, payload).catch(function () {});
        } catch (e) {
          return Promise.resolve();
        }
      };
    }
  } catch (e) { /* never throw */ }
})();
"#;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn remote_capability_grants_only_event_default() {
    let capability: RemoteCapabilityFile =
      serde_json::from_str(REMOTE_CAPABILITY_JSON).expect("remote.json must parse");
    assert_eq!(capability.identifier, "remote");
    assert!(
      capability.windows.contains(&SHELL_WINDOW_LABEL.to_string()),
      "the remote capability must be scoped to the main window"
    );
    assert_eq!(
      capability.permissions,
      vec!["core:event:default".to_string()],
      "the remote capability must grant ONLY core:event:default"
    );
    let urls = capability
      .remote
      .expect("remote capability must carry remote.urls")
      .urls;
    assert!(
      urls.iter().all(|u| !u.contains('?') && !u.contains('#')),
      "remote urls must be origin patterns, not full URLs with query/fragment"
    );
  }

  #[test]
  fn local_capability_has_no_remote_scope() {
    let raw: serde_json::Value = serde_json::from_str(include_str!("../capabilities/default.json"))
      .expect("default.json must parse");
    assert!(
      raw.get("remote").is_none(),
      "default.json must not open remote-origin IPC"
    );
  }

  #[test]
  fn tauri_conf_enables_the_global_tauri_api() {
    let raw: serde_json::Value =
      serde_json::from_str(include_str!("../tauri.conf.json"))
        .expect("tauri.conf.json must parse");
    assert_eq!(
      raw["app"]["withGlobalTauri"], true,
      "withGlobalTauri must be on so remote persea pages get window.__TAURI__"
    );
    assert!(
      raw["app"]["security"].get("dangerousRemoteDomainIpcAccess").is_none(),
      "dangerousRemoteDomainIpcAccess does not exist in the v2 config schema"
    );
  }

  #[test]
  fn pattern_matching_is_exact_by_default() {
    let origin = Url::parse("https://persea.example.com:8443").unwrap();
    assert!(pattern_matches_origin("https://persea.example.com:8443", &origin));
    assert!(pattern_matches_origin("https://persea.example.com", &origin));
    assert!(!pattern_matches_origin("https://other.example.com:8443", &origin));
    assert!(!pattern_matches_origin("http://persea.example.com:8443", &origin));
    assert!(!pattern_matches_origin("https://persea.example.com:9443", &origin));
  }

  #[test]
  fn subdomain_wildcards_work_and_malformed_patterns_fail_closed() {
    let origin = Url::parse("https://persea.example.com").unwrap();
    assert!(pattern_matches_origin("https://*.example.com", &origin));
    assert!(!pattern_matches_origin("https://*.other.com", &origin));
    assert!(!pattern_matches_origin("https://*.example.com:8443", &origin));
    assert!(!pattern_matches_origin("*://persea.example.com", &origin));
    assert!(!pattern_matches_origin("not a url", &origin));
  }

  #[test]
  fn validate_origins_filters_unlisted_origins() {
    let patterns = vec!["https://*.example.com".to_string()];
    let origins = vec![
      "https://persea.example.com".to_string(),
      "https://evil.example.net".to_string(),
    ];
    let allowed = validate_origins_against(&patterns, &origins);
    assert_eq!(allowed, vec!["https://persea.example.com".to_string()]);
  }

  #[test]
  fn baked_capability_with_no_urls_fails_closed() {
    let capability: RemoteCapabilityFile =
      serde_json::from_str(REMOTE_CAPABILITY_JSON).expect("remote.json must parse");
    let urls = capability
      .remote
      .expect("remote capability must carry remote.urls")
      .urls;
    let allowed = validate_origins(&["https://anything.example.com".to_string()]);
    if urls.is_empty() {
      assert!(
        allowed.is_empty(),
        "an empty remote urls list must refuse every origin"
      );
    }
  }

  #[test]
  fn init_script_guards_on_the_tauri_api_and_never_throws() {
    let script = init_script();
    assert!(script.contains("window.__TAURI__"));
    assert!(script.contains("perseaShell"));
    assert!(script.contains("try {"));
    assert!(script.contains("catch (e)"));
  }
}
