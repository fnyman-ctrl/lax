fn main() {
    // App commands must be listed here and allowed explicitly in
    // capabilities/, so the web UI can only call what a window is granted.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "accessibility_status",
            "open_accessibility_settings",
            "filevault_enabled",
            "capture_status",
            "recent_captures",
            "search_captures",
            "capture_detail",
            "captured_apps",
            "forget_captures",
            "delete_all_data",
            "retention_days",
            "set_retention_days",
            "exclusion_settings",
            "add_exclusion",
            "remove_exclusion",
            "set_default_excluded",
            "find_local_models",
            "model_settings",
            "set_model",
            "clear_model",
            "test_model",
        ]),
    ))
    .expect("failed to run tauri-build");
}
