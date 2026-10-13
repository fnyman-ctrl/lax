# 0010. Encrypt the database with SQLCipher, keyed from the Keychain

- **Status:** Proposed
- **Date:** 2026-10-13

## Context

Issue #114. GUARDRAILS §6 says "Database encryption (SQLCipher) is planned",
and [ADR 0006](0006-sqlite-with-rusqlite.md) chose rusqlite partly so that
this would be a feature switch. This ADR says how, and what it costs.

### What the database holds

`lax.sqlite` in the data folder
(`~/Library/Application Support/io.github.fnyman-ctrl.lax`) holds nearly
everything Lax knows:

- **Raw captures:** window text, titles and sites (`captures`, with the
  `captures_fts` index). Secrets are masked before they are stored, but
  names, mail and chat text are kept.
- **The journal:** episodes and their index (`episodes`, `episodes_fts`).
- **The memory:** entities, aliases, mentions, the "don't learn" list, page
  metadata and every version of the blocks Lax wrote (`page_versions`),
  facts and rejected facts.
- **Episode vectors** (`episode_vectors`), the **outbox** (`outbox`,
  `outbox_fts`) and **settings**, including the pause state.

The memory's pages themselves are Markdown files in `~/Lax` (ADR 0005).
They are **not** in the database, and this ADR doesn't change them.

### What protects it today

- FileVault, if it's on. Lax warns when it's off (`filevault.rs`).
- The data folder is `0700`, the database file `0600`.
- `secure_delete`, FTS5 `secure-delete` on all three indexes, and VACUUM
  plus a WAL checkpoint in `Store::forget`, so deleted text doesn't linger.
- Expiry: raw captures go 30 days after capture once summarized, and at
  the maximum age (default 90 days) in any case (`Store::expire`).

### Threat model

| Threat | FileVault only | With an encrypted database |
|---|---|---|
| Mac stolen while off, FileVault on | Protected | No extra gain |
| Mac stolen while off, FileVault off | **Not protected** | Protected, as far as the login password is strong: it locks the keychain |
| Another program running as the same user reads the file (a bad npm package, an app with wide file access, a sync or backup agent) | **Not protected.** `0600` only keeps other users out | Protected: the file is ciphertext, and the key is behind the Keychain's access list |
| Copies of the folder: Time Machine to a disk without encryption, cloud backup tools, a copy on a USB stick | **Not protected** | Protected: the key isn't in the copy, or only in a backed-up login keychain, locked by the login password |
| Freed disk blocks read later | Protected while FileVault is on | Protected from the switch on. Plaintext written before the switch stays in old blocks, snapshots and backups until they are gone |
| Malware running as the user while Lax runs | Not protected | **Not protected.** It can ask the Keychain for the key (the user sees a prompt, and may allow it), use Accessibility to read Lax's window, read `~/Lax`, or just watch the screen itself |
| Someone at an unlocked, logged-in Mac | Not protected | Not protected: Lax is running and the login keychain is unlocked |
| An administrator or root | Not protected | Not protected |

So encryption closes real gaps: other programs of the same user, copies of
the file, and Macs without FileVault. It does not make Lax safe on a Mac
that is already compromised, and it doesn't cover the Markdown pages. As far
as we know (not verified), macOS doesn't ask before another app reads a
non-sandboxed app's `Application Support` folder.

## Decision

Lax encrypts its database with **SQLCipher**, through rusqlite's
`bundled-sqlcipher` feature, using **Apple's CommonCrypto** as the crypto
provider. The key is a random 256-bit key, made by Lax and kept in the
user's **login keychain**, never synced and never written anywhere else.
Existing databases are converted once, at startup. If the key is missing,
Lax fails closed: it does not capture, and it never starts a new database
on its own.

### Build

- `rusqlite = { version = "0.40", features = ["bundled-sqlcipher"] }`
  instead of `["bundled"]`. Not `bundled-sqlcipher-vendored-openssl`.
