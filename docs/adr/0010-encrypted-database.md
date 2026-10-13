# 0010. Encrypt the database with SQLCipher, keyed from the Keychain

- **Status:** Proposed
- **Date:** 2026-10-13

## Summary

Lax's database would be encrypted with SQLCipher, using Apple's own crypto
library, with a random key that Lax keeps in the user's login keychain.
That protects the file from other programs that read files, from copies and
backups, and on Macs without FileVault. It doesn't protect against malware
aimed at Lax, and it doesn't cover the Markdown pages in `~/Lax`. Existing
databases are converted once. If the key can't be found, Lax stops instead
of starting over, and it never throws a key away. Before this is accepted,
a short spike must show how the Keychain behaves with Lax's ad-hoc signed
builds (see "Before deciding").

## Context

Issue #114. GUARDRAILS §6 says "Database encryption (SQLCipher) is planned",
and [ADR 0006](0006-sqlite-with-rusqlite.md) chose rusqlite partly so that
this would be a feature switch.

`lax.sqlite` in the data folder
(`~/Library/Application Support/io.github.fnyman-ctrl.lax`) holds raw
captures and their index, the journal, the memory's entities, aliases,
facts and every version of the blocks Lax wrote on pages, episode vectors,
the outbox and settings. The pages themselves are Markdown files in `~/Lax`
(ADR 0005). They are not in the database, and this ADR doesn't change them.

What protects it today:

- FileVault, if it's on. Lax warns when it's off (`filevault.rs`).
- The data folder is `0700`, the database file `0600`.
- `secure_delete`, FTS5 `secure-delete` on all three indexes, and VACUUM
  plus a WAL checkpoint in `Store::forget`, so deleted text doesn't linger.
- Expiry: raw captures go 30 days after capture once summarized, and at
  the maximum age (default 90 days) in any case (`Store::expire`).

One gap in that: `Store::scrub` runs VACUUM with SQLite's default
temporary storage, so SQLite writes a temporary copy of the live,
plaintext database to the user's temp folder and deletes it afterwards.
Encryption must not bring that back in plaintext. The incremental vacuum
below avoids the copy altogether.

### Threat model

| Threat | FileVault only | With an encrypted database |
|---|---|---|
| Mac stolen while off, FileVault on | Protected | No extra gain |
| Mac stolen while off, FileVault off | **Not protected** | Protected if the login password is strong, unless automatic login is on (it unlocks the keychain at boot) |
| Another program of the same user reads the file | **Not protected.** `0600` only keeps other users out | Protected from programs that only read files (sync and backup agents, indexers, file-scraping packages). Not from code that targets Lax; see the malware row |
| Copies of the folder: Time Machine to a disk without encryption, cloud backup tools, a USB stick | **Not protected** | Protected: the key isn't in the copy, or only in a backed-up login keychain locked by the login password |
| Freed disk blocks read later | Protected while FileVault is on | Protected from the switch on. Plaintext from before stays in old blocks, snapshots and backups until they are gone |
| Malware running as the user | Not protected | **Not protected.** It can trigger the Keychain prompt users learn to accept after updates, replace `~/Applications/Lax.app` (the user can write there), possibly inject code into an ad-hoc build (not verified), use Accessibility to read Lax's window, read `~/Lax`, or watch the screen itself |
| Someone at an unlocked, logged-in Mac | Not protected | Not protected: Lax runs and the keychain is unlocked |
| An administrator or root | Not protected | Not protected |

As far as we know (not verified), macOS doesn't ask before another app
reads a non-sandboxed app's `Application Support` folder.

## Before deciding: a Keychain spike

Ad-hoc signed code is most likely identified by its code hash, which
changes with every build. If so, the Keychain item's access list stops
matching after every `npm run build:app` or `cargo build`, and macOS asks
again, possibly for the login password, before Lax may read its key. Not
verified. A Lax that asks for the password after every update teaches users
to click "Always Allow", which weakens the malware row further.

A spike on a real Mac, before this ADR is accepted, answers:

1. Does an ad-hoc rebuild prompt? With which wording, and is the password
   needed?
2. Does a stable self-signed code-signing certificate for local builds
   (made once in Keychain Access) keep the access list valid across
   rebuilds?
