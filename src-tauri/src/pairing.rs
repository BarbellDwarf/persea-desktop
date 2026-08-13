//! Device pairing: the shell's device-code client.
//!
//! Flow (locked design):
//! 1. The user clicks "Pair this device"; `pairing_start` POSTs
//!    `/api/desktop/pair` (anonymous) with the device hostname and the
//!    shell shows the 8-char code in a modal.
//! 2. "Open pairing page" navigates the main window webview to the
//!    instance's account tokens page (`{url}/account/tokens.html`),
//!    where the logged-in user pastes the code and confirms. This works
//!    with ANY auth method (OIDC, SAML, LDAP, database): the webview
//!    holds the session.
//! 3. A Rust poll loop GETs `/api/desktop/pair/status?code=...` every 3
//!    seconds until approved, with a 10-minute timeout and a cancel
//!    button. The server answers 410 Gone for expired and already-used
//!    codes, which map to distinct terminal states.
//! 4. The minted token is stored in the OS keychain (keyring commands,
//!    service `dev.persea.desktop`, user = `<instance url>/desktop-token/<id>`)
//!    and registered for the session poller and the transfer flow.
//!
//! Identity model (locked): the desktop app has no identity of its own;
//! the paired token belongs to whichever user holds the webview session
//! that confirms the code. Per-identity tokens: the keyring user is
//! instance + token id, and token ids are minted per confirming user, so
//! a multi-identity setup (day-to-day account + admin account) gets one
//! token per identity, each revocable separately from its own identity's
//! token list on the server. Multiple identities therefore mean multiple
//! pairings; re-pairing from the same identity replaces its token (the
//! server revokes same-named tokens on re-pair, mirrored by the local
//! registry).
//!
//! Server gating (locked): pairing is only offered when the instance
//! probe reports the `desktop_pairing` capability (compiled AND admin
//! toggle). `pairing_supported` fails closed: no probe, no pairing UI.
//!
//! Poll cadence: the loop ticks every 3 seconds per the locked design;
//! the server rate-limits status polls to 10/min/code, so the client
//! treats 429 responses as transient and keeps polling (the effective
//! cadence settles near the server limit). A 10-minute client timeout
//! mirrors the server-side pairing TTL.
//!
//! Consumption points:
//! - session poller: `registered_tokens`, `token_for`, `token_secret`
//!   provide the Bearer tokens per instance.
//! - transfers: the revoke command shows the Bearer + CSRF pattern for
//!   state-changing native calls.
//! - `instances::capability` gates the whole feature.

//! `registered_tokens`, `token_for` and `token_secret` are contract
//! surface for the poller that lands after this module; they carry an
//! allow until wired in.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::http;
use crate::instances;
use crate::keyring::{self, SERVICE_NAME};

/// Poll interval for the pairing status endpoint (locked design: 3s).
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Overall poll timeout (locked design: 10 min, matches the server-side
/// pairing TTL).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(600);
/// Account page where the logged-in user confirms the code.
const CONFIRM_PAGE: &str = "/account/tokens.html";
/// Keyring user separator: `<instance url>/desktop-token/<token id>`.
const TOKEN_USER_SEP: &str = "/desktop-token/";
/// Registry file name in the app data dir.
const REGISTRY_FILE: &str = "pairing.json";
/// Hostname length cap (the server caps at 64 too).
const MAX_HOSTNAME_LEN: usize = 64;

/// Per-instance pairing state, surfaced to the shell as
/// `{"status": ..., ...}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PairingState {
    /// No pairing has been started.
    Idle,
    /// Code shown, poll loop running.
    Waiting { code: String, expires_at: String },
    /// The code was approved and the token is in the keychain.
    Approved {
        token_id: i64,
        token_name: String,
        device_name: String,
    },
    /// The server reported the code expired (410).
    Expired,
    /// The server reported the code already used (410).
    Used,
    /// The poll loop reached the 10-minute timeout.
    TimedOut,
    /// The user cancelled.
    Cancelled,
    /// Something went wrong (server error, keychain failure).
    Failed { message: String },
}