- On a Mac, with no `OPENSSL_*` variables set, libsqlite3-sys 0.38.2 builds
  SQLCipher with `-DSQLCIPHER_CRYPTO_CC` and links the Security and
  CoreFoundation frameworks. No OpenSSL is built or linked. A probe build
  confirmed it: `PRAGMA cipher_provider` returns `commoncrypto`, and the
  binary links only Security, CoreFoundation and libSystem.
- The build switches to OpenSSL if `OPENSSL_DIR`, or both
  `OPENSSL_LIB_DIR` and `OPENSSL_INCLUDE_DIR`, are set (a Homebrew setup
  might). A test checks that `PRAGMA cipher_provider` is `commoncrypto`, so
  such a build fails its tests instead of linking a stray `libcrypto`.
- Build flags as today (FTS5 and the rest), plus `SQLITE_HAS_CODEC` and
  `SQLITE_TEMP_STORE=2`: temporary data in memory unless a pragma says
  otherwise.

### The key

- **Made by Lax:** 32 bytes from `SecRandomCopyBytes`, the first time a
  database is created or converted. Never derived from a password, so the
  user has nothing to remember or type.
- **Stored as text:** 64 lowercase hex characters, as a generic password
  with service `io.github.fnyman-ctrl.lax`, account `database-key` and the
  label "Lax database key". Hex rather than raw bytes so the user can see
  and copy it in Keychain Access, which is the recovery path (below).
- **Login keychain, for now.** Apple's data protection keychain is the
  better store: it honours `kSecAttrAccessible`, and items marked
  `ThisDeviceOnly` never leave the Mac. But it needs keychain access group
  entitlements authorized by a provisioning profile (TN3137), and Lax 0.x
  is ad-hoc signed (`"signingIdentity": "-"`). In the login (file-based)
  keychain, `kSecAttrAccessible` has no effect: Apple documents that it only
  applies with `kSecUseDataProtectionKeychain` or `kSecAttrSynchronizable`.
  Access is decided by the item's access list instead, which trusts the app
  that created it. Other apps get a prompt.
- **Never synced.** iCloud Keychain needs the data protection keychain, so
  the login keychain can't sync. Lax still sets
  `kSecAttrSynchronizable = false`, so the code stays right when it moves.
