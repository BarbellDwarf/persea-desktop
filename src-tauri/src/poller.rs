//! Session poller: the tray's session view and lifecycle notifications.
//!
//! One background task per paired instance. Cadence 10s (locked design;
//! the web client's own sessions page polls every 5s, so 10s stays well
//! inside the server's rate limits). Each tick fetches `GET /api/sessions`
//! with the paired Bearer token (pairing.rs) and diffs per-session status
//! through [`DiffEngine`]; the deltas become notifications (started for
//! own sessions, ended, error, idle-warning where derivable) and the
//! session list feeds the tray menu.
//!
//! SSE upgrade (locked design): when the instance probe reports the
//! `session_events` capability (server >= 1.2.0), the task subscribes to
//! `GET /api/sessions/events` instead of polling. The first poll still
//! seeds the engine (so pre-existing sessions never notify), then events
//! stream in with `id:` cursors; on disconnect the task falls back to
//! polling and re-engages SSE on a later tick. The engine is the single
//! diff authority, so a catch-up poll and a `Last-Event-ID` replay can
//! never double-notify: a status the engine already recorded produces no
//! delta. The stream uses a reqwest client with a read timeout only (the
//! server sends `: ping` keepalives every ~15s), never a total timeout,
//! and without the `stream` feature: the response body is read with
//! `chunk()` and parsed incrementally by [`SseParser`].
//!
//! 401 handling (locked design): an auth failure pauses the instance,
//! fires ONE "re-login needed" notification (notify.rs, unconditional),
//! marks the tray signed-out badge, and stops all HTTP for that instance.
//! The pause polls nothing; when the pairing registry changes (the user
//! re-pairs, the token id rotates), the engine is reset and the task
//! reseeds from a fresh poll, so sessions that changed while signed out
//! never spam notifications.
//!
//! Idle-warning approximation (locked design): the list endpoint does not
//! carry the server's idle reaper timeout, but `GET /api/sessions/{id}`
//! does (`session_idle_timeout_secs`, a global config value). The task
//! fetches the detail of one live session per instance per poller
//! lifetime, caches the timeout, and warns once per session when
//! `last_activity` falls inside the last 60s before the reap. No detail
//! reachable = no warnings (documented "where derivable").
//!
//! Thumbnails: `thumbnail_url` is a relative path; the task downloads it
//! to a temp file on Windows only (WinRT toasts take local paths, and
//! only Windows toasts show images per the locked design), and passes the
//! path to notify.rs. Other platforms notify without an image.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use tauri::AppHandle;

use crate::instances;
use crate::{http, notify, pairing};

/// Poll cadence (locked design).
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Read timeout for the SSE stream: the server pings every ~15s, so a
/// 90s silence means the stream died. Errors drop back to polling.
const SSE_READ_TIMEOUT: Duration = Duration::from_secs(90);
/// Connect timeout for the SSE client.
const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Re-engage SSE after this many poll ticks (60s) following a failure.
const SSE_REENGAGE_TICKS: u32 = 6;
/// Heartbeat poll cadence while streaming: keeps names, idle-warning
/// math and token validity fresh (the SSE feed carries no timestamps and
/// the server never re-checks the Bearer mid-stream).
const HEARTBEAT_EVERY: Duration = Duration::from_secs(60);
/// Idle-warning horizon in seconds (matches the page's 60s-before-reap
/// toast).
const IDLE_WARNING_HORIZON_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Session view + diff engine (pure, unit-tested)
// ---------------------------------------------------------------------------

/// One session as observed by the poller.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub session_id: String,
    pub session_type: String,
    /// pending | active | completed | error | expired | disconnected |
    /// logged_out
    pub status: String,
    /// Display name: entry display name, else user@host, else hostname,
    /// else the session type.
    pub name: String,
    pub created_by: String,
    /// Last real activity, epoch seconds.
    pub last_activity: Option<u64>,
    /// Relative thumbnail path (`/api/sessions/{id}/thumbnail`).
    pub thumbnail_url: Option<String>,
    /// Relative client page path (`/client/{id}`).
    pub client_url: String,
    /// Server idle reaper timeout, when known (detail fetch only; the
    /// list endpoint does not carry it).
    pub idle_limit_secs: Option<u64>,
}

/// A transition the engine noticed in one apply. One notification each.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionDelta {
    Started {
        session_id: String,
        name: String,
        session_type: String,
    },
    Ended {
        session_id: String,
        name: String,
        status: String,
    },
    Error {
        session_id: String,
        name: String,
    },
    IdleWarning {
        session_id: String,
        name: String,
    },
}

/// One SSE lifecycle event (subset of the server's SessionEvent).
#[derive(Debug, Clone, PartialEq)]
pub struct EventView {
    /// Monotonic cursor for `Last-Event-ID` resumes.
    pub id: u64,
    /// session_started | status_changed | session_ended
    pub event: String,
    pub session_id: String,
    pub session_type: String,
    pub status: String,
    pub created_by: String,
}

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "error" | "expired" | "logged_out")
}