/// Events the poll loop and the user actions produce; [`apply_event`]
/// is the unit-tested state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum PairingEvent {
    /// A status poll returned `pending` (or a transient failure).
    PollTick,
    /// The server approved the code and handed out the token.
    Approved {
        token_id: i64,
        token_name: String,
        device_name: String,
    },
    /// The poll reached the 10-minute timeout.
    Timeout,
    /// The server reported the code expired.
    Expired,
    /// The server reported the code already used.
    Used,
    /// The user cancelled.
    Cancel,
    /// Persistence failed after approval.
    Failed(String),
}

/// Pure pairing state machine. Returns `(next_state, terminal)`;
/// terminal means the poll loop stops.
pub fn apply_event(state: PairingState, event: PairingEvent) -> (PairingState, bool) {
    use PairingEvent as Evt;
    use PairingState::*;
    match (state, event) {
        (Waiting { code, expires_at }, Evt::PollTick) => (Waiting { code, expires_at }, false),
        (Waiting { .. }, Evt::Timeout) => (TimedOut, true),
        (Waiting { .. }, Evt::Expired) => (Expired, true),
        (Waiting { .. }, Evt::Used) => (Used, true),
        (Waiting { .. }, Evt::Cancel) => (Cancelled, true),
        (Waiting { .. }, Evt::Failed(message)) => (Failed { message }, true),
        (
            Waiting { .. },
            Evt::Approved {
                token_id,
                token_name,
                device_name,
            },
        ) => (
            Approved {
                token_id,
                token_name,
                device_name,
            },
            true,
        ),
        (other, _) => (other, false),
    }
}

/// One in-flight pairing attempt per instance. A newer `pairing_start`
/// replaces the session and bumps `generation`, which orphans the
/// previous poll loop.
struct PairingSession {
    state: PairingState,
    started: Instant,
    generation: u64,
}

static SESSIONS: Mutex<Option<HashMap<String, PairingSession>>> = Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn with_session<R>(instance_url: &str, f: impl FnOnce(&mut PairingSession) -> R) -> Option<R> {
    let mut guard = SESSIONS.lock().ok()?;
    let map = guard.get_or_insert_with(HashMap::new);
    let key = instance_url.trim_end_matches('/');
    let session = map
        .entry(key.to_string())
        .or_insert_with(|| PairingSession {
            state: PairingState::Idle,
            started: Instant::now(),
            generation: 0,
        });
    Some(f(session))
}

fn session_code(instance_url: &str) -> Option<String> {
    with_session(instance_url, |s| match &s.state {
        PairingState::Waiting { code, .. } => Some(code.clone()),
        _ => None,
    })
    .flatten()
}

fn finish_session(instance_url: &str, generation: u64, event: PairingEvent) {
    with_session(instance_url, |s| {
        if s.generation != generation {
            return;
        }
        s.state = apply_event(s.state.clone(), event).0;
    });
}

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Result of one status poll.
#[derive(Debug, Clone, PartialEq)]
enum PollOutcome {
    Pending,
    Approved {
        token_id: i64,
        token_name: String,
        device_name: String,
        token: String,
    },
    Expired,
    Used,
    /// 429 rate limit, 5xx, or network failure: keep polling.
    Transient,
}

