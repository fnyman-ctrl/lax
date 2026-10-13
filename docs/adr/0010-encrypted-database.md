# 0010. Encrypt the database with SQLCipher, keyed from the Keychain

- **Status:** Accepted
- **Date:** 2026-10-13

## Summary

Lax's database would be encrypted with SQLCipher, using Apple's own crypto
library, with a random key that Lax keeps in the user's login keychain.
That protects the file from other programs that read files, from copies and
backups, and on Macs without FileVault. It doesn't protect against malware
aimed at Lax. Existing databases are converted once. If the key can't be
found, Lax stops instead of starting over, and it never throws a key away.

The memory pages are Markdown files in `~/Lax` today, outside the
database. [ADR 0011](0011-pages-in-the-database.md) (proposed) moves them
into this database, so they are encrypted with it.
[ADR 0012](0012-app-lock.md) (proposed) asks for Touch ID or the login
password before Lax's window shows anything. That lock guards the window.
It doesn't encrypt anything, and it doesn't change who can read the key.

The maintainer wants encryption before release 0.1, in the self-built app,
and has answered every open question that doesn't wait for the signed
build (see "Maintainer's answers"). The Keychain spike has been run (see
"Before deciding"). After every update, macOS asks once for the login
password before the new build may read its key, and a self-signed
certificate doesn't change that. The maintainer accepts that prompt and
encrypts now. Lax shows its own note right before it, so a prompt at any
other time stands out as a warning sign. The signed `.dmg` removes the
prompt later.

**Status.** Accepted by the maintainer on 2026-10-13.

## Maintainer's answers (2026-10-13)

The maintainer decided three things first, and later the same evening
answered the open questions. After the Keychain spike he answered question
5 (decision 4 below). The nine open questions below keep the reasoning as
it was written, with each answer added. This section says what was
decided.

1. **Encrypt now, before release 0.1**, in the app people build themselves
   (`npm run build:app`, ad-hoc signed). SQLCipher, with the key in the
   login keychain, as this ADR proposes. User presence on the key itself
   (`SecAccessControl` with `userPresence`, so reading the key needs Touch
   ID or the login password) comes only with the signed `.dmg`: it needs
   the data protection keychain, and that needs a real signing identity.
2. **The memory pages move into the encrypted database.** They leave
   `~/Lax` and are edited in Lax's Memory view. This reverses part of
   [ADR 0008](0008-learning-memory.md), so it has its own ADR,
   [0011](0011-pages-in-the-database.md).
3. **An app lock.** Lax's window asks for Touch ID or the login password
   when it opens and again after 5 minutes away. As ADR 0012 records,
   ordinary settings need only the unlocked window; forgets, exports and
   every change that loosens privacy ask every time. Capture and the
   writing assistant keep working in the background. This is [ADR 0012](0012-app-lock.md).
4. **Accept the prompt once per update, and encrypt now** (question 5,
   after the spike). Lax explains the prompt with its own note right
   before it, and waits for **Continue** before it reads the key. The note
   says what "Always Allow" and "Allow" each mean. See "After an update:
   one Keychain prompt".

What this changes here:

- **The timing is settled.** "Before deciding" said encryption would wait
  for the signed `.dmg` if the prompts are bad and a self-signed
  certificate doesn't help. The spike showed exactly that: a prompt with
  the login password after every rebuild, which a self-signed certificate
  doesn't remove. The case went back to the maintainer, and he decided to
  encrypt now, with Lax's note (decision 4).
- **Pages are in scope.** The context, the threat model, "No recovery key"
  and the tests below now count the pages as part of the database, from
  ADR 0011 on.
- **"Someone at an unlocked, logged-in Mac"** is partly addressed, by the
  app lock in ADR 0012, not by encryption. See the threat model.

The open questions:

