//! Global shortcuts: app-level chords that work while the app runs,
//! foreground or background.
//!
//! - Summon window (`ctrl+alt+p`): shows, unminimizes and focuses the
//!   main window.
//! - Cycle sessions (`ctrl+shift+tab`): emits [`EVENT_CYCLE_SESSIONS`],
//!   which the session window manager consumes.
//!
//! Both chords are user-configurable in the shell settings page and
//! persist at `app_config_dir()/hotkeys.json` (same pattern as
//! `shell.json`).
//!
//! Platform support (locked design):
//! - Windows, macOS and Linux/X11 register through the global-shortcut
//!   plugin. A failed registration (the OS or another program already
//!   owns the chord) logs a warning and marks the shortcut conflicted
//!   in the settings page; there is no auto-fallback.
//! - Wayland: the plugin is X11-only and silently no-ops there, so the
//!   feature is switched off at startup with a visible note in the
//!   settings page; the app stays fully functional otherwise.
//!
//! The kiosk feature suppresses every shortcut at runtime via
//! [`set_enabled`] (all chords except its own exit chord are disabled
//! in kiosk mode) and restores them on exit.
//!
//! The dispatcher installs the global-shortcut plugin (bare Builder,
//! no eager shortcuts: a conflicted chord must never block startup)
//! before [`setup`] runs, and gates that install on
//! [`platform_supported`]. The registry degrades to a logged conflict
//! if the plugin is missing.
#![allow(dead_code)] // dispatcher wiring + kiosk consumer land later

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventTarget, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcut, Shortcut, ShortcutEvent, ShortcutState};

/// Emitted on the main window when the cycle-sessions chord fires. The
/// session window manager listens for it and advances to the next open
/// session window. Payload: none; the chord itself is the message.
pub const EVENT_CYCLE_SESSIONS: &str = "hotkey-cycle-sessions";

/// The main window label, locked by the window plumbing.
const MAIN_WINDOW_LABEL: &str = "main";

static STATE: Mutex<Option<Arc<Mutex<HotkeysState>>>> = Mutex::new(None);

/// Which app action a chord drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyId {
    Summon,
    CycleSessions,
}

impl HotkeyId {
    pub const ALL: [HotkeyId; 2] = [HotkeyId::Summon, HotkeyId::CycleSessions];

    pub fn from_slug(slug: &str) -> Option<HotkeyId> {
        match slug {
            "summon" => Some(HotkeyId::Summon),
            "cycle-sessions" => Some(HotkeyId::CycleSessions),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            HotkeyId::Summon => "summon",
            HotkeyId::CycleSessions => "cycle-sessions",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HotkeyId::Summon => "Summon window",
            HotkeyId::CycleSessions => "Cycle sessions",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            HotkeyId::Summon => "Focus and raise the app window from anywhere",
            HotkeyId::CycleSessions => "Switch to the next open session window",
        }
    }

    pub fn default_shortcut(self) -> &'static str {
        match self {
            HotkeyId::Summon => "ctrl+alt+p",
            HotkeyId::CycleSessions => "ctrl+shift+tab",
        }
    }
}

/// Persisted shape of `hotkeys.json`. Unknown keys in the file fall back
/// to the defaults per field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotkeysFile {
    #[serde(default = "default_summon")]
    pub summon: String,
    #[serde(default = "default_cycle_sessions")]
    pub cycle_sessions: String,
}

fn default_summon() -> String {
    HotkeyId::Summon.default_shortcut().to_string()
}

fn default_cycle_sessions() -> String {
    HotkeyId::CycleSessions.default_shortcut().to_string()
}

impl Default for HotkeysFile {
    fn default() -> Self {
        Self {
            summon: default_summon(),
            cycle_sessions: default_cycle_sessions(),
        }
    }
}

