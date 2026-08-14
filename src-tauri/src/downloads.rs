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
//! # Save dialogs (shipped v1.2.0)
//!
//! Every download is offered a native "choose where to save" dialog
//! (tauri-plugin-dialog, registered in `lib.rs`, `dialog:allow-save`
//! granted in the shell capability). The engine still writes to the
//! download folder first, and the file is moved to the user's choice
//! once the engine reports `Finished`. Two engine realities force that
//! shape:
//!
//! - The `Requested` handler is synchronous: it must return the
//!   destination immediately and cannot wait for a dialog.
//! - All three engines raise `Requested` on the MAIN thread (WebView2
//!   `DownloadStarting` fires on the UI thread, the WebKitGTK
//!   `decide-destination` signal and the WKWebView download delegate on
//!   the main thread; verified in the wry sources). The plugin's
//!   `blocking_save_file` waits on the plugin's `run_on_main_thread`
//!   dispatch and would deadlock there, so the dialog is offered with
//!   the async `save_file(callback)` instead: the plugin hops to the
//!   main thread itself and answers on a worker thread.
//!
//! The callback lands the user's choice in [`DownloadsManager`] as a
//! pending move. Whichever of the dialog answer or `Finished` arrives
//! second performs the move; a cancel (or a choice that cannot be
//! converted to a path) leaves the file in the download folder, which
//! is also the fallback when no dialog can be shown at all.
//!
//! # File inputs
//!
//! None of the three engines intercept `<input type=file>`; the native
//! picker opens in every case. Nothing to wire (verified by reading the
//! wry sources: there is no file-input interception anywhere in the
//! webview layers). The acceptance check is manual per OS.
//!
//! # Wiring
//!
//! `downloads::handler(app.handle().clone())` is applied via
//! `.on_download(...)` on the main window builder and every session
//! window builder (session windows built by the window manager get it
//! automatically).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Manager, Runtime, Webview};
use tauri_plugin_dialog::DialogExt;

/// Upper bound on remembered download records.
const RECORD_CAP: usize = 128;

/// One completed (or cancelled) download, for the transfers UI.
#[derive(Debug, Clone)]
pub struct DownloadRecord {
    pub url: String,
    pub path: Option<PathBuf>,
    pub success: bool,
    pub requested_at_secs: u64,
}

/// A save-dialog choice waiting for its download to finish.
#[derive(Debug, Clone)]
struct PendingMove {
    url: String,
    from: PathBuf,
    to: PathBuf,
}

static MANAGER: OnceLock<Mutex<DownloadsManager>> = OnceLock::new();

/// Tracks the download folder, the recent record list and pending
/// save-dialog choices.
pub struct DownloadsManager {
    dir: Option<PathBuf>,
    records: VecDeque<DownloadRecord>,
    pending: VecDeque<PendingMove>,
}

impl DownloadsManager {
    fn new() -> Self {
        Self {
            dir: None,
            records: VecDeque::new(),
            pending: VecDeque::new(),
        }
    }