3. Does Migration Assistant bring a login-keychain item to a new Mac?

The results go into this ADR. If the prompts are bad and a self-signed
certificate doesn't help, encryption waits for the signed `.dmg`.

## Open questions for the maintainer

The Decision below assumes the recommended answers.

1. **On for everyone, with no off switch?** Recommended: yes. Users
   without FileVault gain the most.
2. **No recovery key, with the key visible in Keychain Access?**
   Recommended: yes, for 0.x. See "No recovery key".
3. **Login keychain now, data protection keychain once Lax is signed?**
   Recommended: yes.
4. **Which access class after the move?** Recommended:
   `kSecAttrAccessibleAfterFirstUnlock`, not synced, **without**
   `ThisDeviceOnly`. `ThisDeviceOnly` items never reach a new Mac, so every
   move to a new Mac would end in "Start over".
5. **Keychain prompts after rebuilds, if the spike shows them?**
   Recommended: acceptable only if a self-signed certificate removes them
   for people who build often; otherwise wait for the signed build.
6. **Incremental vacuum instead of VACUUM?** Recommended: yes.
   `temp_store = FILE` during VACUUM would allow plaintext temporary files.
7. **Keep `cipher_memory_security` off?** Recommended: yes. It guards
   against reading Lax's memory, which encryption doesn't claim to stop.
8. **Convert automatically at startup, after a one-time notice?**
   Recommended: yes. Asking first would leave most databases plaintext.
9. **A plaintext export?** Recommended: not now. The user guide explains
   the `sqlcipher` tool; an export that writes plaintext needs its own
   decision.

## Decision

Lax encrypts its database with **SQLCipher** through rusqlite's
`bundled-sqlcipher` feature, with **CommonCrypto** as the crypto provider.
The key is random, kept in the **login keychain**, never synced, never
written anywhere else, and never replaced or deleted by Lax on its own.

### Build

- `rusqlite = { version = "0.40", features = ["bundled-sqlcipher"] }`
  instead of `["bundled"]`. Not the vendored-OpenSSL feature.
- On a Mac, libsqlite3-sys then builds SQLCipher against CommonCrypto and
  the Security framework, with no OpenSSL. It switches to OpenSSL if
  `OPENSSL_DIR`, or `OPENSSL_LIB_DIR` with `OPENSSL_INCLUDE_DIR`, is set,
  also in target-prefixed form (`AARCH64_APPLE_DARWIN_OPENSSL_DIR`). A test
  on a keyed temporary database checks that `PRAGMA cipher_provider` is
  `commoncrypto` (it returns nothing without a key).
- SQLCipher's own logging, which goes to the macOS unified log outside
  `logging.rs`, is compiled out: `LIBSQLITE3_FLAGS = "-DSQLCIPHER_OMIT_LOG"`
  in `src-tauri/.cargo/config.toml` under `[env]`. A pragma can't do it
  reliably, because the log target is set process-wide at the first open.

### The key

- **Made by Lax:** 32 bytes from `SecRandomCopyBytes`, stored as a generic
  password of 64 hex characters (service `io.github.fnyman-ctrl.lax`,
  account `database-key`, label "Lax database key"). Hex so the user can
  see and copy it in Keychain Access.
- **A new key only when there is nothing to lose:** the lookup returned
  `errSecItemNotFound` **and** `lax.sqlite` is absent, empty or plaintext.
  Any other error (locked, denied, cancelled) fails closed.
- **Never overwritten, never deleted.** Lax doesn't call `SecItemUpdate`
  on the key's data or `SecItemDelete` on it. The only exceptions are
  "Start over", which renames the item but keeps it, and the user's own
  "Delete the old file", which removes an old file and its old key
  together.