impl HotkeysFile {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(file) => file,
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(path, &backup);
                    eprintln!(
                        "persea-desktop: hotkeys.json unreadable ({e}); backed up to {}; \
                         using default shortcuts",
                        backup.display()
                    );
                    HotkeysFile::default()
                }
            },
            Err(_) => HotkeysFile::default(),
        }
    }

    /// Atomic save: write a temp file, rename over the target.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn shortcut(&self, id: HotkeyId) -> &str {
        match id {
            HotkeyId::Summon => &self.summon,
            HotkeyId::CycleSessions => &self.cycle_sessions,
        }
    }

    pub fn set_shortcut(&mut self, id: HotkeyId, shortcut: String) {
        match id {
            HotkeyId::Summon => self.summon = shortcut,
            HotkeyId::CycleSessions => self.cycle_sessions = shortcut,
        }
    }
}

/// Runtime state of one shortcut slot, surfaced in the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyStatus {
    /// The chord is registered and fires while the app runs.
    Registered,
    /// Registration failed: the OS or another program owns the chord.
    Conflict,
    /// The platform cannot grab global keys (Wayland).
    Unavailable,
    /// The chord is suppressed (kiosk mode).
    Disabled,
}

/// The registration backend, mockable for tests.
pub trait HotkeyRegistry: Send + Sync {
    /// Register `chord` and bind it to the app action `id`. Err when the
    /// chord cannot be taken (invalid syntax, or already owned elsewhere).
    fn register(&self, id: HotkeyId, chord: &str) -> Result<(), String>;
    /// Release `chord` (no-op when not registered).
    fn unregister(&self, chord: &str) -> Result<(), String>;
    /// Whether this app currently holds `chord`.
    fn is_registered(&self, chord: &str) -> bool;
}

/// Registry over the tauri-plugin-global-shortcut state.
struct PluginRegistry<R: Runtime> {
    handle: AppHandle<R>,
}

impl<R: Runtime> HotkeyRegistry for PluginRegistry<R> {
    fn register(&self, id: HotkeyId, chord: &str) -> Result<(), String> {
        let Some(global) = self.handle.try_state::<GlobalShortcut<R>>() else {
            return Err("global-shortcut plugin is not installed".to_string());
        };
        global
            .on_shortcut(
                chord,
                move |app: &AppHandle<R>, _shortcut: &Shortcut, event: ShortcutEvent| {
                    if event.state == ShortcutState::Pressed {
                        dispatch(app, id);
                    }
                },
            )
            .map_err(|e| e.to_string())
    }

    fn unregister(&self, chord: &str) -> Result<(), String> {
        match self.handle.try_state::<GlobalShortcut<R>>() {
            Some(global) => global.unregister(chord).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    fn is_registered(&self, chord: &str) -> bool {
        self.handle
            .try_state::<GlobalShortcut<R>>()
            .map(|global| global.is_registered(chord))
            .unwrap_or(false)
    }
}

fn dispatch<R: Runtime>(app: &AppHandle<R>, id: HotkeyId) {
    match id {
        HotkeyId::Summon => summon_main_window(app),
        HotkeyId::CycleSessions => cycle_sessions(app),
    }
}

fn summon_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        eprintln!("[hotkeys] summon: main window not found");
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn cycle_sessions<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit_to(
        EventTarget::webview_window(MAIN_WINDOW_LABEL),
        EVENT_CYCLE_SESSIONS,
        (),
    );
}

struct HotkeysState {
    path: PathBuf,
    file: HotkeysFile,
    registry: Box<dyn HotkeyRegistry>,
    enabled: bool,
    platform_ok: bool,
    statuses: HashMap<HotkeyId, HotkeyStatus>,
}

impl HotkeysState {
    /// Register every configured chord and record the resulting statuses.
    /// On unsupported platforms every slot lands on [`HotkeyStatus::Unavailable`].
    fn register_all(&mut self) {
        for id in HotkeyId::ALL {
            let chord = self.file.shortcut(id).to_string();
            let status = register_chord(self, id, &chord);
            self.statuses.insert(id, status);
        }
    }

