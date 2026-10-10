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
| Secrets are redacted before they are stored | §4 | Not yet implemented | None yet |
| While paused, nothing is read | §4 | Pause state in `src-tauri/src/pause.rs` (`Pause::is_paused`). Capture must check it before every read; capture itself arrives in #6 | Pause, resume and expiry: `pause::tests`. A test that capture reads nothing while paused comes with #6 |
| Captured content never appears in logs | §6 | `src-tauri/src/logging.rs`: entries hold only fixed names and numbers, so text can't be logged; clippy denies printing anywhere else (`Cargo.toml`) | `logging::tests`, including `nothing_prints_outside_this_module` and `names_outside_the_allowed_characters_are_replaced` |
| Remote requests are redacted and logged in the outbox | §3 | Not yet implemented | None yet |
| API keys are only stored in the macOS Keychain | §3 | Not yet implemented | None yet |
| Model output is schema-validated before use | §5 | Not yet implemented | None yet |
