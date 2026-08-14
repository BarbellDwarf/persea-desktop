use std::fs;
use std::path::Path;

fn main() {
    // App-defined commands declared in the manifest are REMOVED from the
    // default allow-all set, so the remote page (and any window without
    // the ACL grant) is rejected before the command runs. The keyring
    // commands gate the secret surface; the tab/monitor commands gate the
    // window manager (remote pages must never drive shell windows).
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "keyring_set",
            "keyring_get",
            "keyring_delete",
            "keyring_tier",
            "cmd_tabs_list",
            "cmd_tabs_switch",
            "cmd_tabs_close",
            "cmd_tabs_next",
            "cmd_tabs_prev",
            "cmd_tabs_pop_out",
            "cmd_tabs_pop_in",
            "cmd_tabs_expand",
            "cmd_tabs_restore",
            "cmd_tabs_open",
            "cmd_tabs_overflow",
            "cmd_tabs_default_mode_get",
            "cmd_tabs_default_mode_set",
            "cmd_tabs_context_menu",
            "cmd_monitors_list",
            "cmd_transfers_list",
            "cmd_transfer_retry",
            "cmd_transfer_open_folder",
            "cmd_transfer_clear_finished",
            "cmd_transfer_download",
            "notifications_get_enabled",
            "notifications_set_enabled",
        ]));
    populate_remote_urls();
    tauri_build::try_build(attributes).expect("tauri-build with app manifest failed");
}

/// Populate the remote capability's `remote.urls` from the build-time
/// allowlist file `remote-urls.txt` (one origin per line,
/// `scheme://host[:port]`; `#` comments and blank lines are ignored).
///
/// The remote capability is the whole remote-origin IPC gate (Tauri 2 has
/// no runtime ACL API) and capabilities are compiled into the binary at
/// build time, so the instance origins trusted with IPC must be known
/// here. The allowlist file is checked in empty: with no file, no entries,
/// or nothing valid, the capability keeps its empty urls array and every
/// remote origin is denied IPC (fail closed). The bridge re-validates the
/// runtime instance store against the baked file at startup, so an origin
/// the user adds later only gets bridge features when the builder
/// allowlisted it.
///
/// The merge is plain string splicing on the checked-in capability file
/// (build.rs has no JSON parser): the file keeps the exact `"urls": [`
/// marker this splice targets, and the allowlist file is authoritative —
/// its entries replace the urls array on every build, so a changed
/// allowlist takes effect on the next rebuild. Entry validation (https/
/// http scheme, no JSON-breaking characters) keeps the splice from
/// corrupting the file; anything unexpected logs a warning and leaves the
/// file untouched (fail closed).
fn populate_remote_urls() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    let allowlist_path = Path::new(&manifest_dir).join("remote-urls.txt");
    println!("cargo:rerun-if-changed={}", allowlist_path.display());
    let Ok(raw) = fs::read_to_string(&allowlist_path) else {
        // Absent or unreadable: leave the capability as checked in.
        return;
    };
    let origins: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(valid_allowlist_origin)
        .collect();
    if origins.is_empty() {
        return;
    }

    let capability_path = Path::new(&manifest_dir)
        .join("capabilities")
        .join("remote.json");
    println!("cargo:rerun-if-changed={}", capability_path.display());
    let Ok(capability) = fs::read_to_string(&capability_path) else {
        eprintln!(
            "[build] cannot read capabilities/remote.json; remote allowlist stays as checked in"
        );
        return;
    };
    let Some(merged) = splice_urls(&capability, &origins) else {
        eprintln!(
            "[build] capabilities/remote.json no longer carries the empty \
             \"urls\": [] marker; remote allowlist stays as checked in"
        );
        return;
    };
    if merged != capability {
        if let Err(e) = fs::write(&capability_path, merged) {
            eprintln!("[build] cannot write capabilities/remote.json ({e}); remote allowlist stays as checked in");
            return;
        }
    }
    eprintln!(
        "[build] remote-urls.txt: allowlisted {} origin(s) in capabilities/remote.json",
        origins.len()
    );
}

/// One allowlist line accepted as a remote capability url pattern.
/// Refuses anything that is not an https/http origin and anything with
/// JSON-breaking characters or control bytes, so a bad entry can neither
/// corrupt the capability file nor slip an unvalidated pattern through.
fn valid_allowlist_origin(line: &str) -> Option<String> {
    let origin = line.trim();
    if !(origin.starts_with("https://") || origin.starts_with("http://")) {
        eprintln!("[build] remote-urls.txt: ignoring non-http(s) entry {origin:?}");
        return None;
    }
    if origin.chars().any(char::is_control) || origin.contains(['"', '\\']) {
        eprintln!("[build] remote-urls.txt: ignoring unsafe entry {origin:?}");
        return None;
    }
    Some(origin.to_string())
}

/// Splice `origins` into the urls array of the capability file content.
/// The allowlist file is authoritative: its entries replace the array
/// contents on every build, so a changed allowlist takes effect on the
/// next rebuild. The checked-in file carries the exact marker `"urls": [`.
/// Returns None when the file shape is unexpected (no marker, or an
/// existing array that contains a `[`, which only happens with an IPv6
/// literal whose closing `]` would split the splice), so the caller
/// leaves the file untouched (fail closed).
fn splice_urls(capability: &str, origins: &[String]) -> Option<String> {
    const MARKER: &str = "\"urls\": [";
    let marker_end = capability.find(MARKER)? + MARKER.len();
    let array_end = capability[marker_end..].find(']')? + marker_end;
    if capability[marker_end..array_end].contains('[') {
        // A hand-edited IPv6-literal entry would make the first ']' the
        // wrong delimiter; refuse rather than corrupt.
        return None;
    }
    let items = origins
        .iter()
        .map(|origin| format!("\"{origin}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut merged = String::with_capacity(capability.len() + items.len());
    merged.push_str(&capability[..marker_end]);
    merged.push_str(&items);
    merged.push_str(&capability[array_end..]);
    Some(merged)
}