/// Per-instance session diff authority. Seeding (first poll, or after a
/// re-pair reset) records the current sessions without deltas; every
/// later snapshot or SSE event diffs against the recorded views. Because
/// the engine is the single authority, poll and SSE inputs dedupe
/// against each other.
#[derive(Debug, Default)]
pub struct DiffEngine {
    views: HashMap<String, SessionView>,
    idle_warned: HashSet<String>,
}

impl DiffEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded view for a session, if any.
    #[cfg(test)]
    pub fn view(&self, session_id: &str) -> Option<&SessionView> {
        self.views.get(session_id)
    }

    /// Every recorded view, for the tray menu.
    pub fn views(&self) -> Vec<SessionView> {
        self.views.values().cloned().collect()
    }

    /// Record a snapshot without emitting deltas. First poll, and the
    /// poll after a re-pair reset.
    pub fn seed(&mut self, views: &[SessionView]) {
        for view in views {
            self.apply_view(view.clone(), false, now_secs());
        }
    }

    /// Diff a fresh snapshot (GET /api/sessions) and emit transitions.
    pub fn apply(&mut self, views: &[SessionView], now: u64) -> Vec<SessionDelta> {
        let mut out = Vec::new();
        for view in views {
            out.extend(self.apply_view(view.clone(), true, now));
        }
        out
    }

    /// Apply one SSE event.
    pub fn apply_event(&mut self, evt: &EventView, now: u64) -> Vec<SessionDelta> {
        let view = SessionView {
            session_id: evt.session_id.clone(),
            session_type: evt.session_type.clone(),
            status: evt.status.clone(),
            name: evt.session_type.clone(),
            created_by: evt.created_by.clone(),
            last_activity: None,
            thumbnail_url: None,
            client_url: format!("/client/{}", evt.session_id),
            idle_limit_secs: None,
        };
        self.apply_view(view, evt.event == "session_started", now)
    }

    /// Enrich a recorded view (detail fetch: real name, thumbnail, idle
    /// limit) without emitting deltas.
    pub fn merge_detail(&mut self, view: SessionView) {
        if let Some(existing) = self.views.get_mut(&view.session_id) {
            *existing = view;
        } else {
            self.apply_view(view, false, now_secs());
        }
    }

    /// The single diff primitive. `notify_new` is false while seeding and
    /// for replay artifacts (terminal-first events).
    fn apply_view(&mut self, view: SessionView, notify_new: bool, now: u64) -> Vec<SessionDelta> {
        let mut out = Vec::new();
        let id = view.session_id.clone();
        let prev = self.views.get(&id);
        // The idle limit only travels in detail fetches; keep the last
        // known value when a list snapshot omits it.
        let idle_limit = view
            .idle_limit_secs
            .or_else(|| prev.and_then(|p| p.idle_limit_secs));
        match prev {
            None => {
                if notify_new && !is_terminal(&view.status) {
                    out.push(SessionDelta::Started {
                        session_id: id.clone(),
                        name: view.name.clone(),
                        session_type: view.session_type.clone(),
                    });
                }
            }
            Some(prev) if prev.status != view.status => match view.status.as_str() {
                "error" => out.push(SessionDelta::Error {
                    session_id: id.clone(),
                    name: view.name.clone(),
                }),
                status if is_terminal(status) => out.push(SessionDelta::Ended {
                    session_id: id.clone(),
                    name: view.name.clone(),
                    status: status.to_string(),
                }),
                _ => {}
            },
            Some(_) => {}
        }
        if !is_terminal(&view.status) {
            if let (Some(activity), Some(limit)) = (view.last_activity, idle_limit) {
                let idle_for = now.saturating_sub(activity);
                if limit > IDLE_WARNING_HORIZON_SECS
                    && idle_for >= limit.saturating_sub(IDLE_WARNING_HORIZON_SECS)
                    && idle_for < limit
                    && !self.idle_warned.contains(&id)
                {
                    self.idle_warned.insert(id.clone());
                    out.push(SessionDelta::IdleWarning {
                        session_id: id.clone(),
                        name: view.name.clone(),
                    });
                }
            }
        } else {
            self.idle_warned.remove(&id);
        }
        let mut view = view;
        view.idle_limit_secs = idle_limit;
        self.views.insert(id, view);
        out
    }
}

// ---------------------------------------------------------------------------
// SSE wire parsing (incremental, unit-tested)
// ---------------------------------------------------------------------------

/// One parsed SSE frame: the fields between two blank lines.
#[derive(Debug, Clone, PartialEq)]
pub struct SseFrame {
    pub id: Option<String>,
    pub event: Option<String>,
    /// `data:` lines joined with '\n'.
    pub data: String,
}

