#![allow(dead_code)]
//! Transfer orchestration: RDP drive uploads and downloads with progress.
//!
//! # Scope (locked design)
//!
//! Native drag-drop uploads land here from `drop.rs`, which captures
//! `DragDropEvent`s on session windows. This module owns everything
//! after the capture:
//!
//! - drive REST calls: `GET /api/sessions/{id}/drive-files` (list),
//!   `PUT .../drive-files/{name}` (upload, raw body streamed
//!   server-side), `GET .../drive-files/{name}` (download).
//! - the CSRF contract for the PUT: one anonymous
//!   `GET /api/auth/status` per instance captures the `csrf_token`
//!   cookie, then the PUT echoes `Cookie: csrf_token=...` plus
//!   `X-CSRF-Token: ...` (the server's double-submit rule for every
//!   state-changing endpoint, mirrored from `http.rs`).
//! - the transfer registry (queued / reading / uploading / downloading /
//!   done / failed / cancelled rows) surfaced to the transfer window
//!   (`shell/transfer.html`) via `transfers-changed` events.
//! - conflict handling: when a dropped name already exists in the drive
//!   listing, a native three-button prompt offers Overwrite / Rename
//!   (auto-suffix) / Cancel.
//! - downloads: shell-side REST fetch with the paired Bearer, written
//!   through a native save dialog. Engine-level downloads (blob / anchor
//!   clicks in the webview) are intercepted by `downloads.rs` and land in
//!   the OS Downloads folder; this module mirrors those records into the
//!   transfer window so every download is visible with an open-folder
//!   action, and offers "Save as" (REST + save dialog) for drive REST
//!   URLs.
//!
//! # HTTP client note
//!
//! `http.rs`'s `ShellHttp::put` takes a JSON body, which cannot carry a
//! raw file stream, so this module runs its own reqwest client with the
//! same CSRF bootstrap contract. That is deliberate: the drive upload
//! endpoint reads the raw request body, not JSON.
//!
//! # Progress honesty
//!
//! The server reports no progress; the shell estimates it from the
//! upload side. The file is read in 1 MiB chunks on the blocking pool
//! with a shared byte counter, so the reading phase shows real
//! percentages. The PUT itself sends the buffered body in one shot, so
//! the uploading phase is indeterminate until the 201 arrives. Files
//! above [`SHELL_UPLOAD_CAP_BYTES`] are refused with a pointer to the
//! in-session upload button (the shell-side path buffers the file in
//! memory; the server's own cap is 4 GiB).
//!
//! # Wiring for the dispatcher
//!
//! 1. `lib.rs`: `mod transfer;`, register the `cmd_transfer*` commands
//!    in `generate_handler!`, and call `transfer::setup(app)?` in the
//!    setup hook (after `windows::setup`).
//! 2. `lib.rs`: `.plugin(tauri_plugin_dialog::init())` (the dependency
//!    is already in `Cargo.toml`); every dialog helper in this module
//!    needs it registered.
//! 3. `build.rs`: add the transfer commands to the manifest command
//!    list (fail-closed ACL, same pattern as the tab commands).
//! 4. `capabilities/default.json`: add `"transfer"` to `windows` and
//!    grant `allow-cmd-transfers-list`, `allow-cmd-transfer-retry`,
//!    `allow-cmd-transfer-open-folder`,
//!    `allow-cmd-transfer-clear-finished`,
//!    `allow-cmd-transfer-download` (the transfer page is a local shell
//!    page; without coverage its IPC is rejected wholesale).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
use url::Url;

use crate::http::{self, CSRF_COOKIE, CSRF_HEADER};
use crate::instances;
use crate::pairing;

/// Event emitted app-wide whenever the transfer list changes; the
/// transfer window page re-renders on it.
pub const EVENT_TRANSFERS_CHANGED: &str = "transfers-changed";
/// The transfer window label (a local shell page, `shell/transfer.html`).
pub const TRANSFER_WINDOW_LABEL: &str = "transfer";
/// Shell-side upload cap: the file is buffered in memory, so very large
/// files go through the in-session upload button instead (server cap:
/// 4 GiB).
pub const SHELL_UPLOAD_CAP_BYTES: u64 = 1024 * 1024 * 1024;
/// Registry cap: older rows are evicted.
const ROW_CAP: usize = 200;
/// Anonymous GET used to bootstrap the CSRF cookie (same as `http.rs`).
const BOOTSTRAP_PATH: &str = "/api/auth/status";
/// Read-phase progress poll cadence.
const PROGRESS_POLL_MS: u64 = 200;
/// Read chunk size.
const READ_CHUNK_BYTES: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Upload vs download transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    Upload,
    Download,
}

/// Lifecycle of one transfer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    Queued,
    Reading,
    Uploading,
    Downloading,
    Done,
    Failed,
    Cancelled,
}

impl TransferStatus {
    pub fn finished(self) -> bool {
        matches!(
            self,
            TransferStatus::Done | TransferStatus::Failed | TransferStatus::Cancelled
        )
    }
}

/// One transfer row.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    pub direction: TransferDirection,
    pub instance: String,
    pub session_id: String,
    pub remote_name: String,
    pub local_path: Option<PathBuf>,
    /// Engine download URL (downloads.rs records) when mirrored.
    pub source_url: Option<String>,
    pub status: TransferStatus,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub error: Option<String>,
    pub created_at: u64,
}

/// Serialized row for the transfer window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub id: u64,
    pub direction: String,
    pub remote_name: String,
    pub local_name: Option<String>,
    pub status: String,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub error: Option<String>,
    pub created_at: u64,
    /// Failed uploads can be retried.
    pub can_retry: bool,
    /// Rows with an existing local path offer open-folder.
    pub can_open_folder: bool,
    /// Rows whose source URL is a drive REST URL offer "Save as"
    /// (re-download via REST + save dialog).
    pub can_save_as: bool,
    pub source_url: Option<String>,
}