/// One status poll: `GET /api/desktop/pair/status?code=...`.
async fn poll_once(http: &http::ShellHttp, instance_url: &str, code: &str) -> PollOutcome {
    let path = format!("/api/desktop/pair/status?code={code}");
    match http.get(instance_url, &path, None).await {
        Ok(result) if result.status == StatusCode::OK => {
            match result.body.get("status").and_then(|s| s.as_str()) {
                Some("approved") => PollOutcome::Approved {
                    token_id: result
                        .body
                        .get("token_id")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    token: result
                        .body
                        .get("token")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    token_name: result
                        .body
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    device_name: result
                        .body
                        .get("device_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
                _ => PollOutcome::Pending,
            }
        }
        Ok(result) if result.status == StatusCode::GONE => match result.server_error().as_deref() {
            Some(msg) if msg.contains("expired") => PollOutcome::Expired,
            Some(msg) if msg.contains("used") => PollOutcome::Used,
            _ => PollOutcome::Expired,
        },
        Ok(_) | Err(_) => PollOutcome::Transient,
    }
}

/// Spawns the poll loop for a freshly started pairing. The loop owns the
/// code lifecycle: tick every 3s, stop on terminal state, superseded
/// generation, or timeout; on approval it persists the token to the
/// keychain and the registry before flipping the state.
fn spawn_poll(app: AppHandle, instance_url: String) {
    let http = http::shell_http();
    let generation = with_session(&instance_url, |s| s.generation).unwrap_or(0);
    tauri::async_runtime::spawn(async move {
        loop {
            http::sleep(POLL_INTERVAL).await;
            enum Step {
                Stop,
                Poll,
            }
            let step = with_session(&instance_url, |s| {
                if s.generation != generation {
                    return None; // superseded by a newer pairing
                }
                if !matches!(s.state, PairingState::Waiting { .. }) {
                    return Some(Step::Stop); // cancelled or already terminal
                }
                if s.started.elapsed() >= POLL_TIMEOUT {
                    s.state = apply_event(s.state.clone(), PairingEvent::Timeout).0;
                    return Some(Step::Stop);
                }
                Some(Step::Poll)
            })
            .flatten();
            match step {
                None | Some(Step::Stop) => return,
                Some(Step::Poll) => {}
            }
            let Some(code) = session_code(&instance_url) else {
                return;
            };
            match poll_once(http, &instance_url, &code).await {
                PollOutcome::Pending | PollOutcome::Transient => {}
                PollOutcome::Approved {
                    token_id,
                    token_name,
                    device_name,
                    token,
                } => {
                    // A superseded poll loop must not persist a token
                    // minted for a replaced pairing.
                    let current = with_session(&instance_url, |s| s.generation == generation)
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                    let persisted = persist_approval(
                        &app,
                        &instance_url,
                        token_id,
                        &token_name,
                        &device_name,
                        &token,
                    )
                    .await;
                    match persisted {
                        Ok(()) => finish_session(
                            &instance_url,
                            generation,
                            PairingEvent::Approved {
                                token_id,
                                token_name,
                                device_name,
                            },
                        ),
                        Err(message) => {
                            finish_session(&instance_url, generation, PairingEvent::Failed(message))
                        }
                    }
                    return;
                }
                PollOutcome::Expired => {
                    finish_session(&instance_url, generation, PairingEvent::Expired);
                    return;
                }
                PollOutcome::Used => {
                    finish_session(&instance_url, generation, PairingEvent::Used);
                    return;
                }
            }
        }
    });
}

/// Persists an approved token: keychain first (the credential), then the
/// registry (metadata). On re-pair the server revokes same-named tokens,
/// so the registry drops the replaced entries and their keychain secrets
/// here too.
async fn persist_approval(
    app: &AppHandle,
    instance_url: &str,
    token_id: i64,
    token_name: &str,
    device_name: &str,
    token: &str,
) -> Result<(), String> {
    let user = keyring_user(instance_url, token_id);
    keyring::keyring_set(
        SERVICE_NAME.to_string(),
        user,
        token.to_string(),
        app.clone(),
    )
    .await?;
    let removed_ids = with_registry(app, |registry| {
        registry.register(RegisteredToken {
            instance_url: instance_url.trim_end_matches('/').to_string(),
            token_id,
            token_name: token_name.to_string(),
            device_name: device_name.to_string(),
            created_at: now_secs(),
        })
    })?;
    for old_id in removed_ids {
        let old_user = keyring_user(instance_url, old_id);
        let _ = keyring::keyring_delete(SERVICE_NAME.to_string(), old_user, app.clone()).await;
    }
    with_registry(app, |registry| registry.save())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Token registry (metadata; the secrets live in the keychain)
// ---------------------------------------------------------------------------

/// A registered paired token. Persisted in `pairing.json` next to the
/// instance store; the secret itself stays in the OS keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredToken {
    #[serde(rename = "instanceUrl")]
    pub instance_url: String,
    #[serde(rename = "tokenId")]
    pub token_id: i64,
    #[serde(rename = "tokenName")]
    pub token_name: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct RegistryFile {
    #[serde(default)]
    tokens: Vec<RegisteredToken>,
}

#[derive(Debug, Clone)]
struct TokenRegistry {
    path: PathBuf,
    file: RegistryFile,
}

impl TokenRegistry {
    fn load(path: PathBuf) -> Self {
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<RegistryFile>(&raw) {
                Ok(file) => Self { path, file },
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(&path, &backup);
                    eprintln!(
                        "persea-desktop: pairing registry unreadable ({e}); \
                         backed up to {}; starting with an empty registry",
                        backup.display()
                    );
                    Self {
                        path,
                        file: RegistryFile::default(),
                    }
                }
            },
            Err(_) => Self {
                path,
                file: RegistryFile::default(),
            },
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(&self.file).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Adds a token and returns the ids of the entries it replaced
    /// (same instance and token name, mirroring the server's re-pair
    /// revocation; the caller drops their keychain secrets).
    fn register(&mut self, token: RegisteredToken) -> Vec<i64> {
        let mut removed = Vec::new();
        self.file.tokens.retain(|t| {
            let replaced = t.instance_url == token.instance_url
                && (t.token_id == token.token_id || t.token_name == token.token_name);
            if replaced {
                removed.push(t.token_id);
            }
            !replaced
        });
        self.file.tokens.push(token);
        removed
    }

    fn remove(&mut self, instance_url: &str, token_id: i64) -> bool {
        let before = self.file.tokens.len();
        self.file
            .tokens
            .retain(|t| !(t.instance_url == instance_url && t.token_id == token_id));
        self.file.tokens.len() != before
    }

    fn for_instance(&self, instance_url: &str) -> Vec<RegisteredToken> {
        self.file
            .tokens
            .iter()
            .filter(|t| t.instance_url == instance_url)
            .cloned()
            .collect()
    }

    fn latest(&self, instance_url: &str) -> Option<&RegisteredToken> {
        self.file
            .tokens
            .iter()
            .filter(|t| t.instance_url == instance_url)
            .max_by_key(|t| t.created_at)
    }
}

static REGISTRY: Mutex<Option<Arc<Mutex<TokenRegistry>>>> = Mutex::new(None);

fn registry_handle(app: &AppHandle) -> Result<Arc<Mutex<TokenRegistry>>, String> {
    let mut guard = REGISTRY
        .lock()
        .map_err(|_| "pairing registry lock poisoned".to_string())?;
    if guard.is_none() {
        let dir = app_data_dir(app)?;
        *guard = Some(Arc::new(Mutex::new(TokenRegistry::load(
            dir.join(REGISTRY_FILE),
        ))));
    }
    Ok(guard.as_ref().expect("just initialized").clone())
}

fn with_registry<R>(app: &AppHandle, f: impl FnOnce(&mut TokenRegistry) -> R) -> Result<R, String> {
    let registry = registry_handle(app)?;
    let mut registry = registry
        .lock()
        .map_err(|_| "pairing registry is locked".to_string())?;
    Ok(f(&mut registry))
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create app data dir: {e}"))?;
    Ok(dir)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Keyring user for a paired token: `<instance url>/desktop-token/<id>`.
/// The token id is minted per confirming user, which is what makes the
/// keyring key per-identity: each identity's pairings land in their own
/// entry.
pub fn keyring_user(instance_url: &str, token_id: i64) -> String {
    format!(
        "{}{}{}",
        instance_url.trim_end_matches('/'),
        TOKEN_USER_SEP,
        token_id
    )
}

fn server_error(context: &str, result: &http::HttpResult) -> String {
    match result.server_error() {
        Some(msg) => format!("{context}: {msg}"),
        None => format!("{context} (HTTP {})", result.status.as_u16()),
    }
}

/// The device hostname sent to the server as the token label. No
/// hostname crate is available, so env vars and `/etc/hostname` cover
/// the common platforms; the server falls back to a generic label when
/// empty.
fn device_hostname() -> String {
    let raw = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(read_etc_hostname)
        .unwrap_or_default();
    sanitize_hostname(&raw)
}

#[cfg(unix)]
fn read_etc_hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(not(unix))]
fn read_etc_hostname() -> Option<String> {
    None
}

/// Mirrors the server's hostname sanitization: safe characters only,
/// capped at [`MAX_HOSTNAME_LEN`].
fn sanitize_hostname(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | ':' | ' ' | '(' | ')' | '[' | ']')
        })
        .take(MAX_HOSTNAME_LEN)
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Commands (registered by the dispatcher in lib.rs)
// ---------------------------------------------------------------------------

/// Server-gating accessor: pairing UI is only offered when the instance
/// probe reports the `desktop_pairing` capability (compiled AND admin
/// toggle). Fails closed.
#[tauri::command]
pub fn pairing_supported(instance_url: String) -> bool {
    instances::capability(&instance_url, "desktop_pairing")
}

/// Starts the pairing flow: creates the code server-side and launches
/// the poll loop. Replaces any in-flight attempt for the same instance.
#[tauri::command]
pub async fn pairing_start(app: AppHandle, instance_url: String) -> Result<PairingState, String> {
    let url = instances::validate_instance_url(&instance_url)?;
    if !instances::capability(&url, "desktop_pairing") {
        return Err("device pairing is disabled by this server".to_string());
    }
    let body = json!({ "hostname": device_hostname() });
    let result = http::shell_http()
        .post(&url, "/api/desktop/pair", None, Some(body))
        .await?;
    if !result.is_success() {
        return Err(server_error("pairing request failed", &result));
    }
    let code = result
        .body
        .get("code")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "server returned no pairing code".to_string())?
        .to_string();
    let expires_at = result
        .body
        .get("expires_at")
        .and_then(|e| e.as_str())
        .unwrap_or_default()
        .to_string();
    let generation = next_generation();
    with_session(&url, |s| {
        s.generation = generation;
        s.started = Instant::now();
        s.state = PairingState::Waiting {
            code: code.clone(),
            expires_at: expires_at.clone(),
        };
    });
    spawn_poll(app, url);
    Ok(PairingState::Waiting { code, expires_at })
}

