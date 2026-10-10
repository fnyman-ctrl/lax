# 0006. SQLite through rusqlite, with SQLite compiled in

- **Status:** Accepted
- **Date:** 2026-10-10

## Context

Lax stores captures, and later episodes and search indexes, in SQLite
([ADR 0005](0005-memory-as-markdown.md)). It needs full-text search (FTS5) that
handles Swedish and English, and should be able to move to an encrypted database
(SQLCipher, GUARDRAILS §6) and to vector search (`sqlite-vec`) later. The data is
the most sensitive Lax has, so the code that touches it should be small and easy
to review, and only Rust code that we control may reach the database.

## Decision

Use [rusqlite](https://github.com/rusqlite/rusqlite) with the `bundled` feature,
which compiles a known SQLite version, with FTS5, into Lax.

- Database access is synchronous, from Rust only. The web UI reaches data through
  narrow Tauri commands, never through SQL.
- The schema is plain SQL in `src-tauri/migrations/`, applied in order and tracked
  with `PRAGMA user_version`. A database from a newer Lax is refused, not changed.
- The store redacts text and window titles itself before writing them.

## Consequences

- One mature, widely used library with a thin API over SQLite. Queries are plain
  SQL that anyone can read and run against the file without Lax.
- We know which SQLite version runs and that FTS5 and its `secure-delete` option are
  there, whatever the macOS version. In return, we're responsible for updating
  SQLite when it has security fixes. Dependabot raises rusqlite updates.
- Moving to SQLCipher is a feature switch (`bundled-sqlcipher`), not a rewrite.
- SQLite is C code compiled into Lax, like with every SQLite option.
- A few new crates: `rusqlite`, `libsqlite3-sys`, `hashlink`, `hashbrown`,
  `fallible-iterator` and `fallible-streaming-iterator` (MIT or MIT/Apache-2.0).

## Alternatives considered

- **`tauri-plugin-sql`:** lets the web UI run SQL directly. That breaks least
  privilege: the database should only be reachable through Rust code we control.
- **`sqlx`:** async and checks queries at compile time, but heavier, with more
  dependencies and setup. Neither benefit matters for a local database with one writer.
- **`diesel`:** a full ORM. More than Lax needs, and it hides the SQL that users and
  reviewers should be able to read.
- **The SQLite that ships with macOS:** no build step, but its version and enabled
  features depend on the OS version, and it can't become SQLCipher.
