//! Provisioning: enterprise server injection.
//!
//! The installer, an MDM push, or a build-time bake drops a provision
//! document at a machine-level location; the shell reads it at startup
//! and merges its instances into the instance store as locked entries.
//! The document can also pin kiosk mode and shell-settings overrides
//! (locked: provision > user settings > defaults).
//!
//! Source order, first match wins:
//!   1. app bundle `Resources/provision.json` (build-time bake)
//!   2. Windows: `HKLM\Software\Persea Desktop\Provisioning`, registry
//!      value `config` (JSON string; the HKLM hive is admin-only and is
//!      the TRUSTED Windows path)
//!   3. Linux: `/etc/persea-desktop/provision.json`, honored only when
//!      owned by uid 0 or the root group
//!   4. macOS: `/Library/Application Support/Persea Desktop/provision.json`,
//!      honored under the same ownership rule
//!
//! Trust and failure behavior (locked design): sources that are
//! unreadable, invalid JSON, or fail the ownership check are logged and
//! ignored. The app always launches; a source that fails validation is
//! rejected as a whole, so there are no half-imports. The chain continues
//! to the next source after a rejected one.
//!
//! Re-sync on every launch: the validated document is hashed (SHA-256 of
//! its canonical serialization, so formatting changes are free). The hash
//! is stored on the instance store; an unchanged hash skips the merge
//! entirely (idempotent: no writes, no churn). A changed hash merges: ADD
//! new entries, UPDATE changed name/default, REMOVE deleted provisioned
//! entries. User-added instances are never touched.
//!
//! Removing a provision source does not unlock previously applied
//! entries: the lock follows the provision entry, and the store keeps the
//! last applied hash. A subsequent launch without any source is a no-op.
//!
//! The full contract (schema, per-platform delivery steps, build-time
//! bake flow) is documented in `docs/provisioning.md`.

//! The accessor surface here (effective, is_active, kiosk_enabled_override,
//! settings_override, settings_overrides) is the contract consumed by the
//! instances merge (effective) and, later, the kiosk mode and the
//! settings page (the override accessors). Not-yet-consumed items carry
//! an allow until their consumers wire in.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// The effective provision document plus the identity used for
/// idempotence (canonical content hash) and logging.
#[derive(Debug, Clone)]
pub struct EffectiveProvision {
    pub doc: ProvisionFile,
    /// SHA-256 (hex) of the canonical serialization of `doc`.
    pub hash: String,
    /// Human-readable source description for logs.
    pub source: String,
}

static ACTIVE: Mutex<Option<EffectiveProvision>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Schema (the contract in docs/provisioning.md)
// ---------------------------------------------------------------------------

/// The provision document:
///
/// ```json
/// { "instances": [ {"name": "...", "url": "https://...", "default": true} ],
///   "kiosk": { "enabled": false },
///   "settings": { "appearance": "auto", "shortcuts": {} } }
/// ```
///
/// `instances` matches the instance store schema (name + url + default).
/// `settings` is an opaque object of shell-settings overrides; the shell
/// consumes individual keys through [`settings_override`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionFile {
    #[serde(default)]
    pub instances: Vec<ProvisionedInstance>,
    #[serde(default)]
    pub kiosk: ProvisionedKiosk,
    #[serde(default)]
    pub settings: serde_json::Value,
}

impl Default for ProvisionFile {
    fn default() -> Self {
        Self {
            instances: Vec::new(),
            kiosk: ProvisionedKiosk::default(),
            settings: serde_json::Value::Null,
        }
    }
}

/// One provisioned instance. Field names match the instance store schema
/// so the merge is a straight field copy. Entries become locked on apply;
/// the lock flag is never part of the source schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionedInstance {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub default: bool,
}

/// Kiosk override. `enabled: Option<bool>`: `Some(true)` pins kiosk on,
/// `Some(false)` pins it off, `None` (section absent) leaves kiosk to the
/// user setting. Either pin is a locked override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionedKiosk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl Default for ProvisionedKiosk {
    fn default() -> Self {
        Self { enabled: None }
    }
}

