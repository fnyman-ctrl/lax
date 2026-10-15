// Build scripts talk to cargo through stdout.
#[allow(clippy::print_stdout)]
fn main() {
    // SQLCipher's own logging is compiled out by LIBSQLITE3_FLAGS in
    // .cargo/config.toml (ADR 0010). Cargo reads that file only when it runs
    // in src-tauri or below, so refuse to build Lax without the flags rather
    // than build a SQLCipher that can log.
    println!("cargo:rerun-if-env-changed=LIBSQLITE3_FLAGS");
    let flags = std::env::var("LIBSQLITE3_FLAGS").unwrap_or_default();
    for flag in ["-DSQLCIPHER_OMIT_LOG", "-DSQLCIPHER_OMIT_LOG_DEVICE"] {
        assert!(
            flags.split_whitespace().any(|f| f == flag),
            "LIBSQLITE3_FLAGS lacks {flag}: run cargo in src-tauri, where it reads .cargo/config.toml"
        );
    }

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
            "outbox_entries",
            "outbox_payload",
            "delete_outbox",
            "outbox_retention_days",
            "set_outbox_retention_days",
            "journal_status",
            "set_journal_enabled",
            "journal_episodes",
            "ask_memory",
            "memory_status",
            "set_memory_enabled",
            "memory_entities",
            "forget_entity",
            "forget_memory",
            "keep_entity",
            "rename_entity",
            "merge_entities",
            "dont_learn_list",
            "allow_learning",
            "setup_status",
            "finish_setup",
            "writing_status",
            "set_writing_enabled",
            "set_writing_key",
            "close_panel",
            "panel_call",
            "write_draft",
            "fit_panel",
            "insert_draft",
            "database_status",
            "continue_opening",
            "retry_opening",
            "start_over",
            "page_text",
            "save_page",
            "close_page",
            "delete_old_page_files",
            "keep_old_page_files",
            "pick_export_folder",
            "export_pages",
            "page_notes",
            "recreate_page",
            "recreate_about_me",
            "open_memory_folder",
            "set_own_names",
            "page_history",
            "restore_page_version",
        ]),
    ))
    .expect("failed to run tauri-build");
}
