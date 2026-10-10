# Claims

Every promise Lax makes about privacy and security, the code that keeps it, and
the test that checks it. If a claim has no test, it says so.

This page is updated in the same pull request that implements or changes a claim.
The idea comes from [Mason](https://github.com/Baltsar/mason/blob/main/docs/CLAIMS.md).

| Claim | Guardrail | Code | Test |
|---|---|---|---|
| A fresh install sends nothing anywhere | §1, §7 | Not yet implemented | None yet |
| Password fields are never read | §4 | `is_secure_text_field` in `src-tauri/src/exclusions.rs`. Capture must check every element with it (#6) | `exclusions::tests::password_fields_are_never_read`. A test that capture skips them comes with #6 |
| Private browser windows are never captured | §4 | `Exclusions::check_window` in `src-tauri/src/exclusions.rs`, by title markers in several languages. Safari puts no marker in the title, so a reliable per-browser signal is still needed (#6) | `exclusions::tests::private_browser_windows_are_never_captured` |
| Password managers are never captured | §4 | `Exclusions::check_app` in `src-tauri/src/exclusions.rs` (also system password prompts and Lax itself) | `exclusions::tests::password_managers_are_never_captured`, `re_enabling_cannot_unlock_the_never_tier` |
| Exclusions are checked before content is read | §4 | The order is defined in `src-tauri/src/exclusions.rs`: app, then title, then site, before any content. Capture must follow it (#6) | None yet. Comes with #6 |
| Only the host of a URL is stored | §4 | `site_from_url` in `src-tauri/src/site.rs` keeps only the host of `http`/`https` addresses. Capture must store its result, never the address (#6, #8) | `site::tests` (paths, queries, fragments, credentials and ports dropped; other schemes keep nothing) |
| Secrets are redacted before they are stored | §4 | `redact()` in `src-tauri/src/redact.rs` masks passwords, keys, tokens, private keys, card numbers and personal identity numbers. `Store::insert` in `src-tauri/src/store.rs` runs it on every title and text, so nothing is stored unredacted | `redact::tests`, and `store::tests::secrets_are_redacted_before_they_are_stored` |
| While paused, nothing is read | §4 | Pause state in `src-tauri/src/pause.rs` (`Pause::is_paused`), saved in the database and restored at startup before anything else runs, so a pause survives a restart. Capture must check it before every read; capture itself arrives in #6 | Pause, resume and expiry: `pause::tests`. Surviving a restart: `store::tests::pause_survives_a_restart`, `a_damaged_pause_keeps_capture_paused`. A test that capture reads nothing while paused comes with #6 |
| The data folder and database are private to the user | §6 | `prepare_data_dir` (`0700`) and `Store::open` (`0600`) in `src-tauri/src/store.rs` | `store::tests::data_folder_is_private`, `existing_data_folder_is_made_private`, `database_file_is_private` |
| Raw captures expire automatically (default 90 days) | §4 | `Store::expire` in `src-tauri/src/store.rs`, run at startup and every hour by `src-tauri/src/expiry.rs` | `store::tests::expire_deletes_captures_older_than_the_retention`, `retention_*` |
| Forgotten data is gone, also from the files on disk | §4 | `Store::forget` in `src-tauri/src/store.rs`: delete, FTS5 secure-delete, `secure_delete`, VACUUM, WAL checkpoint | `store::tests::forgotten_text_is_gone_from_the_files_on_disk` and one test per kind of forget |
| Captured content never appears in logs | §6 | `src-tauri/src/logging.rs`: entries hold only fixed names and numbers, so text can't be logged; clippy denies printing anywhere else (`Cargo.toml`) | `logging::tests`, including `nothing_prints_outside_this_module` and `names_outside_the_allowed_characters_are_replaced` |
| Remote requests are redacted and logged in the outbox | §3 | Not yet implemented | None yet |
| API keys are only stored in the macOS Keychain | §3 | Not yet implemented | None yet |
| Model output is schema-validated before use | §5 | Not yet implemented | None yet |