    fn record(&mut self, record: DownloadRecord) {
        if self.records.len() >= RECORD_CAP {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    fn push_pending(&mut self, url: String, from: PathBuf, to: PathBuf) {
        self.pending.push_back(PendingMove { url, from, to });
    }

    /// Hand over the pending move for a download that just finished
    /// successfully, if the user already chose a destination. On engines
    /// without a finished path (macOS) the match falls back to the URL;
    /// a same-URL pair of downloads cannot be told apart there, so the
    /// first match wins and the rest of that URL's choices are dropped.
    fn take_pending_for_finished(&mut self, url: &str, path: Option<&Path>) -> Option<PendingMove> {
        let idx = self.pending.iter().position(|p| match path {
            Some(path) => p.from == path,
            None => p.url == url,
        })?;
        let taken = self.pending.remove(idx)?;
        if path.is_none() {
            self.pending.retain(|p| p.url != url);
        }
        Some(taken)
    }

    /// A download finished without success: its file never landed, so
    /// any pending choice for it is pointless. Same URL-fallback rule as
    /// [`Self::take_pending_for_finished`].
    fn drop_pending_for_failure(&mut self, url: &str, path: Option<&Path>) {
        self.pending.retain(|p| match path {
            Some(path) => p.from != path,
            None => p.url != url,
        });
    }

    /// A save-dialog choice arrived: hand over the move when the
    /// download already finished successfully, drop it when the download
    /// already failed, and keep it when the download is still running.
    fn pending_choice(&mut self, url: &str, from: &Path) -> Option<PendingMove> {
        let idx = self.pending.iter().position(|p| p.from == from)?;
        let matched = |r: &DownloadRecord| match r.path.as_deref() {
            Some(p) => p == from,
            None => r.url == url,
        };
        if self.records.iter().any(|r| r.success && matched(r)) {
            return Some(self.pending.remove(idx).unwrap());
        }
        if self.records.iter().any(|r| !r.success && matched(r)) {
            self.pending.remove(idx);
        }
        None
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

/// Override the download folder (`None` restores the OS default). It is
/// the fallback destination and the save dialog's starting directory.
#[allow(dead_code)]
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
/// collision-free path under the download folder, allows the download,
/// and offers the save dialog. Returning `false` cancels; the shell
/// currently lets every download through.
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
            let Some(dir) = download_dir(&app) else {
                return false;
            };
            let name = filename_from_url(url.as_str())
                .or_else(|| engine_suggested_name(destination))
                .unwrap_or_else(|| "download".to_string());
            let dest = dedup_path(&dir, &name);
            *destination = dest.clone();
            offer_save_dialog(&app, url.to_string(), dest);
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            let pending = {
                let mut guard = manager();
                let pending = if success {
                    guard.take_pending_for_finished(url.as_str(), path.as_deref())
                } else {
                    guard.drop_pending_for_failure(url.as_str(), path.as_deref());
                    None
                };
                guard.record(DownloadRecord {
                    url: url.to_string(),
                    path,
                    success,
                    requested_at_secs: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                });
                pending
            };
            if let Some(pending) = pending {
                move_download_async(url.to_string(), pending);
            }
            true
        }
        // Engine-specific events we do not act on (e.g. the cancelled
        // event on some engines). Download interception still applies.
        _ => true,
    }
}

/// Offer the native save dialog for a download landing at `fallback`.
///
/// Async by design: the `Requested` handler runs on the main thread on
/// every engine and must return immediately, while the plugin's
/// blocking APIs wait on a main-thread dispatch and would deadlock.
/// The plugin opens the dialog on the main thread itself and invokes
/// the callback on a worker thread with the user's choice. A cancel (or
/// a choice that cannot be resolved to a path) keeps the fallback.
fn offer_save_dialog(app: &AppHandle, url: String, fallback: PathBuf) {
    let Some(name) = fallback
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
    else {
        return;
    };
    let mut builder = app
        .dialog()
        .file()
        .set_title("Save download")
        .set_file_name(name);
    if let Some(dir) = fallback.parent() {
        builder = builder.set_directory(dir);
    }
    builder.save_file(move |choice| {
        let Some(choice) = choice else {
            return;
        };
        let Ok(chosen) = choice.into_path() else {
            return;
        };
        let move_now = {
            let mut guard = manager();
            guard.push_pending(url.clone(), fallback.clone(), chosen);
            guard.pending_choice(&url, &fallback)
        };
        if let Some(pending) = move_now {
            move_download_async(url, pending);
        }
    });
}

/// Move a finished download to the user's choice off the main thread,
/// then repoint the record at the final location.
fn move_download_async(url: String, pending: PendingMove) {
    let _ = std::thread::Builder::new()
        .name("persea-download-move".to_string())
        .spawn(move || {
            if pending.to == pending.from {
                return;
            }
            let target = if pending.to.exists() {
                let dir = pending
                    .to
                    .parent()
                    .unwrap_or(pending.from.parent().unwrap_or(Path::new(".")));
                let name = pending
                    .to
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("download");
                dedup_path(dir, name)
            } else {
                pending.to.clone()
            };
            let from = &pending.from;
            if move_file(from, &target).is_ok() {
                update_record_path(&url, from, &target);
            }
        });
}

/// `rename`, falling back to copy + remove across devices.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
    }
}

/// Repoint the finished record at the moved file. On engines without a
/// finished path (macOS) the record is matched by URL; only successful
/// records are repointed.
fn update_record_path(url: &str, from: &Path, to: &Path) {
    let mut guard = manager();
    for record in guard.records.iter_mut().rev() {
        if record.url == url && record.path.as_deref() == Some(from) {
            record.path = Some(to.to_path_buf());
            return;
        }
    }
    for record in guard.records.iter_mut().rev() {
        if record.url == url && record.path.is_none() && record.success {
            record.path = Some(to.to_path_buf());
            return;
        }
    }
}

