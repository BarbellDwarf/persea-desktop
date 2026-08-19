mod bridge;
mod dialogs;
mod downloads;
mod drop;
mod hooks;
mod hotkeys;
mod http;
mod instances;
mod keyring;
mod kiosk;
mod navigation;
mod notify;
mod pairing;
mod platform;
mod poller;
mod provisioning;
mod shell_config;
mod token_store;
mod transfer;
mod tray;
mod windows;

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(notify::updater_plugin());

    // Embedded WebDriver server for the E2E suite (macOS leg). Debug
    // builds only: release binaries never register it, so the server
    // never starts there.
    #[cfg(debug_assertions)]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());

    builder
        .setup(|app| {
            // Provisioning BEFORE the instance store: the merge consumes
            // the resolved provision document, and the store's startup
            // re-sync applies it before probes and auto-open.
            provisioning::setup(app)?;

            // Theme first: shell pages render with it.
            shell_config::setup(app)?;

            // GPU env before any webview exists: with the toggle OFF this
            // exports the WebKitGTK software fallback variables on Linux.
            platform::apply_gpu_env();

            // Instance store BEFORE the window policy: the navigation
            // allowlist and the initial URL both need the loaded store.
            // (auto_open inside instances::setup skips silently while the
            // window does not exist; we navigate after building it below.)
            instances::setup(app)?;

            // Global shortcuts: the plugin only on platforms that support
            // it (Wayland no-ops otherwise); bare Builder so a conflicted
            // chord can never block startup.
            if hotkeys::platform_supported() {
                app.handle()
                    .plugin(tauri_plugin_global_shortcut::Builder::new().build())?;
            }
            hotkeys::setup(app)?;

            // Kiosk decision before the main window exists (the chord
            // registers here; entry happens after the window manager is
            // up so the tab strip can be hidden).
            kiosk::setup(app)?;

            // The main window is code-built: on_navigation and
            // on_new_window are build-time-only in tauri 2.11, so a
            // config-declared window can never carry the lockdown
            // handlers. The window opens on the local welcome page;
            // auto-open then navigates to the default/last instance.
            let origins: Vec<String> = instances::instances()
                .iter()
                .map(|i| i.url.clone())
                .collect();
            let default_url = origins.first().cloned().unwrap_or_default();
            // The main window is code-built (navigation lockdown, bridge
            // init, downloads, per-instance webview store, Linux close
            // policy); instance switches rebuild it via windows.rs.
            let _main = windows::build_main_window(app.handle(), &default_url)?;

            // Apply the persisted untrusted-TLS policy to the shared
            // WebKitGTK web context now that the first webview exists
            // (no-op elsewhere; Windows reads the flag from the launch
            // args at window creation).
            platform::apply_insecure_tls_policy(app.handle());

            // Bridge: validate the runtime instance origins against the
            // baked remote-URL allowlist (fail closed), install the
            // page→shell listeners.
            bridge::register(app, origins);

            // Session window/tab manager: needs the main window to exist
            // (it builds the tabstrip window and session windows).
            windows::setup(app)?;

            // App menu bar after the window manager (Close Tab / Toggle
            // Tabs reach the tab manager; skipped in kiosk mode).
            platform::setup_menu(app)?;

            // Kiosk entry after the window manager is up (the strip must
            // exist to be hidden).
            if kiosk::is_active() {
                kiosk::enter(app.handle());
            }

            // Transfers + drag-drop: after the window manager (drops land
            // on its windows).
            transfer::setup(app)?;
            drop::setup(app)?;

            // Tray + poller + notifications: skipped in kiosk (the kiosk
            // module forbids the tray while active).
            if !kiosk::is_active() {
                tray::setup(app)?;
            }

            // Auto-open the default/last instance now that the window
            // exists (the same call auto_open made early was a silent
            // no-op).
            let handle = app.handle().clone();
            let _ = instances::cmd_instances_open_default(handle);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            instances::cmd_instances_list,
            instances::cmd_instances_add,
            instances::cmd_instances_update,
            instances::cmd_instances_remove,
            instances::cmd_instances_set_default,
            instances::cmd_instances_probe,
            instances::cmd_instances_open,
            instances::cmd_instances_open_default,
            instances::cmd_instances_open_setup,
            shell_config::cmd_shell_get_settings,
            shell_config::cmd_shell_set_appearance,
            shell_config::cmd_shell_set_gpu_acceleration,
            shell_config::cmd_shell_set_insecure_tls,
            shell_config::cmd_app_version,
            keyring::keyring_set,
            keyring::keyring_get,
            keyring::keyring_delete,
            keyring::keyring_tier,
            token_store::cmd_token_acquire,
            hotkeys::cmd_hotkeys_get_settings,
            hotkeys::cmd_hotkeys_set_shortcut,
            pairing::pairing_supported,
            pairing::pairing_start,
            pairing::pairing_status,
            pairing::pairing_cancel,
            pairing::pairing_open_confirm_page,
            pairing::pairing_list_tokens,
            pairing::pairing_revoke,
            windows::cmd_tabs_list,
            windows::cmd_tabs_switch,
            windows::cmd_tabs_close,
            windows::cmd_tabs_next,
            windows::cmd_tabs_prev,
            windows::cmd_tabs_pop_out,
            windows::cmd_tabs_pop_in,
            windows::cmd_tabs_expand,
            windows::cmd_tabs_restore,
            windows::cmd_tabs_open,
            windows::cmd_tabs_overflow,
            windows::cmd_tabs_default_mode_get,
            windows::cmd_tabs_default_mode_set,
            windows::cmd_tabs_context_menu,
            windows::cmd_monitors_list,
            transfer::cmd_transfers_list,
            transfer::cmd_transfer_retry,
            transfer::cmd_transfer_open_folder,
            transfer::cmd_transfer_clear_finished,
            transfer::cmd_transfer_download,
            notify::notifications_get_enabled,
            notify::notifications_set_enabled,
            notify::cmd_updater_check,
            notify::cmd_updater_download_and_restart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Persea Desktop");
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TauriConfig {
        product_name: String,
        version: String,
        identifier: String,
        build: BuildConfig,
        app: AppConfig,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BuildConfig {
        frontend_dist: String,
    }

    #[derive(Deserialize)]
    struct AppConfig {
        windows: Vec<serde_json::Value>,
    }

    fn load_config() -> TauriConfig {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json");
        let raw = std::fs::read_to_string(path).expect("tauri.conf.json must exist");
        serde_json::from_str(&raw).expect("tauri.conf.json must parse")
    }

    #[test]
    fn app_identity_is_locked_to_the_d01_decisions() {
        let cfg = load_config();
        assert_eq!(cfg.product_name, "Persea Desktop");
        // The version follows the package version: releases bump
        // Cargo.toml + tauri.conf.json together (see AGENTS.md, Releases),
        // so the check locks the identity, not a specific release number.
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cfg.identifier, "dev.persea.desktop");
        assert_eq!(cfg.build.frontend_dist, "../shell");
    }

    #[test]
    fn main_window_is_code_built_not_config_declared() {
        // on_navigation/on_new_window are build-time-only, so the main
        // window is constructed in lib.rs setup() and must NOT be
        // declared in tauri.conf.json.
        let cfg = load_config();
        assert!(
            cfg.app.windows.is_empty(),
            "the main window must be code-built (navigation lockdown needs builder hooks)"
        );
    }
}
