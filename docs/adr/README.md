# Architecture Decision Records

Short records of significant decisions: what we decided, why, and what it costs.
They let anyone understand why Lax looks the way it does without digging through
chat logs.

- Create a new ADR when a decision is hard to reverse or affects many parts of the code.
- ADRs are never rewritten after they're accepted. To change a decision, write a new
  ADR that supersedes the old one and update the old one's status.
- Use [template.md](template.md).

| # | Decision | Status |
|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-tauri-and-rust.md) | Tauri and Rust, building on Handy's approach | Accepted |
| [0003](0003-agpl-license.md) | AGPL-3.0 license | Accepted |
| [0004](0004-openai-compatible-model-providers.md) | OpenAI-compatible model providers, local by default | Accepted |
| [0005](0005-memory-as-markdown.md) | Memory pages stored as Markdown files | Accepted; superseded in part by [0011](0011-pages-in-the-database.md) |
| [0006](0006-sqlite-with-rusqlite.md) | SQLite through rusqlite, with SQLite compiled in | Accepted |
| [0007](0007-http-client-for-model-calls.md) | reqwest with macOS TLS for model calls | Accepted |
| [0008](0008-learning-memory.md) | How the memory learns, and how user edits are protected | Accepted; superseded in part by [0011](0011-pages-in-the-database.md) |
| [0009](0009-writing-assistant.md) | A writing assistant called with the Option key | Accepted |
| [0010](0010-encrypted-database.md) | Encrypt the database with SQLCipher, keyed from the Keychain | Accepted |
| [0011](0011-pages-in-the-database.md) | Keep the memory pages in the encrypted database | Accepted |
| [0012](0012-app-lock.md) | Lock Lax's window with Touch ID or the login password | Accepted |
| [0013](0013-tidying-the-memory.md) | Tidying the memory: less noise, suggested merges, projects in parts | Accepted |