/// One entry of the drive listing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DriveEntry {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

/// Why a drop cannot proceed (native notice shown for each).
#[derive(Debug, Clone, PartialEq)]
pub enum DropReject {
    /// The instance probe does not report `desktop_transfers`.
    Disabled,
    /// No paired device token for the instance.
    PairRequired,
    /// The session has no REST drive (SSH sessions; the in-session
    /// upload button still works).
    SshOnly,
    /// 404 on the drive endpoints.
    SessionGone,
    /// 403: the paired identity is not the session owner.
    NotOwner,
    /// 401: the paired token was rejected.
    TokenRejected,
    /// Network failure or 5xx.
    Unreachable(String),
}

impl DropReject {
    pub fn title(&self) -> &'static str {
        match self {
            DropReject::Disabled => "Transfers disabled",
            DropReject::PairRequired => "Device not paired",
            DropReject::SshOnly => "SSH session",
            DropReject::SessionGone => "Session gone",
            DropReject::NotOwner => "Not your session",
            DropReject::TokenRejected => "Token rejected",
            DropReject::Unreachable(_) => "Transfer failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            DropReject::Disabled => "File transfers are disabled by this server. \
                 The in-session upload button still works."
                .to_string(),
            DropReject::PairRequired => "File transfers need a paired device token. \
                 Pair this device from the settings page first."
                .to_string(),
            DropReject::SshOnly => "SSH sessions do not support drag-drop transfers yet. \
                 Use the upload button inside the session to send files."
                .to_string(),
            DropReject::SessionGone => {
                "The session is no longer active. Reconnect and try again.".to_string()
            }
            DropReject::NotOwner => "The paired token does not own this session. \
                 Pair with the account that started it."
                .to_string(),
            DropReject::TokenRejected => "The paired token was rejected by the server. \
                 Re-pair this device from the settings page."
                .to_string(),
            DropReject::Unreachable(message) => format!("Could not reach the server: {message}"),
        }
    }
}

/// Outcome of a drive listing call, used to steer the drop.
#[derive(Debug, Clone, PartialEq)]
pub enum ListOutcome {
    Ready(Vec<DriveEntry>),
    Reject(DropReject),
}

/// User decision at a conflict prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Overwrite,
    Rename,
    Cancel,
}

/// One file ready to upload after conflict resolution.
#[derive(Debug, Clone)]
pub struct PlannedUpload {
    pub local_path: PathBuf,
    pub remote_name: String,
}

// ---------------------------------------------------------------------------
// Drive REST client (own reqwest client; see module doc)
// ---------------------------------------------------------------------------

/// Raw response of a drive call: status plus parsed JSON body.
#[derive(Debug, Clone)]
pub struct DriveResponse {
    pub status: reqwest::StatusCode,
    pub body: serde_json::Value,
}

impl DriveResponse {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    pub fn server_error(&self) -> Option<String> {
        self.body
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
    }
}

/// Drive REST client with the CSRF bootstrap contract. One instance per
/// process, shared by every transfer.
pub struct DriveClient {
    client: reqwest::Client,
    csrf: Mutex<HashMap<String, Option<String>>>,
}

impl DriveClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(8))
            .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with static options cannot fail");
        Self {
            client,
            csrf: Mutex::new(HashMap::new()),
        }
    }

    fn drive_url(
        &self,
        instance: &str,
        session_id: &str,
        name: Option<&str>,
    ) -> Result<Url, String> {
        let mut url = Url::parse(instance.trim_end_matches('/'))
            .map_err(|e| format!("invalid instance URL: {e}"))?;
        url.set_path(&format!("/api/sessions/{session_id}/drive-files"));
        if let Some(name) = name {
            url.path_segments_mut()
                .map_err(|_| "invalid drive URL".to_string())?
                .push(name);
        }
        Ok(url)
    }

    /// CSRF bootstrap: one anonymous GET per instance, retried up to
    /// three times. Repeated calls return immediately once a token is
    /// stored.
    pub async fn bootstrap(&self, instance: &str) -> Result<(), String> {
        if self.csrf_token(instance).is_some() {
            return Ok(());
        }
        let url = format!("{}{}", instance.trim_end_matches('/'), BOOTSTRAP_PATH);
        for attempt in 0..3 {
            if attempt > 0 {
                http::sleep(Duration::from_millis(750)).await;
            }
            let resp = match self.client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt == 2 {
                        return Err(format!("request failed: {e}"));
                    }
                    continue;
                }
            };
            if let Some(token) = extract_cookie(&resp, CSRF_COOKIE) {
                self.store_token(instance, token);
                return Ok(());
            }
        }
        Err("server set no csrf cookie".to_string())
    }

    async fn ensure_csrf(&self, instance: &str) -> Result<Option<String>, String> {
        if let Some(token) = self.csrf_token(instance) {
            return Ok(Some(token));
        }
        self.bootstrap(instance).await?;
        Ok(self.csrf_token(instance))
    }

    fn store_token(&self, instance: &str, token: String) {
        if let Ok(mut map) = self.csrf.lock() {
            map.insert(instance.trim_end_matches('/').to_string(), Some(token));
        }
    }

    fn csrf_token(&self, instance: &str) -> Option<String> {
        self.csrf
            .lock()
            .ok()
            .and_then(|map| map.get(instance.trim_end_matches('/')).cloned())
            .flatten()
    }

    /// GET the drive listing. The caller classifies the response.
    pub async fn list(
        &self,
        instance: &str,
        session_id: &str,
        bearer: &str,
    ) -> Result<DriveResponse, String> {
        let url = self
            .drive_url(instance, session_id, None)
            .map_err(|e| e.to_string())?;
        let resp = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        Ok(self.finish(resp).await)
    }

    /// PUT the raw file body; echoes the CSRF cookie and header.
    pub async fn upload(
        &self,
        instance: &str,
        session_id: &str,
        name: &str,
        bytes: &[u8],
        bearer: &str,
    ) -> Result<DriveResponse, String> {
        let token = self.ensure_csrf(instance).await?;
        let url = self
            .drive_url(instance, session_id, Some(name))
            .map_err(|e| e.to_string())?;
        let mut req = self
            .client
            .put(url)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/octet-stream");
        if let Some(token) = token {
            req = req
                .header(header::COOKIE, format!("{CSRF_COOKIE}={token}"))
                .header(CSRF_HEADER, token);
        }
        let resp = req
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|e| format!("upload request failed: {e}"))?;
        Ok(self.finish(resp).await)
    }

    /// GET a drive file's raw bytes (download).
    pub async fn download(
        &self,
        instance: &str,
        session_id: &str,
        name: &str,
        bearer: &str,
    ) -> Result<Vec<u8>, String> {
        let url = self
            .drive_url(instance, session_id, Some(name))
            .map_err(|e| e.to_string())?;
        let resp = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("failed to read download body: {e}"))?
            .to_vec();
        if status.is_success() {
            Ok(body)
        } else {
            let parsed = serde_json::from_slice::<serde_json::Value>(&body)
                .unwrap_or(serde_json::Value::Null);
            Err(format!(
                "download failed (HTTP {}): {}",
                status.as_u16(),
                parsed
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error")
            ))
        }
    }

    async fn finish(&self, resp: reqwest::Response) -> DriveResponse {
        // The server re-sets the CSRF cookie on every response; refresh
        // the stored value so a rotated token never leaves the client
        // stale.
        let instance = resp.url().origin().ascii_serialization();
        if let Some(token) = extract_cookie(&resp, CSRF_COOKIE) {
            self.store_token(&instance, token);
        }
        let status = resp.status();
        let body = match resp.json().await {
            Ok(value) => value,
            Err(_) => serde_json::Value::Null,
        };
        DriveResponse { status, body }
    }
}