/// Incremental SSE parser over arbitrary byte chunks. Handles CRLF and
/// LF line endings, `: comment` keepalives, and frames split across
/// chunks. Field state (id/event/data) accumulates across `feed` calls
/// until the blank line that terminates a frame.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    id: Option<String>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of the stream; returns any completed frames.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();
        loop {
            let Some(newline) = self.buffer.find('\n') else {
                break;
            };
            let line = self.buffer[..newline].trim_end_matches('\r').to_string();
            self.buffer.drain(..=newline);
            if line.is_empty() {
                if !self.data.is_empty() || self.id.is_some() || self.event.is_some() {
                    out.push(SseFrame {
                        id: self.id.take(),
                        event: self.event.take(),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // keepalive comment
            }
            let Some((field, value)) = line.split_once(':') else {
                continue; // lines without ':' are ignored
            };
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "id" => self.id = Some(value.to_string()),
                "event" => self.event = Some(value.to_string()),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

/// Parse an RFC 3339 UTC timestamp (as the server serializes
/// `DateTime<Utc>`) into epoch seconds. Accepts `Z` and `+00:00`
/// offsets; anything else yields None.
fn parse_rfc3339_epoch(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (date, rest) = raw.split_once('T')?;
    let rest = rest
        .strip_suffix('Z')
        .or_else(|| rest.strip_suffix("+00:00"))?;
    let time = rest.split_once('.').map(|(t, _)| t).unwrap_or(rest);
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(
        (days * 86_400) as u64
            + u64::from(hour) * 3_600
            + u64::from(minute) * 60
            + u64::from(second),
    )
}

/// Howard Hinnant's days-from-civil algorithm, inverted: civil date to
/// days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

// ---------------------------------------------------------------------------
// Wire parsing (server JSON)
// ---------------------------------------------------------------------------

/// Build the display name for a session, mirroring the web UI's labels.
fn display_name(view: &serde_json::Value) -> String {
    let entry = view
        .get("entry_display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let host = view.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
    let user = view.get("username").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(entry) = entry {
        return entry.to_string();
    }
    if !user.is_empty() && !host.is_empty() {
        return format!("{user}@{host}");
    }
    if !host.is_empty() {
        return host.to_string();
    }
    view.get("session_type")
        .and_then(|v| v.as_str())
        .unwrap_or("session")
        .to_string()
}

fn parse_last_activity(view: &serde_json::Value) -> Option<u64> {
    view.get("last_activity")
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339_epoch)
}

/// Parse one SessionInfo JSON object (from the list or the detail
/// endpoint) into a SessionView. The detail endpoint additionally carries
/// `session_idle_timeout_secs`.
fn parse_session_view(view: &serde_json::Value) -> Option<SessionView> {
    let session_id = view.get("session_id").and_then(|v| v.as_str())?;
    Some(SessionView {
        session_id: session_id.to_string(),
        session_type: view
            .get("session_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        status: view
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name: display_name(view),
        created_by: view
            .get("created_by")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        last_activity: parse_last_activity(view),
        thumbnail_url: view
            .get("thumbnail_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        client_url: view
            .get("client_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        idle_limit_secs: view
            .get("session_idle_timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0),
    })
}

// ---------------------------------------------------------------------------
// Per-instance task
// ---------------------------------------------------------------------------

enum Mode {
    /// Regular 10s polling; `ticks` counts successful ticks (SSE
    /// re-engage cadence).
    Polling { ticks: u32 },
    /// SSE subscribed; falls back to polling on any failure.
    Streaming,
    /// 401: paused until the pairing registry changes.
    SignedOut,
}

/// Outcome of one poll tick.
enum TickOutcome {
    Ok,
    /// 401: token rejected.
    AuthFailed,
    /// Network error, 5xx, malformed body: keep polling.
    Transient,
}

/// Outcome of one SSE session.
enum StreamOutcome {
    /// Stream ended or errored; `cursor` is the last event id seen.
    Disconnected(Option<u64>),
    /// 401 on connect: token rejected.
    AuthFailed,
    /// Could not connect at all (network, 409 slot busy, 5xx): poll and
    /// re-engage later.
    Transient,
}

/// Per-instance task state.
struct InstanceState {
    engine: DiffEngine,
    mode: Mode,
    /// Last SSE event cursor, for `Last-Event-ID` resumes.
    cursor: Option<u64>,
    /// Fingerprint of the instance's registered tokens; a change means
    /// re-pair / identity switch and resets everything.
    token_fingerprint: Vec<(i64, String)>,
    /// Cached idle reaper timeout (global server config).
    idle_limit: Option<u64>,
}

/// The supervisor: one task per configured, paired instance. Tasks exit
/// when their instance disappears; the handle is aborted on removal so a
/// re-added instance never runs two pollers (duplicate notifications).
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut running: HashMap<String, tauri::async_runtime::JoinHandle<()>> = HashMap::new();
        loop {
            let desired: HashSet<String> = instances::instances()
                .iter()
                .map(|i| i.url.clone())
                .filter(|url| {
                    pairing::registered_tokens(&app)
                        .iter()
                        .any(|t| t.instance_url == *url)
                })
                .collect();
            running.retain(|url, handle| {
                if desired.contains(url) {
                    true
                } else {
                    handle.abort();
                    false
                }
            });
            for url in &desired {
                if !running.contains_key(url) {
                    let handle = app.clone();
                    let url = url.clone();
                    running.insert(
                        url.clone(),
                        tauri::async_runtime::spawn(async move {
                            run_instance(handle, url).await;
                        }),
                    );
                }
            }
            http::sleep(POLL_INTERVAL).await;
        }
    });
}

fn token_fingerprint(app: &AppHandle, instance_url: &str) -> Vec<(i64, String)> {
    let mut tokens: Vec<(i64, String)> = pairing::registered_tokens(app)
        .iter()
        .filter(|t| t.instance_url == instance_url)
        .map(|t| (t.token_id, t.token_name.clone()))
        .collect();
    tokens.sort();
    tokens
}

async fn run_instance(app: AppHandle, url: String) {
    let mut state = InstanceState {
        engine: DiffEngine::new(),
        mode: Mode::Polling { ticks: 0 },
        cursor: None,
        token_fingerprint: token_fingerprint(&app, &url),
        idle_limit: None,
    };
    loop {
        if instances::instance(&url).is_none() {
            return;
        }
        let fingerprint = token_fingerprint(&app, &url);
        if fingerprint != state.token_fingerprint {
            // Re-pair (or a different identity): reset and reseed so
            // sessions that changed while we were paused never notify.
            state.token_fingerprint = fingerprint.clone();
            state.engine = DiffEngine::new();
            state.cursor = None;
            state.idle_limit = None;
            state.mode = Mode::Polling { ticks: 0 };
            crate::tray::set_signed_out(&app, &url, false);
        }
        if fingerprint.is_empty() {
            // Not paired: nothing to poll.
            http::sleep(POLL_INTERVAL).await;
            continue;
        }
        match state.mode {
            Mode::SignedOut => {
                http::sleep(POLL_INTERVAL).await;
                // The fingerprint check above already flipped us back to
                // Polling when a new token landed; stay paused otherwise.
            }
            Mode::Polling { ticks } => {
                http::sleep(POLL_INTERVAL).await;
                let Some(token) = pairing::token_for(&app, &url).await else {
                    continue;
                };
                match tick_poll(&app, &url, &token, &mut state).await {
                    TickOutcome::Ok => {
                        let reengage = ticks % SSE_REENGAGE_TICKS == 0;
                        state.mode = if reengage && instances::capability(&url, "session_events") {
                            Mode::Streaming
                        } else {
                            Mode::Polling { ticks: ticks + 1 }
                        };
                    }
                    TickOutcome::AuthFailed => {
                        crate::tray::set_signed_out(&app, &url, true);
                        notify::relogin_needed(&app, &instance_name(&url));
                        state.mode = Mode::SignedOut;
                    }
                    TickOutcome::Transient => {
                        state.mode = Mode::Polling { ticks: ticks + 1 };
                    }
                }
            }
            Mode::Streaming => {
                let Some(token) = pairing::token_for(&app, &url).await else {
                    state.mode = Mode::Polling { ticks: 0 };
                    continue;
                };
                match stream_sessions(&app, &url, &token, state.cursor, &mut state).await {
                    StreamOutcome::Disconnected(cursor) => {
                        state.cursor = cursor;
                        state.mode = Mode::Polling { ticks: 0 };
                    }
                    StreamOutcome::AuthFailed => {
                        crate::tray::set_signed_out(&app, &url, true);
                        notify::relogin_needed(&app, &instance_name(&url));
                        state.mode = Mode::SignedOut;
                    }
                    StreamOutcome::Transient => {
                        // Could not (re)connect: poll, and pace the next
                        // stream attempt via the re-engage cadence (the
                        // tick counter starts at 1 so a dead server gets
                        // one connection attempt per minute, not per tick).
                        state.mode = Mode::Polling { ticks: 1 };
                    }
                }
            }
        }
    }
}

fn instance_name(url: &str) -> String {
    instances::instance(url)
        .map(|i| i.name)
        .unwrap_or_else(|| url.to_string())
}

/// One poll tick: fetch the session list, diff, notify, refresh the tray.
/// The first tick after a reset seeds instead of diffing (pre-existing
/// sessions never notify).
async fn tick_poll(
    app: &AppHandle,
    url: &str,
    token: &str,
    state: &mut InstanceState,
) -> TickOutcome {
    let fresh = state.engine.views().is_empty();
    let result = match http::shell_http()
        .get(url, "/api/sessions", Some(token))
        .await
    {
        Ok(result) => result,
        Err(_) => return TickOutcome::Transient,
    };
    if result.status == StatusCode::UNAUTHORIZED {
        return TickOutcome::AuthFailed;
    }
    if !result.status.is_success() {
        return TickOutcome::Transient;
    }
    let Some(array) = result.body.as_array() else {
        return TickOutcome::Transient;
    };
    let mut views: Vec<SessionView> = Vec::with_capacity(array.len());
    for item in array {
        if let Some(view) = parse_session_view(item) {
            views.push(view);
        }
    }
    // The idle reaper timeout is a per-instance config value; stamp the
    // cached value on every list view (the list endpoint omits it).
    if let Some(limit) = state.idle_limit {
        for view in &mut views {
            view.idle_limit_secs = Some(limit);
        }
    }
    if fresh {
        state.engine.seed(&views);
        if state.idle_limit.is_none() {
            discover_idle_limit(app, url, token, state).await;
        }
    } else {
        let now = now_secs();
        let deltas = state.engine.apply(&views, now);
        fire_deltas(app, url, token, state, deltas).await;
    }
    crate::tray::set_sessions(app, url, tray_sessions(url, state.engine.views()));
    TickOutcome::Ok
}

/// One detail fetch for a live session, to learn the global idle reaper
/// timeout (`session_idle_timeout_secs` is a config value the list
/// endpoint does not carry). Best effort: a failure just leaves the
/// cache empty (no idle warnings, "where derivable").
async fn discover_idle_limit(app: &AppHandle, url: &str, token: &str, state: &mut InstanceState) {
    let live = state
        .engine
        .views()
        .into_iter()
        .find(|v| !is_terminal(&v.status));
    let Some(view) = live else { return };
    let Some(detail) = fetch_session_detail(app, url, token, &view.session_id).await else {
        return;
    };
    state.idle_limit = detail.idle_limit_secs;
    if state.idle_limit.is_some() {
        state.engine.merge_detail(detail);
    }
}

/// The tray's view of an instance: absolute client URLs, live statuses.
fn tray_sessions(instance_url: &str, views: Vec<SessionView>) -> Vec<crate::tray::TraySession> {
    let base = instance_url.trim_end_matches('/');
    let mut sessions: Vec<crate::tray::TraySession> = views
        .into_iter()
        .map(|v| crate::tray::TraySession {
            id: v.session_id,
            name: v.name,
            status: v.status,
            url: format!("{base}{}", v.client_url),
        })
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    sessions
}

/// Turn engine deltas into notifications. Session names come from the
/// engine's recorded views (enriched by detail fetches).
async fn fire_deltas(
    app: &AppHandle,
    url: &str,
    token: &str,
    state: &mut InstanceState,
    deltas: Vec<SessionDelta>,
) {
    for delta in deltas {
        match delta {
            SessionDelta::Started {
                session_id,
                name,
                session_type,
            } => {
                // The list view may only carry the fallback name; fetch
                // the detail for the real name, thumbnail and idle limit.
                if let Some(detail) = fetch_session_detail(app, url, token, &session_id).await {
                    if state.idle_limit.is_none() {
                        state.idle_limit = detail.idle_limit_secs;
                    }
                    let name = detail.name.clone();
                    let session_type = detail.session_type.clone();
                    state.engine.merge_detail(detail);
                    notify::session_started(app, &name, &session_type);
                } else {
                    notify::session_started(app, &name, &session_type);
                }
            }
            SessionDelta::Ended {
                session_id,
                name,
                status,
            } => {
                let thumbnail = thumbnail_path(app, url, token, &session_id).await;
                notify::session_ended(app, &name, &status, thumbnail.as_deref());
            }
            SessionDelta::Error { session_id, name } => {
                let thumbnail = thumbnail_path(app, url, token, &session_id).await;
                notify::session_error(app, &name, thumbnail.as_deref());
            }
            SessionDelta::IdleWarning { name, .. } => {
                notify::session_idle_warning(app, &name);
            }
        }
    }
}

/// Fetch the detail of one session (`GET /api/sessions/{id}`): real
/// name, thumbnail, idle reaper timeout. Best effort; failures yield
/// None (the engine keeps the list-level view).
async fn fetch_session_detail(
    _app: &AppHandle,
    url: &str,
    token: &str,
    session_id: &str,
) -> Option<SessionView> {
    let result = http::shell_http()
        .get(url, &format!("/api/sessions/{session_id}"), Some(token))
        .await
        .ok()?;
    if !result.status.is_success() {
        return None;
    }
    parse_session_view(&result.body)
}

/// Windows toasts only: download the session thumbnail to a temp file
/// and return its path. The WinRT image element needs a local file, and
/// per the locked design only Windows toasts carry images.
#[cfg(target_os = "windows")]
async fn thumbnail_path(
    app: &AppHandle,
    url: &str,
    token: &str,
    session_id: &str,
) -> Option<String> {
    let view = fetch_session_detail(app, url, token, session_id).await?;
    let rel = view.thumbnail_url?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let resp = client
        .get(format!("{}{}", url.trim_end_matches('/'), rel))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > 200 * 1024 {
        return None;
    }
    let dir = std::env::temp_dir().join("persea-desktop-thumbnails");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{session_id}.png"));
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().to_string())
}

#[cfg(not(target_os = "windows"))]
async fn thumbnail_path(
    _app: &AppHandle,
    _url: &str,
    _token: &str,
    _session_id: &str,
) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// SSE subscription
// ---------------------------------------------------------------------------

/// Subscribe to `GET /api/sessions/events` and apply events until the
/// stream ends. Resumes from `cursor` with `Last-Event-ID` when set.
///
/// A slow heartbeat poll runs alongside the stream, paced by chunk
/// arrival: the server pings every ~15s, so the loop wakes at least that
/// often while the stream lives. The heartbeat covers what events cannot:
/// session names and last-activity drift, the idle-warning derivation
/// (events carry no timestamps), and token revocation (the server never
/// re-checks the Bearer mid-stream; a 401 on the heartbeat surfaces as
/// an auth failure and the caller pauses).
async fn stream_sessions(
    app: &AppHandle,
    url: &str,
    token: &str,
    cursor: Option<u64>,
    state: &mut InstanceState,
) -> StreamOutcome {
    let client = match reqwest::Client::builder()
        .connect_timeout(SSE_CONNECT_TIMEOUT)
        .read_timeout(SSE_READ_TIMEOUT)
        .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => return StreamOutcome::Transient,
    };
    let mut request = client
        .get(format!("{}/api/sessions/events", url.trim_end_matches('/')))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::ACCEPT, "text/event-stream");
    if let Some(cursor) = cursor {
        request = request.header("Last-Event-ID", cursor.to_string());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => return StreamOutcome::Transient,
    };
    if response.status() == StatusCode::UNAUTHORIZED {
        return StreamOutcome::AuthFailed;
    }
    if !response.status().is_success() {
        // 409 = another client holds the per-user slot; 5xx = server
        // trouble. Both resolve by polling and re-engaging later.
        return StreamOutcome::Transient;
    }
    let mut parser = SseParser::new();
    let mut last_id = cursor;
    let mut response = response;
    let mut last_heartbeat = std::time::Instant::now();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                for frame in parser.feed(&chunk) {
                    if let Some(id) = &frame.id {
                        if let Ok(id) = id.parse::<u64>() {
                            last_id = Some(id);
                        }
                    }
                    let Some(event) = frame.event.as_deref() else {
                        continue;
                    };
                    let Ok(data) = serde_json::from_str::<serde_json::Value>(&frame.data) else {
                        continue;
                    };
                    let Some(event_view) = parse_event(&data, event) else {
                        continue;
                    };
                    let deltas = state.engine.apply_event(&event_view, now_secs());
                    fire_deltas(app, url, token, state, deltas).await;
                    crate::tray::set_sessions(app, url, tray_sessions(url, state.engine.views()));
                }
                if last_heartbeat.elapsed() >= HEARTBEAT_EVERY {
                    last_heartbeat = std::time::Instant::now();
                    match tick_poll(app, url, token, state).await {
                        TickOutcome::Ok => {}
                        TickOutcome::AuthFailed => return StreamOutcome::AuthFailed,
                        TickOutcome::Transient => {}
                    }
                }
            }
            Ok(None) => return StreamOutcome::Disconnected(last_id),
            Err(_) => return StreamOutcome::Disconnected(last_id),
        }
    }
}

