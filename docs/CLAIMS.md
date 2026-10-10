# Claims

Every promise Lax makes about privacy and security, the code that keeps it, and
the test that checks it. If a claim has no test, it says so.

This page is updated in the same pull request that implements or changes a claim.
The idea comes from [Mason](https://github.com/Baltsar/mason/blob/main/docs/CLAIMS.md).

| Claim | Guardrail | Code | Test |
|---|---|---|---|
| A fresh install sends nothing anywhere | §1, §7 | Not yet implemented | None yet |
| Password fields are never read | §4 | Not yet implemented | None yet |
| Private browser windows are never captured | §4 | Not yet implemented | None yet |
| Password managers are never captured | §4 | Not yet implemented | None yet |
| Exclusions are checked before content is read | §4 | Not yet implemented | None yet |
| Only the host of a URL is stored | §4 | Not yet implemented | None yet |
| Secrets are redacted before they are stored | §4 | `redact()` in `src-tauri/src/redact.rs` masks passwords, keys, tokens, private keys, card numbers and personal identity numbers. `Store::insert` in `src-tauri/src/store.rs` runs it on every title and text, so nothing is stored unredacted | `redact::tests`, and `store::tests::secrets_are_redacted_before_they_are_stored` |
| While paused, nothing is read | §4 | Pause state in `src-tauri/src/pause.rs` (`Pause::is_paused`), saved in the database and restored at startup before anything else runs, so a pause survives a restart. Capture must check it before every read; capture itself arrives in #6 | Pause, resume and expiry: `pause::tests`. Surviving a restart: `store::tests::pause_survives_a_restart`, `a_damaged_pause_keeps_capture_paused`. A test that capture reads nothing while paused comes with #6 |
| The data folder and database are private to the user | §6 | `prepare_data_dir` (`0700`) and `Store::open` (`0600`) in `src-tauri/src/store.rs` | `store::tests::data_folder_is_private`, `existing_data_folder_is_made_private`, `database_file_is_private` |
| Raw captures expire automatically (default 30 days) | §4 | `Store::expire` in `src-tauri/src/store.rs`, run at startup and every hour by `src-tauri/src/expiry.rs` | `store::tests::expire_deletes_captures_older_than_the_retention`, `retention_*` |
| Forgotten data is gone, also from the files on disk | §4 | `Store::forget` in `src-tauri/src/store.rs`: delete, FTS5 secure-delete, `secure_delete`, VACUUM, WAL checkpoint | `store::tests::forgotten_text_is_gone_from_the_files_on_disk` and one test per kind of forget |
| Captured content never appears in logs | §6 | Not yet implemented | None yet |
| Remote requests are redacted and logged in the outbox | §3 | Not yet implemented | None yet |
| API keys are only stored in the macOS Keychain | §3 | Not yet implemented | None yet |
| Model output is schema-validated before use | §5 | Not yet implemented | None yet |