// ---------------------------------------------------------------------------
// Source discovery and trust
// ---------------------------------------------------------------------------

/// One candidate provision source, in priority order.
#[derive(Debug, Clone)]
pub(crate) enum SourceSpec {
    /// `resource_dir()/provision.json`: shipped inside the app bundle.
    /// The installer-built bundle is trusted as-is (no ownership check).
    BundleResource { path: PathBuf },
    /// Windows machine policy. The HKLM hive is admin-only, so the value
    /// is trusted without an ownership check.
    #[cfg(target_os = "windows")]
    Registry { key: String, value: String },
    /// Machine-level file with an ownership requirement: honored only
    /// when owned by uid 0 or the root group. `root_owned` is evaluated
    /// at chain construction time (and injected for tests).
    MachineFile { path: PathBuf, root_owned: bool },
}

impl SourceSpec {
    fn describe(&self) -> String {
        match self {
            SourceSpec::BundleResource { path } => path.display().to_string(),
            #[cfg(target_os = "windows")]
            SourceSpec::Registry { key, value } => {
                format!(r"HKLM\{key}\{value}")
            }
            SourceSpec::MachineFile { path, .. } => path.display().to_string(),
        }
    }
}

/// Entry point. Called by the dispatcher from the setup hook BEFORE
/// `instances::setup`: the instance merge consumes the effective document
/// resolved here. Never fails: bad sources are logged and ignored.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let chain = source_chain(app);
    if let Some(effective) = apply_chain(&chain) {
        *ACTIVE.lock().unwrap() = Some(effective);
    }
    Ok(())
}

/// The source chain in locked order: bundle Resources first, then the
/// platform machine location.
fn source_chain(app: &tauri::App) -> Vec<SourceSpec> {
    let mut chain = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        chain.push(SourceSpec::BundleResource {
            path: resources.join("provision.json"),
        });
    }
    #[cfg(target_os = "windows")]
    chain.push(SourceSpec::Registry {
        key: r"Software\Persea Desktop\Provisioning".to_string(),
        value: "config".to_string(),
    });
    #[cfg(target_os = "linux")]
    chain.push(SourceSpec::MachineFile {
        path: PathBuf::from("/etc/persea-desktop/provision.json"),
        root_owned: file_is_root_owned(Path::new("/etc/persea-desktop/provision.json")),
    });
    #[cfg(target_os = "macos")]
    chain.push(SourceSpec::MachineFile {
        path: PathBuf::from("/Library/Application Support/Persea Desktop/provision.json"),
        root_owned: file_is_root_owned(Path::new(
            "/Library/Application Support/Persea Desktop/provision.json",
        )),
    });
    chain
}

/// First valid source wins; rejected sources are logged and the chain
/// continues. Returns the effective document or `None` (no provision).
fn apply_chain(chain: &[SourceSpec]) -> Option<EffectiveProvision> {
    for spec in chain {
        match try_source(spec) {
            SourceOutcome::Applied(effective) => {
                eprintln!(
                    "persea-desktop: provisioning active from {} ({} instances, hash {})",
                    effective.source,
                    effective.doc.instances.len(),
                    &effective.hash[..effective.hash.len().min(12)],
                );
                return Some(effective);
            }
            SourceOutcome::Absent => {}
            SourceOutcome::Invalid(reason) => {
                eprintln!(
                    "persea-desktop: ignoring provision source {}: {reason}",
                    spec.describe()
                );
            }
        }
    }
    None
}

enum SourceOutcome {
    Applied(EffectiveProvision),
    /// Source does not exist (or the value is not set): not an error,
    /// keep walking the chain.
    Absent,
    /// Source exists but is untrusted or unparseable: log and continue.
    Invalid(String),
}