impl Default for DriveClient {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide drive client.
pub fn drive_client() -> &'static DriveClient {
    static DRIVE_CLIENT: OnceLock<DriveClient> = OnceLock::new();
    DRIVE_CLIENT.get_or_init(DriveClient::new)
}

/// Pulls `name` from a `Set-Cookie` response header, if present.
fn extract_cookie(resp: &reqwest::Response, name: &str) -> Option<String> {
    for value in resp.headers().get_all(header::SET_COOKIE) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        let pair = raw.split_once(';').map(|(pair, _)| pair).unwrap_or(raw);
        let (key, val) = pair.split_once('=')?;
        if key.trim() == name {
            return Some(val.trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Classification and planning (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Map a drive listing response onto the drop flow's branches.
pub fn classify_list(resp: &DriveResponse) -> ListOutcome {
    use reqwest::StatusCode as S;
    match resp.status {
        S::OK => {
            let entries: Vec<DriveEntry> =
                serde_json::from_value(resp.body.clone()).unwrap_or_default();
            ListOutcome::Ready(entries)
        }
        S::NOT_FOUND => match resp.server_error().as_deref() {
            Some(msg) if msg.contains("no file-transfer drive") => {
                ListOutcome::Reject(DropReject::SshOnly)
            }
            _ => ListOutcome::Reject(DropReject::SessionGone),
        },
        S::FORBIDDEN => ListOutcome::Reject(DropReject::NotOwner),
        S::UNAUTHORIZED => ListOutcome::Reject(DropReject::TokenRejected),
        _ => ListOutcome::Reject(DropReject::Unreachable(
            resp.server_error()
                .unwrap_or_else(|| format!("HTTP {}", resp.status.as_u16())),
        )),
    }
}

/// Auto-suffix a conflicting name: `x.txt` → `x (1).txt`, skipping every
/// name already present.
pub fn rename_away(name: &str, existing: &[String]) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(idx) if idx > 0 => (&name[..idx], &name[idx..]),
        _ => (name, ""),
    };
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){ext}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
    }
    format!(
        "{stem} ({}){ext}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// Decide the upload plan for a batch of dropped files against the
/// current drive listing. `decide` is invoked once per conflicting name;
/// the production caller answers with the native prompt.
pub fn plan_uploads(
    files: &[PathBuf],
    existing: &[DriveEntry],
    mut decide: impl FnMut(&str) -> ConflictChoice,
) -> Vec<PlannedUpload> {
    let mut names: Vec<String> = existing.iter().map(|e| e.name.clone()).collect();
    let mut planned = Vec::new();
    for file in files {
        let Some(raw) = file.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(base) = crate::downloads::sanitize_filename(raw) else {
            continue;
        };
        if names.iter().any(|n| n == &base) {
            match decide(&base) {
                ConflictChoice::Overwrite => planned.push(PlannedUpload {
                    local_path: file.clone(),
                    remote_name: base,
                }),
                ConflictChoice::Rename => {
                    let renamed = rename_away(&base, &names);
                    names.push(renamed.clone());
                    planned.push(PlannedUpload {
                        local_path: file.clone(),
                        remote_name: renamed,
                    });
                }
                ConflictChoice::Cancel => {}
            }
        } else {
            names.push(base.clone());
            planned.push(PlannedUpload {
                local_path: file.clone(),
                remote_name: base,
            });
        }
    }
    planned
}

/// Extract `(instance, session_id, name)` from a drive REST download
/// URL (`/api/sessions/{id}/drive-files/{name}`). `None` for anything
/// else (blob URLs, screenshots, recordings).
pub fn parse_drive_url(raw: &str) -> Option<(String, String, String)> {
    let url = Url::parse(raw).ok()?;
    let segments: Vec<String> = url.path_segments()?.map(str::to_string).collect();
    if segments.len() == 5
        && segments[0] == "api"
        && segments[1] == "sessions"
        && segments[3] == "drive-files"
    {
        Some((
            url.origin().ascii_serialization(),
            segments[2].clone(),
            percent_decode(&segments[4]),
        ))
    } else {
        None
    }
}

/// Minimal percent-decoding (`%XX`) for names from URLs.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

static REGISTRY: OnceLock<Mutex<Vec<Transfer>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn registry() -> std::sync::MutexGuard<'static, Vec<Transfer>> {
    REGISTRY
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn with_row<R>(id: u64, f: impl FnOnce(&mut Transfer) -> R) -> Option<R> {
    let mut rows = registry();
    let row = rows.iter_mut().find(|t| t.id == id)?;
    Some(f(row))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Serialized snapshot for the transfer window.
pub fn transfers_view() -> Vec<TransferView> {
    registry()
        .iter()
        .map(|t| TransferView {
            id: t.id,
            direction: match t.direction {
                TransferDirection::Upload => "upload",
                TransferDirection::Download => "download",
            }
            .to_string(),
            remote_name: t.remote_name.clone(),
            local_name: t
                .local_path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(str::to_string),
            status: match t.status {
                TransferStatus::Queued => "queued",
                TransferStatus::Reading => "reading",
                TransferStatus::Uploading => "uploading",
                TransferStatus::Downloading => "downloading",
                TransferStatus::Done => "done",
                TransferStatus::Failed => "failed",
                TransferStatus::Cancelled => "cancelled",
            }
            .to_string(),
            bytes_total: t.bytes_total,
            bytes_done: t.bytes_done,
            error: t.error.clone(),
            created_at: t.created_at,
            can_retry: t.direction == TransferDirection::Upload
                && t.status == TransferStatus::Failed,
            can_open_folder: t.local_path.as_ref().is_some_and(|p| p.exists()),
            can_save_as: t.source_url.as_deref().and_then(parse_drive_url).is_some(),
            source_url: t.source_url.clone(),
        })
        .collect()
}

fn push_row(mut row: Transfer) -> u64 {
    let mut rows = registry();
    if rows.len() >= ROW_CAP {
        rows.remove(0);
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    row.id = id;
    rows.push(row);
    id
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(EVENT_TRANSFERS_CHANGED, transfers_view());
}

// ---------------------------------------------------------------------------
// Upload orchestration
// ---------------------------------------------------------------------------

/// Full drop pipeline: gating, pairing, drive classification, conflict
/// prompt, upload spawn, native notices for rejections. Called by
/// `drop.rs` from a spawned task.
pub async fn handle_drop(
    app: AppHandle,
    instance: String,
    session_id: String,
    paths: Vec<PathBuf>,
) {
    if paths.is_empty() {
        notice(
            &app,
            "Empty drop",
            "No file paths were received (a known quirk on KDE Wayland). \
             Try the drag again.",
        );
        return;
    }
    if !instances::capability(&instance, "desktop_transfers") {
        notice(
            &app,
            DropReject::Disabled.title(),
            &DropReject::Disabled.message(),
        );
        return;
    }
    let Some(bearer) = pairing::token_for(&app, &instance).await else {
        notice(
            &app,
            DropReject::PairRequired.title(),
            &DropReject::PairRequired.message(),
        );
        return;
    };
    let resp = match drive_client().list(&instance, &session_id, &bearer).await {
        Ok(resp) => resp,
        Err(e) => {
            let reject = DropReject::Unreachable(e);
            notice(&app, reject.title(), &reject.message());
            return;
        }
    };
    let entries = match classify_list(&resp) {
        ListOutcome::Reject(reject) => {
            notice(&app, reject.title(), &reject.message());
            return;
        }
        ListOutcome::Ready(entries) => entries,
    };
    show_transfer_window(&app);
    let planned = plan_uploads(&paths, &entries, |name| conflict_prompt(&app, name));
    for upload in planned {
        let _ = spawn_upload(
            app.clone(),
            instance.clone(),
            session_id.clone(),
            upload.local_path,
            upload.remote_name,
        );
    }
}

/// Register an upload row and spawn its task.
pub fn spawn_upload(
    app: AppHandle,
    instance: String,
    session_id: String,
    local_path: PathBuf,
    remote_name: String,
) -> Result<u64, String> {
    let id = push_row(Transfer {
        id: 0,
        direction: TransferDirection::Upload,
        instance: instance.clone(),
        session_id: session_id.clone(),
        remote_name: remote_name.clone(),
        local_path: Some(local_path.clone()),
        source_url: None,
        status: TransferStatus::Queued,
        bytes_total: 0,
        bytes_done: 0,
        error: None,
        created_at: now_secs(),
    });
    emit_changed(&app);
    tauri::async_runtime::spawn(async move {
        let outcome = upload_one(&app, id, &instance, &session_id, &local_path, &remote_name).await;
        match outcome {
            Ok(total) => {
                with_row(id, |t| {
                    t.status = TransferStatus::Done;
                    t.bytes_total = total;
                    t.bytes_done = total;
                });
            }
            Err(error) => {
                with_row(id, |t| {
                    t.status = TransferStatus::Failed;
                    t.error = Some(error);
                });
            }
        }
        emit_changed(&app);
    });
    Ok(id)
}

/// One upload: read (with progress) then PUT. Testable core.
pub async fn upload_one(
    app: &AppHandle,
    row_id: u64,
    instance: &str,
    session_id: &str,
    local_path: &Path,
    remote_name: &str,
) -> Result<u64, String> {
    let meta = std::fs::metadata(local_path)
        .map_err(|e| format!("cannot stat {}: {e}", local_path.display()))?;
    if meta.len() > SHELL_UPLOAD_CAP_BYTES {
        return Err(format!(
            "{} is larger than the 1 GiB drag-drop cap; \
             use the upload button inside the session",
            local_path.display()
        ));
    }
    let total = meta.len();
    let progress = Arc::new(AtomicU64::new(0));
    let progress_clone = Arc::clone(&progress);
    let path = local_path.to_path_buf();
    let done_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_flag_clone = done_flag.clone();
    let read = tauri::async_runtime::spawn_blocking(move || {
        let r = read_chunked(&path, &progress_clone);
        done_flag_clone.store(true, Ordering::Relaxed);
        r
    });
    with_row(row_id, |t| {
        t.status = TransferStatus::Reading;
        t.bytes_total = total;
    });
    emit_changed(app);
    loop {
        if done_flag.load(Ordering::Relaxed) {
            break;
        }
        let done = progress.load(Ordering::Relaxed);
        with_row(row_id, |t| {
            t.bytes_done = done;
        });
        emit_changed(app);
        http::sleep(Duration::from_millis(PROGRESS_POLL_MS)).await;
    }
    let bytes = read.await.map_err(|e| format!("read task failed: {e}"))??;
    with_row(row_id, |t| {
        t.status = TransferStatus::Uploading;
        t.bytes_done = total;
    });
    emit_changed(app);
    let bearer = pairing::token_for(app, instance)
        .await
        .ok_or_else(|| "no paired token for this instance".to_string())?;
    let resp = drive_client()
        .upload(instance, session_id, remote_name, &bytes, &bearer)
        .await?;
    if resp.status == reqwest::StatusCode::CREATED {
        Ok(resp
            .body
            .get("size")
            .and_then(|s| s.as_u64())
            .unwrap_or(bytes.len() as u64))
    } else {
        let msg = match resp.status {
            reqwest::StatusCode::UNAUTHORIZED => {
                "the paired token was rejected; re-pair the device".to_string()
            }
            reqwest::StatusCode::FORBIDDEN => {
                "the paired token does not own this session".to_string()
            }
            _ => resp
                .server_error()
                .unwrap_or_else(|| format!("HTTP {}", resp.status.as_u16())),
        };
        Err(format!("upload failed ({msg})"))
    }
}

/// Read a file fully in 1 MiB chunks, updating `progress` per chunk.
fn read_chunked(path: &Path, progress: &AtomicU64) -> Result<Vec<u8>, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        progress.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Download orchestration
// ---------------------------------------------------------------------------

/// Shell-side drive download: REST fetch with the paired Bearer, native
/// save dialog, write, transfer row. Triggered by the transfer window's
/// "Save as" action on drive REST download rows.
pub async fn download_from_url(app: AppHandle, url: String) -> Result<u64, String> {
    let (instance, session_id, name) =
        parse_drive_url(&url).ok_or_else(|| "not a drive download URL".to_string())?;
    download_drive_file(app, instance, session_id, name).await
}

/// Download one drive file via REST with a save dialog.
pub async fn download_drive_file(
    app: AppHandle,
    instance: String,
    session_id: String,
    name: String,
) -> Result<u64, String> {
    let Some(path) = save_dialog(&app, &name) else {
        return Err("cancelled".to_string());
    };
    let bearer = pairing::token_for(&app, &instance)
        .await
        .ok_or_else(|| "no paired token for this instance".to_string())?;
    let bytes = drive_client()
        .download(&instance, &session_id, &name, &bearer)
        .await?;
    let total = bytes.len() as u64;
    let saved = path.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        std::fs::write(&saved, &bytes).map_err(|e| format!("cannot write {}: {e}", saved.display()))
    })
    .await
    .map_err(|e| format!("write task failed: {e}"))??;
    push_row(Transfer {
        id: 0,
        direction: TransferDirection::Download,
        instance,
        session_id,
        remote_name: name,
        local_path: Some(path),
        source_url: None,
        status: TransferStatus::Done,
        bytes_total: total,
        bytes_done: total,
        error: None,
        created_at: now_secs(),
    });
    emit_changed(&app);
    Ok(total)
}

/// Rows to mirror from `downloads.rs` records that are not yet in the
/// registry. Pure, so the dedup rule is unit-tested.
fn new_mirror_rows(
    rows: &[Transfer],
    records: &[crate::downloads::DownloadRecord],
) -> Vec<Transfer> {
    let mut out = Vec::new();
    for record in records {
        let duplicated = rows.iter().any(|t| {
            t.source_url.as_deref() == Some(record.url.as_str())
                && t.created_at == record.requested_at_secs
        });
        if duplicated {
            continue;
        }
        out.push(Transfer {
            id: 0,
            direction: TransferDirection::Download,
            instance: String::new(),
            session_id: String::new(),
            remote_name: record
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| "download".to_string()),
            local_path: record.path.clone(),
            source_url: Some(record.url.clone()),
            status: if record.success {
                TransferStatus::Done
            } else {
                TransferStatus::Failed
            },
            bytes_total: 0,
            bytes_done: 0,
            error: if record.success {
                None
            } else {
                Some("the download was interrupted".to_string())
            },
            created_at: record.requested_at_secs,
        });
    }
    out
}

/// Mirror `downloads.rs` records into the transfer list so every
/// engine download is visible with an open-folder action. Polled once
/// per second from `drop.rs`'s loop.
pub fn mirror_engine_downloads(app: &AppHandle) {
    let records = crate::downloads::records();
    let mut rows = registry();
    let new_rows = new_mirror_rows(&rows, &records);
    if new_rows.is_empty() {
        return;
    }
    for mut row in new_rows {
        row.id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        rows.push(row);
    }
    drop(rows);
    emit_changed(app);
}

// ---------------------------------------------------------------------------
// Native dialogs (tauri-plugin-dialog; needs the dispatcher to register
// the plugin)
// ---------------------------------------------------------------------------

/// Native message notice. Runs the dialog on the main thread and blocks
/// the caller. Never panics: without the plugin registered the call is
/// swallowed and logged.
pub fn notice(app: &AppHandle, title: &str, message: &str) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = app
            .dialog()
            .message(message.to_string())
            .title(title.to_string())
            .buttons(MessageDialogButtons::Ok)
            .blocking_show();
    }));
}

