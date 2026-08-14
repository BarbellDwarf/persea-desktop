//! Platform enablement: the per-OS app menu bar and the GPU
//! environment helpers.
//!
//! # Menu bar (locked design)
//!
//! One standard menu, per OS:
//!
//! - File: New Session / Close Tab / Quit
//! - Edit: standard clipboard items (undo/redo on macOS + Windows)
//! - View: Fullscreen / Toggle Tabs
//! - Help: Docs / About
//!
//! macOS gets the standard app menu position (About, then the File
//! submenu); the Quit item lives in the app menu there
//! (`PredefinedMenuItem::quit`, native Cmd+Q). Windows/Linux get the
//! menu bar in-window on the MAIN window only: `set_menu` is applied to
//! that window explicitly, so the tab strip and session windows stay
//! menuless (a GTK menu bar inside the 44 px strip would break its
//! layout; `app.set_menu` would have applied it to every window). Menu
//! events are handled app-wide, so the items work no matter which
//! window has focus.
//!
//! Kiosk mode skips the menu entirely (locked design: kiosk surfaces
//! only session UI).
//!
//! Actions:
//!
//! - New Session: navigate the main window to the default instance's
//!   sessions page (`<origin>/sessions.html`); falls back to the first
//!   configured instance when none is marked default. The tab manager
//!   treats the departure like the page's own close flow: the active
//!   tab view closes, the server-side session survives.
//! - Close Tab: close the ACTIVE tab through the window manager
//!   (`cmd_tabs_list` + `cmd_tabs_close`); no-op when no tab is active.
//! - Fullscreen: toggle the main window's native fullscreen state.
//! - Toggle Tabs: flip the shell-side strip visibility override
//!   (`set_strip_visible`).
//! - Docs: open the active instance's docs page (`<origin>/docs`) in
//!   the system browser via the opener plugin.
//! - About: the native about dialog (same `PredefinedMenuItem::about`
//!   the tray uses; the shell settings page also carries an About
//!   section for the in-app variant).
//!
//! # GPU environment
//!
//! [`gpu_override`] reads the persisted "Hardware acceleration" toggle
//! from the shell config store (`shell_config::gpu_acceleration`):
//! `Some(true)` / `Some(false)` mirror the toggle, `None` means "no
//! override, engine defaults" and is the state every existing install
//! starts in. [`apply_gpu_env`] runs before any webview exists, in the
//! setup hook AFTER `shell_config::setup` (the toggle must be loaded to
//! be readable) and BEFORE the main window builder: with the toggle OFF
//! it exports the WebKitGTK software fallback variables on Linux
//! (WebKitGTK reads them at process start; late exports are ignored),
//! and Windows disables the GPU through the WebView2 launch args of
//! [`webview2_gpu_args`]. macOS has nothing to disable (Metal). There is
//! deliberately no shipped default that disables acceleration; the
//! NVIDIA DMABUF workaround is documented in
//! `docs/linux-troubleshooting.md`, not applied by default.

#![allow(dead_code)] // consumed by the lib.rs wiring (dispatcher)

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::Manager;

const ID_NEW_SESSION: &str = "menu-new-session";
const ID_CLOSE_TAB: &str = "menu-close-tab";
const ID_FULLSCREEN: &str = "menu-fullscreen";
const ID_TOGGLE_TABS: &str = "menu-toggle-tabs";
const ID_DOCS: &str = "menu-docs";
/// Plain-item Quit, used on Linux only: `PredefinedMenuItem::quit` is
/// unsupported there and would be silently dropped.
const ID_QUIT: &str = "menu-quit";

// ---------------------------------------------------------------------------
// GPU helpers
// ---------------------------------------------------------------------------

/// The persisted "Hardware acceleration" toggle.
///
/// `Some(true)` / `Some(false)` mirror the shell settings toggle;
/// `None` means "no override, engine defaults" and is returned while
/// the setting is unset (and, defensively, while the shell config is
/// not initialized yet, which is why callers must treat `None` the
/// same as `Some(true)`).
pub fn gpu_override() -> Option<bool> {
    crate::shell_config::gpu_acceleration()
}