/// Current pairing state for an instance (`idle` when none exists).
#[tauri::command]
pub fn pairing_status(instance_url: String) -> PairingState {
    with_session(&instance_url, |s| s.state.clone()).unwrap_or(PairingState::Idle)
}

/// Cancels the in-flight pairing for an instance. The poll loop observes
/// the state change at its next tick.
#[tauri::command]
pub fn pairing_cancel(instance_url: String) -> Result<(), String> {
    with_session(&instance_url, |s| {
        s.state = apply_event(s.state.clone(), PairingEvent::Cancel).0;
    })
    .ok_or_else(|| "pairing state store is unavailable".to_string())
}

/// Navigates the main window webview to the instance's account tokens
/// page, where the logged-in user confirms the code. Uses the instances
/// module's window label so the navigation matches the allowlist.
#[tauri::command]
pub fn pairing_open_confirm_page(app: AppHandle, instance_url: String) -> Result<(), String> {
    let url = instances::validate_instance_url(&instance_url)?;
    let page = format!("{url}{CONFIRM_PAGE}");
    let parsed = url::Url::parse(&page).map_err(|e| format!("invalid pairing page URL: {e}"))?;
    let win = app
        .get_webview_window(instances::window_label(&url))
        .ok_or_else(|| "main window not found".to_string())?;
    win.navigate(parsed)
        .map_err(|e| format!("navigation failed: {e}"))
}