fn parse_event(data: &serde_json::Value, event: &str) -> Option<EventView> {
    let session_id = data.get("session_id").and_then(|v| v.as_str())?;
    Some(EventView {
        id: data.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
        event: event.to_string(),
        session_id: session_id.to_string(),
        session_type: data
            .get("session_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        status: data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_by: data
            .get("created_by")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(id: &str, status: &str, name: &str) -> SessionView {
        SessionView {
            session_id: id.to_string(),
            session_type: "ssh".to_string(),
            status: status.to_string(),
            name: name.to_string(),
            created_by: "alice".to_string(),
            last_activity: None,
            thumbnail_url: None,
            client_url: format!("/client/{id}"),
            idle_limit_secs: None,
        }
    }

    fn event(id: u64, kind: &str, session: &str, status: &str) -> EventView {
        EventView {
            id,
            event: kind.to_string(),
            session_id: session.to_string(),
            session_type: "rdp".to_string(),
            status: status.to_string(),
            created_by: "alice".to_string(),
        }
    }

    #[test]
    fn seed_records_without_deltas() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "active", "db")]);
        assert!(engine.apply(&[view("a", "active", "db")], 1000).is_empty());
        assert!(engine.view("a").is_some());
    }

    #[test]
    fn new_session_fires_started() {
        let mut engine = DiffEngine::new();
        engine.seed(&[]);
        let deltas = engine.apply(&[view("a", "pending", "db")], 1000);
        assert_eq!(
            deltas,
            vec![SessionDelta::Started {
                session_id: "a".to_string(),
                name: "db".to_string(),
                session_type: "ssh".to_string(),
            }]
        );
    }

    #[test]
    fn unchanged_snapshot_is_quiet() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "active", "db")]);
        assert!(engine.apply(&[view("a", "active", "db")], 1000).is_empty());
        assert!(engine.apply(&[view("a", "active", "db")], 1010).is_empty());
    }

    #[test]
    fn terminal_transitions_fire_ended() {
        for status in ["completed", "expired", "logged_out"] {
            let mut engine = DiffEngine::new();
            engine.seed(&[view("a", "active", "db")]);
            let deltas = engine.apply(&[view("a", status, "db")], 1000);
            assert_eq!(
                deltas,
                vec![SessionDelta::Ended {
                    session_id: "a".to_string(),
                    name: "db".to_string(),
                    status: status.to_string(),
                }],
                "{status}"
            );
        }
    }

    #[test]
    fn error_fires_error_not_ended() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "active", "db")]);
        let deltas = engine.apply(&[view("a", "error", "db")], 1000);
        assert_eq!(
            deltas,
            vec![SessionDelta::Error {
                session_id: "a".to_string(),
                name: "db".to_string(),
            }]
        );
        // Terminal: a later snapshot stays quiet.
        assert!(engine.apply(&[view("a", "error", "db")], 1010).is_empty());
    }

    #[test]
    fn non_terminal_transitions_are_quiet() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "pending", "db")]);
        assert!(engine.apply(&[view("a", "active", "db")], 1000).is_empty());
        assert!(engine
            .apply(&[view("a", "disconnected", "db")], 1010)
            .is_empty());
    }

    #[test]
    fn terminal_first_appearance_never_fires() {
        let mut engine = DiffEngine::new();
        engine.seed(&[]);
        // A session that appears already-ended (e.g. the seed missed it):
        // no notification.
        assert!(engine
            .apply(&[view("a", "completed", "db")], 1000)
            .is_empty());
    }

    #[test]
    fn sse_and_poll_dedupe() {
        let mut engine = DiffEngine::new();
        engine.seed(&[]);
        // SSE delivers the start...
        let deltas = engine.apply_event(&event(1, "session_started", "a", "pending"), 1000);
        assert_eq!(deltas.len(), 1);
        // ...the poll sees the same session: quiet.
        assert!(engine.apply(&[view("a", "pending", "db")], 1005).is_empty());
        // SSE ends it...
        let deltas = engine.apply_event(&event(2, "session_ended", "a", "completed"), 1010);
        assert_eq!(deltas.len(), 1);
        // ...the catch-up poll after an SSE drop sees it too: quiet.
        assert!(engine
            .apply(&[view("a", "completed", "db")], 1015)
            .is_empty());
    }

    #[test]
    fn replay_after_disconnect_does_not_duplicate() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "active", "db")]);
        // The stream dropped; the catch-up poll saw the end.
        engine.apply(&[view("a", "completed", "db")], 1000);
        // Resume replays the retained end event: quiet.
        let deltas = engine.apply_event(&event(9, "session_ended", "a", "completed"), 1001);
        assert!(deltas.is_empty());
    }

    #[test]
    fn idle_warning_fires_once_and_only_in_horizon() {
        let mut engine = DiffEngine::new();
        let mut s = view("a", "active", "db");
        s.last_activity = Some(900);
        s.idle_limit_secs = Some(300);
        engine.seed(&[s.clone()]);
        // 100s idle < 240s threshold: quiet.
        assert!(engine.apply(&[s.clone()], 1000).is_empty());
        // 250s idle: inside the 60s horizon.
        let deltas = engine.apply(&[s.clone()], 1150);
        assert_eq!(
            deltas,
            vec![SessionDelta::IdleWarning {
                session_id: "a".to_string(),
                name: "db".to_string(),
            }]
        );
        // Still inside: quiet (warned once).
        assert!(engine.apply(&[s.clone()], 1160).is_empty());
        // Past the limit the session is reaped anyway; quiet.
        assert!(engine.apply(&[s.clone()], 1200).is_empty());
    }

    #[test]
    fn idle_warning_clears_after_terminal() {
        let mut engine = DiffEngine::new();
        let mut s = view("a", "active", "db");
        s.last_activity = Some(900);
        s.idle_limit_secs = Some(300);
        engine.apply(&[s.clone()], 1150);
        engine.apply(&[view("a", "completed", "db")], 1160);
        // A fresh live view after a (hypothetical) restart of the same
        // id re-warns.
        let mut s2 = view("a", "active", "db");
        s2.last_activity = Some(1150);
        s2.idle_limit_secs = Some(300);
        assert_eq!(engine.apply(&[s2], 1400).len(), 1);
    }

    #[test]
    fn idle_warning_skips_unknown_limits() {
        let mut engine = DiffEngine::new();
        let mut s = view("a", "active", "db");
        s.last_activity = Some(900);
        engine.seed(&[s.clone()]);
        // No idle limit known: no warning (derivable-only).
        assert!(engine.apply(&[s], 1150).is_empty());
    }

    #[test]
    fn merge_detail_enriches_and_limit_survives_polls() {
        let mut engine = DiffEngine::new();
        engine.seed(&[view("a", "active", "db")]);
        let mut detail = view("a", "active", "prod-db");
        detail.idle_limit_secs = Some(300);
        detail.thumbnail_url = Some("/api/sessions/a/thumbnail".to_string());
        engine.merge_detail(detail);
        assert_eq!(engine.view("a").unwrap().name, "prod-db");
        // A fresh list snapshot (no idle limit) keeps the cached limit.
        let mut list = view("a", "active", "prod-db");
        list.idle_limit_secs = None;
        engine.apply(&[list], 1000);
        assert_eq!(engine.view("a").unwrap().idle_limit_secs, Some(300));
    }

    #[test]
    fn sse_started_with_terminal_status_is_a_replay_artifact() {
        let mut engine = DiffEngine::new();
        let deltas = engine.apply_event(&event(1, "session_started", "a", "completed"), 1000);
        assert!(deltas.is_empty());
        assert!(engine.view("a").is_some());
    }

    #[test]
    fn sse_status_changed_for_unknown_session_is_quiet() {
        let mut engine = DiffEngine::new();
        let deltas = engine.apply_event(&event(1, "status_changed", "a", "active"), 1000);
        assert!(deltas.is_empty());
        let deltas = engine.apply_event(&event(2, "status_changed", "a", "error"), 1010);
        assert_eq!(
            deltas,
            vec![SessionDelta::Error {
                session_id: "a".to_string(),
                name: "rdp".to_string(),
            }]
        );
    }

    #[test]
    fn parser_handles_full_frames() {
        let mut parser = SseParser::new();
        let frames =
            parser.feed(b"id: 7\nevent: session_started\ndata: {\"ok\":true}\n\n: ping\n\nid: 8\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id.as_deref(), Some("7"));
        assert_eq!(frames[0].event.as_deref(), Some("session_started"));
        assert_eq!(frames[0].data, "{\"ok\":true}");
        // The trailing id line waits for its frame terminator: the next
        // blank line completes it.
        let frames = parser.feed(b"data: tail\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id.as_deref(), Some("8"));
        assert_eq!(frames[0].event, None);
        assert_eq!(frames[0].data, "tail");
    }

    #[test]
    fn parser_handles_chunk_splits_and_crlf() {
        let mut parser = SseParser::new();
        assert!(parser.feed(b"id: 1\r\ndata: a\r").is_empty());
        // Line terminator for the pending "data: a", a blank line ends
        // frame 1, then "data: b" gets its own frame.
        let frames = parser.feed(b"\n\ndata: b\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id.as_deref(), Some("1"));
        assert_eq!(frames[0].data, "a");
        assert_eq!(frames[1].id, None);
        assert_eq!(frames[1].data, "b");
        assert!(parser.feed(b"").is_empty());
    }

    #[test]
    fn parser_ignores_unknown_fields() {
        let mut parser = SseParser::new();
        let frames = parser.feed(b"retry: 1000\ndata: x\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "x");
        assert_eq!(frames[0].id, None);
        assert_eq!(frames[0].event, None);
    }

    #[test]
    fn parser_skips_comment_keepalives() {
        let mut parser = SseParser::new();
        // Comment-only frame: no output.
        assert!(parser.feed(b": ping\n\n").is_empty());
        let frames = parser.feed(b"data: a\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, None);
    }

    #[test]
    fn rfc3339_parsing() {
        assert_eq!(
            parse_rfc3339_epoch("2026-08-13T00:00:00Z"),
            Some(1786579200)
        );
        assert_eq!(
            parse_rfc3339_epoch("2026-08-13T00:00:00.123456Z"),
            Some(1786579200)
        );
        assert_eq!(
            parse_rfc3339_epoch("2026-08-13T01:02:03+00:00"),
            Some(1786582923)
        );
        assert_eq!(parse_rfc3339_epoch("2026-08-13T00:00:00+02:00"), None);
        assert_eq!(parse_rfc3339_epoch("not-a-date"), None);
        assert_eq!(parse_rfc3339_epoch("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn display_names_prefer_entry_then_user_at_host() {
        let entry = json!({
            "session_id": "a",
            "session_type": "rdp",
            "status": "active",
            "hostname": "10.0.0.5",
            "username": "admin",
            "entry_display_name": "Prod SQL",
            "created_by": "alice"
        });
        assert_eq!(display_name(&entry), "Prod SQL");
        let user_host = json!({
            "hostname": "10.0.0.5",
            "username": "admin",
        });
        assert_eq!(display_name(&user_host), "admin@10.0.0.5");
        let host_only = json!({ "hostname": "10.0.0.5" });
        assert_eq!(display_name(&host_only), "10.0.0.5");
        assert_eq!(display_name(&json!({})), "session");
    }

    #[test]
    fn parse_session_view_maps_wire_fields() {
        let raw = json!({
            "session_id": "a1b2",
            "session_type": "vnc",
            "status": "active",
            "created_by": "alice",
            "last_activity": "2026-08-13T00:00:00Z",
            "thumbnail_url": "/api/sessions/a1b2/thumbnail",
            "client_url": "/client/a1b2",
            "hostname": "host-a",
            "username": "bob",
            "session_idle_timeout_secs": 0
        });
        let view = parse_session_view(&raw).expect("parses");
        assert_eq!(view.session_id, "a1b2");
        assert_eq!(view.status, "active");
        assert_eq!(view.name, "bob@host-a");
        assert_eq!(view.last_activity, Some(1786579200));
        assert_eq!(view.client_url, "/client/a1b2");
        assert_eq!(view.idle_limit_secs, None, "0 means disabled");
        let detail = json!({ "session_id": "a1b2", "session_idle_timeout_secs": 1800 });
        assert_eq!(
            parse_session_view(&detail).unwrap().idle_limit_secs,
            Some(1800)
        );
    }

    #[test]
    fn tray_sessions_build_absolute_urls() {
        let views = vec![view("b", "active", "z"), view("a", "completed", "a")];
        let sessions = tray_sessions("https://persea.example.com/", views);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "a");
        assert_eq!(sessions[0].url, "https://persea.example.com/client/a");
        assert_eq!(sessions[1].url, "https://persea.example.com/client/b");
    }

    #[test]
    fn terminal_status_classification() {
        assert!(is_terminal("completed"));
        assert!(is_terminal("error"));
        assert!(is_terminal("expired"));
        assert!(is_terminal("logged_out"));
        assert!(!is_terminal("active"));
        assert!(!is_terminal("pending"));
        assert!(!is_terminal("disconnected"));
    }
}
