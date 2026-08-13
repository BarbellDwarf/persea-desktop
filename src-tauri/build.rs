fn main() {
    // Keyring commands are app-defined commands; declaring them in the
    // app manifest removes them from the default allow-all set, so the
    // remote page (and any window without the ACL grant) is rejected
    // before the command runs.
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "keyring_set",
            "keyring_get",
            "keyring_delete",
            "keyring_tier",
        ]));
    tauri_build::try_build(attributes).expect("tauri-build with app manifest failed");
}