/// WebView2 additional browser arguments driven by the toggles:
/// GPU fallback args when the "Hardware acceleration" toggle is OFF
/// (WebView2 replaces the defaults wholesale when any additional
/// argument is set, so the string carries both), plus
/// `--ignore-certificate-errors` when "Allow untrusted TLS
/// certificates" is ON. `None` means "no arguments, engine defaults".
/// Windows-only in effect; the builder method is a no-op elsewhere.
/// `windows.rs` applies it to the strip and session windows; the
/// main-window builder in `lib.rs` uses it the same way.
pub fn webview2_browser_args() -> Option<String> {
    let mut parts = Vec::new();
    if gpu_override() == Some(false) {
        parts.push("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu");
    }
    if crate::shell_config::allow_insecure_tls() {
        parts.push("--ignore-certificate-errors");
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Apply the process-level GPU environment. Call once in the setup
/// hook, after `shell_config::setup` (the toggle must be loaded) and
/// before any webview exists: WebKitGTK reads these variables at
/// process start, so late exports are ignored.
pub fn apply_gpu_env() {
    if gpu_override() == Some(false) {
        #[cfg(target_os = "linux")]
        {
            // Software fallback for the WebKitGTK compositor/DMABUF
            // path (blank-window drivers). Deliberately never set as a
            // default; see docs/linux-troubleshooting.md.
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        // Windows disables the GPU via the WebView2 launch args in
        // webview2_gpu_args; macOS has nothing to disable (Metal).
    }
}

// ---------------------------------------------------------------------------
// App menu bar
// ---------------------------------------------------------------------------

/// Apply the "Allow untrusted TLS certificates" toggle to the web
/// engines. Linux: every webview shares one WebKitGTK web context, so
/// flipping its TLS errors policy (IGNORE when the toggle is on, FAIL
/// when it is off) covers the whole app and takes effect immediately,
/// in both directions. Windows: the flag rides in the WebView2 launch
/// args ([`webview2_browser_args`]), which are read when a window is
/// created, so it applies at startup and to windows created after a
/// runtime toggle (turning it off restores validation on the next
/// launch). macOS: wry/tauri expose no certificate-bypass API, so the
/// system trust store is the only path there.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn apply_insecure_tls_policy(app: &tauri::AppHandle) {
    #[cfg(target_os = "linux")]
    {
        use tauri::Manager;
        use webkit2gtk::{WebContextExt, WebViewExt};
        let policy = if crate::shell_config::allow_insecure_tls() {
            webkit2gtk::TLSErrorsPolicy::Ignore
        } else {
            webkit2gtk::TLSErrorsPolicy::Fail
        };
        if let Some(win) = app.get_webview_window(crate::windows::MAIN_WINDOW_LABEL) {
            let _ = win.with_webview(move |platform| {
                if let Some(context) = platform.inner().context() {
                    // Deprecated since WebKitGTK 2.32 in favor of
                    // per-host allow_tls_certificate_for_host, which
                    // needs the server's certificate object; the probe
                    // (reqwest) cannot hand it over without a raw TLS
                    // handshake, and the toggle is app-global anyway,
                    // so the context-wide policy is the right fit.
                    #[allow(deprecated)]
                    context.set_tls_errors_policy(policy);
                }
            });
        }
    }
}

/// Build and attach the standard app menu. Call from the setup hook
/// AFTER `windows::setup` (the main window must exist; the tab manager
/// must be up so Close Tab and Toggle Tabs can reach it). Skipped in
/// kiosk mode.
pub fn setup_menu(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    if crate::kiosk::is_active() {
        return Ok(());
    }
    let menu = build_menu(app)?;
    #[cfg(target_os = "macos")]
    {
        // The app-wide menu: macOS has one menu bar per app, not per
        // window.
        app.set_menu(menu)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        // In-window menu bar, main window only (see the module docs).
        let Some(main) = app.get_webview_window(crate::windows::MAIN_WINDOW_LABEL) else {
            return Ok(());
        };
        main.set_menu(menu)?;
    }
    app.on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()));
    Ok(())
}