/// Native three-way conflict prompt: Overwrite / Rename / Cancel.
pub fn conflict_prompt(app: &AppHandle, remote: &str) -> ConflictChoice {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.dialog()
            .message(format!(
                "{remote} already exists in the session drive. \
                 Overwrite it, upload under a new name, or cancel?"
            ))
            .title("File exists")
            .buttons(MessageDialogButtons::YesNoCancelCustom(
                "Overwrite".to_string(),
                "Rename".to_string(),
                "Cancel".to_string(),
            ))
            .blocking_show_with_result()
    }));
    match result {
        Err(_) => ConflictChoice::Cancel,
        Ok(MessageDialogResult::Yes) => ConflictChoice::Overwrite,
        Ok(MessageDialogResult::No) => ConflictChoice::Rename,
        Ok(MessageDialogResult::Ok) => ConflictChoice::Overwrite,
        Ok(MessageDialogResult::Cancel) => ConflictChoice::Cancel,
        Ok(MessageDialogResult::Custom(label)) => match label.as_str() {
            "Overwrite" => ConflictChoice::Overwrite,
            "Rename" => ConflictChoice::Rename,
            _ => ConflictChoice::Cancel,
        },
    }
}

/// Native save dialog, pre-filled with the file name.
pub fn save_dialog(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let picked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.dialog().file().set_file_name(name).blocking_save_file()
    }));
    match picked {
        Ok(Some(tauri_plugin_dialog::FilePath::Path(path))) => Some(path),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Transfer window
// ---------------------------------------------------------------------------

/// Create the hidden transfer window. Called once from the setup hook
/// (main thread); shown on demand via [`show_transfer_window`].
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    WebviewWindowBuilder::new(
        app,
        TRANSFER_WINDOW_LABEL,
        WebviewUrl::App("transfer.html".into()),
    )
    .title("Transfers")
    .inner_size(520.0, 460.0)
    .min_inner_size(360.0, 280.0)
    .visible(false)
    .build()?;
    Ok(())
}