    /// Change one slot's chord: validate, take the new chord, then drop
    /// the old one and persist. Returns the slot's resulting status;
    /// registration problems surface as [`HotkeyStatus::Conflict`], never
    /// as a crash.
    fn apply_shortcut(&mut self, id: HotkeyId, chord: &str) -> Result<HotkeyStatus, String> {
        if !self.enabled {
            return Err("shortcuts are disabled while kiosk mode is active".to_string());
        }
        if !self.platform_ok {
            return Err("global shortcuts are unavailable on this platform".to_string());
        }
        validate_chord(chord)?;
        let old = self.file.shortcut(id).to_string();
        if old == chord {
            let current = self
                .statuses
                .get(&id)
                .copied()
                .unwrap_or(HotkeyStatus::Unavailable);
            if current != HotkeyStatus::Conflict {
                return Ok(current);
            }
            // The chord was previously conflicted: retry it in case the
            // OS freed the chord in the meantime.
        }
        let status = register_chord(self, id, chord);
        if status == HotkeyStatus::Registered {
            if old != chord {
                let _ = self.registry.unregister(&old);
                self.file.set_shortcut(id, chord.to_string());
                if let Err(e) = self.file.save(&self.path) {
                    eprintln!("[hotkeys] cannot persist hotkeys.json: {e}");
                }
            }
        }
        self.statuses.insert(id, status);
        Ok(status)
    }

    /// Suppress or restore every shortcut at runtime (kiosk mode).
    fn set_runtime_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        for id in HotkeyId::ALL {
            let chord = self.file.shortcut(id).to_string();
            let status = if enabled {
                register_chord(self, id, &chord)
            } else {
                let _ = self.registry.unregister(&chord);
                HotkeyStatus::Disabled
            };
            self.statuses.insert(id, status);
        }
    }

    fn view(&self) -> HotkeyView {
        HotkeyView {
            platform_supported: self.platform_ok,
            enabled: self.enabled,
            shortcuts: HotkeyId::ALL
                .iter()
                .map(|id| ShortcutView {
                    id: id.slug().to_string(),
                    label: id.label().to_string(),
                    description: id.description().to_string(),
                    shortcut: self.file.shortcut(*id).to_string(),
                    status: self
                        .statuses
                        .get(id)
                        .copied()
                        .unwrap_or(HotkeyStatus::Unavailable),
                })
                .collect(),
        }
    }
}

/// Attempt to register `chord` for `id`, returning the resulting status.
/// A chord this app already holds under another slot is a conflict; the
/// slot's own current chord is exempt (re-registering it happens only
/// when the caller retries a conflicted chord).
fn register_chord(state: &mut HotkeysState, id: HotkeyId, chord: &str) -> HotkeyStatus {
    if !state.enabled {
        return HotkeyStatus::Disabled;
    }
    if !state.platform_ok {
        return HotkeyStatus::Unavailable;
    }
    if chord != state.file.shortcut(id) && state.registry.is_registered(chord) {
        eprintln!(
            "[hotkeys] {id:?}: {chord:?} is already registered by this app or another program"
        );
        return HotkeyStatus::Conflict;
    }
    match state.registry.register(id, chord) {
        Ok(()) => HotkeyStatus::Registered,
        Err(e) => {
            eprintln!("[hotkeys] {id:?}: cannot register {chord:?}: {e}");
            HotkeyStatus::Conflict
        }
    }
}

fn validate_chord(chord: &str) -> Result<(), String> {
    if chord.trim().is_empty() {
        return Err("the shortcut is empty; use a chord like \"ctrl+alt+p\"".to_string());
    }
    chord
        .parse::<Shortcut>()
        .map(|_| ())
        .map_err(|e| format!("cannot parse shortcut {chord:?}: {e}"))
}

/// Whether global shortcuts can work on this platform. Windows and macOS
/// always support them; on Linux only X11 sessions do. Wayland compositors
/// do not expose global key grabbing and the plugin silently no-ops
/// there, so the feature is switched off with a visible note instead.
pub fn platform_supported() -> bool {
    if cfg!(target_os = "linux") {
        !is_wayland_session(
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
            &std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        )
    } else {
        true
    }
}

fn is_wayland_session(wayland_display: Option<&str>, session_type: &str) -> bool {
    wayland_display.is_some() || session_type == "wayland"
}

