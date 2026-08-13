mod bridge;
mod instances;
mod keyring;
mod navigation;
mod shell_config;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Theme first: shell pages render with it.
            shell_config::setup(app)?;

            // Instance store BEFORE the window policy: the navigation
            // allowlist and the initial URL both need the loaded store.
            // (auto_open inside instances::setup skips silently while the
            // window does not exist; we navigate after building it below.)
            instances::setup(app)?;

            // The main window is code-built: on_navigation and
            // on_new_window are build-time-only in tauri 2.11 (D03), so a
            // config-declared window can never carry the lockdown
            // handlers. The window opens on the local welcome page;
            // auto-open then navigates to the default/last instance.
            let origins: Vec<String> = instances::instances()
                .iter()
                .map(|i| i.url.clone())
                .collect();
            let policy = navigation::NavigationPolicy::new(origins.clone(), Vec::new());
            let builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Persea Desktop")
                    .inner_size(1280.0, 800.0)
                    .min_inner_size(800.0, 600.0)
                    .center()
                    .initialization_script(bridge::init_script());
            let builder = navigation::lock_window_builder(builder, policy);
            builder.build()?;

            // Bridge: validate the runtime instance origins against the
            // baked remote-URL allowlist (fail closed), install the
            // page→shell listeners.
            bridge::register(app, origins);

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
            shell_config::cmd_app_version,
            keyring::keyring_set,
            keyring::keyring_get,
            keyring::keyring_delete,
            keyring::keyring_tier,
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
        windows: Vec<WindowConfig>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WindowConfig {
        label: Option<String>,
        title: String,
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
        assert_eq!(cfg.version, "1.2.0");
        assert_eq!(cfg.identifier, "dev.persea.desktop");
        assert_eq!(cfg.build.frontend_dist, "../shell");
    }

    #[test]
    fn main_window_is_code_built_not_config_declared() {
        // D03: on_navigation/on_new_window are build-time-only, so the
        // main window is constructed in lib.rs setup() and must NOT be
        // declared in tauri.conf.json.
        let cfg = load_config();
        assert!(
            cfg.app.windows.is_empty(),
            "the main window must be code-built (navigation lockdown needs builder hooks)"
        );
    }
}