/// Show and focus the transfer window (e.g. when an upload batch
/// starts). Window operations run on the main thread.
pub fn show_transfer_window(app: &AppHandle) {
    let app = app.clone();
    let thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = thread_app.get_webview_window(TRANSFER_WINDOW_LABEL) {
            let _ = win.show();
            let _ = win.set_focus();
        }
    });
}

// ---------------------------------------------------------------------------
// Commands (registered by the dispatcher in lib.rs)
// ---------------------------------------------------------------------------

/// Current transfer list for the transfer window.
#[tauri::command]
pub fn cmd_transfers_list() -> Vec<TransferView> {
    transfers_view()
}

/// Retry a failed upload: re-checks conflicts and re-uploads.
#[tauri::command]
pub async fn cmd_transfer_retry(app: AppHandle, id: u64) -> Result<(), String> {
    let upload = {
        let rows = registry();
        rows.iter()
            .find(|t| t.id == id)
            .filter(|t| t.direction == TransferDirection::Upload)
            .map(|t| {
                (
                    t.instance.clone(),
                    t.session_id.clone(),
                    t.local_path.clone(),
                    t.remote_name.clone(),
                )
            })
            .ok_or_else(|| "no upload transfer with that id".to_string())?
    };
    let (instance, session_id, local_path, _remote_name) = upload;
    let local_path = local_path.ok_or_else(|| "the upload has no source file".to_string())?;
    let bearer = pairing::token_for(&app, &instance)
        .await
        .ok_or_else(|| "no paired token for this instance".to_string())?;
    let resp = drive_client()
        .list(&instance, &session_id, &bearer)
        .await
        .map_err(|e| format!("could not reach the server: {e}"))?;
    let entries = match classify_list(&resp) {
        ListOutcome::Ready(entries) => entries,
        ListOutcome::Reject(reject) => return Err(reject.message()),
    };
    let planned = plan_uploads(&[local_path], &entries, |name| conflict_prompt(&app, name));
    let Some(planned) = planned.into_iter().next() else {
        return Ok(());
    };
    spawn_upload(
        app,
        instance,
        session_id,
        planned.local_path,
        planned.remote_name,
    )?;
    Ok(())
}

