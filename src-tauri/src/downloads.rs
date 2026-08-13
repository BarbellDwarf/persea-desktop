#![allow(dead_code)]
//! Download interception: every engine download lands in a shell-chosen
//! folder through one tauri-level handler.
//!
//! # Verified engine facts (tauri 2.11.5 / wry 0.55.1)
//!
//! `WebviewWindowBuilder::on_download` is the single wiring point. wry
//! maps it onto each engine's own download event:
//!
//! - **WebView2**: `CoreWebView2.DownloadStarting` (via
//!   `add_DownloadStarting` on the controller). The handler reads the
//!   operation's URI and result file path, lets the shell replace the
//!   path (`SetResultFilePath`), and returning `false` cancels the
//!   download. Completion is observed through the download operation's
//!   `StateChanged` (COMPLETED / INTERRUPTED).
//! - **WebKitGTK**: `WebContext` download handling — the
//!   `decide-policy` signal with `WEBKIT_POLICY_DECISION_TYPE_DOWNLOAD`
//!   plus the `WebKitDownload` lifecycle. The path handed to the handler
//!   is the engine's proposed destination; the shell replaces it.
//! - **WKWebView**: `WKDownloadDelegate` (macOS 11.3+), implemented by
//!   wry's `WryDownloadDelegate`. The finished-path is empty on macOS
//!   (API limitation, documented by tauri), so success is tracked via
//!   the `success` flag, not the path.
//!
//! The raw `CoreWebView2` is additionally reachable on Windows via
//! `WebviewWindow::with_webview` (`PlatformWebview::Webview2(...)`), so
//! deeper engine-specific wiring is possible later without new
//! dependencies; the tauri-level handler below is sufficient for the
//! save-to-folder flow.
//!
//! # Save dialogs (honest v1.2.0)
//!
//! Tauri 2.11 core has NO dialog API (verified: no dialog module in the
//! crate; the only dialog method on `WebviewWindow` is the print
//! dialog). A native "choose where to save" dialog requires the dialog
//! plugin (`tauri-plugin-dialog`, `blocking_save_file` on the main
//! thread). Until the dispatcher pre-wires it, downloads land in the
//! OS Downloads folder with collision-avoiding names; the interception
//! itself is fully wired, and the handler's destination assignment is
//! the seam where the dialog slots in.
//!
//! # File inputs
//!
//! None of the three engines intercept `<input type=file>`; the native
//! picker opens in every case. Nothing to wire (verified by reading the
//! wry sources: there is no file-input interception anywhere in the
//! webview layers). The acceptance check is manual per OS.
//!
//! # Wiring for the dispatcher
//!
//! 1. Apply `downloads::handler(app.handle().clone())` via
//!    `.on_download(...)` on the main window builder and every session
//!    window builder (session windows built by the window manager get
//!    it automatically).
//! 2. (Optional, native save dialogs) add `tauri-plugin-dialog` to
//!    `Cargo.toml`, register the plugin, grant `dialog:allow-save` in
//!    the shell capability, and replace the destination computation in
//!    [`DownloadsManager::destination`] with a
//!    `blocking_save_file` call. The handler runs on the webview's
//!    thread (not the main thread), so blocking it on a dialog is safe
//!    on every engine.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Manager, Runtime, Webview};

/// Upper bound on remembered download records.
const RECORD_CAP: usize = 128;

/// One completed (or cancelled) download, for a future downloads UI.
#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub url: String,
    pub path: Option<PathBuf>,
    pub success: bool,
    pub requested_at_secs: u64,
}

static MANAGER: OnceLock<Mutex<DownloadsManager>> = OnceLock::new();

/// Tracks the download folder and the recent record list.
pub struct DownloadsManager {
    dir: Option<PathBuf>,
    records: VecDeque<DownloadRecord>,
}

impl DownloadsManager {
    fn new() -> Self {
        Self {
            dir: None,
            records: VecDeque::new(),
        }
    }

    fn record(&mut self, record: DownloadRecord) {
        if self.records.len() >= RECORD_CAP {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }
}

fn manager() -> std::sync::MutexGuard<'static, DownloadsManager> {
    MANAGER
        .get_or_init(|| Mutex::new(DownloadsManager::new()))
        .lock()
        .unwrap()
}

/// The OS Downloads folder, or the home directory as a last resort.
pub fn default_download_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .download_dir()
        .ok()
        .or_else(|| app.path().home_dir().ok())
}

/// Override the download folder (the settings UI / save-dialog flow
/// lands here; `None` restores the OS default).
pub fn set_download_dir(dir: Option<PathBuf>) {
    manager().dir = dir;
}

/// Current download folder (the configured one, else the OS default).
pub fn download_dir(app: &AppHandle) -> Option<PathBuf> {
    manager().dir.clone().or_else(|| default_download_dir(app))
}

/// Recent download records (newest last).
pub fn records() -> Vec<DownloadRecord> {
    manager().records.iter().cloned().collect()
}

