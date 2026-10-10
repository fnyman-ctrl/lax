fn main() {
    // App commands must be listed here and allowed explicitly in
    // capabilities/, so the web UI can only call what a window is granted.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "accessibility_status",
            "open_accessibility_settings",
            "filevault_enabled",
        ]),
    ))
    .expect("failed to run tauri-build");
}
