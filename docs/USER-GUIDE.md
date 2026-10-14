# Using Lax

Lax is a private memory for your Mac. It reads the text of the window you're
working in, writes a short journal of your day, learns who and what you work
with, answers questions about it, and helps you write. Everything stays on your
Mac: the AI runs on a model server on your Mac too.

## Install

Lax 0.1 is built from source, which takes a few minutes and needs no Apple
account. You need macOS, the Xcode Command Line Tools
(`xcode-select --install`), [rustup](https://rustup.rs) and
[Node.js](https://nodejs.org).

```sh
git clone https://github.com/fnyman-ctrl/lax.git
cd lax
npm run build:app
open ~/Applications/Lax.app
```

To update, run `git pull` and `npm run build:app` again. Your data stays.
After a new build, macOS may ask you to turn Lax on under **System Settings →
Privacy & Security → Accessibility** again, and asks once for your login
password so the new build may read the key to its database (see
[Encryption](#encryption)).

## The first start

1. **Accessibility.** Lax explains what it reads and what it never reads, and
   opens System Settings so you can turn it on.
2. **A model on this Mac.** Lax suggests models for your Mac's memory. Install
   [LM Studio](https://lmstudio.ai) or [Ollama](https://ollama.com), download
   the models, start its local server, and press **Find models on this Mac**.
   [MODELS.md](MODELS.md) has the details.
3. **What to turn on.** The journal, the memory and the writing assistant are
   all off until you turn them on.

Lax lives in the menu bar. Click the salmon to open the window or to pause.

## The window

The sections sit along the line at the top; the salmon swims to the one you
choose.

- **Ask:** a conversation about your work. "What did I do yesterday?", "What did
  I work on with Anna last week?", then ask on: "and the day before?". Ask in
  English or Swedish. The answer appears as it's written, and each answer
  shows the journal entries it's based on. Nothing you ask is saved.
- **Write:** turn the writing assistant on and pick the key that calls it.
- **Journal:** what you did, a few lines every 15 minutes or so.
- **Memory:** the people, projects and organisations Lax has learned about. You
  can rename them, merge duplicates, keep them, forget them, or tell Lax never
  to learn about someone. Under **About you**, add the names you go by.
- **Captures:** everything Lax has read. Search it, forget a time or an app, or
  delete everything.
- **Exclusions:** apps, websites and window titles Lax never reads. Banking and
  health are excluded from the start; password managers and private browser
  windows are never read.
- **Models:** the models Lax uses.
- **Sent:** everything that left your Mac. In 0.1, nothing does.

## The writing assistant

1. In any text field, write what you want ("reply in my tone and thank them for
   the quote"), or select text you want changed. English and Swedish both work.
2. Double-tap **⌥ Option** (or the key you picked under Write). The salmon swims
   to your cursor with your instruction.
3. Press **Enter**. The draft appears; edit it if you like.
4. Press **Enter** again to put it in the field, in place of your instruction.
   **Escape** sends Lax away. Lax never presses Send.

With nothing selected, the instruction is the paragraph you're writing in. The
draft draws on what Lax knows about you, the people you name in the instruction,
and the window you're in.

## Your memory in ~/Lax

Each person, project and organisation Lax keeps has a Markdown page in `~/Lax`,
plus `about-me.md`. You can read and edit them in any editor. Lax only writes
between its own markers; what you write elsewhere is never changed, and a fact
you delete is never written again. **Memory → Change… → History** shows earlier
versions.

These pages are not part of the encrypted database yet. See
[Encryption](#encryption) for what that means.

## Encryption

Lax encrypts its database. You don't turn it on, and there is no setting to
turn it off.

### What is encrypted, and what isn't

Encrypted, in the database `lax.sqlite` in the data folder
(`~/Library/Application Support/io.github.fnyman-ctrl.lax`):

- everything Lax captured, and the index used to search it
- the journal
- the memory's people, projects and organisations, their names and facts, and
  the versions of the blocks Lax wrote on pages
- the log of what was sent, and your settings

Not encrypted by Lax:

- **The pages in `~/Lax`.** They are plain Markdown files, so you can edit them
  in any editor. Until they move into the database, only FileVault and the
  folder's permissions protect them. Lax warns you if FileVault is off.
- **The logs.** They hold counts and timings, never what you did.
- **Backups and snapshots from before the conversion.** If you used Lax before
  it encrypted its database, Time Machine and local snapshots may still hold the
  old, unencrypted file. They are not changed, and they go away only when the
  backups do. FileVault covers the disk blocks the old file left behind.

Encryption keeps the database safe from other programs that read files, from
copies of your folder (a backup, a USB stick) and from someone who takes the
Mac while it is off. It does not stop a program that is built to attack Lax, or
someone who sits down at your Mac while you are logged in.

### The key

Lax makes a random key and keeps it only in your login keychain. It is never
synced and never written anywhere else.

To see it, open **Keychain Access**, choose the **login** keychain, and search
for **Lax database key**. Double-click it and tick **Show password**. The key
is 64 characters of hex.

**There is no recovery key.** If you lose your keychain and have no other copy
of the key, the database can't be opened by anyone, and what is in it is gone.
Keep a copy of the key in your password manager. It also lets you move to
another Mac. Never put it anywhere else: not in a note, a message or an issue.

Raw captures expire after a few weeks anyway, but the journal and the memory
are only in the database. The pages in `~/Lax` survive a lost key.

### The first start after encrypting

If you used Lax before it encrypted its database, the first start shows
**Lax encrypts its database**. It says what is about to happen: Lax makes an
encrypted copy, checks it, and puts it in the old file's place. Press
**Encrypt**. It takes a moment and happens once. Nothing is captured until it
is done.

- It is one-way. An older version of Lax can't open the encrypted database.
- Lax needs free space of twice the database's size. If there isn't enough,
  Lax skips the notice, opens the old database as before and says so. If the
  check of the copy fails, nothing is changed either. Either way Lax tries
  again the next time it starts.
- macOS may ask whether Lax may use its key. Type your login password and
  choose **Always Allow**.

A new install has no notice: its database is encrypted from the start.

### After every update, macOS asks once

macOS identifies a program by a code hash, and every build of Lax has a new
one. So after you build Lax again, macOS asks once whether the new build may
read the key. Lax tells you first. It shows **Lax was updated** and waits for
you to press **Continue**. Then macOS asks:

1. Type your login password.
2. Choose **Always Allow**. This build may then read the key without asking,
   until the next update.

If you choose **Allow** instead, macOS asks again every time Lax starts. Lax
doesn't show its note again then, so choose **Always Allow** the next time.
If you choose **Deny**, Lax says it can't read its key; press **Try again**.

**A prompt for Lax's key at any other time is a warning sign.** So is a prompt
for it from any other program. If macOS asks for the key without you having
just built Lax, or if the prompt names something other than Lax, choose
**Deny**. A program that wants the key wants all of what Lax has read.

### When Lax can't open its database

Lax never makes a new database next to one it can't open, and never captures
without one. It shows what went wrong instead.

- **Lax can't read its key.** The keychain is locked, or Lax was denied the
  key. The key is probably fine. Press **Try again**, and when macOS asks, type
  your login password and choose **Always Allow**.
- **Lax can't find its key.** The database is encrypted, and "Lax database key"
  isn't in your login keychain. Press **Try again** first. If you reset your
  login password, macOS may have kept your old keychain under another name
  (`login_renamed_1`). Open Keychain Access, unlock the old keychain with your
  old password, and press **Try again**. If you have a copy of the key, for
  example on a new Mac, put it back with the command below, which asks for the
  key instead of taking it as an argument, then press **Try again**. macOS will
  ask once whether Lax may read it.

  ```sh
  security add-generic-password -s io.github.fnyman-ctrl.lax -a database-key -l "Lax database key" -w
  ```

- **Lax can't open its database.** The key in your keychain doesn't open the
  file. The file is damaged, or it was made with another key.
- **Something went wrong with the data folder or the disk.** Check that the disk
  isn't full, then press **Try again**.
- **It was made by a newer version of Lax.** Build or install the newer version.

**Start over** is offered only when the key is missing or doesn't open the
file, and asks you to confirm. Use it only when you have no other way back. It:

- moves `lax.sqlite` (and its `-wal` and `-shm` files, if any) in the data
  folder to `lax.sqlite.unreadable-<date-time>`, still encrypted
- renames the old key in your keychain to `database-key-unreadable-<date-time>`
  and keeps it
- starts again with a new key, an empty journal and an empty memory

It doesn't touch `~/Lax`. The old file and the old key are kept, so if the right
key turns up you can still open the old file (see the next section). Lax doesn't
delete them. If you are sure you don't want them, delete the file in the data
folder, and the old key in Keychain Access, by hand.

### Opening the database without Lax

The database is a standard SQLCipher 4 file, and you can read it yourself. Quit
Lax first: while it runs, the latest changes are still in `lax.sqlite-wal`, and
a copy of `lax.sqlite` alone would miss them. You need the `sqlcipher` command-line
tool (for example `brew install sqlcipher`) and the key from Keychain Access:

```sh
sqlcipher ~/Library/Application\ Support/io.github.fnyman-ctrl.lax/lax.sqlite
```

```sql
PRAGMA key = "x'<the 64 hex characters>'";
.tables
```

The plain `sqlite3` tool can't open it. Never paste the key anywhere but
there: not into a chat, a web page, an issue or an AI assistant. Anyone with
the key and the file can read everything in it.

## Privacy, in short

- Lax reads text through Accessibility, never screenshots, and never password
  fields, password managers or private browser windows.
- Secrets such as passwords, keys and card numbers are masked before anything is
  stored.
- The database is encrypted with a key that only your login keychain holds. The
  memory pages in `~/Lax` are not, yet; they rely on FileVault.
- Raw text is deleted 30 days after the journal has summarized it, and after 90
  days (your choice) at the latest. The journal and memory keep what was learned.
- Lax reads nothing once you've been away for ten minutes, and pauses from the
  menu bar for as long as you like.
- Nothing leaves your Mac. No analytics, no telemetry, no update checks.

The rules are in [GUARDRAILS.md](GUARDRAILS.md), with the test behind each in
[CLAIMS.md](CLAIMS.md).

## When something doesn't work

- **"Lax couldn't reach the model."** The model server isn't running. Start it
  (in LM Studio: the Developer tab, or `~/.lmstudio/bin/lms server start`).
- **Ask and drafts take 15–30 seconds.** In LM Studio, turn off "Only Keep Last
  JIT Loaded Model" under Developer → Server Settings, so the chat and embedding
  models stay loaded together. [MODELS.md](MODELS.md) has the numbers.
- **Nothing is captured after an update.** A Lax you built yourself counts as a
  new app after every build: under Accessibility, remove Lax with − and add it
  again with +, even if its switch looks on.
- **macOS asks for Lax's key.** Right after you built Lax again, and after Lax's
  own note, that is expected: type your login password and choose **Always
  Allow**. At any other time, choose **Deny**. See [Encryption](#encryption).
- **Lax says it can't read or find its key.** See
  [When Lax can't open its database](#when-lax-cant-open-its-database).
- **The salmon doesn't come.** Check that the writing assistant is on under
  **Write**, that the app isn't excluded, and that Lax isn't paused. It never
  comes to password fields.
- **The salmon goes to the wrong place.** Some apps report the text cursor
  wrongly; Lax then goes by the mouse pointer. Click in the field first.
- **The fans spin.** The memory's work runs while you're away from the keyboard,
  and never on battery. `npm run perf` shows how Lax is doing.

Found a bug? [Open an issue](https://github.com/fnyman-ctrl/lax/issues), and
leave out anything personal.