/// Open the local folder of a finished download in the file manager.
#[tauri::command]
pub fn cmd_transfer_open_folder(id: u64) -> Result<(), String> {
    let path = registry()
        .iter()
        .find(|t| t.id == id)
        .and_then(|t| t.local_path.clone())
        .ok_or_else(|| "no local path for that transfer".to_string())?;
    tauri_plugin_opener::reveal_item_in_dir(&path)
        .or_else(|_| {
            let dir = path.parent().unwrap_or(&path);
            tauri_plugin_opener::open_path(dir, None::<&str>)
        })
        .map_err(|e| format!("cannot open folder: {e}"))
}

/// Remove finished rows (done / failed / cancelled) from the list.
#[tauri::command]
pub fn cmd_transfer_clear_finished() -> Result<(), String> {
    let mut rows = registry();
    rows.retain(|t| !t.status.finished());
    Ok(())
}

/// Shell-side download of a drive file: REST fetch with the paired
/// Bearer, native save dialog, transfer row. The argument is the drive
/// REST URL from a download row's `sourceUrl`.
#[tauri::command]
pub async fn cmd_transfer_download(app: AppHandle, url: String) -> Result<(), String> {
    download_from_url(app, url).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_mock::{MockResponse, MockScript, MockServer};

    fn ok_json(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.to_string(),
        }
    }

    fn created_json(body: &str) -> MockResponse {
        MockResponse {
            status: 201,
            reason: "Created",
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.to_string(),
        }
    }

    fn csrf_response(token: &str) -> MockResponse {
        MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "Set-Cookie".to_string(),
                    format!("{CSRF_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax"),
                ),
            ],
            body: "{}".to_string(),
        }
    }

    fn error_response(status: u16, reason: &'static str, message: &str) -> MockResponse {
        MockResponse {
            status,
            reason,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: format!("{{\"error\":\"{message}\"}}"),
        }
    }

    fn entries(names: &[&str]) -> Vec<DriveEntry> {
        names
            .iter()
            .map(|n| DriveEntry {
                name: n.to_string(),
                size: 10,
                modified: "2026-08-13T00:00:00Z".to_string(),
            })
            .collect()
    }

    #[test]
    fn upload_bootstraps_csrf_and_echoes_it_with_bearer() {
        let server = MockServer::start(MockScript::new(vec![
            csrf_response("csrf-drive-1"),
            created_json("{\"name\":\"a.txt\",\"size\":4,\"modified\":\"2026-08-13T00:00:00Z\"}"),
        ]));
        let client = DriveClient::new();
        let result = tauri::async_runtime::block_on(async {
            client
                .upload(&server.url(), "sess-1", "a.txt", b"data", "tkn-1")
                .await
        });
        assert!(result.unwrap().is_success());
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/auth/status");
        let put = &requests[1];
        assert_eq!(put.method, "PUT");
        assert_eq!(put.path, "/api/sessions/sess-1/drive-files/a.txt");
        assert_eq!(
            put.headers.get("cookie").map(|s| s.as_str()),
            Some("csrf_token=csrf-drive-1")
        );
        assert_eq!(
            put.headers.get("x-csrf-token").map(|s| s.as_str()),
            Some("csrf-drive-1")
        );
        assert_eq!(
            put.headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer tkn-1")
        );
        assert_eq!(put.body, "data");
    }

    #[test]
    fn upload_surfaces_server_errors() {
        let server = MockServer::start(MockScript::new(vec![
            csrf_response("csrf-drive-2"),
            error_response(403, "Forbidden", "owner or admin only"),
        ]));
        let client = DriveClient::new();
        let result = tauri::async_runtime::block_on(async {
            client
                .upload(&server.url(), "sess-1", "a.txt", b"data", "tkn-1")
                .await
        });
        let resp = result.unwrap();
        assert_eq!(resp.status, reqwest::StatusCode::FORBIDDEN);
        assert_eq!(resp.server_error().as_deref(), Some("owner or admin only"));
    }

    #[test]
    fn list_parses_entries_and_echoes_bearer() {
        let server = MockServer::start(MockScript::new(vec![ok_json(
            "[{\"name\":\"a.txt\",\"size\":4,\"modified\":\"2026-08-13T00:00:00Z\"},\
              {\"name\":\"b.bin\",\"size\":7,\"modified\":\"2026-08-13T00:00:00Z\"}]",
        )]));
        let client = DriveClient::new();
        let resp = tauri::async_runtime::block_on(async {
            client.list(&server.url(), "sess-1", "tkn-1").await
        })
        .unwrap();
        assert!(matches!(
            classify_list(&resp),
            ListOutcome::Ready(entries) if entries.len() == 2
        ));
        let requests = server.requests();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/sessions/sess-1/drive-files");
        assert_eq!(
            requests[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer tkn-1")
        );
    }

    #[test]
    fn list_classification_maps_every_failure_mode() {
        let ssh = classify_list(&DriveResponse {
            status: reqwest::StatusCode::NOT_FOUND,
            body: serde_json::json!({"error": "this session has no file-transfer drive"}),
        });
        assert_eq!(ssh, ListOutcome::Reject(DropReject::SshOnly));
        let gone = classify_list(&DriveResponse {
            status: reqwest::StatusCode::NOT_FOUND,
            body: serde_json::json!({"error": "session not found"}),
        });
        assert_eq!(gone, ListOutcome::Reject(DropReject::SessionGone));
        let owner = classify_list(&DriveResponse {
            status: reqwest::StatusCode::FORBIDDEN,
            body: serde_json::json!({"error": "owner or admin only"}),
        });
        assert_eq!(owner, ListOutcome::Reject(DropReject::NotOwner));
        let token = classify_list(&DriveResponse {
            status: reqwest::StatusCode::UNAUTHORIZED,
            body: serde_json::json!({"error": "invalid token"}),
        });
        assert_eq!(token, ListOutcome::Reject(DropReject::TokenRejected));
        let unreachable = classify_list(&DriveResponse {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            body: serde_json::json!({"error": "boom"}),
        });
        assert_eq!(
            unreachable,
            ListOutcome::Reject(DropReject::Unreachable("boom".to_string()))
        );
        let ready = classify_list(&DriveResponse {
            status: reqwest::StatusCode::OK,
            body: serde_json::json!([]),
        });
        assert_eq!(ready, ListOutcome::Ready(vec![]));
    }

    #[test]
    fn plan_uploads_resolves_conflicts_per_decision() {
        let existing = entries(&["a.txt", "c.txt"]);
        let files = vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            PathBuf::from("/tmp/c.txt"),
        ];
        let decisions = ["Overwrite", "Rename"];
        let mut i = 0;
        let planned = plan_uploads(&files, &existing, |_name| {
            let choice = match decisions[i] {
                "Overwrite" => ConflictChoice::Overwrite,
                _ => ConflictChoice::Rename,
            };
            i += 1;
            choice
        });
        let names: Vec<&str> = planned.iter().map(|p| p.remote_name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c (1).txt"]);
        assert_eq!(i, 2);
    }

    #[test]
    fn plan_uploads_cancel_skips_the_file() {
        let existing = entries(&["a.txt"]);
        let planned = plan_uploads(
            &[PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
            &existing,
            |_| ConflictChoice::Cancel,
        );
        let names: Vec<&str> = planned.iter().map(|p| p.remote_name.as_str()).collect();
        assert_eq!(names, vec!["b.txt"]);
    }

    #[test]
    fn plan_uploads_without_conflicts_keeps_names() {
        let planned = plan_uploads(
            &[PathBuf::from("/tmp/x.txt"), PathBuf::from("/tmp/y.bin")],
            &[],
            |_| ConflictChoice::Cancel,
        );
        let names: Vec<&str> = planned.iter().map(|p| p.remote_name.as_str()).collect();
        assert_eq!(names, vec!["x.txt", "y.bin"]);
    }

    #[test]
    fn plan_uploads_sanitizes_hostile_names() {
        let planned = plan_uploads(
            &[
                PathBuf::from("/tmp/a/b:c.txt"),
                PathBuf::from("/tmp/..hidden"),
            ],
            &[],
            |_| ConflictChoice::Cancel,
        );
        let names: Vec<&str> = planned.iter().map(|p| p.remote_name.as_str()).collect();
        assert_eq!(names, vec!["b_c.txt", "hidden"]);
    }

    #[test]
    fn rename_away_skips_taken_suffixes() {
        let existing: Vec<String> = vec!["a.txt".into(), "a (1).txt".into(), "a (2).txt".into()];
        assert_eq!(rename_away("a.txt", &existing), "a (3).txt");
        assert_eq!(rename_away("notes", &[]), "notes (1)");
        assert_eq!(
            rename_away("notes", &["notes".into(), "notes (1)".into()]),
            "notes (2)"
        );
    }

    #[test]
    fn download_round_trips_bytes() {
        let server = MockServer::start(MockScript::new(vec![MockResponse {
            status: 200,
            reason: "OK",
            headers: vec![(
                "Content-Type".to_string(),
                "application/octet-stream".to_string(),
            )],
            body: "hello drive".to_string(),
        }]));
        let client = DriveClient::new();
        let bytes = tauri::async_runtime::block_on(async {
            client
                .download(&server.url(), "sess-1", "x.txt", "tkn-1")
                .await
        })
        .unwrap();
        assert_eq!(bytes, b"hello drive");
        let requests = server.requests();
        assert_eq!(requests[0].path, "/api/sessions/sess-1/drive-files/x.txt");
        assert_eq!(
            requests[0].headers.get("authorization").map(|s| s.as_str()),
            Some("Bearer tkn-1")
        );
    }

    #[test]
    fn download_errors_carry_server_message() {
        let server = MockServer::start(MockScript::new(vec![error_response(
            404,
            "Not Found",
            "file not found",
        )]));
        let client = DriveClient::new();
        let err = tauri::async_runtime::block_on(async {
            client
                .download(&server.url(), "sess-1", "x.txt", "tkn-1")
                .await
        })
        .unwrap_err();
        assert!(err.contains("404"));
        assert!(err.contains("file not found"));
    }

    #[test]
    fn drive_url_parsing_extracts_session_and_name() {
        assert_eq!(
            parse_drive_url(
                "https://persea.example.com/api/sessions/abc-123/drive-files/report%20q1.csv"
            ),
            Some((
                "https://persea.example.com".to_string(),
                "abc-123".to_string(),
                "report q1.csv".to_string()
            ))
        );
        assert_eq!(
            parse_drive_url(
                "https://persea.example.com/api/sessions/abc-123/drive-files/a.txt?token=x"
            ),
            Some((
                "https://persea.example.com".to_string(),
                "abc-123".to_string(),
                "a.txt".to_string()
            ))
        );
        assert_eq!(
            parse_drive_url("blob:https://persea.example.com/uuid"),
            None
        );
        assert_eq!(
            parse_drive_url("https://persea.example.com/export/report.csv"),
            None
        );
    }

    #[test]
    fn mirror_rows_dedupe_and_mark_interruptions() {
        let records = vec![
            crate::downloads::DownloadRecord {
                url: "https://persea.example.com/api/sessions/s1/drive-files/a.txt".to_string(),
                path: Some(PathBuf::from("/home/user/Downloads/a.txt")),
                success: true,
                requested_at_secs: 10,
            },
            crate::downloads::DownloadRecord {
                url: "https://persea.example.com/api/sessions/s1/drive-files/a.txt".to_string(),
                path: Some(PathBuf::from("/home/user/Downloads/a.txt")),
                success: true,
                requested_at_secs: 11,
            },
            crate::downloads::DownloadRecord {
                url: "blob:https://persea.example.com/uuid".to_string(),
                path: Some(PathBuf::from("/home/user/Downloads/shot.png")),
                success: false,
                requested_at_secs: 12,
            },
        ];
        let mut rows: Vec<Transfer> = Vec::new();
        let first = new_mirror_rows(&rows, &records);
        assert_eq!(first.len(), 3);
        assert_eq!(first[2].status, TransferStatus::Failed);
        rows.extend(first);
        // Re-polling the same records adds nothing.
        assert!(new_mirror_rows(&rows, &records).is_empty());
        // A new record for the same URL at a new time adds a row.
        let again = new_mirror_rows(
            &rows,
            &[crate::downloads::DownloadRecord {
                url: "https://persea.example.com/api/sessions/s1/drive-files/a.txt".to_string(),
                path: Some(PathBuf::from("/home/user/Downloads/a.txt")),
                success: true,
                requested_at_secs: 13,
            }],
        );
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn registry_caps_and_clears_finished() {
        {
            let mut guard = registry();
            guard.clear();
        }
        for _ in 0..(ROW_CAP + 5) {
            push_row(Transfer {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                direction: TransferDirection::Upload,
                instance: "https://persea.example.com".into(),
                session_id: "sess-1".into(),
                remote_name: "x.txt".into(),
                local_path: None,
                source_url: None,
                status: TransferStatus::Done,
                bytes_total: 1,
                bytes_done: 1,
                error: None,
                created_at: 1,
            });
        }
        assert_eq!(registry().len(), ROW_CAP);
        registry().clear();
        assert!(registry().is_empty());
    }
}