/// The `on_download` handler for every webview that hosts persea pages.
///
/// On `Requested`: replaces the engine's destination with a sanitized,
/// collision-free path under the download folder and allows the
/// download. Returning `false` cancels; the shell currently lets every
/// download through (the save dialog seam is the destination
/// assignment).
///
/// Name source: the URL's last path segment; when the URL yields
/// nothing (`blob:` and `data:` downloads — the web client's screenshots
/// and drive files use blob URLs with the anchor `download` attribute),
/// the engine's suggested file name is used instead. All three engines
/// pass that suggestion in the `destination` PathBuf (WebView2
/// `ResultFilePath`, WebKitGTK `decide-destination`, WKWebView
/// `WKDownloadDelegate`), and wry derives it from the download
/// attribute, so the anchor-provided name survives.
pub fn handler<R: Runtime>(
    app: AppHandle,
) -> impl Fn(Webview<R>, DownloadEvent<'_>) -> bool + Send + Sync + 'static {
    move |_webview, event| match event {
        DownloadEvent::Requested { url, destination } => {
            let dir = manager().dir.clone().or_else(|| default_download_dir(&app));
            let Some(dir) = dir else {
                return false;
            };
            let name = filename_from_url(url.as_str())
                .or_else(|| engine_suggested_name(destination))
                .unwrap_or_else(|| "download".to_string());
            *destination = dedup_path(&dir, &name);
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            manager().record(DownloadRecord {
                url: url.to_string(),
                path,
                success,
                requested_at_secs: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
            true
        }
    }
}

/// The engine's suggested file name for the download (from the anchor's
/// `download` attribute on `blob:`/`data:` downloads), sanitized.
fn engine_suggested_name(destination: &Path) -> Option<String> {
    let name = destination.file_name()?.to_str()?;
    sanitize_filename(name)
}

/// Derive a safe file name from a download URL: the last path segment,
/// percent-decoded, stripped of anything the filesystem dislikes.
pub fn filename_from_url(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let segment = url.path_segments()?.next_back()?.to_string();
    let decoded = percent_decode(&segment);
    sanitize_filename(&decoded)
}

/// Minimal percent-decoding (`%XX`) for file names from URLs.
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

/// Filesystem-safe file name: control chars, separators, leading dots
/// and Windows reserved names removed/replaced. Returns `None` when
/// nothing usable remains.
pub fn sanitize_filename(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        return None;
    }
    let upper = cleaned.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.contains(&stem) {
        return Some(format!("_{cleaned}"));
    }
    Some(cleaned)
}

/// Collision-free path: `name.ext` → `name (1).ext` → `name (2).ext`.
pub fn dedup_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(idx) if idx > 0 => (&name[..idx], &name[idx..]),
        _ => (name, ""),
    };
    for n in 1..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!(
        "{stem} ({}){ext}",
        std::time::SystemTime::now()
            .elapsed()
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_derived_and_percent_decoded() {
        assert_eq!(
            filename_from_url("https://persea.example.com/api/export/report%20q1.csv"),
            Some("report q1.csv".to_string())
        );
        assert_eq!(
            filename_from_url("https://persea.example.com/screenshot.png?token=abc"),
            Some("screenshot.png".to_string())
        );
        assert_eq!(filename_from_url("https://persea.example.com/"), None);
        assert_eq!(filename_from_url("not a url"), None);
    }

    #[test]
    fn blob_downloads_fall_back_to_the_engine_suggested_name() {
        // The web client's screenshots and drive files download blob
        // URLs with the anchor `download` attribute; the engine's
        // suggested name arrives inside the destination PathBuf.
        assert_eq!(
            filename_from_url("blob:https://persea.example.com/uuid"),
            None
        );
        assert_eq!(
            engine_suggested_name(Path::new(
                "/home/user/Downloads/persea-a1b2c3d4-1700000000000.png"
            )),
            Some("persea-a1b2c3d4-1700000000000.png".to_string())
        );
        assert_eq!(
            engine_suggested_name(Path::new("/home/user/Downloads/../evil/name.txt")),
            Some("evil_name.txt".to_string())
        );
        assert_eq!(engine_suggested_name(Path::new("/")), None);
    }

    #[test]
    fn sanitization_strips_hostiles() {
        assert_eq!(
            sanitize_filename("report.csv"),
            Some("report.csv".to_string())
        );
        assert_eq!(
            sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"),
            Some("a_b_c_d_e_f_g_h_i_j".to_string())
        );
        assert_eq!(sanitize_filename("..hidden"), Some("hidden".to_string()));
        assert_eq!(sanitize_filename("  "), None);
        assert_eq!(sanitize_filename("."), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("CON.txt"), Some("_CON.txt".to_string()));
        assert_eq!(sanitize_filename("LPT9"), Some("_LPT9".to_string()));
        assert_eq!(
            sanitize_filename("normal name.txt"),
            Some("normal name.txt".to_string())
        );
    }

    #[test]
    fn dedup_avoids_collisions_in_order() {
        let dir = std::env::temp_dir().join(format!(
            "persea-desktop-dl-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dedup_path(&dir, "x.bin");
        std::fs::write(&a, b"1").unwrap();
        let b = dedup_path(&dir, "x.bin");
        assert_eq!(b.file_name().unwrap().to_string_lossy(), "x (1).bin");
        std::fs::write(&b, b"2").unwrap();
        let c = dedup_path(&dir, "x.bin");
        assert_eq!(c.file_name().unwrap().to_string_lossy(), "x (2).bin");
        // No extension: suffix lands at the end.
        let d = dedup_path(&dir, "notes");
        assert_eq!(d.file_name().unwrap().to_string_lossy(), "notes");
        std::fs::write(&d, b"3").unwrap();
        let e = dedup_path(&dir, "notes");
        assert_eq!(e.file_name().unwrap().to_string_lossy(), "notes (1)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_are_capped_and_evicted() {
        let mut guard = manager();
        guard.records.clear();
        for i in 0..(RECORD_CAP + 10) {
            guard.record(DownloadRecord {
                url: format!("https://persea.example.com/file-{i}.bin"),
                path: None,
                success: true,
                requested_at_secs: i as u64,
            });
        }
        assert_eq!(guard.records.len(), RECORD_CAP);
        assert!(
            guard
                .records
                .iter()
                .all(|r| r.url.starts_with("https://persea.example.com/file-10.")),
            "the oldest records must be evicted"
        );
    }
}
