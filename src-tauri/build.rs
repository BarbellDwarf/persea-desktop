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
    tauri_build::try_build(attributes).expect("tauri-build with app manifest failed");
}
