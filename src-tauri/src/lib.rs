pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
    fn main_window_exists_and_is_titled() {
        let cfg = load_config();
        let main = cfg
            .app
            .windows
            .iter()
            .find(|w| w.label.as_deref() == Some("main"))
            .unwrap_or_else(|| panic!("a window labeled \"main\" must be configured"));
        assert_eq!(main.title, "Persea Desktop");
    }
}