fn build_menu(app: &tauri::App) -> tauri::Result<Menu<tauri::Wry>> {
    let new_session = MenuItem::with_id(
        app,
        ID_NEW_SESSION,
        "New Session",
        true,
        Some("CmdOrCtrl+N"),
    )?;
    let close_tab = MenuItem::with_id(app, ID_CLOSE_TAB, "Close Tab", true, Some("CmdOrCtrl+W"))?;
    let file = Submenu::with_id(app, "File", "File", true)?;
    file.append_items(&[&new_session, &close_tab])?;
    #[cfg(not(target_os = "macos"))]
    {
        let sep = PredefinedMenuItem::separator(app)?;
        file.append_items(&[&sep])?;
        #[cfg(target_os = "linux")]
        {
            // PredefinedMenuItem::quit is unsupported on GTK and would
            // not render; a plain item with the standard chord.
            let quit = MenuItem::with_id(app, ID_QUIT, "Quit", true, Some("CmdOrCtrl+Q"))?;
            file.append_items(&[&quit])?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let quit = PredefinedMenuItem::quit(app, Some("Quit"))?;
            file.append_items(&[&quit])?;
        }
    }

    let edit = Submenu::with_id(app, "Edit", "Edit", true)?;
    #[cfg(target_os = "linux")]
    {
        // Undo/redo predefined items are unsupported on GTK; the
        // clipboard items are native there.
        let cut = PredefinedMenuItem::cut(app, None)?;
        let copy = PredefinedMenuItem::copy(app, None)?;
        let paste = PredefinedMenuItem::paste(app, None)?;
        let sep = PredefinedMenuItem::separator(app)?;
        let select_all = PredefinedMenuItem::select_all(app, None)?;
        edit.append_items(&[&cut, &copy, &paste, &sep, &select_all])?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let undo = PredefinedMenuItem::undo(app, None)?;
        let redo = PredefinedMenuItem::redo(app, None)?;
        let cut = PredefinedMenuItem::cut(app, None)?;
        let copy = PredefinedMenuItem::copy(app, None)?;
        let paste = PredefinedMenuItem::paste(app, None)?;
        let sep1 = PredefinedMenuItem::separator(app)?;
        let select_all = PredefinedMenuItem::select_all(app, None)?;
        let sep2 = PredefinedMenuItem::separator(app)?;
        edit.append_items(&[&undo, &redo, &sep1, &cut, &copy, &paste, &sep2, &select_all])?;
    }

    let fullscreen_accel = if cfg!(target_os = "macos") {
        "Ctrl+Cmd+F"
    } else {
        "F11"
    };
    let fullscreen = MenuItem::with_id(
        app,
        ID_FULLSCREEN,
        "Fullscreen",
        true,
        Some(fullscreen_accel),
    )?;
    let toggle_tabs = MenuItem::with_id(app, ID_TOGGLE_TABS, "Toggle Tabs", true, None::<&str>)?;
    let view = Submenu::with_id(app, "View", "View", true)?;
    view.append_items(&[&fullscreen, &toggle_tabs])?;

    let docs = MenuItem::with_id(app, ID_DOCS, "Docs", true, None::<&str>)?;
    let about = PredefinedMenuItem::about(
        app,
        Some("About"),
        Some(tauri::menu::AboutMetadata {
            name: Some(app.package_info().name.clone()),
            version: Some(app.package_info().version.to_string()),
            ..Default::default()
        }),
    )?;
    let help = Submenu::with_id(app, "Help", "Help", true)?;
    help.append_items(&[&docs, &about])?;

    let menu = Menu::new(app)?;
    #[cfg(target_os = "macos")]
    {
        // The standard app menu: About, separator, Quit. The system
        // renders its title as the app name.
        let about = PredefinedMenuItem::about(
            app,
            Some("About"),
            Some(tauri::menu::AboutMetadata {
                name: Some(app.package_info().name.clone()),
                version: Some(app.package_info().version.to_string()),
                ..Default::default()
            }),
        )?;
        let sep = PredefinedMenuItem::separator(app)?;
        let quit = PredefinedMenuItem::quit(app, None)?;
        let app_menu = Submenu::with_id(app, "Persea Desktop", "Persea Desktop", true)?;
        app_menu.append_items(&[&about, &sep, &quit])?;
        menu.append_items(&[&app_menu])?;
    }
    menu.append_items(&[&file, &edit, &view, &help])?;
    Ok(menu)
}

fn handle_menu_event(app: &tauri::AppHandle, id: &str) {
    match id {
        ID_NEW_SESSION => open_sessions_page(app),
        ID_CLOSE_TAB => close_active_tab(),
        ID_FULLSCREEN => toggle_main_fullscreen(app),
        ID_TOGGLE_TABS => toggle_tab_strip(),
        ID_DOCS => open_docs(),
        ID_QUIT => app.exit(0),
        // Everything else (tab context menu ids, tray ids) belongs to
        // other owners.
        _ => {}
    }
}

/// The default instance, else the first configured one.
fn default_instance() -> Option<crate::instances::Instance> {
    let all = crate::instances::instances();
    all.iter()
        .find(|i| i.default)
        .or_else(|| all.first())
        .cloned()
}

fn open_sessions_page(app: &tauri::AppHandle) {
    let Some(instance) = default_instance() else {
        return;
    };
    let base = instance.url.trim_end_matches('/');
    let target = format!("{base}/sessions.html");
    let Some(win) = app.get_webview_window(crate::windows::MAIN_WINDOW_LABEL) else {
        return;
    };
    if let Ok(url) = tauri::Url::parse(&target) {
        let _ = win.navigate(url);
    }
}

fn close_active_tab() {
    let Ok(tabs) = crate::windows::cmd_tabs_list() else {
        return;
    };
    if let Some(tab) = tabs.into_iter().find(|t| t.active) {
        let _ = crate::windows::cmd_tabs_close(tab.id);
    }
}

fn toggle_main_fullscreen(app: &tauri::AppHandle) {
    let Some(win) = app.get_webview_window(crate::windows::MAIN_WINDOW_LABEL) else {
        return;
    };
    let full = win.is_fullscreen().unwrap_or(false);
    let _ = win.set_fullscreen(!full);
}

fn toggle_tab_strip() {
    crate::windows::set_strip_visible(!crate::windows::strip_visible());
}

fn open_docs() {
    let Some(instance) = default_instance() else {
        return;
    };
    let base = instance.url.trim_end_matches('/');
    let _ = tauri_plugin_opener::open_url(format!("{base}/docs"), None::<&str>);
}
