//! Shell-side settings, separate from the instance store: the appearance
//! theme (light / dark / auto), the hardware acceleration toggle and app
//! identity. Persisted at `app_config_dir()/shell.json`.
//!
//! The shell theme mirrors the persea web UI design language; the tokens
//! live in `shell/settings.css` and apply to every shell page. The shell
//! theme is independent from each instance's web theme: the webviews keep
//! their own theme handling.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::Manager;

static CONFIG: Mutex<Option<Arc<Mutex<ShellConfig>>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShellConfigFile {
    /// "auto" | "light" | "dark". "auto" follows the OS via
    /// `prefers-color-scheme` (or the Tauri window theme).
    #[serde(default = "default_appearance")]
    pub appearance: String,
    /// The "Hardware acceleration" toggle. `Some(true)` forces
    /// acceleration on, `Some(false)` forces it off (software
    /// rendering), `None` leaves the engine defaults untouched, which is
    /// the initial state for every existing install. Consumed by the
    /// platform GPU wiring in `platform.rs` and `windows.rs`.
    #[serde(default, rename = "gpuAcceleration")]
    pub gpu_acceleration: Option<bool>,
    /// The "Allow untrusted TLS certificates" toggle: skips certificate
    /// validation in the probe and the webviews (Linux web-context
    /// policy, WebView2 launch arg on Windows). Off by default; only
    /// meant for self-signed / private-CA servers the operator trusts.
    #[serde(default, rename = "allowInsecureTls")]
    pub allow_insecure_tls: bool,
}

fn default_appearance() -> String {
    "auto".to_string()
}

impl Default for ShellConfigFile {
    fn default() -> Self {
        Self {
            appearance: default_appearance(),
            gpu_acceleration: None,
            allow_insecure_tls: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShellConfig {
    path: PathBuf,
    pub file: ShellConfigFile,
}

impl ShellConfig {
    pub fn load(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(file) => Self {
                    path: path.clone(),
                    file,
                },
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(path, &backup);
                    eprintln!(
                        "persea-desktop: shell.json unreadable ({e}); backed up to {}",
                        backup.display()
                    );
                    Self {
                        path: path.clone(),
                        file: ShellConfigFile::default(),
                    }
                }
            },
            Err(_) => Self {
                path: path.clone(),
                file: ShellConfigFile::default(),
            },
        }
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(&self.file).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_appearance(&mut self, appearance: &str) -> Result<(), String> {
        match appearance {
            "auto" | "light" | "dark" => {
                self.file.appearance = appearance.to_string();
                Ok(())
            }
            other => Err(format!(
                "Unknown appearance {other:?} (expected auto, light or dark)"
            )),
        }
    }

    pub fn set_gpu_acceleration(&mut self, enabled: bool) {
        self.file.gpu_acceleration = Some(enabled);
    }

    pub fn set_allow_insecure_tls(&mut self, enabled: bool) {
        self.file.allow_insecure_tls = enabled;
    }
}

/// Initialize the process global. Called by the dispatcher from the
/// setup hook in `lib.rs run()`.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&config_dir)?;
    let cfg = Arc::new(Mutex::new(ShellConfig::load(
        &config_dir.join("shell.json"),
    )));
    *CONFIG.lock().unwrap() = Some(cfg);
    Ok(())
}

fn with_config<R>(f: impl FnOnce(&mut ShellConfig) -> R) -> Option<R> {
    let cfg = CONFIG.lock().ok()?.clone()?;
    let mut cfg = cfg.lock().ok()?;
    Some(f(&mut cfg))
}

#[allow(dead_code)] // consumed by the appearance settings UI wiring
pub fn appearance() -> String {
    with_config(|c| c.file.appearance.clone()).unwrap_or_else(default_appearance)
}

/// The persisted "Hardware acceleration" toggle; `None` = unset, engine
/// defaults. Consumed by `platform::gpu_override` (WebKitGTK env on
/// Linux, WebView2 launch args on Windows, see the platform module
/// docs).
pub fn gpu_acceleration() -> Option<bool> {
    with_config(|c| c.file.gpu_acceleration).flatten()
}

pub fn allow_insecure_tls() -> bool {
    with_config(|c| c.file.allow_insecure_tls).unwrap_or(false)
}

fn shell_unavailable() -> String {
    "shell config is not initialized".to_string()
}

