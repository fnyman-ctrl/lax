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
| [0005](0005-memory-as-markdown.md) | Memory pages stored as Markdown files | Accepted |
| [0006](0006-sqlite-with-rusqlite.md) | SQLite through rusqlite, with SQLite compiled in | Accepted |