- **Login keychain, for now.** The data protection keychain needs
  entitlements authorized by a provisioning profile (Apple TN3137), and Lax
  0.x is ad-hoc signed. In the login keychain, `kSecAttrAccessible` has no
  effect (Apple's docs); access is decided by the item's access list.
  `kSecAttrSynchronizable = false` is set anyway, so the code stays right
  when it moves. The access class after the move: open question 4.
- **In code:** one module, `src-tauri/src/keychain.rs`, on
  `security-framework` (already in the tree). The store asks a
  `KeySource`, so tests use a fixed key and never touch the real Keychain.
- The key is held in `Zeroizing` buffers (`zeroize` is already in the
  tree). That only clears Lax's own copies: the Keychain's result, SQLite's
  copy of a bound value and freed heap may still hold it. SQLCipher keeps
  its working copy in memory it locks and wipes.

### Opening the database

**Off the main thread.** Today `Store::open` runs in Tauri's `setup`,
before the tray and the window exist. A Keychain prompt or a conversion of
several seconds would make Lax look hung, with no window to explain why.
So setup creates the tray and window first, with capture paused, and opens
the store on a background thread. The window shows "Unlocking…" or
"Encrypting…" until it's done; commands that need the store answer "not
ready yet". Capture, expiry and the journal start only once the store is
open, as today.

`Store::open` already creates an empty `0600` file before opening. Then:

| File | What happens |
|---|---|
| Empty (new install) | New key (by the rule above); key; `auto_vacuum = INCREMENTAL`; then WAL; then migrations |
| Starts with `SQLite format 3\0` | Plaintext: convert it (below) |
| Anything else | Encrypted: look up the key; key; `cipher_compatibility = 4`; read `sqlite_schema`; then WAL, `secure_delete`, `foreign_keys`, `migrate`, `repair` as today |

Keying uses `sqlite3_key_v2` with the `x'<64 hex>'` form, so the key
never appears in SQL. The raw form skips PBKDF2, so opening stays fast.
`auto_vacuum` must come before WAL: set after it, it silently does nothing.

**No key in error messages.** With `bundled`, rusqlite turns a failed
prepare into `SqlInputError`, whose text is "{msg} in {sql} at offset", and
`lib.rs` shows `({error})` in the window. A probe showed a malformed key
pragma's error text containing the key. So no SQL holds the key: the
conversion binds it as a parameter (`ATTACH ?1 AS encrypted KEY ?2`), and
every error from the key and conversion steps is mapped to a fixed
`StoreError` (`KeychainLocked`, `KeyMissing`, `CannotUnlock`,
`ConversionFailed`) that carries no SQLite text.

### Settings

- **Cipher defaults** (SQLCipher 4), unchanged: AES-256-CBC with a random
  IV per page, HMAC-SHA512 on every page, 4096-byte pages.
  `cipher_compatibility = 4` keeps them if a later SQLCipher changes its
  defaults.
- **WAL and journals** are encrypted with the same key.
- **`secure_delete`** stays on: freed pages are zeroed, then encrypted.
- **FTS5** works unchanged; its shadow tables are pages like any other.
- **`temp_store = MEMORY`**, set explicitly. SQLCipher encrypts the main
  file, journals and WAL, but "other transient files are not encrypted".
  This moves every sort and temporary index into RAM, including future
  migrations that build indexes on large tables. PERFORMANCE.md gets a line
  on it, and such migrations are measured.
- **`cipher_memory_security`** stays off (the default), and
  **`cipher_plaintext_header_size`** is unused (it's for iOS).

### Forgetting without VACUUM

With temporary storage in memory, VACUUM builds its copy in RAM: 296 MB for
a 246 MB file in the probe, against a budget of 150 MB. So encrypted
databases use `auto_vacuum = INCREMENTAL`, and `Store::scrub` runs `PRAGMA
incremental_vacuum` instead of VACUUM, keeping the FTS5 `optimize` and the
WAL checkpoint. It frees whole pages without a copy (10 MB peak in the
probe). Partly used pages aren't packed, but their deleted cells are zeroed
by `secure_delete`. `scrub` chooses by the database's `auto_vacuum` mode,
not by whether it has a key. Mechanics in the appendix.

## Converting an existing database

Runs on the background thread, before capture or the journal start.

1. **Check space:** at least twice the size of `lax.sqlite` and its
   `-wal`. If not, keep the plaintext database, warn, retry next start.
2. **Get the key:** look it up, or make one by the rule above and read it
   back. Never write a file whose key couldn't be stored.
3. **Close the WAL:** `wal_checkpoint(TRUNCATE)` and `journal_mode =
   DELETE` on the plaintext file, which removes its `-wal` and `-shm`.
4. **Export** into `lax.sqlite.encrypting` (created `0600`):
   `ATTACH ?1 AS encrypted KEY ?2` with both bound,
   `PRAGMA encrypted.auto_vacuum = INCREMENTAL`,
   `SELECT sqlcipher_export('encrypted')`, then `PRAGMA
   encrypted.user_version` set by hand (the export doesn't copy it), and
   `DETACH`.
5. **Verify** with the key: `integrity_check`, FTS5 `integrity-check` on
   the three indexes, equal row counts in every table, equal
   `user_version`.
6. **Swap:** close the plaintext connection, `fsync` the new file, `rename`
   it over `lax.sqlite`, `fsync` the folder, open as usual.

**Crash safety.** Before the rename, `lax.sqlite` is the untouched
plaintext database; at the next start, leftover `lax.sqlite.encrypting` and
`lax.sqlite.encrypting-journal` are deleted and the conversion runs again
with the stored key. After the rename, `lax.sqlite` is the verified
encrypted file. There is no moment with neither.

**The old plaintext** becomes free space. Lax doesn't overwrite it first:
on APFS and SSDs that doesn't reliably reach the old blocks. FileVault,
where on, covers them. Local snapshots and Time Machine backups from before
the conversion still hold it until they're gone. The one-time notice says
so.

**Migrations and `repair()`.** Encryption is not a schema change. No
migration file changes, so `migration_files_are_never_changed` is
untouched, and `user_version` is kept. `migrate` and `repair` run on the
encrypted connection as before. Tests that open the file with a bare
`Connection::open` (such as `newer_schema_is_refused`) need a keyed helper.

**Going back.** An older Lax can't open the converted file. The CHANGELOG
says the conversion is one-way.

## When the key is missing or wrong

Lax never creates a new database next to one it can't open, never captures
without one, and never makes a new key while an encrypted file exists.

| Situation | What the user sees and can do |
|---|---|
| Keychain locked, prompt denied or cancelled | "Lax can't unlock its database." **Try again**. No "Start over": the key is probably fine |
| Key not found, encrypted file exists | "Lax can't find its key." **Try again**, or **Start over** |
| Login password reset: macOS makes a new login keychain and keeps the old one as `login_renamed_1` (Apple's documented behaviour, not verified here) | Same as above, plus: "If you reset your login password, open Keychain Access, unlock the old keychain with the old password, and try again." Lax makes no new key in the meantime |
| Key found, but the file won't open (wrong key, or damaged; they look the same) | "Lax can't open its database." **Start over** |
| A new Mac | Depends on the spike and on open question 4 |

**Start over** is explicit and confirmed, like every forget. It moves the
file aside as `lax.sqlite.unreadable-<date>` (still encrypted, with any
`-wal`), renames an existing key item to
`database-key-unreadable-<date>` without touching its data, then makes a
new key and database. A separate, confirmed "Delete the old file" removes
the old file and its old key together. If the old key turns up, the old
file can still be opened.

### No recovery key

- Lax is a memory, not a vault. Raw captures expire within 30 to 90 days.
  Losing the database loses the journal and entity data, but the Markdown
  pages survive.
- The key is already visible in Keychain Access. The user guide says where,
  and that a copy in a password manager works on another Mac.
- A recovery key is one more secret that can leak, and one more screen.

The cost: lose the keychain without a copy of the key, and the database is
gone. The user guide says so plainly.

## Consequences

### Dependencies

- No new crates. `bundled-sqlcipher` is a feature of rusqlite 0.40.2 and
  libsqlite3-sys 0.38.2 (both MIT). `security-framework` 3.7.0 (MIT OR
  Apache-2.0) and `zeroize` 1.9.1 are in `Cargo.lock` already.
- SQLCipher 4.14.0 community is under a BSD-style 3-clause licence,
  compatible with AGPL-3.0; its notice must ship with binaries. cargo-deny
  sees only the crate's licence, so `deny.toml` doesn't change.
- **SQLite gets older, for everyone, from PR 2 on**, even while databases
  are still plaintext: SQLCipher 4.14.0 is built on SQLite 3.51.3, plain
  `bundled` has 3.53.2. Lax follows Zetetic's pace for SQLite fixes.

### Speed, size and memory

On an M1 Pro, the binary grows by about 50 KB, bulk writes cost the same,
and a small commit costs about 0.04 ms more. Lax commits every few seconds
at most, well within the 2% CPU budget. Memory is the risk, handled by the
incremental vacuum and by measuring large migrations. Numbers in the
appendix; `npm run perf` is checked before a release.

### CI

Tests and clippy already run on `macos-latest`, which gets the CommonCrypto
build. No test touches the real Keychain, which could block on a prompt;
`keychain.rs` gets one `#[ignore]` test for a real Mac.

### Tests

- **The disk scans stay meaningful.**
  `forgotten_text_is_gone_from_the_files_on_disk` and
  `expired_text_is_gone_from_the_files_on_disk` read the files for a marker.
  On ciphertext they would always pass. They keep running on an unkeyed
  store with the same layout (`auto_vacuum = INCREMENTAL`, the same
  `scrub`), so they still prove deletion. The CLAIMS row on forgetting
  points to them as before.
- **Beside them, the encryption test:** with a key, a capture, an episode,
  a page version and an outbox entry hold a synthetic marker ("Anna
  Example"); the marker is in none of `lax.sqlite`, `-wal` and `-shm`.
- Keyed open works; no key and a wrong key both fail with fixed errors.
- A new key is never made while an encrypted file exists, and "Start over"
  keeps the old item and file.
- Conversion: row counts, `user_version`, search and FTS5 `secure-delete`
  survive; no plaintext `-wal` or `-shm` is left; stopping after the
  export leaves a working plaintext file and the next start converts
  again; too little space changes nothing.
- No `StoreError` text, log entry or window message contains the key.
- `cipher_provider` is `commoncrypto` on a keyed temporary database, and a
  forget on a large database stays under a set memory ceiling.

### Docs

- **CLAIMS.md:** a new row, "The database is encrypted, and its text is
  never on disk in plaintext"; the forget row says incremental vacuum.
- **USER-GUIDE.md:** what is and isn't encrypted, where the key is, how to
  keep a copy, what "Start over" does, the login-password-reset case, and
  how to open the database with the `sqlcipher` tool.
- **PERFORMANCE.md:** temporary data in RAM. **ARCHITECTURE.md:** a line
  pointing here.

### What we give up

- `sqlite3 lax.sqlite` no longer works; inspecting needs SQLCipher and the
  key.
- A lost keychain without a copy of the key means a lost database.
- Possibly Keychain prompts after rebuilds, until the spike says otherwise.
- A slower source of SQLite fixes.

## Rollout

Each PR green on its own, labelled `security-sensitive`, updating CLAIMS.md
as it lands.

0. **GUARDRAILS §6, in its own PR first**, as the guardrails' preamble
   requires. "Lax relies on FileVault … Database encryption (SQLCipher) is
   planned" becomes: the database is encrypted with a random key in the
   login keychain, never synced; the pages in `~/Lax` are plain Markdown,
   protected by FileVault and permissions; Lax warns if FileVault is off.
   "Inspect it without Lax" gains: the database needs SQLCipher and the key
   from Keychain Access. Merged once this ADR is accepted. Alongside it,
   the **spike** (not merged), whose results go into this ADR.
1. **`keychain.rs`** with `KeySource` and tests. No change in behaviour.
2. **Build with `bundled-sqlcipher`**, databases still plaintext (SQLite
   3.53.2 → 3.51.3 for everyone). Logging compiled out. `temp_store = FILE`
   while plaintext, to keep today's VACUUM memory. The `cipher_provider`
   test.
3. **Encrypted new databases:** opening off the main thread with
   "Unlocking…", the plaintext-header branch, the failure screens and
   "Start over", incremental vacuum, the tests above.
4. **Conversion** with "Encrypting…" and the one-time notice. The
   maintainer's database first, with a copy kept by hand.
5. **Docs:** USER-GUIDE, PERFORMANCE, ARCHITECTURE, CHANGELOG ("one-way;
   older backups still hold the old file").

## Alternatives considered

- **SQLCipher with vendored OpenSSL:** same format, but builds and ships
  OpenSSL (`openssl-src` 400.0.2, OpenSSL 4.0.3): a second crypto library
  to keep patched, for nothing CommonCrypto doesn't do.
- **SQLite's own encryption (SEE):** a paid licence with source only for
  licensees. Can't ship under AGPL-3.0.
- **SQLite3 Multiple Ciphers** (MIT): can read SQLCipher files, but
  libsqlite3-sys has no feature for it, so Lax would maintain its own
  SQLite build.
- **Encrypting columns in the application:** FTS5 needs plaintext tokens,
  so search stops working or the index keeps the plaintext. Vectors,
  sorting and `LIKE` break, app names and times stay readable, and it means
  new crypto code of our own.
- **FileVault only, documented:** the simplest, but it leaves the gaps
  #114 asks to close.
- **An encrypted disk image** (`hdiutil`) for the data folder: mounted at
  login with a password from the Keychain, visible in Finder, and readable
  by every program of the user while mounted, which is always.

## Appendix: the probe

A small program with rusqlite 0.40.2, built like Lax's release profile on
an M1 Pro, once with `bundled` and once with `bundled-sqlcipher`. One run
each; rough numbers, not a benchmark.

**Build.** With no `OPENSSL_*` set, libsqlite3-sys 0.38.2 compiles with
`-DSQLCIPHER_CRYPTO_CC`, `SQLITE_HAS_CODEC` and `SQLITE_TEMP_STORE=2`, and
links Security and CoreFoundation; `otool -L` shows only those and
libSystem. `PRAGMA cipher_provider` returns `commoncrypto` on a keyed
connection and nothing on an unkeyed one. The bundled SQLCipher logs at
WARN to the unified log by default (`os_log`, `%{public}s`), unless built
with `SQLCIPHER_OMIT_LOG`.

**Behaviour.**

- `sqlcipher_export` kept FTS5 search, the `secure-delete` option, FTS5
  `integrity-check`, triggers and `STRICT`; `user_version` came back 0;
  `journal_mode` came back `delete`; `auto_vacuum` set on the attached file
  before the export stuck.
- `ATTACH ?1 AS encrypted KEY ?2` with the path and `x'…'` text bound
  works, and the result opens with `sqlite3_key_v2`.
- A marker string was in none of the encrypted `.db`, `-wal` and `-shm`.
  The encrypted file doesn't start with `SQLite format 3`.
- No key and a wrong key both give "file is not a database".
- A key pragma with a syntax error gives an error whose text contains the
  key.

| | Plain SQLite | SQLCipher |
|---|---|---|
| SQLite version | 3.53.2 | 3.51.3 (SQLCipher 4.14.0) |
| Stripped binary | 2,065,856 bytes | 2,117,328 bytes |
| 20,000 inserts in one transaction | 233 ms | 235 ms |
| 500 single-row transactions | 45 ms | 66 ms |
| VACUUM, 246 MB file, peak memory | 9 MB (temporary file) | 296 MB; 13 MB with `temp_store = FILE` |
| Incremental vacuum after deleting half of it | – | 246 → 111 MB in 1.1 s, 10 MB peak |

**Incremental vacuum.** `auto_vacuum = INCREMENTAL` must be set before
`journal_mode = WAL` on a new file; afterwards it silently stays off.
`PRAGMA incremental_vacuum` frees one page per step, so it must be stepped
until it returns no more rows: through `execute_batch` it freed a single
page.

## Sources

- libsqlite3-sys 0.38.2: `build.rs`, `Cargo.toml` and the bundled
  `sqlcipher/` sources. rusqlite 0.40.2: `Cargo.toml` (`bundled` enables
  `modern_sqlite`) and `src/error.rs` (`SqlInputError`'s text).
- `cargo info` for rusqlite and openssl-src; security-framework 3.7.0's
  `Cargo.toml`; `Cargo.lock` for zeroize.
- [SQLCipher API](https://www.zetetic.net/sqlcipher/sqlcipher-api/) and
  [design](https://www.zetetic.net/sqlcipher/design/) pages.
- Apple: TN3137 "On Mac keychain APIs and implementations"; the
  `kSecAttrAccessible` and `kSecAttrAccessible…ThisDeviceOnly` pages.
- Lax: `src-tauri/src/lib.rs` (setup order, `({error})`),
  `scripts/build-app.sh` (installs to `~/Applications`),
  `tauri.conf.json` (`"signingIdentity": "-"`).