/// The engine's suggested file name for the download (from the anchor's
/// `download` attribute on `blob:`/`data:` downloads), sanitized.
fn engine_suggested_name(destination: &Path) -> Option<String> {
    // A suggested path escaping the download folder (.. components) must
    // not silently collapse to the bare file name: fold the traversal
    // into the sanitized name so the user sees what was attempted
    // (mirrors the "…_name.txt" shape of the download interception).
    let mut name = destination.file_name()?.to_str()?.to_string();
    if destination
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        if let Some(parent) = destination.parent().and_then(|p| p.file_name()) {
            let parent = parent.to_string_lossy();
            if !parent.is_empty() && parent != ".." {
                name = format!("{parent}_{name}");
            }
        }
    }
    sanitize_filename(&name)
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
        // The first 10 pushes (file-0..file-9) must be gone; the rest
        // survive in order.
        assert!(
            guard
                .records
                .iter()
                .all(|r| !r.url.contains("/file-0.") && !r.url.contains("/file-9.")),
            "the oldest records must be evicted"
        );
        assert!(
            guard.records.front().unwrap().url.contains("/file-10."),
            "the first survivor must be file-10"
        );
    }

    #[test]
    fn choice_before_finish_moves_on_finish_exactly_once() {
        let mut guard = manager();
        guard.records.clear();
        guard.pending.clear();

        // The choice arrives while the download is still running: kept.
        guard.push_pending("u1".to_string(), "/dl/a.bin".into(), "/chosen/a.bin".into());
        assert!(guard.pending_choice("u1", Path::new("/dl/a.bin")).is_none());
        assert_eq!(guard.pending.len(), 1);

        // The finish hands the move over exactly once.
        guard.record(DownloadRecord {
            url: "u1".to_string(),
            path: Some("/dl/a.bin".into()),
            success: true,
            requested_at_secs: 1,
        });
        let moved = guard.take_pending_for_finished("u1", Some(Path::new("/dl/a.bin")));
        assert_eq!(moved.unwrap().to, PathBuf::from("/chosen/a.bin"));
        assert!(guard.pending.is_empty());
        assert!(guard
            .take_pending_for_finished("u1", Some(Path::new("/dl/a.bin")))
            .is_none());
    }

    #[test]
    fn choice_after_finish_moves_immediately() {
        let mut guard = manager();
        guard.records.clear();
        guard.pending.clear();

        guard.record(DownloadRecord {
            url: "u2".to_string(),
            path: Some("/dl/b.bin".into()),
            success: true,
            requested_at_secs: 1,
        });
        guard.push_pending("u2".to_string(), "/dl/b.bin".into(), "/chosen/b.bin".into());
        let moved = guard.pending_choice("u2", Path::new("/dl/b.bin"));
        assert_eq!(moved.unwrap().to, PathBuf::from("/chosen/b.bin"));
        assert!(guard.pending.is_empty());
    }

    #[test]
    fn failed_download_drops_pending_choice() {
        let mut guard = manager();
        guard.records.clear();
        guard.pending.clear();

        // Choice arrives after the download already failed.
        guard.record(DownloadRecord {
            url: "u3".to_string(),
            path: Some("/dl/c.bin".into()),
            success: false,
            requested_at_secs: 1,
        });
        guard.push_pending("u3".to_string(), "/dl/c.bin".into(), "/chosen/c.bin".into());
        assert!(guard.pending_choice("u3", Path::new("/dl/c.bin")).is_none());
        assert!(guard.pending.is_empty());

        // The finish-side drop covers the other order.
        guard.push_pending("u3".to_string(), "/dl/c.bin".into(), "/chosen/c.bin".into());
        guard.drop_pending_for_failure("u3", Some(Path::new("/dl/c.bin")));
        assert!(guard.pending.is_empty());
    }

    #[test]
    fn macos_finished_without_path_matches_by_url() {
        let mut guard = manager();
        guard.records.clear();
        guard.pending.clear();

        guard.push_pending(
            "u4".to_string(),
            "/dl/mac.bin".into(),
            "/chosen/mac.bin".into(),
        );
        let moved = guard.take_pending_for_finished("u4", None);
        assert_eq!(moved.unwrap().from, PathBuf::from("/dl/mac.bin"));
        assert!(guard.pending.is_empty());
    }

    #[test]
    fn choice_for_unknown_download_stays_pending() {
        let mut guard = manager();
        guard.records.clear();
        guard.pending.clear();

        guard.push_pending("u5".to_string(), "/dl/d.bin".into(), "/chosen/d.bin".into());
        assert!(guard.pending_choice("u5", Path::new("/dl/d.bin")).is_none());
        assert_eq!(guard.pending.len(), 1, "no record yet: keep the choice");
    }
}