/// View of one registered token for the shell UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenView {
    pub instance_url: String,
    pub token_id: i64,
    pub token_name: String,
    pub device_name: String,
    pub created_at: u64,
    /// Whether the secret is currently present in the OS keychain. A
    /// keyring read error counts as missing (the revoke path surfaces
    /// keychain failures loudly).
    pub in_keychain: bool,
}

/// The registered tokens for an instance, with keychain presence. The
/// webview's token list on the server remains the source of truth for
/// what the server holds.
#[tauri::command]
pub async fn pairing_list_tokens(
    app: AppHandle,
    instance_url: String,
) -> Result<Vec<TokenView>, String> {
    let url = instances::validate_instance_url(&instance_url)?;
    let tokens = with_registry(&app, |r| r.for_instance(&url))?;
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        let secret = keyring::keyring_get(
            SERVICE_NAME.to_string(),
            keyring_user(&url, token.token_id),
            app.clone(),
        )
        .await?;
        out.push(TokenView {
            instance_url: token.instance_url,
            token_id: token.token_id,
            token_name: token.token_name,
            device_name: token.device_name,
            created_at: token.created_at,
            in_keychain: secret.is_some(),
        });
    }
    Ok(out)
}

/// Revokes a paired token: DELETE on the server with the token's own
/// Bearer (plus the CSRF contract), then drops the keychain entry and
/// the registry record. A 404 server-side (already revoked elsewhere)
/// still cleans up locally.
#[tauri::command]
pub async fn pairing_revoke(
    app: AppHandle,
    instance_url: String,
    token_id: i64,
) -> Result<(), String> {
    let url = instances::validate_instance_url(&instance_url)?;
    let found = with_registry(&app, |r| {
        r.for_instance(&url).iter().any(|t| t.token_id == token_id)
    })?;
    if !found {
        return Err("no paired token with that id".to_string());
    }
    let secret = keyring::keyring_get(
        SERVICE_NAME.to_string(),
        keyring_user(&url, token_id),
        app.clone(),
    )
    .await?;
    let result = http::shell_http()
        .delete(
            &url,
            &format!("/api/me/tokens/{token_id}"),
            secret.as_deref(),
        )
        .await?;
    if !result.is_success() && result.status != StatusCode::NOT_FOUND {
        return Err(server_error("revocation failed", &result));
    }
    let _ = keyring::keyring_delete(
        SERVICE_NAME.to_string(),
        keyring_user(&url, token_id),
        app.clone(),
    )
    .await;
    with_registry(&app, |r| {
        r.remove(&url, token_id);
        r.save()
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rust-side accessors (session poller, transfers)
// ---------------------------------------------------------------------------

/// Every registered paired token across instances. The poller builds
/// its per-instance Bearer list from this.
pub fn registered_tokens(app: &AppHandle) -> Vec<RegisteredToken> {
    with_registry(app, |r| r.file.tokens.clone()).unwrap_or_default()
}

/// Keychain lookup for one paired token.
pub async fn token_secret(app: &AppHandle, instance_url: &str, token_id: i64) -> Option<String> {
    keyring::keyring_get(
        SERVICE_NAME.to_string(),
        keyring_user(instance_url, token_id),
        app.clone(),
    )
    .await
    .ok()
    .flatten()
}

/// The most recent paired token secret for an instance; the poller's
/// default Bearer token. On a 401 the poller pauses and asks for a
/// re-pair (token refresh is a new pairing, per the locked design).
pub async fn token_for(app: &AppHandle, instance_url: &str) -> Option<String> {
    let latest = with_registry(app, |r| r.latest(instance_url).cloned()).ok()??;
    token_secret(app, instance_url, latest.token_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_mock::{MockResponse, MockScript, MockServer};

    fn waiting() -> PairingState {
        PairingState::Waiting {
            code: "ABCD2345".to_string(),
            expires_at: "2026-08-13T00:00:00Z".to_string(),
        }
    }

    fn approved_event() -> PairingEvent {
        PairingEvent::Approved {
            token_id: 7,
            token_name: "Persea Desktop (dev-box)".to_string(),
            device_name: "dev-box".to_string(),
        }
    }

    fn terminal_states() -> Vec<PairingState> {
        vec![
            PairingState::Approved {
                token_id: 7,
                token_name: "n".to_string(),
                device_name: "d".to_string(),
            },
            PairingState::Expired,
            PairingState::Used,
            PairingState::TimedOut,
            PairingState::Cancelled,
            PairingState::Failed {
                message: "boom".to_string(),
            },
        ]
    }

    #[test]
    fn tick_keeps_waiting() {
        let (next, done) = apply_event(waiting(), PairingEvent::PollTick);
        assert_eq!(next, waiting());
        assert!(!done);
    }

    #[test]
    fn approval_is_terminal() {
        let (next, done) = apply_event(waiting(), approved_event());
        assert!(matches!(next, PairingState::Approved { token_id: 7, .. }));
        assert!(done);
    }

    #[test]
    fn expiry_use_timeout_and_cancel_are_terminal() {
        for (event, expected) in [
            (PairingEvent::Expired, PairingState::Expired),
            (PairingEvent::Used, PairingState::Used),
            (PairingEvent::Timeout, PairingState::TimedOut),
            (PairingEvent::Cancel, PairingState::Cancelled),
        ] {
            let (next, done) = apply_event(waiting(), event);
            assert_eq!(next, expected);
            assert!(done);
        }
    }

    #[test]
    fn failed_persistence_is_terminal() {
        let (next, done) = apply_event(waiting(), PairingEvent::Failed("keychain".to_string()));
        assert_eq!(
            next,
            PairingState::Failed {
                message: "keychain".to_string()
            }
        );
        assert!(done);
    }

    #[test]
    fn terminal_states_ignore_all_events() {
        let events = vec![
            PairingEvent::PollTick,
            approved_event(),
            PairingEvent::Timeout,
            PairingEvent::Expired,
            PairingEvent::Used,
            PairingEvent::Cancel,
            PairingEvent::Failed("x".to_string()),
        ];
        for state in terminal_states() {
            for event in events.clone() {
                let (next, done) = apply_event(state.clone(), event);
                assert_eq!(next, state, "terminal state must not change");
                assert!(done);
            }
        }
    }

    #[test]
    fn idle_ignores_events() {
        let (next, done) = apply_event(PairingState::Idle, PairingEvent::PollTick);
        assert_eq!(next, PairingState::Idle);
        assert!(!done);
    }

    fn ok_json(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.to_string(),
        }
    }

    fn gone(error: &str) -> MockResponse {
        MockResponse {
            status: 410,
            reason: "Gone",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: format!("{{\"error\":\"{error}\"}}"),
        }
    }

    fn poll_with(script: MockScript, code: &str) -> PollOutcome {
        let server = MockServer::start(script);
        let http = crate::http::ShellHttp::new();
        tauri::async_runtime::block_on(async { poll_once(&http, &server.url(), code).await })
    }

    #[test]
    fn poll_once_pending_stays_pending() {
        let outcome = poll_with(
            MockScript::new(vec![ok_json("{\"status\":\"pending\"}")]),
            "ABCD2345",
        );
        assert_eq!(outcome, PollOutcome::Pending);
    }

    #[test]
    fn poll_once_approved_carries_token() {
        let outcome = poll_with(
            MockScript::new(vec![ok_json(
                "{\"status\":\"approved\",\"token\":\"tkn-1\",\"token_id\":7,\
                 \"name\":\"Persea Desktop (dev-box)\",\"device_name\":\"dev-box\"}",
            )]),
            "ABCD2345",
        );
        assert_eq!(
            outcome,
            PollOutcome::Approved {
                token_id: 7,
                token_name: "Persea Desktop (dev-box)".to_string(),
                device_name: "dev-box".to_string(),
                token: "tkn-1".to_string(),
            }
        );
    }

    #[test]
    fn poll_once_expired_and_used_are_distinct() {
        assert_eq!(
            poll_with(
                MockScript::new(vec![gone("pairing code expired")]),
                "ABCD2345"
            ),
            PollOutcome::Expired
        );
        assert_eq!(
            poll_with(
                MockScript::new(vec![gone("pairing code already used")]),
                "ABCD2345"
            ),
            PollOutcome::Used
        );
    }

    #[test]
    fn poll_once_rate_limit_is_transient() {
        let outcome = poll_with(
            MockScript::new(vec![MockResponse {
                status: 429,
                reason: "Too Many Requests",
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: "{\"error\":\"too many requests, try again later\"}".to_string(),
            }]),
            "ABCD2345",
        );
        assert_eq!(outcome, PollOutcome::Transient);
    }

    #[test]
    fn poll_once_network_failure_is_transient() {
        let http = crate::http::ShellHttp::new();
        let outcome = tauri::async_runtime::block_on(async {
            poll_once(&http, "http://127.0.0.1:1", "ABCD2345").await
        });
        assert_eq!(outcome, PollOutcome::Transient);
    }

    fn temp_registry(tag: &str) -> TokenRegistry {
        let dir = std::env::temp_dir().join(format!(
            "persea-pairing-test-{tag}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        TokenRegistry::load(dir.join(REGISTRY_FILE))
    }

    fn sample_token(id: i64, name: &str) -> RegisteredToken {
        RegisteredToken {
            instance_url: "https://persea.example.com".to_string(),
            token_id: id,
            token_name: name.to_string(),
            device_name: "dev-box".to_string(),
            created_at: id as u64,
        }
    }

    #[test]
    fn registry_register_dedupes_same_name_and_returns_removed() {
        let mut registry = temp_registry("dedupe");
        let removed = registry.register(sample_token(1, "Persea Desktop (dev-box)"));
        assert!(removed.is_empty());
        let removed = registry.register(sample_token(2, "Persea Desktop (dev-box)"));
        assert_eq!(removed, vec![1]);
        assert_eq!(registry.for_instance("https://persea.example.com").len(), 1);
        // Different name (different identity): both stay.
        let removed = registry.register(sample_token(3, "Persea Desktop (other)"));
        assert!(removed.is_empty());
        assert_eq!(registry.for_instance("https://persea.example.com").len(), 2);
    }

    #[test]
    fn registry_round_trip_preserves_tokens() {
        let mut registry = temp_registry("roundtrip");
        registry.register(sample_token(1, "Persea Desktop (dev-box)"));
        registry.save().expect("save");
        let loaded = TokenRegistry::load(registry.path.clone());
        assert_eq!(loaded.file, registry.file);
    }

    #[test]
    fn registry_corrupt_file_is_backed_up() {
        let registry = temp_registry("corrupt");
        std::fs::write(&registry.path, "{ not json").expect("write corrupt");
        let loaded = TokenRegistry::load(registry.path.clone());
        assert!(loaded.file.tokens.is_empty());
        assert!(registry.path.with_extension("json.corrupt").exists());
    }

    #[test]
    fn registry_remove_and_latest() {
        let mut registry = temp_registry("latest");
        registry.register(sample_token(1, "a"));
        registry.register(sample_token(2, "b"));
        assert_eq!(
            registry
                .latest("https://persea.example.com")
                .unwrap()
                .token_id,
            2
        );
        assert!(registry.remove("https://persea.example.com", 2));
        assert_eq!(
            registry
                .latest("https://persea.example.com")
                .unwrap()
                .token_id,
            1
        );
        assert!(!registry.remove("https://persea.example.com", 99));
        assert!(registry
            .for_instance("https://other.example.com")
            .is_empty());
    }

    #[test]
    fn keyring_user_is_instance_plus_token_id() {
        assert_eq!(
            keyring_user("https://persea.example.com", 7),
            "https://persea.example.com/desktop-token/7"
        );
        assert_eq!(
            keyring_user("https://persea.example.com/", 7),
            "https://persea.example.com/desktop-token/7"
        );
    }

    #[test]
    fn hostname_sanitization_matches_server() {
        assert_eq!(sanitize_hostname("dev-box:2"), "dev-box:2");
        assert_eq!(sanitize_hostname("a<b>\"c\\d`e"), "abcde");
        assert_eq!(sanitize_hostname(&"abc".repeat(30)).len(), MAX_HOSTNAME_LEN);
        assert_eq!(sanitize_hostname("  "), "");
    }
}
