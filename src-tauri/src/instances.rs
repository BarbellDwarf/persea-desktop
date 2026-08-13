//! Instance store: the shell-side list of persea servers (name + URL),
//! the cached capability probe per instance, and the startup behavior.
//!
//! Instances are a shell concept only (research 04-shell-integration.md
//! §5): there is no server-side instance registry. The store lives at
//! `app_config_dir()/instances.json` and is the single source of truth
//! for D07 (pairing), D08 (tray), D11 (transfers), D12 (kiosk) and D19
//! (provisioning merge, which takes over this module after D02).
//!
//! Server-gating rule (locked design): every consumer gates on what the
//! cached probe reports via [`capability`]. No probe, no capability: the
//! shell surfaces only what the server advertised.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Timeout for a single probe HTTP request.
pub const PROBE_TIMEOUT_SECS: u64 = 6;
/// Minimum server version for the drive API (T23 landed in v1.1.1).
pub const MIN_SERVER_DRIVE: &str = "1.1.1";
/// Minimum server version for events/pairing/version/capabilities (G1-G4).
pub const MIN_SERVER_FULL: &str = "1.2.0";

static STORE: Mutex<Option<Arc<Mutex<InstanceStore>>>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One configured instance. Persisted in `instances.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Instance {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub default: bool,
    /// Per-instance kiosk preference; D12 consumes it. Optional so older
    /// configs and provisioned entries stay valid.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "kioskAllowed"
    )]
    pub kiosk_allowed: Option<bool>,
    /// D19 provisioning: locked entries cannot be edited or removed by
    /// the user. The settings UI must hide edit/remove for them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Last probe result, cached so an unreachable instance still shows
    /// its last known version and capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<CachedProbe>,
}

/// Cached result of `GET /api/auth/status` (S05), persisted per instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedProbe {
    /// False when the last check could not reach the server. The version
    /// and capabilities below then hold the last known values.
    pub ok: bool,
    pub version: String,
    #[serde(default)]
    pub capabilities: HashMap<String, bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "latestVersion"
    )]
    pub latest_version: Option<String>,
    #[serde(default, rename = "updateAvailable")]
    pub update_available: bool,
    /// True when `GET /` redirects to `/setup` (server first run).
    #[serde(default, rename = "needsSetup")]
    pub needs_setup: bool,
    #[serde(default, rename = "checkedAt")]
    pub checked_at: u64,
}

/// On-disk shape of `instances.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StoreFile {
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastUsed")]
    pub last_used: Option<String>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    NotFound(String),
    Duplicate(String),
    Invalid(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Json(e) => write!(f, "invalid JSON: {e}"),
            StoreError::NotFound(u) => write!(f, "no instance with URL {u}"),
            StoreError::Duplicate(u) => write!(f, "an instance with URL {u} already exists"),
            StoreError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The loaded store: the file path plus its contents. Mutable access is
/// serialized through a `Mutex` in the process global.
#[derive(Debug, Clone)]
pub struct InstanceStore {
    path: PathBuf,
    pub file: StoreFile,
}

impl InstanceStore {
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::empty(path.to_path_buf()))
            }
            Err(e) => {
                // Unreadable config must not block startup: start empty.
                eprintln!(
                    "persea-desktop: instances.json unreadable ({e}); \
                     starting with an empty instance list"
                );
                Ok(Self::empty(path.to_path_buf()))
            }
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(file) => Ok(Self {
                    path: path.to_path_buf(),
                    file,
                }),
                Err(e) => {
                    // Never crash on a corrupt config: back it up and
                    // start empty (startup resilience, D02 review gap).
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(path, &backup);
                    eprintln!(
                        "persea-desktop: instances.json unreadable ({e}); \
                         backed up to {}; starting with an empty instance list",
                        backup.display()
                    );
                    Ok(Self::empty(path.to_path_buf()))
                }
            },
        }
    }

    fn empty(path: PathBuf) -> Self {
        Self {
            path,
            file: StoreFile::default(),
        }
    }

    /// Atomic save: write a temp file, rename over the target.
    pub fn save(&self) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(StoreError::Io)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(&self.file).map_err(StoreError::Json)?;
        std::fs::write(&tmp, data).map_err(StoreError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(StoreError::Io)?;
        Ok(())
    }

    pub fn find(&self, url: &str) -> Option<&Instance> {
        self.file.instances.iter().find(|i| i.url == url)
    }

    pub fn index_of(&self, url: &str) -> Option<usize> {
        self.file.instances.iter().position(|i| i.url == url)
    }

    /// Startup target (locked design): the default instance, else the
    /// last-used instance, else the first configured one.
    pub fn default_or_last(&self) -> Option<&Instance> {
        self.file
            .instances
            .iter()
            .find(|i| i.default)
            .or_else(|| {
                self.file
                    .last_used
                    .as_deref()
                    .and_then(|u| self.file.instances.iter().find(|i| i.url == u))
            })
            .or_else(|| self.file.instances.first())
    }

    pub fn capability(&self, url: &str, key: &str) -> bool {
        self.find(url)
            .and_then(|i| i.probe.as_ref())
            .and_then(|p| p.capabilities.get(key))
            .copied()
            .unwrap_or(false)
    }

    pub fn record_used(&mut self, url: &str) {
        self.file.last_used = Some(url.to_string());
    }

    /// Remove an instance, clearing last-used when it pointed at it.
    /// Locked entries (D19 provisioning) are refused.
    pub fn remove(&mut self, url: &str) -> Result<(), StoreError> {
        let idx = self
            .index_of(url)
            .ok_or_else(|| StoreError::NotFound(url.to_string()))?;
        if self.file.instances[idx].locked {
            return Err(StoreError::Invalid(
                "This instance is locked by your administrator".to_string(),
            ));
        }
        self.file.instances.remove(idx);
        if self.file.last_used.as_deref() == Some(url) {
            self.file.last_used = None;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Process-global access
// ---------------------------------------------------------------------------

/// Initialize the process global from an app instance. Called by the
/// dispatcher from `lib.rs run()` via the setup hook.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&config_dir)?;
    let store = Arc::new(Mutex::new(InstanceStore::load(
        &config_dir.join("instances.json"),
    )?));
    *STORE.lock().unwrap() = Some(store.clone());
    refresh_probes_in_background(store.clone());
    auto_open(app, &store);
    Ok(())
}

fn store_handle() -> Option<Arc<Mutex<InstanceStore>>> {
    STORE.lock().ok()?.clone()
}

fn unavailable_store() -> String {
    "instance store is not initialized".to_string()
}

fn no_instances() -> String {
    "No instances configured".to_string()
}

fn main_window_missing() -> String {
    "main window not found".to_string()
}

fn with_store<R>(f: impl FnOnce(&mut InstanceStore) -> R) -> Option<R> {
    let handle = store_handle()?;
    let mut store = handle.lock().ok()?;
    Some(f(&mut store))
}

/// Snapshot of every configured instance (D08 tray, settings).
pub fn instances() -> Vec<Instance> {
    with_store(|s| s.file.instances.clone()).unwrap_or_default()
}

pub fn instance(instance_url: &str) -> Option<Instance> {
    with_store(|s| {
        s.file
            .instances
            .iter()
            .find(|i| i.url == instance_url)
            .cloned()
    })
    .flatten()
}

pub fn probe(instance_url: &str) -> Option<CachedProbe> {
    with_store(|s| s.find(instance_url).and_then(|i| i.probe.clone())).flatten()
}

pub fn last_known_version(instance_url: &str) -> Option<String> {
    probe(instance_url)
        .filter(|p| p.ok || !p.version.is_empty())
        .map(|p| p.version)
}

/// Server-gating accessor (locked design): the shell surfaces only what
/// the cached probe reports. Fails closed when the store is uninitialized,
/// the instance is unknown, or no probe has ever succeeded.
///
/// Keys consumed by other tickets: `kiosk_allowed` (D12), `desktop_transfers`
/// (D11), `desktop_pairing` (D07), plus `drive_upload`, `session_events`,
/// `drive_api`, `desktop_bridge` for settings display.
pub fn capability(instance_url: &str, key: &str) -> bool {
    with_store(|s| s.capability(instance_url, key)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Startup behavior (locked design)
// ---------------------------------------------------------------------------

/// Auto-open the default instance, or the last-used instance when no
/// default is set. Called once from setup; index.html also re-runs the
/// same decision when the user lands on it with instances configured.
fn auto_open(app: &tauri::App, store: &Mutex<InstanceStore>) {
    let target = {
        match store.lock() {
            Ok(s) => s.default_or_last().map(|i| i.url.clone()),
            Err(_) => None,
        }
    };
    let Some(url) = target else { return };
    let Some(win) = app.get_webview_window(window_label(&url)) else {
        return;
    };
    let Ok(parsed) = url::Url::parse(&url) else {
        return;
    };
    let _ = win.navigate(tauri::WebviewUrl::External(parsed));
}

/// Background probe of every configured instance at startup: caches
/// reachable/unreachable state so the tray and settings never block and
/// never spam errors when an instance is down. Failures are stored in the
/// probe cache (ok = false, last-known values kept), not logged.
fn refresh_probes_in_background(store: Arc<Mutex<InstanceStore>>) {
    tauri::async_runtime::spawn(async move {
        let urls: Vec<String> = {
            let Ok(s) = store.lock() else { return };
            s.file.instances.iter().map(|i| i.url.clone()).collect()
        };
        for url in urls {
            let outcome = probe_server(&url).await;
            let mut changed = false;
            if let Ok(mut s) = store.lock() {
                if let Some(inst) = s.file.instances.iter_mut().find(|i| i.url == url) {
                    let prior = inst.probe.clone();
                    inst.probe = Some(apply_probe(prior, outcome));
                    changed = true;
                }
            }
            if changed {
                if let Ok(s) = store.lock() {
                    let _ = s.save();
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Probe (S05: GET /api/auth/status)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProbeView {
    pub ok: bool,
    pub version: String,
    pub capabilities: HashMap<String, bool>,
    #[serde(rename = "latestVersion")]
    pub latest_version: Option<String>,
    #[serde(rename = "updateAvailable")]
    pub update_available: bool,
    #[serde(rename = "needsSetup")]
    pub needs_setup: bool,
    #[serde(rename = "checkedAt")]
    pub checked_at: u64,
    /// Human-readable warnings derived from the probe: old server,
    /// plain-http transport.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceView {
    pub name: String,
    pub url: String,
    pub default: bool,
    pub locked: bool,
    pub probe: Option<ProbeView>,
}

fn instance_view(i: &Instance) -> InstanceView {
    InstanceView {
        name: i.name.clone(),
        url: i.url.clone(),
        default: i.default,
        locked: i.locked,
        probe: i.probe.as_ref().map(|p| ProbeView {
            ok: p.ok,
            version: p.version.clone(),
            capabilities: p.capabilities.clone(),
            latest_version: p.latest_version.clone(),
            update_available: p.update_available,
            needs_setup: p.needs_setup,
            checked_at: p.checked_at,
            warnings: probe_warnings(&i.url, p),
        }),
    }
}

/// Loopback hosts may use plain http (local dev servers).
fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.")
}

fn probe_warnings(url: &str, p: &CachedProbe) -> Vec<String> {
    let mut out = server_version_warnings(&p.version);
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("");
        if is_loopback_host(host) {
            out.push(
                "Plain http transport: clipboard and other secure-context \
                 features degrade in the webview; use https in production."
                    .to_string(),
            );
        }
    }
    out
}

/// One full probe pass: status endpoint + first-run setup detection.
/// Never panics and never blocks the caller for longer than the timeout.
pub async fn probe_server(base_url: &str) -> CachedProbe {
    let base = base_url.trim_end_matches('/').to_string();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
        .user_agent(concat!("persea-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(_) => return unreachable_probe(),
    };
    match fetch_status(&client, &base).await {
        Ok((version, capabilities, latest_version, update_available)) => {
            let needs_setup = fetch_needs_setup(&client, &base).await;
            CachedProbe {
                ok: true,
                version,
                capabilities,
                latest_version,
                update_available,
                needs_setup,
                checked_at: now_secs(),
            }
        }
        Err(_) => unreachable_probe(),
    }
}

fn unreachable_probe() -> CachedProbe {
    CachedProbe {
        ok: false,
        version: "unknown".to_string(),
        capabilities: HashMap::new(),
        latest_version: None,
        update_available: false,
        needs_setup: false,
        checked_at: now_secs(),
    }
}

/// Merge a fresh probe outcome over the cached one: a failed re-probe
/// keeps the last known version and capabilities (probe failure state,
/// review gap), only the ok flag and timestamp change.
fn apply_probe(prior: Option<CachedProbe>, outcome: CachedProbe) -> CachedProbe {
    if outcome.ok {
        return outcome;
    }
    match prior {
        Some(mut p) => {
            p.ok = false;
            p.checked_at = now_secs();
            p
        }
        None => outcome,
    }
}

async fn fetch_status(
    client: &reqwest::Client,
    base: &str,
) -> Result<(String, HashMap<String, bool>, Option<String>, bool), String> {
    let resp = client
        .get(format!("{base}/api/auth/status"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status endpoint returned {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let version = json
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut capabilities = HashMap::new();
    if let Some(caps) = json.get("capabilities").and_then(|v| v.as_object()) {
        for (k, v) in caps {
            capabilities.insert(k.clone(), v.as_bool().unwrap_or(false));
        }
    }
    let latest_version = json
        .get("latest_version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let update_available = json
        .get("update_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok((version, capabilities, latest_version, update_available))
}

/// Server-needs-setup hint: a fresh server redirects `GET /` to `/setup`
/// (src/handlers/auth.rs:145-147). Detected from the final redirect chain.
async fn fetch_needs_setup(client: &reqwest::Client, base: &str) -> bool {
    let Ok(resp) = client
        .get(base)
        .redirect(reqwest::redirect::Policy::limited(5))
        .send()
        .await
    else {
        return false;
    };
    resp.url().path().starts_with("/setup")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Version helpers
// ---------------------------------------------------------------------------

pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
    Some((major, minor, patch))
}

pub fn version_lt(a: &str, b: &str) -> Option<bool> {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => Some(x < y),
        _ => None,
    }
}

/// Warnings for servers below the v1.1.1 (drive) / v1.2.0 (full) floors.
/// Unparseable versions produce no warnings (fail open on display, the
/// capability gate still fails closed on missing probe data).
pub fn server_version_warnings(version: &str) -> Vec<String> {
    if parse_version(version).is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if version_lt(version, MIN_SERVER_DRIVE) == Some(true) {
        out.push(format!(
            "Server {MIN_SERVER_DRIVE}+ required for the drive API (this server: {version})"
        ));
    }
    if version_lt(version, MIN_SERVER_FULL) == Some(true) {
        out.push(format!(
            "Server {MIN_SERVER_FULL}+ required for session events and device pairing \
             (this server: {version})"
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// URL validation
// ---------------------------------------------------------------------------

/// Normalize + validate an instance URL. Rejects non-https non-loopback
/// http; allows http for localhost only (dev servers), with a warning
/// surfaced through the probe view.
pub fn validate_instance_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("URL is required".to_string());
    }
    let parsed = url::Url::parse(trimmed).map_err(|e| format!("Invalid URL: {e}"))?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            let host = parsed.host_str().unwrap_or("");
            if !is_loopback_host(host) {
                return Err(
                    "Only https URLs are allowed (http is accepted for localhost only). \
                     Use https in production."
                        .to_string(),
                );
            }
        }
        _ => return Err("URL must start with https:// (or http:// for localhost)".to_string()),
    }
    if parsed.host_str().is_none() {
        return Err("URL must include a host".to_string());
    }
    Ok(trimmed.to_string())
}

// ---------------------------------------------------------------------------
// Per-instance data stores
// ---------------------------------------------------------------------------

/// Stable per-instance data-store key, deterministic per URL so the same
/// instance always lands in the same store across restarts.
///
/// D05 consumes this when building instance webviews:
/// ```text
/// WebviewBuilder::default()
///     .data_store_identifier(instances::store_key(&url))
///     .data_directory(config_dir.join("stores").join(instances::store_key(&url)))
/// ```
/// On macOS < 14 `dataStoreIdentifier` is unsupported; D05 falls back to
/// the shared store there (documented note, D02 decision). On Linux and
/// Windows the key is used as the per-instance data directory name so
/// cookies never collide (research 02 §6).
pub fn store_key(instance_url: &str) -> String {
    // Normalize trailing slashes so the same server always lands in the
    // same store regardless of how the URL was typed.
    let normalized = instance_url.trim_end_matches('/');
    let h1 = fnv1a_64(normalized.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let h2 = fnv1a_64(normalized.as_bytes(), 0x8422_2325_cbf2_9ce4);
    format!("persea-{h1:016x}{h2:016x}")
}

fn fnv1a_64(data: &[u8], seed: u64) -> u64 {
    let mut h = seed;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Window label for an instance's main window. The single-window
/// navigation design keeps the label "main" for every instance (locked
/// design, subagent contract); D05 switches to per-instance labels when
/// the multi-window manager lands, and the navigation allowlist (D03)
/// and tray (D08) match on this label.
pub fn window_label(_instance_url: &str) -> &'static str {
    "main"
}

// ---------------------------------------------------------------------------
// Commands (registered by the dispatcher in lib.rs)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn cmd_instances_list() -> Result<Vec<InstanceView>, String> {
    with_store(|s| s.file.instances.iter().map(instance_view).collect())
        .ok_or_else(unavailable_store)
}

#[tauri::command]
pub async fn cmd_instances_add(name: String, url: String) -> Result<InstanceView, String> {
    let url = validate_instance_url(&url)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Name is required".to_string());
    }
    let outcome = probe_server(&url).await;
    let handle = store_handle().ok_or_else(unavailable_store)?;
    let mut store = handle
        .lock()
        .map_err(|_| "instance store is locked".to_string())?;
    if store.index_of(&url).is_some() {
        return Err(format!("An instance with URL {url} already exists"));
    }
    store.file.instances.push(Instance {
        name,
        url: url.clone(),
        default: store.file.instances.is_empty(),
        kiosk_allowed: None,
        locked: false,
        probe: Some(outcome),
    });
    store.save().map_err(|e| e.to_string())?;
    Ok(instance_view(store.find(&url).unwrap()))
}

#[tauri::command]
pub async fn cmd_instances_update(
    url: String,
    name: Option<String>,
    new_url: Option<String>,
) -> Result<InstanceView, String> {
    let handle = store_handle().ok_or_else(unavailable_store)?;
    let mut store = handle
        .lock()
        .map_err(|_| "instance store is locked".to_string())?;
    let idx = store
        .index_of(&url)
        .ok_or_else(|| format!("No instance with URL {url}"))?;
    if let Some(n) = name {
        let n = n.trim().to_string();
        if n.is_empty() {
            return Err("Name is required".to_string());
        }
        store.file.instances[idx].name = n;
    }
    if let Some(nu) = new_url {
        let nu = validate_instance_url(&nu)?;
        if nu != url && store.index_of(&nu).is_some() {
            return Err(format!("An instance with URL {nu} already exists"));
        }
        store.file.instances[idx].url = nu.clone();
        store.file.instances[idx].probe = None;
        if store.file.last_used.as_deref() == Some(url.as_str()) {
            store.file.last_used = Some(nu.clone());
        }
        let outcome = probe_server(&nu).await;
        store.file.instances[idx].probe = Some(outcome);
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(instance_view(&store.file.instances[idx]))
}

#[tauri::command]
pub fn cmd_instances_remove(url: String) -> Result<(), String> {
    with_store(|s| {
        s.remove(&url).map_err(|e| e.to_string())?;
        s.save().map_err(|e| e.to_string())
    })
    .ok_or_else(unavailable_store)?
}

#[tauri::command]
pub fn cmd_instances_set_default(url: String) -> Result<(), String> {
    with_store(|s| {
        let idx = s
            .index_of(&url)
            .ok_or_else(|| format!("No instance with URL {url}"))?;
        for i in &mut s.file.instances {
            i.default = false;
        }
        s.file.instances[idx].default = true;
        s.file.last_used = Some(url.clone());
        s.save().map_err(|e| e.to_string())
    })
    .ok_or_else(unavailable_store)?
}

#[tauri::command]
pub async fn cmd_instances_probe(url: String) -> Result<ProbeView, String> {
    let url = validate_instance_url(&url)?;
    let outcome = probe_server(&url).await;
    let handle = store_handle().ok_or_else(unavailable_store)?;
    let mut store = handle
        .lock()
        .map_err(|_| "instance store is locked".to_string())?;
    let idx = store
        .index_of(&url)
        .ok_or_else(|| format!("No instance with URL {url}"))?;
    let prior = store.file.instances[idx].probe.clone();
    store.file.instances[idx].probe = Some(apply_probe(prior, outcome));
    store.save().map_err(|e| e.to_string())?;
    let inst = &store.file.instances[idx];
    let view = instance_view(inst);
    Ok(view.probe.unwrap())
}

/// "Open": mark last-used and navigate the main window to the instance.
#[tauri::command]
pub fn cmd_instances_open(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let url = validate_instance_url(&url)?;
    with_store(|s| {
        if s.index_of(&url).is_none() {
            return Err(format!("No instance with URL {url}"));
        }
        s.record_used(&url);
        s.save().map_err(|e| e.to_string())
    })
    .ok_or_else(unavailable_store)??;
    navigate_main(&app, &url)
}

/// Open the default instance, or the last-used one. index.html calls this
/// so a manual visit to the shell page bounces back to the active server.
#[tauri::command]
pub fn cmd_instances_open_default(app: tauri::AppHandle) -> Result<(), String> {
    let target =
        with_store(|s| s.default_or_last().map(|i| i.url.clone())).ok_or_else(unavailable_store)?;
    let url = target.ok_or_else(no_instances)?;
    cmd_instances_open(app, url)
}

/// Open the server's setup wizard in the webview (server-needs-setup
/// hint, locked design).
#[tauri::command]
pub fn cmd_instances_open_setup(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let url = validate_instance_url(&url)?;
    let setup_url = format!("{url}/setup");
    let parsed = url::Url::parse(&setup_url).map_err(|e| e.to_string())?;
    let win = app
        .get_webview_window(window_label(&url))
        .ok_or_else(main_window_missing)?;
    win.navigate(tauri::WebviewUrl::External(parsed))
        .map_err(|e| e.to_string())
}

fn navigate_main(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    let win = app
        .get_webview_window(window_label(url))
        .ok_or_else(main_window_missing)?;
    win.navigate(tauri::WebviewUrl::External(parsed))
        .map_err(|e| e.to_string())
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "persea-desktop-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join("instances.json")
    }

    fn sample_store() -> InstanceStore {
        let mut store = InstanceStore::empty(tmp_store_path("roundtrip"));
        store.file.instances.push(Instance {
            name: "Prod".into(),
            url: "https://persea.example.com".into(),
            default: true,
            kiosk_allowed: None,
            locked: false,
            probe: Some(CachedProbe {
                ok: true,
                version: "1.2.0".into(),
                capabilities: HashMap::from([
                    ("kiosk_allowed".into(), true),
                    ("desktop_pairing".into(), true),
                ]),
                latest_version: Some("1.3.0".into()),
                update_available: true,
                needs_setup: false,
                checked_at: 42,
            }),
        });
        store.file.instances.push(Instance {
            name: "Lab".into(),
            url: "https://lab.example.com".into(),
            default: false,
            kiosk_allowed: None,
            locked: false,
            probe: None,
        });
        store.file.last_used = Some("https://lab.example.com".into());
        store
    }

    #[test]
    fn config_round_trip_preserves_instances_and_probes() {
        let path = tmp_store_path("roundtrip");
        let mut store = sample_store();
        store.path = path.clone();
        store.save().unwrap();

        let loaded = InstanceStore::load(&path).unwrap();
        assert_eq!(loaded.file, store.file);
        assert_eq!(loaded.file.instances.len(), 2);
        assert_eq!(
            loaded.find("https://persea.example.com").unwrap().name,
            "Prod"
        );
        let probe = loaded
            .find("https://persea.example.com")
            .unwrap()
            .probe
            .as_ref()
            .unwrap();
        assert!(probe.ok);
        assert_eq!(probe.version, "1.2.0");
        assert!(probe.capabilities["kiosk_allowed"]);
        assert_eq!(probe.latest_version.as_deref(), Some("1.3.0"));
        assert!(probe.update_available);
        assert_eq!(
            loaded.file.last_used.as_deref(),
            Some("https://lab.example.com")
        );
    }

    #[test]
    fn missing_file_loads_empty_and_save_creates_it() {
        let path = tmp_store_path("missing");
        let store = InstanceStore::load(&path).unwrap();
        assert!(store.file.instances.is_empty());
        store.save().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn corrupt_file_is_backed_up_and_startup_stays_empty() {
        let path = tmp_store_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let store = InstanceStore::load(&path).unwrap();
        assert!(store.file.instances.is_empty());
        assert!(path.with_extension("json.corrupt").exists());
        assert!(!path.exists());
    }

    #[test]
    fn default_or_last_resolves_default_then_last_used_then_first() {
        let mut store = sample_store();
        assert_eq!(
            store.default_or_last().unwrap().url,
            "https://persea.example.com"
        );
        store.file.instances[0].default = false;
        assert_eq!(
            store.default_or_last().unwrap().url,
            "https://lab.example.com"
        );
        store.file.last_used = None;
        assert_eq!(
            store.default_or_last().unwrap().url,
            "https://persea.example.com"
        );
        store.file.instances.clear();
        assert!(store.default_or_last().is_none());
    }

    #[test]
    fn store_key_mapping_is_stable_and_unique() {
        let a = "https://persea.example.com";
        let b = "https://lab.example.com";
        assert_eq!(store_key(a), store_key(a));
        assert_eq!(store_key(b), store_key(b));
        assert_ne!(store_key(a), store_key(b));
        assert!(store_key(a).starts_with("persea-"));
        assert_eq!(store_key(a).len(), 7 + 32);
        assert_eq!(store_key("https://persea.example.com/"), store_key(a));
    }

    #[test]
    fn window_label_is_main_for_single_window_design() {
        assert_eq!(window_label("https://anything.example.com"), "main");
    }

    #[test]
    fn version_warnings_flag_old_servers() {
        assert!(server_version_warnings("1.0.9").len() == 2);
        assert!(server_version_warnings("1.1.1").len() == 1);
        assert!(server_version_warnings("1.2.0").is_empty());
        assert!(server_version_warnings("1.2.3").is_empty());
        assert!(server_version_warnings("v1.2.0").is_empty());
        assert!(server_version_warnings("not-a-version").is_empty());
        assert!(server_version_warnings("1.2.0-beta.1").is_empty());
        assert_eq!(parse_version("v1.2.0"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.0-alpha+xyz"), Some((1, 2, 0)));
    }

    #[test]
    fn url_validation_requires_https_except_loopback() {
        assert!(validate_instance_url("https://persea.example.com").is_ok());
        assert!(validate_instance_url("https://persea.example.com/").is_ok());
        assert_eq!(
            validate_instance_url("https://persea.example.com/").unwrap(),
            "https://persea.example.com"
        );
        assert!(validate_instance_url("http://persea.example.com").is_err());
        assert!(validate_instance_url("http://localhost:8089").is_ok());
        assert!(validate_instance_url("http://127.0.0.1:8089").is_ok());
        assert!(validate_instance_url("http://[::1]:8089").is_ok());
        assert!(validate_instance_url("ftp://persea.example.com").is_err());
        assert!(validate_instance_url("https://").is_err());
        assert!(validate_instance_url("").is_err());
    }

    #[test]
    fn failed_reprobe_keeps_last_known_version() {
        let probe = CachedProbe {
            ok: true,
            version: "1.2.0".into(),
            capabilities: HashMap::from([("kiosk_allowed".into(), true)]),
            latest_version: None,
            update_available: false,
            needs_setup: false,
            checked_at: 1,
        };
        let merged = apply_probe(Some(probe.clone()), unreachable_probe());
        assert!(!merged.ok);
        assert_eq!(merged.version, "1.2.0");
        assert!(merged.capabilities["kiosk_allowed"]);
        assert!(merged.checked_at >= 1);
        let fresh = apply_probe(Some(probe.clone()), probe.clone());
        assert!(fresh.ok);
        // Never-probed instance: unreachable outcome stands as-is.
        let none = apply_probe(None, unreachable_probe());
        assert!(!none.ok);
        assert_eq!(none.version, "unknown");
    }

    #[test]
    fn removal_clears_last_used_and_respects_locks() {
        let mut store = sample_store();
        store.remove("https://lab.example.com").unwrap();
        assert_eq!(store.file.instances.len(), 1);
        assert!(store.file.last_used.is_none());
        store.file.instances[0].locked = true;
        assert!(store.remove("https://persea.example.com").is_err());
        assert_eq!(store.file.instances.len(), 1);
        assert!(store.remove("https://nope.example.com").is_err());
    }
}