/// Enable or disable every shortcut at runtime. The kiosk feature calls
/// this with `false` on kiosk entry (every chord except its own exit
/// chord is suppressed) and `true` on exit.
pub fn set_enabled(enabled: bool) {
    if let Some(state) = state_handle() {
        state.lock().unwrap().set_runtime_enabled(enabled);
    }
}

/// Initialize the process global and register the configured chords.
/// Called by the dispatcher from the setup hook in `lib.rs run()`, after
/// the global-shortcut plugin is installed (only on supported
/// platforms). Detects the platform once here: the session type cannot
/// change while the app runs.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&config_dir)?;
    let path = config_dir.join("hotkeys.json");
    let file = HotkeysFile::load(&path);
    let registry: Box<dyn HotkeyRegistry> = Box::new(PluginRegistry {
        handle: app.handle().clone(),
    });
    let mut state = HotkeysState {
        path,
        file,
        registry,
        enabled: true,
        platform_ok: platform_supported(),
        statuses: HashMap::new(),
    };
    if !state.platform_ok {
        eprintln!(
            "[hotkeys] global shortcuts unavailable on this platform; \
             the feature stays off (see the settings page note)"
        );
    }
    state.register_all();
    *STATE.lock().unwrap() = Some(Arc::new(Mutex::new(state)));
    Ok(())
}

fn state_handle() -> Option<Arc<Mutex<HotkeysState>>> {
    STATE.lock().ok()?.clone()
}

/// The settings-page view: platform support, enablement and one entry
/// per shortcut slot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyView {
    pub platform_supported: bool,
    pub enabled: bool,
    pub shortcuts: Vec<ShortcutView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutView {
    pub id: String,
    pub label: String,
    pub description: String,
    pub shortcut: String,
    pub status: HotkeyStatus,
}

#[tauri::command]
pub fn cmd_hotkeys_get_settings() -> Result<HotkeyView, String> {
    let Some(state) = state_handle() else {
        return Err("hotkeys are not initialized".to_string());
    };
    Ok(state.lock().unwrap().view())
}