| # | Question | Where it stands |
|---|---|---|
| 1 | On for everyone, no off switch | **Answered: yes.** No setting turns encryption off |
| 2 | No recovery key | **Answered: yes, for 0.x**, as recommended. Weighed with ADR 0011 in mind: a lost key also loses the pages, including the user's own notes. Page export (ADR 0011) is the user's way to keep a copy |
| 3 | Login keychain now, data protection keychain once signed | **Answered: yes** (decision 1) |
| 4 | Access class after the move | Open. To be decided with the signed build. Now also: with `userPresence` on the key, Lax can't open the database after a restart until the user confirms with Touch ID or the login password, so capture waits for that. Whether that class and `userPresence` go together is to verify |
| 5 | Keychain prompts after rebuilds | **Answered differently from the recommendation:** accept the prompt once per update and encrypt now, with Lax's own note right before it (decision 4). The spike showed a self-signed certificate doesn't remove the prompt |
| 6 | Incremental vacuum | **Answered: yes**, as recommended |
| 7 | `cipher_memory_security` off | **Answered: yes, keep it off**, as recommended |
| 8 | Convert automatically at startup, after a one-time notice | **Answered: yes** |
| 9 | A plaintext export | **Answered: as recommended.** The pages' export is in ADR 0011. An export of anything else waits for its own decision |

**Status.** The spike is done and its results are in "Before deciding".
Only question 4 is still open, and it belongs to the signed build, so it
doesn't hold this ADR back. Accepted by the maintainer on 2026-10-13.

## Context

Issue #114. GUARDRAILS §6 says "Database encryption (SQLCipher) is planned",
and [ADR 0006](0006-sqlite-with-rusqlite.md) chose rusqlite partly so that
this would be a feature switch.