#[tauri::command]
pub fn cmd_shell_get_settings() -> Result<ShellConfigFile, String> {
    with_config(|c| c.file.clone()).ok_or_else(shell_unavailable)
}

#[tauri::command]
pub fn cmd_shell_set_appearance(appearance: String) -> Result<ShellConfigFile, String> {
    with_config(|c| {
        c.set_appearance(&appearance)?;
        c.save()?;
        Ok(c.file.clone())
    })
    .ok_or_else(shell_unavailable)?
}

#[tauri::command]
#[allow(dead_code)] // registered by the dispatcher wiring (invoke_handler)
pub fn cmd_shell_set_gpu_acceleration(enabled: bool) -> Result<ShellConfigFile, String> {
    with_config(|c| {
        c.set_gpu_acceleration(enabled);
        c.save()?;
        Ok(c.file.clone())
    })
    .ok_or_else(shell_unavailable)?
}

/// Flip the "Allow untrusted TLS certificates" toggle. Persists the
/// setting, then applies the platform web-engine policy immediately
/// (Linux web-context policy; WebView2 takes the flag from the launch
/// args on the next window creation).
#[tauri::command]
#[allow(dead_code)] // registered by the dispatcher wiring (invoke_handler)
pub fn cmd_shell_set_insecure_tls(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<ShellConfigFile, String> {
    let result = with_config(|c| {
        c.set_allow_insecure_tls(enabled);
        c.save()?;
        Ok(c.file.clone())
    })
    .ok_or_else(shell_unavailable)?;
    crate::platform::apply_insecure_tls_policy(&app);
    result
}

/// App version for the About section, from Cargo at build time.
#[tauri::command]
pub fn cmd_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "persea-desktop-shellcfg-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn appearance_round_trip() {
        let path = tmp_path("roundtrip");
        let mut cfg = ShellConfig::load(&path);
        assert_eq!(cfg.file.appearance, "auto");
        cfg.set_appearance("dark").unwrap();
        cfg.save().unwrap();
        let loaded = ShellConfig::load(&path);
        assert_eq!(loaded.file.appearance, "dark");
    }

    #[test]
    fn appearance_rejects_unknown_values() {
        let mut cfg = ShellConfig::load(&tmp_path("validate"));
        assert!(cfg.set_appearance("sepia").is_err());
        assert_eq!(cfg.file.appearance, "auto");
        assert!(cfg.set_appearance("light").is_ok());
        assert!(cfg.set_appearance("dark").is_ok());
        assert!(cfg.set_appearance("auto").is_ok());
    }

    #[test]
    fn gpu_acceleration_round_trip() {
        let path = tmp_path("gpu");
        let mut cfg = ShellConfig::load(&path);
        assert_eq!(cfg.file.gpu_acceleration, None);
        cfg.set_gpu_acceleration(true);
        cfg.save().unwrap();
        let loaded = ShellConfig::load(&path);
        assert_eq!(loaded.file.gpu_acceleration, Some(true));
        let mut cfg = ShellConfig::load(&path);
        cfg.set_gpu_acceleration(false);
        cfg.save().unwrap();
        let loaded = ShellConfig::load(&path);
        assert_eq!(loaded.file.gpu_acceleration, Some(false));
    }

    #[test]
    fn insecure_tls_round_trip_and_legacy_default() {
        let legacy = tmp_path("legacytls");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"appearance":"auto"}"#).unwrap();
        let mut cfg = ShellConfig::load(&legacy);
        assert!(!cfg.file.allow_insecure_tls);
        cfg.set_allow_insecure_tls(true);
        cfg.save().unwrap();
        let loaded = ShellConfig::load(&legacy);
        assert!(loaded.file.allow_insecure_tls);
        cfg.set_allow_insecure_tls(false);
        cfg.save().unwrap();
        assert!(!ShellConfig::load(&legacy).file.allow_insecure_tls);
    }

    #[test]
    fn legacy_shell_json_without_gpu_field_loads_as_none() {
        let path = tmp_path("legacygpu");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"appearance":"dark"}"#).unwrap();
        let cfg = ShellConfig::load(&path);
        assert_eq!(cfg.file.appearance, "dark");
        assert_eq!(cfg.file.gpu_acceleration, None);
    }

    #[test]
    fn corrupt_shell_json_falls_back_to_defaults() {
        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "nope").unwrap();
        let cfg = ShellConfig::load(&path);
        assert_eq!(cfg.file.appearance, "auto");
        assert!(path.with_extension("json.corrupt").exists());
    }
}