#[tauri::command]
pub fn cmd_hotkeys_set_shortcut(id: String, shortcut: String) -> Result<HotkeyView, String> {
    let Some(id) = HotkeyId::from_slug(&id) else {
        return Err(format!("unknown shortcut {id:?}"));
    };
    let Some(state) = state_handle() else {
        return Err("hotkeys are not initialized".to_string());
    };
    let mut state = state.lock().unwrap();
    let _status = state.apply_shortcut(id, &shortcut)?;
    Ok(state.view())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct MockRegistry {
        data: Arc<Mutex<MockData>>,
    }

    #[derive(Default)]
    struct MockData {
        fail: Vec<String>,
        registered: Vec<String>,
        calls: Vec<(HotkeyId, String)>,
    }

    impl MockRegistry {
        fn failing(chords: &[&str]) -> Self {
            let this = Self::default();
            this.data.lock().unwrap().fail = chords.iter().map(|c| c.to_string()).collect();
            this
        }

        fn registered(&self) -> Vec<String> {
            self.data.lock().unwrap().registered.clone()
        }
    }

    impl HotkeyRegistry for MockRegistry {
        fn register(&self, id: HotkeyId, chord: &str) -> Result<(), String> {
            let mut data = self.data.lock().unwrap();
            data.calls.push((id, chord.to_string()));
            if data.fail.iter().any(|f| f == chord) {
                return Err(format!("shortcut {chord:?} is taken by another program"));
            }
            data.registered.push(chord.to_string());
            Ok(())
        }

        fn unregister(&self, chord: &str) -> Result<(), String> {
            self.data.lock().unwrap().registered.retain(|c| c != chord);
            Ok(())
        }

        fn is_registered(&self, chord: &str) -> bool {
            self.data
                .lock()
                .unwrap()
                .registered
                .iter()
                .any(|c| c == chord)
        }
    }

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "persea-desktop-hotkeys-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn state_with(registry: Box<dyn HotkeyRegistry>, platform_ok: bool) -> HotkeysState {
        let mut state = HotkeysState {
            path: tmp_path("state"),
            file: HotkeysFile::default(),
            registry,
            enabled: true,
            platform_ok,
            statuses: HashMap::new(),
        };
        state.register_all();
        state
    }

    #[test]
    fn defaults_match_the_locked_decisions() {
        let file = HotkeysFile::default();
        assert_eq!(file.summon, "ctrl+alt+p");
        assert_eq!(file.cycle_sessions, "ctrl+shift+tab");
    }

    #[test]
    fn startup_registers_both_defaults() {
        let mock = MockRegistry::default();
        let observer = mock.clone();
        let state = state_with(Box::new(mock), true);

        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Registered)
        );
        assert_eq!(
            state.statuses.get(&HotkeyId::CycleSessions),
            Some(&HotkeyStatus::Registered)
        );
        assert_eq!(
            observer.registered(),
            vec!["ctrl+alt+p".to_string(), "ctrl+shift+tab".to_string()]
        );
    }

    #[test]
    fn conflicted_chord_is_marked_not_crashing() {
        let mock = MockRegistry::failing(&["ctrl+alt+p"]);
        let observer = mock.clone();
        let state = state_with(Box::new(mock), true);

        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Conflict)
        );
        assert_eq!(
            state.statuses.get(&HotkeyId::CycleSessions),
            Some(&HotkeyStatus::Registered)
        );
        assert_eq!(observer.registered(), vec!["ctrl+shift+tab".to_string()]);
    }

    #[test]
    fn unsupported_platform_marks_everything_unavailable() {
        let mock = MockRegistry::default();
        let observer = mock.clone();
        let mut state = state_with(Box::new(mock), false);

        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Unavailable)
        );
        assert_eq!(
            state.statuses.get(&HotkeyId::CycleSessions),
            Some(&HotkeyStatus::Unavailable)
        );
        assert!(observer.registered().is_empty(), "nothing is registered");
        assert!(state
            .apply_shortcut(HotkeyId::Summon, "ctrl+alt+9")
            .is_err());
    }

    #[test]
    fn changing_a_chord_replaces_and_persists() {
        let mock = MockRegistry::default();
        let observer = mock.clone();
        let mut state = state_with(Box::new(mock), true);

        let status = state
            .apply_shortcut(HotkeyId::Summon, "ctrl+alt+9")
            .unwrap();
        assert_eq!(status, HotkeyStatus::Registered);
        assert_eq!(state.file.summon, "ctrl+alt+9");
        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Registered)
        );
        // the old chord is released, the new one held
        assert_eq!(
            observer.registered(),
            vec!["ctrl+shift+tab".to_string(), "ctrl+alt+9".to_string()]
        );
        // persisted on disk
        let loaded = HotkeysFile::load(&state.path);
        assert_eq!(loaded.summon, "ctrl+alt+9");
        assert_eq!(loaded.cycle_sessions, "ctrl+shift+tab");
    }

    #[test]
    fn conflicting_new_chord_keeps_the_old_one() {
        let mock = MockRegistry::failing(&["ctrl+alt+9"]);
        let observer = mock.clone();
        let mut state = state_with(Box::new(mock), true);

        let status = state
            .apply_shortcut(HotkeyId::Summon, "ctrl+alt+9")
            .unwrap();
        assert_eq!(status, HotkeyStatus::Conflict);
        assert_eq!(
            state.file.summon, "ctrl+alt+p",
            "the old chord stays configured"
        );
        assert!(
            observer.registered().iter().any(|c| c == "ctrl+alt+p"),
            "the old chord stays registered"
        );
        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Conflict)
        );
    }

    #[test]
    fn duplicate_chord_across_slots_is_a_conflict() {
        let mut state = state_with(Box::new(MockRegistry::default()), true);

        let status = state
            .apply_shortcut(HotkeyId::CycleSessions, "ctrl+alt+p")
            .unwrap();
        assert_eq!(status, HotkeyStatus::Conflict);
        assert_eq!(state.file.cycle_sessions, "ctrl+shift+tab");
    }

    #[test]
    fn saving_the_current_chord_is_a_noop() {
        let mut state = state_with(Box::new(MockRegistry::default()), true);

        let status = state
            .apply_shortcut(HotkeyId::Summon, "ctrl+alt+p")
            .unwrap();
        assert_eq!(status, HotkeyStatus::Registered);
        assert_eq!(state.file.summon, "ctrl+alt+p");
    }

    #[test]
    fn retry_succeeds_once_the_chord_is_freed() {
        let mock = MockRegistry::failing(&["ctrl+alt+p"]);
        let mut state = state_with(Box::new(mock.clone()), true);
        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Conflict)
        );

        mock.data.lock().unwrap().fail.clear();
        let status = state
            .apply_shortcut(HotkeyId::Summon, "ctrl+alt+p")
            .unwrap();
        assert_eq!(status, HotkeyStatus::Registered);
    }

    #[test]
    fn kiosk_disable_releases_chords_and_reenable_restores() {
        let mock = MockRegistry::default();
        let observer = mock.clone();
        let mut state = state_with(Box::new(mock), true);

        state.set_runtime_enabled(false);
        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Disabled)
        );
        assert!(observer.registered().is_empty(), "chords are released");

        assert!(
            state
                .apply_shortcut(HotkeyId::Summon, "ctrl+alt+9")
                .is_err(),
            "changes are refused while disabled"
        );

        state.set_runtime_enabled(true);
        assert_eq!(
            state.statuses.get(&HotkeyId::Summon),
            Some(&HotkeyStatus::Registered)
        );
        assert_eq!(observer.registered().len(), 2);
    }

    #[test]
    fn wayland_session_detection() {
        assert!(is_wayland_session(Some("wayland-0"), "x11"));
        assert!(is_wayland_session(None, "wayland"));
        assert!(is_wayland_session(Some("wayland-0"), "wayland"));
        assert!(!is_wayland_session(None, "x11"));
        assert!(!is_wayland_session(None, ""));
    }

    #[test]
    fn chord_validation_rejects_garbage_and_empty() {
        assert!(validate_chord("ctrl+alt+p").is_ok());
        assert!(validate_chord("ctrl+shift+tab").is_ok());
        assert!(validate_chord("").is_err());
        assert!(validate_chord("   ").is_err());
        assert!(validate_chord("ctrl+alt+notakey").is_err());
        assert!(
            validate_chord("ctrl+p+alt").is_err(),
            "modifiers must come first"
        );
    }

    #[test]
    fn slugs_round_trip_and_unknown_slugs_are_rejected() {
        assert_eq!(HotkeyId::from_slug("summon"), Some(HotkeyId::Summon));
        assert_eq!(
            HotkeyId::from_slug("cycle-sessions"),
            Some(HotkeyId::CycleSessions)
        );
        assert_eq!(HotkeyId::from_slug("nope"), None);
    }

    #[test]
    fn missing_and_corrupt_files_fall_back_to_defaults() {
        let missing = HotkeysFile::load(&tmp_path("missing"));
        assert_eq!(missing, HotkeysFile::default());

        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "nope").unwrap();
        let corrupt = HotkeysFile::load(&path);
        assert_eq!(corrupt, HotkeysFile::default());
        assert!(path.with_extension("json.corrupt").exists());
    }

    #[test]
    fn partial_files_fill_missing_fields_with_defaults() {
        let path = tmp_path("partial");
        std::fs::write(&path, r#"{"cycle_sessions": "ctrl+alt+9"}"#).unwrap();
        let file = HotkeysFile::load(&path);
        assert_eq!(file.summon, "ctrl+alt+p");
        assert_eq!(file.cycle_sessions, "ctrl+alt+9");
    }

    #[test]
    fn views_carry_slugs_and_statuses() {
        let mock = MockRegistry::failing(&["ctrl+shift+tab"]);
        let state = state_with(Box::new(mock), true);

        let view = state.view();
        assert!(view.platform_supported);
        assert!(view.enabled);
        assert_eq!(view.shortcuts.len(), 2);
        let summon = &view.shortcuts[0];
        assert_eq!(summon.id, "summon");
        assert_eq!(summon.shortcut, "ctrl+alt+p");
        assert_eq!(summon.status, HotkeyStatus::Registered);
        assert_eq!(view.shortcuts[1].status, HotkeyStatus::Conflict);
    }
}