`lax.sqlite` in the data folder
(`~/Library/Application Support/io.github.fnyman-ctrl.lax`) holds raw
captures and their index, the journal, the memory's entities, aliases,
facts and every version of the blocks Lax wrote on pages, episode vectors,
the outbox and settings. The pages themselves are Markdown files in `~/Lax`
today (ADR 0005), outside the database. Lax's blocks and facts are in the
database as well, but the user's own text on a page is only in its file.
[ADR 0011](0011-pages-in-the-database.md)
moves them into the database. This ADR doesn't move them, but from ADR
0011 on, everything said here about the database covers the pages too.

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
| The memory pages, read by another program of the user | **Not protected.** They are Markdown files in `~/Lax`, `0600`, readable by every program the user runs, and indexed by Spotlight unless the user excludes the folder | Not protected by this ADR alone. Protected once [ADR 0011](0011-pages-in-the-database.md) moves them into the database, as in the row above. Copies the user exports are not |
| Malware running as the user | Not protected | **Not protected.** It can ask for the key itself and hope the user approves the Keychain prompt, as they do once after every update (see "After an update"). Lax's note narrows this: the prompt is expected only right after the user rebuilt Lax and Lax said so, and the user guide says a prompt at any other time is a warning sign. It doesn't close it: malware can time its request to just after an update, and the user may not read the prompt. It can also replace `~/Applications/Lax.app` (the user can write there), whose next start then shows the note itself, possibly inject code into an ad-hoc build (not verified), use Accessibility to read Lax's window, read `~/Lax` until ADR 0011 empties it, or watch the screen itself. The app lock (ADR 0012) changes none of this |
| Someone at an unlocked, logged-in Mac | Not protected | **Partly**, and not by encryption: with [ADR 0012](0012-app-lock.md), Lax's window asks for Touch ID or the login password when it opens and after 5 minutes away. The keychain is still unlocked, so a program that person starts can still ask for the key and get the prompt from the malware row; they can still read what is on screen; and in the first 5 minutes after the user leaves, Lax may still be unlocked |
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
certificate doesn't help, encryption waits for the signed `.dmg`. Since
the maintainer has decided to encrypt before 0.1 (see "Maintainer's
answers"), that case goes back to him before rollout step 3 instead.

### Results (2026-10-13)

The maintainer ran the spike (`local/keychain-spike`) on his Mac, macOS
26, on 2026-10-13. A small program wrote a generic password to the login
keychain and read it back.

1. **Ad-hoc build, write then read, no rebuild:** no prompt.
2. **Ad-hoc rebuild, then read:** macOS asked, and the login password was
   needed.
3. **A self-signed code-signing certificate** ("Lax Dev", made in Keychain
   Access): write, rebuild with the same certificate, then read. macOS
   **still asked for the login password.** While he chose "Allow", every
   read of that same build asked again. After he chose **"Always Allow"**,
   the next read of the same build took no time and showed no prompt. So
   a self-signed certificate doesn't keep access across rebuilds. The
   likely reason, not verified: a self-signed certificate has no Team ID,
   so the login keychain's partition list falls back to the build's code
   hash (cdhash), as it does for ad-hoc code.
4. **Migration Assistant** (question 3 above): not tested.

`codesign` also asked to use the certificate's private key during each
build. That is about the certificate, not the key item.

**What it means.** The prompt comes **once per new build**. It needs the
login password and "Always Allow", and after that it is silent until the
next build. Lax users get a new build only when they rebuild Lax
themselves, so the prompt comes right after something they did. The
maintainer accepted this and decided to encrypt now (decision 4). The
signed `.dmg`, with a Developer ID that has a Team ID and the data
protection keychain, removes the prompt later.

Migration Assistant is still untested. It doesn't hold the decision back:
the user guide tells users to keep a copy of the key, and a key that
doesn't arrive ends in "Lax can't find its key" with **Try again**, never
in a new key (see "When the key is missing or wrong"). It is still worth
testing before release 0.1.

## Open questions for the maintainer

The Decision below follows the answers. Question 4 is still open; it
belongs to the signed build.

1. **On for everyone, with no off switch?** Recommended: yes. Users
   without FileVault gain the most. **Answered: yes.**
2. **No recovery key, with the key visible in Keychain Access?**
   Recommended: yes, for 0.x. See "No recovery key". **Answered: yes, for
   0.x.**
3. **Login keychain now, data protection keychain once Lax is signed?**
   Recommended: yes. **Answered: yes.**
4. **Which access class after the move?** Recommended:
   `kSecAttrAccessibleAfterFirstUnlock`, not synced, **without**
   `ThisDeviceOnly`. `ThisDeviceOnly` items never reach a new Mac, so every
   move to a new Mac would end in "Start over". **Open:** decided with the
   signed build.
5. **Keychain prompts after rebuilds, if the spike shows them?**
   Recommended: acceptable only if a self-signed certificate removes them
   for people who build often; otherwise wait for the signed build.
   **Answered differently, after the spike:** a self-signed certificate
   doesn't remove the prompt, and the maintainer accepts it anyway. Lax
   encrypts now and explains the prompt with its own note, once per
   update. See "After an update: one Keychain prompt".
6. **Incremental vacuum instead of VACUUM?** Recommended: yes.
   `temp_store = FILE` during VACUUM would allow plaintext temporary files.
   **Answered: yes.**
7. **Keep `cipher_memory_security` off?** Recommended: yes. It guards
   against reading Lax's memory, which encryption doesn't claim to stop.
   **Answered: yes.**
8. **Convert automatically at startup, after a one-time notice?**
   Recommended: yes. Asking first would leave most databases plaintext.
   **Answered: yes.**
9. **A plaintext export?** Recommended: not now. The user guide explains
   the `sqlcipher` tool; an export that writes plaintext needs its own
   decision. **Answered: as recommended.** The pages' export is in ADR
   0011. An export of anything else waits for its own decision.

## Decision

Lax encrypts its database with **SQLCipher** through rusqlite's
`bundled-sqlcipher` feature, with **CommonCrypto** as the crypto provider.
It does so for everyone, and no setting turns it off. Existing databases
are converted automatically at startup, after a one-time notice. The key
is random, kept in the **login keychain**, never synced, never written
anywhere else, and never replaced or deleted by Lax on its own. There is
no recovery key in 0.x. The only plaintext export is the pages' export of
ADR 0011.

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
the store on a background thread. The window shows "Opening the
database…", "Encrypting…" or, after an update, the note below until it's
done; commands that need the store answer "not ready yet". ("Unlocking" is
kept for the app lock in ADR 0012, which is a different thing: it doesn't
open the database.) Capture, expiry and the journal start only once the
store is open, as today.

`Store::open` already creates an empty `0600` file before opening. Then:

| File | What happens |
|---|---|
| Empty (new install) | New key (by the rule above); key; `auto_vacuum = INCREMENTAL`; then WAL; then migrations |
| Starts with `SQLite format 3\0` | Plaintext: convert it (below) |
| Anything else | Encrypted: after an update, the note (below); look up the key; key; `cipher_compatibility = 4`; read `sqlite_schema`; then WAL, `secure_delete`, `foreign_keys`, `migrate`, `repair` as today |

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

### After an update: one Keychain prompt

The spike showed that every new build of Lax must be allowed once to read
its key: macOS asks for the login password, and the user chooses "Always
Allow" (see "Before deciding"). Lax says so first, so the prompt never
comes out of nowhere.

- **Before it reads an existing key**, Lax checks whether it is a new
  build. If it is, the window shows Lax's own note and waits for
  **Continue**, which is the maintainer's choice. Only then does Lax read
  the key, and macOS asks.
- **The note explains both choices** in the macOS dialog, plainly:

  > Lax was updated. macOS will ask once whether Lax may use its key.
  > Type your login password and choose **Always Allow**: then this
  > version of Lax may read its key without asking, until the next update.
  > If you choose **Allow**, macOS will ask again every time Lax starts.

  It is localized like the rest of Lax. The Swedish text is needed too,
  with the button names macOS shows in Swedish (to check against the
  Swedish dialog):

  > Lax har uppdaterats. macOS frågar en gång om Lax får använda sin
  > nyckel. Skriv ditt inloggningslösenord och välj **Tillåt alltid**: då
  > får den här versionen av Lax läsa sin nyckel utan att fråga, fram till
  > nästa uppdatering. Om du väljer **Tillåt** frågar macOS igen varje gång
  > Lax startar.
- **How Lax knows it is a new build:** it reads its own code hash (cdhash)
  through the Security framework (`SecCodeCopySelf` and
  `SecCodeCopySigningInformation`; whether `security-framework` exposes
  them is to verify) and compares it with the hash it stored after the
  last successful read. That hash is kept in a small file in the data
  folder, `key-build`, `0600`. It can't go in the database's settings,
  because the database can't be read before the key, and SQLCipher's
  plaintext header stays unused. The hash isn't secret. If Lax can't read
  its own hash, or the file is missing while an encrypted database exists,
  it counts as a new build and shows the note.
- **After a successful read**, Lax stores the current hash. A new install
  or a conversion makes the key with the current build, which may then
  read it without a prompt (spike result 1), so it stores the hash with no
  note.
- **"Allow" instead of "Always Allow"** lets this start through, but the
  next start asks again, without the note. The note says so, but a user
  may still choose Allow. To catch that, Lax can first
  try the read with Keychain prompts turned off
  (`kSecUseAuthenticationUIFail`, or the older
  `SecKeychainSetUserInteractionAllowed(false)`), and show the note
  whenever that read is refused for needing a prompt. Whether either works
  for the login keychain's access prompt is to verify. If one does, it
  decides when the note shows, and the stored hash is only the fallback.
- **Denied or cancelled:** "Lax can't read its key." with **Try
  again**, as in "When the key is missing or wrong". The note says what to
  choose; it never chooses for the user.
- **A prompt at any other time is a warning sign.** Users get a new build
  only when they rebuild Lax themselves, and then Lax's note comes first.
  The user guide says: if macOS asks for Lax's key without that note, or
  for another program, choose Deny.

The signed `.dmg` (a Developer ID with a Team ID, and the data protection
keychain) removes the prompt, and with it the note.

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
| Keychain locked, prompt denied or cancelled | "Lax can't read its key." **Try again**. No "Start over": the key is probably fine |
| Key not found, encrypted file exists | "Lax can't find its key." **Try again**, or **Start over** |
| Login password reset: macOS makes a new login keychain and keeps the old one as `login_renamed_1` (Apple's documented behaviour, not verified here) | Same as above, plus: "If you reset your login password, open Keychain Access, unlock the old keychain with the old password, and try again." Lax makes no new key in the meantime |
| Key found, but the file won't open (wrong key, or damaged; they look the same) | "Lax can't open its database." **Start over** |
| A new Mac | Migration Assistant wasn't tested in the spike. If the key doesn't arrive: "Lax can't find its key", as above; the user guide says how to bring a copy of the key. After the move to the data protection keychain, open question 4 decides it |

**Start over** is explicit and confirmed, like every forget. It moves the
file aside as `lax.sqlite.unreadable-<date>` (still encrypted, with any
`-wal`), renames an existing key item to
`database-key-unreadable-<date>` without touching its data, then makes a
new key and database. A separate, confirmed "Delete the old file" removes
the old file and its old key together. If the old key turns up, the old
file can still be opened.

### No recovery key

- Lax is a memory, not a vault. Raw captures expire within 30 to 90 days.
  Losing the database loses the journal and entity data. Today the Markdown
  pages survive that. Once ADR 0011 moves them into the database, they
  don't: a lost key loses the pages too, including what the user wrote on
  them. ADR 0011's page export is the user's own copy. This made open
  question 2 weightier than when it was first asked. The maintainer
  weighed it and answered: no recovery key, for 0.x.
- The key is already visible in Keychain Access. The user guide says where,
  and that a copy in a password manager works on another Mac.
- A recovery key is one more secret that can leak, and one more screen.

The cost: lose the keychain without a copy of the key, and the database is
gone, with the pages once ADR 0011 is in. The user guide says so plainly.

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
  Once ADR 0011 is in, a page's text, with a line of the user's own,
  holds the marker too.
- Keyed open works; no key and a wrong key both fail with fixed errors.
- A new key is never made while an encrypted file exists, and "Start over"
  keeps the old item and file.
- Conversion: row counts, `user_version`, search and FTS5 `secure-delete`
  survive; no plaintext `-wal` or `-shm` is left; stopping after the
  export leaves a working plaintext file and the next start converts
  again; too little space changes nothing.
- No `StoreError` text, log entry or window message contains the key.
- The note after an update, with a fake `KeySource` and a fake code hash:
  a new hash, or a missing `key-build` file next to an encrypted database,
  shows the note and reads no key before **Continue**; the same hash
  shows no note; the hash is stored only after a successful read; a new
  install or a conversion stores it with no note; the note's text exists
  in English and Swedish.
- `cipher_provider` is `commoncrypto` on a keyed temporary database, and a
  forget on a large database stays under a set memory ceiling.

### Docs

- **CLAIMS.md:** a new row, "The database is encrypted, and its text is
  never on disk in plaintext"; the forget row says incremental vacuum.
- **USER-GUIDE.md:** what is and isn't encrypted, where the key is, how to
  keep a copy, what "Start over" does, the login-password-reset case, and
  how to open the database with the `sqlcipher` tool. The prompt after an
  update: Lax's note comes first, type the login password and choose
  Always Allow; Allow means macOS asks again at every start. A prompt for Lax's key at any other time, or from another
  program, is a warning sign: choose Deny.
- **PERFORMANCE.md:** temporary data in RAM. **ARCHITECTURE.md:** a line
  pointing here.

### What we give up

- `sqlite3 lax.sqlite` no longer works; inspecting needs SQLCipher and the
  key.
- A lost keychain without a copy of the key means a lost database.
- One Keychain prompt with the login password after every update, until
  the signed `.dmg`. People who build Lax often, during development, get
  it after every build.
- A slower source of SQLite fixes.

## Rollout

Each PR green on its own, labelled `security-sensitive`, updating CLAIMS.md
as it lands.

0. **GUARDRAILS §6, in its own PR first**, as the guardrails' preamble
   requires. "Lax relies on FileVault … Database encryption (SQLCipher) is
   planned" becomes: the database is encrypted with a random key in the
   login keychain, never synced; until ADR 0011 is in, the pages in `~/Lax`
   are plain Markdown, protected by FileVault and permissions; Lax warns if
   FileVault is off. ADR 0011 changes the pages' part of §6 in its own PR.
   "Inspect it without Lax" gains: the database needs SQLCipher and the key
   from Keychain Access. Merged once this ADR is accepted. The **spike**
   is done (`local/keychain-spike`, not merged); its results are in
   "Before deciding".
1. **`keychain.rs`** with `KeySource`, reading Lax's own code hash, and
   tests. No change in behaviour.
2. **Build with `bundled-sqlcipher`**, databases still plaintext (SQLite
   3.53.2 → 3.51.3 for everyone). Logging compiled out. `temp_store = FILE`
   while plaintext, to keep today's VACUUM memory. The `cipher_provider`
   test.
3. **Encrypted new databases:** opening off the main thread with
   "Opening the database…", the plaintext-header branch, the failure
   screens and "Start over", the note after an update with `key-build`,
   incremental vacuum, the tests above.
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
