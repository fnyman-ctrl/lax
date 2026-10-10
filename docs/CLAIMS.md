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
| While paused, nothing is read | §4 | Not yet implemented | None yet |
| Captured content never appears in logs | §6 | Not yet implemented | None yet |
| Remote requests are redacted and logged in the outbox | §3 | Not yet implemented | None yet |
| API keys are only stored in the macOS Keychain | §3 | Not yet implemented | None yet |
| Model output is schema-validated before use | §5 | Not yet implemented | None yet |