fn try_source(spec: &SourceSpec) -> SourceOutcome {
    match spec {
        SourceSpec::BundleResource { path } => match read_file(path) {
            ReadOutcome::Absent => SourceOutcome::Absent,
            ReadOutcome::Data(bytes) => validate_and_hash(spec, &bytes),
            ReadOutcome::Failure(reason) => SourceOutcome::Invalid(reason),
        },
        SourceSpec::MachineFile { path, root_owned } => {
            if !root_owned {
                return SourceOutcome::Invalid(
                    "file is not owned by uid 0 or the root group".to_string(),
                );
            }
            match read_file(path) {
                ReadOutcome::Absent => SourceOutcome::Absent,
                ReadOutcome::Data(bytes) => validate_and_hash(spec, &bytes),
                ReadOutcome::Failure(reason) => SourceOutcome::Invalid(reason),
            }
        }
        #[cfg(target_os = "windows")]
        SourceSpec::Registry { key, value } => match read_registry(key, value) {
            ReadOutcome::Absent => SourceOutcome::Absent,
            ReadOutcome::Data(bytes) => validate_and_hash(spec, &bytes),
            ReadOutcome::Failure(reason) => SourceOutcome::Invalid(reason),
        },
    }
}

enum ReadOutcome {
    Absent,
    Data(Vec<u8>),
    Failure(String),
}

fn read_file(path: &Path) -> ReadOutcome {
    match std::fs::read(path) {
        Ok(bytes) => ReadOutcome::Data(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReadOutcome::Absent,
        Err(e) => ReadOutcome::Failure(e.to_string()),
    }
}

/// Ownership rule for machine-level files (Linux + macOS): honored only
/// when owned by uid 0 or the root group. Installer-written files are
/// root-owned; anything else is untrusted and ignored.
#[cfg(unix)]
fn file_is_root_owned(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.uid() == 0 || m.gid() == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn file_is_root_owned(_path: &Path) -> bool {
    false
}

/// HKLM read, trusted (admin-only hive). The value `config` holds the
/// provision JSON as a REG_SZ string.
#[cfg(target_os = "windows")]
fn read_registry(key_path: &str, value_name: &str) -> ReadOutcome {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    unsafe {
        let mut key: HKEY = std::mem::zeroed();
        let key_wide = wide(key_path);
        let status = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR::from_raw(key_wide.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        );
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return ReadOutcome::Absent;
        }
        if status != ERROR_SUCCESS {
            return ReadOutcome::Failure(format!("cannot open registry key: {status:?}"));
        }

        let value_wide = wide(value_name);
        let name = PCWSTR::from_raw(value_wide.as_ptr());
        // First pass: size only (the value type is validated below).
        let mut value_type = std::mem::zeroed();
        let mut len: u32 = 0;
        let status = RegQueryValueExW(key, name, None, Some(&mut value_type), None, Some(&mut len));
        if status == ERROR_FILE_NOT_FOUND {
            RegCloseKey(key);
            return ReadOutcome::Absent;
        }
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            RegCloseKey(key);
            return ReadOutcome::Failure(format!("cannot query registry value: {status:?}"));
        }
        // Second pass: fetch the payload.
        let mut buf = vec![0u8; len as usize];
        let mut actual = len;
        let status = RegQueryValueExW(
            key,
            name,
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr()),
            Some(&mut actual),
        );
        RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return ReadOutcome::Failure(format!("cannot read registry value: {status:?}"));
        }
        if value_type != REG_SZ {
            return ReadOutcome::Failure("registry value is not a string (REG_SZ)".to_string());
        }
        // REG_SZ data is UTF-16LE, null-terminated.
        let chars: Vec<u16> = buf[..actual as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let chars: Vec<u16> = chars.into_iter().take_while(|&c| c != 0).collect();
        match String::from_utf16(&chars) {
            Ok(text) => ReadOutcome::Data(text.into_bytes()),
            Err(e) => ReadOutcome::Failure(format!("registry value is not valid UTF-16: {e}")),
        }
    }
}

#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// Validation and hash
// ---------------------------------------------------------------------------