- **Later, once Lax is signed** (the roadmap's `.dmg`): move the key to the
  data protection keychain with
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`. Lax reads the key
  when it opens the database, which can happen while the screen is locked
  (a restart after a crash or an update); `WhenUnlocked` would keep it
  closed until the user unlocks. `ThisDeviceOnly` items never reach a new
  Mac from a backup. A small PR, not a new ADR.
- **In code:** one module, `src-tauri/src/keychain.rs`, on
  `security-framework` (already in the tree via native-tls). The store asks
  a `KeySource`, so tests use a fixed key and never touch the real
  Keychain. API keys can use the same module later.
- The key lives in a `Zeroizing` buffer (`zeroize` is already in the tree),
  wiped once SQLite has it, and never goes into an error, a log line or
  `Debug` output.

### Opening the database

The order in `Store::open` becomes:

1. `PRAGMA cipher_log_level = NONE`. SQLCipher logs warnings to the macOS
   unified log by default, outside `src-tauri/src/logging.rs`. Lax turns
   that off.
2. `PRAGMA key = "x'<64 hex>'"` before anything reads the database, with
   `execute_batch` (not the statement cache). A raw key in this form
   skips PBKDF2: there is no password to stretch, and opening stays fast.
3. `PRAGMA cipher_compatibility = 4`, so a future SQLCipher with new
   defaults still opens today's files the same way.
4. A read of `sqlite_schema`. A missing or wrong key fails here with "file
   is not a database" (confirmed in the probe).
5. Then as today: `journal_mode = WAL`, `secure_delete = ON`,
   `foreign_keys = ON`, `migrate`, `repair`. Plus `temp_store = MEMORY`,
   set explicitly.

Settings, and why:

- **Cipher defaults** (SQLCipher 4), unchanged: AES-256-CBC with a random
  IV per page, HMAC-SHA512 on every page, 4096-byte pages.
- **WAL:** pages in the WAL are encrypted with the same key. In the probe,
  a known string was in neither `-wal` nor `-shm`.
- **`secure_delete`** stays on. Freed pages are zeroed and then encrypted,
  so deleted content stays gone even if the key leaks later.
- **FTS5** works unchanged: its shadow tables are pages like any other. In
  the probe, search, `secure-delete` and `integrity-check` survived
  conversion.
- **Temporary files:** SQLCipher encrypts the main file, journals and WAL,
  but "other transient files are not encrypted" (design page). With
  `temp_store = MEMORY` there are none.
- **`cipher_memory_security`** stays off (the default). SQLCipher always
  wipes its own crypto buffers; this wipes every allocation, costs speed,
  and only helps against reading Lax's memory, which this ADR doesn't claim
  to stop.
- **`cipher_plaintext_header_size`** is not used (it's for iOS shared
  containers), so the file has no readable header.

### VACUUM and memory

With temporary storage in memory, VACUUM builds its copy of the database in
RAM. In the probe, VACUUM of a 246 MB encrypted file peaked at 296 MB of
memory, against 9 MB for today's build, which by SQLite's default puts that
copy in a temporary file (unencrypted, for the plain build). A forget would
blow the 150 MB budget in PERFORMANCE.md.

So, for encrypted databases:

- The conversion turns on `auto_vacuum = INCREMENTAL` in the new file
  (`sqlcipher_export` doesn't carry `auto_vacuum` over, so it is set on the
  new file before the export; the probe confirmed the setting sticks). A
  new database sets it first of all: in the probe, setting it after
  `journal_mode = WAL` silently did nothing.
- `Store::scrub` replaces `VACUUM` with `PRAGMA incremental_vacuum`, which
  hands free pages back to the file system without a copy.
  `secure_delete` has already zeroed them, the FTS5 `optimize` and the WAL
  checkpoint stay. In the probe, after deleting half of a 246 MB file, it
  shrank the file to 111 MB in 1.1 s with a 10 MB peak.
- The pragma frees one page per step, so it must be stepped until it
  returns no more rows. Through `execute_batch` it freed a single page.
- It only frees whole pages. Partly used pages aren't packed, so the file
  can stay larger than after a VACUUM. The deleted cells in them are zeroed
  by `secure_delete` and encrypted, so nothing readable is left.

Setting `temp_store = FILE` just for VACUUM also keeps memory low (13 MB in
the probe), and VACUUM's own copy is attached with the main database's key.
But sorts that spill to disk while indexes are rebuilt would be plaintext
temporary files. Lax doesn't take that risk.

## Converting an existing database

Runs in `Store::open`, before capture, the journal or anything else starts
(that order is already fixed in `lib.rs`). A plaintext file starts with
`SQLite format 3\0`; an encrypted one doesn't (checked in the probe).

1. **Check space.** Free space on the volume must be at least twice the size
   of `lax.sqlite` plus its `-wal`. If not, keep the plaintext database,
   warn in the window, and try again at the next start.
2. **Get the key.** Read it from the Keychain, or make one and store it.
   Read it back before going on: never write a file whose key couldn't be
   stored.
3. **Close the WAL.** `PRAGMA wal_checkpoint(TRUNCATE)` and
   `journal_mode = DELETE` on the plaintext file, which removes its `-wal`
   and `-shm`. A plaintext `-wal` left next to the new file must never be
   possible.
4. **Export.** Create `lax.sqlite.encrypting` with mode `0600`, then:

   ```sql
   ATTACH DATABASE '<path>/lax.sqlite.encrypting' AS encrypted KEY "x'…'";
   PRAGMA encrypted.auto_vacuum = INCREMENTAL;
   SELECT sqlcipher_export('encrypted');
   PRAGMA encrypted.user_version = <the plaintext user_version>;
   DETACH DATABASE encrypted;
   ```

   `sqlcipher_export` copies tables, indexes, triggers and virtual tables,
   but not `user_version` (SQLCipher docs, and confirmed in the probe), so
   it is copied by hand.
5. **Verify** the new file, opened with the key: `PRAGMA integrity_check`,
   FTS5 `integrity-check` on the three indexes, the same row count in every
   table, the same `user_version`.
6. **Swap.** `fsync` the new file, `rename` it over `lax.sqlite` (atomic on
   the same volume), `fsync` the folder. Then open it as usual, which turns
   WAL back on.

**Crash safety.** Before the rename, `lax.sqlite` is the untouched
plaintext database. A leftover `lax.sqlite.encrypting` is deleted at the
next start, and the conversion runs again with the key already in the
Keychain. After the rename, `lax.sqlite` is the verified encrypted file.
There is no moment with neither.

**The old plaintext.** After the rename, its blocks are free space. Lax
doesn't overwrite the old file first: on APFS and SSDs, overwriting a file
doesn't reliably reach the blocks it used, so it would be a promise we
can't keep. FileVault, where on, covers freed blocks. Local snapshots and
Time Machine backups made before the conversion still hold the plaintext
database until they expire or are deleted. The one-time notice says so.

**Migrations and `repair()`.** Encryption is not a schema change. No
migration file is added or changed, so `migration_files_are_never_changed`
is untouched, and `user_version` is kept. `migrate` and `repair` run on the
encrypted connection as before. Tests that open the file with a bare
`Connection::open` (such as `newer_schema_is_refused`) need a helper that
sets the key. `open_in_memory` stays: it never touches the disk.

**Going back.** An older Lax can't open the converted file ("file is not a
database"). The CHANGELOG says the conversion is one-way.

## When the key is missing or wrong

Today, if `Store::open` fails, Lax still starts, keeps capture paused and
shows why (`viewer::StoreUnavailable` in `lib.rs`). That is already fail
closed, and this ADR keeps it: **Lax never creates a new database next to
one it can't open, and never captures without one.**

| Situation | What happens | What the user sees and can do |
|---|---|---|
| Keychain locked, or the user dismissed the prompt | Open fails, capture stays paused | "Lax can't unlock its database." **Try again** (after unlocking or allowing). Lax also retries when the Mac is unlocked |
| Key gone (Keychain reset, file copied to another Mac without the keychain) | Open fails with "file is not a database" | Same message, plus **Start over** |
| Wrong key in the Keychain | Same as above. A wrong key and a damaged file look the same | Same |
| New Mac from Migration Assistant or a full Time Machine restore | Works if the login keychain came along | Not verified on a real Mac (see open questions) |

**Start over** is explicit and confirmed, like every forget. It moves the
unreadable file aside as `lax.sqlite.unreadable-<date>` (still encrypted, so
keeping it costs only space), makes a new key and a new database, and offers
a separate "Delete the old file" button. If the key turns up again, the old
file can still be opened. Lax never deletes it silently.

The Keychain prompt itself: with an ad-hoc signature, the access list
trusts this exact build. After an update built from source, macOS will most
likely ask once whether the new Lax may use "Lax database key". Until the
user answers, the database stays closed. This needs checking on a real Mac
before the rollout's third PR.

### No recovery key in 0.x

Lax doesn't show a recovery key or ask the user to write one down:

- Lax is a memory, not a vault. Raw captures expire within 30 to 90 days.
  Losing the database loses the journal and what Lax knows about entities,
  but the Markdown pages, the memory people actually read, survive.
- The key is already visible: Keychain Access shows it after the login
  password. The user guide says where, and that a copy in a password
  manager is a backup that works on another Mac.
- A recovery key is one more secret that can leak, and one more screen.

The cost: if the login keychain is lost and the user never copied the key,
the database is gone. The user guide says so plainly.

## Consequences

### Dependencies and licences

- No new crates. `bundled-sqlcipher` is a feature of the crates we have
  (rusqlite 0.40.2, libsqlite3-sys 0.38.2, both MIT). `security-framework`
  3.7.0 (MIT OR Apache-2.0) and `zeroize` 1.9.1 are already in `Cargo.lock`
  and become direct dependencies.
- SQLCipher 4.14.0 community edition is under Zetetic's BSD-style licence
  (three clauses), compatible with AGPL-3.0. It needs its notice kept in
  binary distributions. cargo-deny only sees the crate's MIT licence, so
  `deny.toml` doesn't change; the notice goes into the third-party notices
  with the signed `.dmg`.
- **SQLite gets older.** SQLCipher 4.14.0 is built on SQLite 3.51.3; plain
  `bundled` in the same libsqlite3-sys has 3.53.2. Lax follows Zetetic's
  release pace for SQLite security fixes from now on. Dependabot still
  raises rusqlite updates.
- The vendored OpenSSL option would add `openssl-src` (400.0.2, which
  builds OpenSSL 4.0.3; MIT/Apache-2.0): a second crypto library to patch,
  for nothing CommonCrypto doesn't do. Not used.

### Size, speed and memory

Measured once on an M1 Pro with a small probe built like Lax's release
profile. Rough numbers, not a benchmark:

| | Plain SQLite | SQLCipher |
|---|---|---|
| Stripped binary | 2,065,856 bytes | 2,117,328 bytes (+50 KB) |
| 20,000 inserts in one transaction | 233 ms | 235 ms |
| 500 single-row transactions | 45 ms | 66 ms |
| Delete half of 20,500 short rows, then VACUUM | 1.28 s | 1.32 s |
| VACUUM of a 246 MB file, peak memory | 9 MB | 296 MB (13 MB with `temp_store = FILE`) |
| Incremental vacuum after deleting half of it, peak memory | – | 10 MB |

Lax writes a capture at most every few seconds, so 0.04 ms more per commit
is far below the 2% CPU budget. Memory is the real risk, and the
incremental vacuum above is the answer. `npm run perf` is checked after the
switch, before a release.

### CI

Tests and clippy already run on `macos-latest`, which gets the CommonCrypto
build; `cargo deny` and its licence list don't change. No test touches the
real Keychain, which could block on a prompt in CI; `keychain.rs` gets one
`#[ignore]` test for a real Mac.

### Tests to add

- A keyed database opens with its key, and not without it or with another
  (both give "file is not a database").
- **Plaintext never on disk:** a capture, an episode, a page version and an
  outbox entry hold a synthetic string ("Anna Example"); read `lax.sqlite`,
  `-wal` and `-shm` as bytes, and it is in none of them. Also after a
  forget and after expiry.
- Conversion: every table keeps its row count, `user_version` is the same,
  search works, FTS5 `secure-delete` is still on, no plaintext `-wal` or
  `-shm` is left. Stopping after the export leaves the plaintext file
  working, and the next start converts again. Too little space changes
  nothing.
- A missing key never leads to a new database or to capture starting.
- `cipher_provider` is `commoncrypto`, `temp_store` is memory, and a forget
  on a large database stays under a set memory ceiling.
- `StoreError` and log entries can't hold the key.

### Docs

- **GUARDRAILS §6:** "Lax relies on FileVault for disk encryption and warns
  if FileVault is off. Database encryption (SQLCipher) is planned." becomes:
  "The database is encrypted (SQLCipher) with a random key kept in your
  login keychain on this Mac, never synced. The memory's pages in `~/Lax`
  are plain Markdown, protected by FileVault and their folder's permissions.
  Lax warns if FileVault is off." The "open formats" bullet gains: "To open
  the database with other tools you need SQLCipher and the key from
  Keychain Access; the user guide says how."
- **CLAIMS.md:** a new row, "The database is encrypted, and its text is
  never on disk in plaintext", with the tests above. The forget row says
  incremental vacuum instead of VACUUM.
- **USER-GUIDE.md:** what is and isn't encrypted, where the key is, how to
  keep a copy, what "Start over" does, and how to open the database with
  the `sqlcipher` tool. ARCHITECTURE.md gets a line pointing here.

### What we give up

- `sqlite3 lax.sqlite` no longer works. Inspecting the database needs
  SQLCipher and the key. That is the point, but it is a step away from "open
  formats you can inspect without Lax", and the docs have to carry it.
- A lost login keychain means a lost database.
- Keychain prompts after rebuilds, until Lax is signed.
- A second, slower source of SQLite fixes.

## Rollout

Small PRs, each green on its own, labelled `security-sensitive`, and
updating CLAIMS.md as it lands:

1. **`keychain.rs`** with `KeySource` and tests. No change in behaviour.
2. **Build with `bundled-sqlcipher`**, database still plaintext (SQLCipher
   without a key is plain SQLite; the probe made its plaintext file that
   way). `temp_store = FILE` while plaintext, to keep today's VACUUM
   memory. The `cipher_provider` and logging tests.
3. **Encrypted new databases.** Keyed open, the fail-closed screen with
   "Try again" and "Start over", incremental vacuum, the plaintext-on-disk
   test. Tried on a real Mac: the Keychain prompt after a rebuild.
4. **Conversion of existing databases**, with the one-time notice. The
   maintainer's own database first, with a copy kept by hand.
5. **Docs:** GUARDRAILS §6, USER-GUIDE, CHANGELOG ("one-way; older backups
   still hold the old file").

No setting to turn encryption off: a second code path for the most
sensitive data isn't worth it.

## Alternatives considered

- **SQLCipher with vendored OpenSSL** (`bundled-sqlcipher-vendored-openssl`):
  the same database format, but builds and ships OpenSSL. Useful on Linux
  or Windows; on a Mac it only adds a crypto library to keep patched.
- **SQLite's own encryption (SEE):** a paid licence with source only for
  licensees. Lax can't ship it under AGPL-3.0.
- **SQLite3 Multiple Ciphers** (MIT): open source and can read SQLCipher
  files, but libsqlite3-sys has no feature for it, so Lax would maintain its
  own build of SQLite. Worth another look only if SQLCipher stalls.
- **Encrypting columns in the application** (for example AES-GCM from
  RustCrypto): FTS5 needs plaintext tokens, so search over captures,
  episodes and the outbox stops working, or the index keeps the plaintext.
  Vectors, sorting and `LIKE` break too, app names and times stay readable,
  and it means new crypto crates and code of our own.
- **FileVault only, documented:** the simplest, but it leaves both gaps
  this ADR is about, which #114 asks to close.
- **An encrypted disk image for the data folder** (`hdiutil`): no change to
  SQLite, but it must be mounted at login with a password from the
  Keychain, shows up in Finder, and while mounted, which is always, every
  program of the user can read it.

## Open questions for the maintainer

1. **On for everyone, with no off switch?** Recommended: yes. Users
   without FileVault gain the most.
2. **No recovery key, with the key visible in Keychain Access?**
   Recommended: yes, for 0.x.
3. **Login keychain now, data protection keychain once Lax is signed?**
   Recommended: yes.
4. **A Keychain prompt after each rebuild until then?** Recommended:
   acceptable if onboarding and the CHANGELOG explain it. Check on a real
   Mac in PR 3, with whether Migration Assistant brings the key along.
5. **Incremental vacuum instead of VACUUM?** Recommended: yes;
   `temp_store = FILE` during VACUUM risks plaintext temporary files.
6. **Keep `cipher_memory_security` off?** Recommended: yes.
7. **"Start over" moves the unreadable file aside?** Recommended: yes,
   with a separate, confirmed delete.
8. **Convert automatically at startup, after a one-time notice?**
   Recommended: yes. Asking first would leave most databases plaintext.
9. **A plaintext export?** Recommended: not now. The user guide explains
   the `sqlcipher` tool; an export that writes plaintext needs its own
   decision.

## Sources

- libsqlite3-sys 0.38.2: `build.rs`, `Cargo.toml` and the bundled
  `sqlcipher/` sources (versions, defaults, CommonCrypto, logging).
- `cargo info` for rusqlite and openssl-src; `Cargo.toml` of
  security-framework 3.7.0; `Cargo.lock` for zeroize.
- [SQLCipher API](https://www.zetetic.net/sqlcipher/sqlcipher-api/) and
  [design](https://www.zetetic.net/sqlcipher/design/) pages.
- Apple: TN3137 "On Mac keychain APIs and implementations", and the
  `kSecAttrAccessible` and `kSecAttrAccessible…ThisDeviceOnly` pages.
- The probe: rusqlite 0.40.2 with each feature, on an M1 Pro.