fn validate_and_hash(spec: &SourceSpec, raw: &[u8]) -> SourceOutcome {
    match parse_and_validate(raw) {
        Ok(doc) => {
            let canonical = serde_json::to_string(&doc).unwrap_or_default();
            let hash = sha256_hex(canonical.as_bytes());
            SourceOutcome::Applied(EffectiveProvision {
                doc,
                hash,
                source: spec.describe(),
            })
        }
        Err(reason) => SourceOutcome::Invalid(reason),
    }
}

/// Parse + validate a provision document. A source that fails here is
/// rejected as a whole (no half-imports): a structurally valid doc with
/// one bad instance entry is rejected entirely.
fn parse_and_validate(raw: &[u8]) -> Result<ProvisionFile, String> {
    let text = std::str::from_utf8(raw).map_err(|e| format!("not valid UTF-8: {e}"))?;
    let doc: ProvisionFile =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    if !doc.settings.is_null() && !doc.settings.is_object() {
        return Err("\"settings\" must be a JSON object".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for inst in &doc.instances {
        if inst.name.trim().is_empty() {
            return Err(format!("instance {} has an empty name", inst.url));
        }
        crate::instances::validate_instance_url(&inst.url)
            .map_err(|e| format!("instance {:?}: {e}", inst.name))?;
        if !seen.insert(inst.url.as_str()) {
            return Err(format!("duplicate instance URL {}", inst.url));
        }
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Accessors for the consumers (instances merge, kiosk, settings)
// ---------------------------------------------------------------------------

/// The effective provision document, when a valid source is active.
/// Consumed by the instance-store merge hook (`instances::sync_provisioned`).
pub fn effective() -> Option<EffectiveProvision> {
    ACTIVE.lock().ok()?.clone()
}

/// True when a provision source is active at all (instances, kiosk or
/// settings).
pub fn is_active() -> bool {
    ACTIVE.lock().ok().is_some_and(|a| a.is_some())
}

/// Locked kiosk override: `Some(true)` pins kiosk on, `Some(false)` pins
/// it off, `None` means the source does not govern kiosk (user setting /
/// default applies). Kiosk mode applies the pin and blocks user override
/// while it is set.
pub fn kiosk_enabled_override() -> Option<bool> {
    ACTIVE
        .lock()
        .ok()
        .and_then(|a| a.as_ref()?.doc.kiosk.enabled)
}

/// Locked shell-settings override for one key, when the provision
/// document pins it (provision > user setting > default).
pub fn settings_override(key: &str) -> Option<serde_json::Value> {
    ACTIVE
        .lock()
        .ok()?
        .as_ref()?
        .doc
        .settings
        .as_object()?
        .get(key)
        .cloned()
}

/// All locked shell-settings overrides (empty when the document pins
/// none). The settings page merges these over the user settings.
pub fn settings_overrides() -> serde_json::Map<String, serde_json::Value> {
    let Ok(guard) = ACTIVE.lock() else {
        return Default::default();
    };
    let Some(effective) = (*guard).as_ref() else {
        return Default::default();
    };
    effective
        .doc
        .settings
        .as_object()
        .cloned()
        .unwrap_or_default()
}

/// Test-only: replace the active document so the instance-store merge
/// tests drive the same entrypoint the app uses.
#[cfg(test)]
pub(crate) fn set_active_for_tests(active: Option<EffectiveProvision>) {
    *ACTIVE.lock().unwrap() = active;
}

// ---------------------------------------------------------------------------
// SHA-256 (pure Rust: no external digest dependency)
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let off = i * 4;
            *word =
                u32::from_be_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc() -> ProvisionFile {
        ProvisionFile {
            instances: vec![
                ProvisionedInstance {
                    name: "Prod".into(),
                    url: "https://persea.example.com".into(),
                    default: true,
                },
                ProvisionedInstance {
                    name: "Lab".into(),
                    url: "https://lab.example.com".into(),
                    default: false,
                },
            ],
            kiosk: ProvisionedKiosk {
                enabled: Some(true),
            },
            settings: serde_json::json!({ "appearance": "dark" }),
        }
    }

    fn tmp_path(tag: &str, name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "persea-desktop-provision-test-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join(name)
    }

    #[test]
    fn sha256_matches_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn parse_and_validate_accepts_full_document() {
        let doc =
            parse_and_validate(serde_json::to_vec(&sample_doc()).unwrap().as_slice()).unwrap();
        assert_eq!(doc.instances.len(), 2);
        assert_eq!(doc.kiosk.enabled, Some(true));
        assert_eq!(doc.settings["appearance"], "dark");
    }

    #[test]
    fn parse_and_validate_rejects_bad_input() {
        assert!(parse_and_validate(b"not json").is_err());
        assert!(parse_and_validate(b"").is_err());
        let dup = r#"{"instances":[
            {"name":"A","url":"https://a.example.com"},
            {"name":"B","url":"https://a.example.com"}]}"#;
        assert!(parse_and_validate(dup.as_bytes()).is_err());
        let http = r#"{"instances":[{"name":"A","url":"http://a.example.com"}]}"#;
        assert!(parse_and_validate(http.as_bytes()).is_err());
        let empty_name = r#"{"instances":[{"name":"  ","url":"https://a.example.com"}]}"#;
        assert!(parse_and_validate(empty_name.as_bytes()).is_err());
        let bad_settings = r#"{"settings":["not","an","object"]}"#;
        assert!(parse_and_validate(bad_settings.as_bytes()).is_err());
        // A doc with one bad entry is rejected as a whole (no half-imports).
        let mixed = r#"{"instances":[
            {"name":"Good","url":"https://good.example.com"},
            {"name":"Bad","url":"ftp://bad.example.com"}]}"#;
        assert!(parse_and_validate(mixed.as_bytes()).is_err());
    }

    #[test]
    fn parse_and_validate_accepts_partial_documents() {
        let settings_only = r#"{"settings":{"appearance":"dark"}}"#;
        let doc = parse_and_validate(settings_only.as_bytes()).unwrap();
        assert!(doc.instances.is_empty());
        assert_eq!(doc.kiosk.enabled, None);
        assert_eq!(doc.settings["appearance"], "dark");

        let kiosk_only = r#"{"kiosk":{"enabled":false}}"#;
        let doc = parse_and_validate(kiosk_only.as_bytes()).unwrap();
        assert!(doc.instances.is_empty());
        assert_eq!(doc.kiosk.enabled, Some(false));

        let empty = r#"{}"#;
        let doc = parse_and_validate(empty.as_bytes()).unwrap();
        assert!(doc.instances.is_empty());
        assert_eq!(doc.kiosk.enabled, None);
        assert!(doc.settings.is_null());
    }

    #[test]
    fn hash_is_canonical_across_formatting() {
        let a = r#"{ "instances": [ {"name":"A","url":"https://a.example.com","default":true} ], "kiosk": { "enabled": true } }"#;
        let b = "{\"instances\":[{\"name\":\"A\",\"url\":\"https://a.example.com\",\"default\":true}],\"kiosk\":{\"enabled\":true}}";
        let doc_a = parse_and_validate(a.as_bytes()).unwrap();
        let doc_b = parse_and_validate(b.as_bytes()).unwrap();
        assert_eq!(doc_a, doc_b);
        let hash_a = sha256_hex(serde_json::to_string(&doc_a).unwrap().as_bytes());
        let hash_b = sha256_hex(serde_json::to_string(&doc_b).unwrap().as_bytes());
        assert_eq!(hash_a, hash_b);
        let doc_c = parse_and_validate(
            r#"{"instances":[{"name":"A","url":"https://a.example.com","default":false}]}"#
                .as_bytes(),
        )
        .unwrap();
        assert_ne!(
            hash_a,
            sha256_hex(serde_json::to_string(&doc_c).unwrap().as_bytes())
        );
    }

    #[test]
    fn source_order_first_valid_wins() {
        let base = tmp_path("order", "order.json");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let bundle = base.join("bundle");
        let machine = base.join("machine");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&machine).unwrap();
        let bundle_file = bundle.join("provision.json");
        let machine_file = machine.join("provision.json");
        let json = r#"{"instances":[{"name":"A","url":"https://a.example.com"}]}"#;

        // Bundle present and valid: bundle wins even with a valid machine file.
        std::fs::write(&bundle_file, json).unwrap();
        std::fs::write(&machine_file, json).unwrap();
        let chain = vec![
            SourceSpec::BundleResource {
                path: bundle_file.clone(),
            },
            SourceSpec::MachineFile {
                path: machine_file.clone(),
                root_owned: true,
            },
        ];
        let eff = apply_chain(&chain).expect("bundle source must apply");
        assert!(eff.source.ends_with("provision.json"));
        assert_eq!(eff.doc.instances.len(), 1);

        // Bundle invalid: logged and the machine file applies.
        std::fs::write(&bundle_file, "{ nope").unwrap();
        let eff = apply_chain(&chain).expect("machine source must apply after invalid bundle");
        assert!(eff.source.contains("machine"));
        assert_eq!(eff.doc.instances.len(), 1);

        // Bundle absent: machine file applies.
        std::fs::remove_file(&bundle_file).unwrap();
        let eff = apply_chain(&chain).expect("machine source must apply when bundle absent");
        assert!(eff.source.contains("machine"));

        // Untrusted machine file: logged and ignored, nothing applies.
        let chain = vec![SourceSpec::MachineFile {
            path: machine_file,
            root_owned: false,
        }];
        assert!(apply_chain(&chain).is_none());
    }

    #[test]
    fn untrusted_and_missing_sources_are_ignored() {
        let base = tmp_path("absent", "absent.json");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let missing = SourceSpec::MachineFile {
            path: base.clone(),
            root_owned: true,
        };
        assert!(matches!(try_source(&missing), SourceOutcome::Absent));

        std::fs::write(&base, "not json").unwrap();
        assert!(matches!(try_source(&missing), SourceOutcome::Invalid(_)));

        let unowned = SourceSpec::MachineFile {
            path: base.clone(),
            root_owned: false,
        };
        assert!(matches!(try_source(&unowned), SourceOutcome::Invalid(_)));
    }

    #[cfg(unix)]
    #[test]
    fn ownership_rule_rejects_user_owned_files() {
        use std::os::unix::fs::MetadataExt;
        let path = tmp_path("owner", "owner.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        if meta.uid() == 0 || meta.gid() == 0 {
            // Running as root: a freshly created file cannot be
            // user-owned here, skip.
            return;
        }
        assert!(!file_is_root_owned(&path));
    }

    #[cfg(unix)]
    #[test]
    fn ownership_rule_accepts_root_owned_files_when_privileged() {
        use std::os::unix::fs::chown;
        let path = tmp_path("owner2", "owner2.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        if chown(&path, Some(0), Some(0)).is_ok() {
            assert!(file_is_root_owned(&path));
        }
    }

    #[test]
    fn accessors_reflect_the_active_document() {
        let eff = EffectiveProvision {
            doc: sample_doc(),
            hash: sha256_hex(b"x"),
            source: "test".into(),
        };
        set_active_for_tests(Some(eff.clone()));
        assert!(is_active());
        assert_eq!(kiosk_enabled_override(), Some(true));
        assert_eq!(
            settings_override("appearance"),
            Some(serde_json::json!("dark"))
        );
        assert_eq!(settings_override("nope"), None);
        assert_eq!(settings_overrides().len(), 1);
        assert_eq!(effective().unwrap().hash, eff.hash);

        set_active_for_tests(None);
        assert!(!is_active());
        assert_eq!(kiosk_enabled_override(), None);
        assert_eq!(settings_override("appearance"), None);
        assert!(settings_overrides().is_empty());
        assert!(effective().is_none());
    }
}
