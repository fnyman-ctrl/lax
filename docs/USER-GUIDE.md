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
  to learn about someone. When you merge, what you wrote on the merged one's
  page moves to the other's page. Each has a page you can edit and export (see
  [Your memory pages](#your-memory-pages)). Under **About you**, add the names
  you go by and edit your own page. New names wait, folded away under **New**,
  until they come back on another day. One that doesn't come back within two
  weeks of your newest journal entry (four weeks if it was mentioned three
  times or more) leaves the memory. The journal entries stay, and the name can
  come back as new. Press **Keep** to keep one for good. Organisations whose
  names differ only by a legal suffix or punctuation ("Example" and "Example
  AB") are suggested at the top, with **Merge**, **Not the same** and **Merge
  all**. Lax never merges on its own, and never suggests a pair again once you
  say they're not the same. Other names that may be the same, such as "Anna"
  and "Anna Example" or "Nyterios" and "Nyterio", are judged by the model on
  this Mac from the entries that mention them, while you're away and the Mac
  is on power. Its "yes" is shown under **These may be the same**, with its
  reason. If a short name fits several, Lax asks which one, or none. In a
  name's **Change** panel, **Same as** lists the likely names first; with
  many names, you can search it.
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

## Your memory pages

Each person, project and organisation Lax keeps has a page, and so do you:
the About me page. The pages are in Lax's encrypted database, and you read
and edit them in Lax. They are not files on your disk, unless you export
them (see [Export](#export-your-pages)).

### Editing a page

- In **Memory**, press **Edit page** under a name. For your own page, press
  **Edit your page** under **About you**.
- The page opens as plain text, never as formatted text. Lax's part is shown
  as it is, because it is written from what Lax read.
- Lax saves a moment after you stop typing, and when you close the page. While
  a page is open, Lax doesn't write to it.
- If the page changed while you were editing, Lax shows **The page changed
  while you were editing**, with your text and the page's text. Press **Keep
  my text** or **Use the page's text**. Nothing you wrote is thrown away.
- **Memory → Change… → History** shows earlier versions: what Lax wrote, and
  your own edits, one each time you opened the page and changed it. The last
  20 of each are kept.

### What is yours, and what is Lax's

Lax writes only between two marker lines, `<!-- lax:begin … -->` and
`<!-- lax:end -->`. Everything outside them is yours.

- Lax never changes your text outside its block. It checks before every
  write, whatever the model answered.
- A line you add or change between the markers becomes yours. Lax moves it
  above its block and keeps it.
- A fact you delete between the markers is never written again.
- If you remove the markers, Lax stops writing to the page and offers
  **Write a new page**, which adds a new block below your text.
- A page Lax can't find is shown as missing. Lax never writes it again, and
  never forgets it, unless you ask.

### Your pages move into the database, once

Earlier versions of Lax kept the pages as Markdown files in `~/Lax`. If you
have those, Lax moves them into the database once, in the background, as soon
as the database is encrypted, and not before.

- It reads each file whole, with every line you added, and keeps it as the
  page. It moves all pages in one step. If Lax stops halfway, nothing has
  changed, and it tries again.
- Nothing is deleted by the move. A page whose file is gone stays missing, with
  **Recreate page** and **Forget**. A file whose markers you removed moves in as
  yours, and Lax leaves it alone. If two
  files carry the same page, one moves in and the other stays where it is.
  Files that aren't Lax's pages are never moved and never deleted.
- A new install has no files to move. Its pages start in the database.

Then Lax asks once: **Your pages are now in Lax's encrypted database. Delete
the old files in ~/Lax?** It tells you:

- Edits to the old files are no longer read.
- Deleting doesn't reliably erase the old text from the disk. On APFS and
  SSDs, old blocks can stay until they are reused. FileVault, if it is on,
  keeps them unreadable. Time Machine backups and local snapshots keep the
  files until they expire.
- If you keep the files, every program can still read them.

**Delete the old files** is the default button. It deletes only the files Lax
moved, and only those that still hold exactly what was moved. A file you
changed since is kept, and Lax lists it. Then it removes the folders left
empty, and `~/Lax` itself if it is empty. Your own files stay. You confirm
this in the dialog, every time.

**Keep them** leaves the files. Lax doesn't ask again. Memory shows a quiet
line, **Your old page files are still in ~/Lax**, with **Delete the old files**
if you change your mind.

An older Lax can't read pages that have moved. The move is one-way, like the
encryption.

### Export your pages

The database is the only copy Lax keeps, so you can export your pages as
Markdown files.

- **One page:** press **Export this page…** in the editor. Or press **Export it
  first** when you forget a name (see below).
- **All pages:** press **Export pages…** in **Memory**. There is no way to pick
  several.

Lax first says that the copy will not be encrypted: every program you run can
read it, Spotlight may index it, and backups and sync copy it. Forgetting
someone in Lax later doesn't reach that copy. Then you choose a folder in the
macOS folder picker. Lax keeps your choice to itself and writes there.

If the folder is inside iCloud Drive or another synced folder, Lax asks again,
because the copy would leave this Mac. Choose **Export there anyway** or
another folder.

Lax writes a new folder, `Lax pages <date>`, inside the one you chose. If that
name is taken, it adds a number. The folder is private to you (`0700`), and
so are the files (`0600`). Lax never overwrites anything. Inside are
`people/`, `organisations/` and `projects/`, and `about-me.md`. Each file is the
page's whole text, markers included. Lax doesn't keep track of exported copies
and doesn't read them back.

### No app lock yet

An app lock comes after release 0.1 (issue #158). Until then, the page editor
is open whenever Lax's window is. Export and **Delete the old files** ask you
to confirm in their dialogs instead.

### Forgetting a name

Forgetting a name deletes its page, including what you wrote on it. Lax shows
you that text first, under **Its page goes too, with what you wrote on it**,
with **Export it first** next to **Forget**. A page that is open in the editor
goes when you close it. **Forget what it noted…** deletes every page the same
way, About me included.

Forgetting a time range or an app doesn't touch your text. It rewrites Lax's
block on the pages that drew on what you forgot.

### If the database isn't encrypted yet

Pages move into the database only once it is encrypted. If Lax couldn't
convert the database (see [Encryption](#encryption)), because the disk was
too full or the check of the copy failed, the pages stay as Markdown files in
`~/Lax`. They work as before: edit them in any editor, and Lax writes only
between its markers. Lax doesn't write them if `~/Lax` is inside iCloud Drive or
another synced folder. In this case the Memory view has no page editor and no
export, because the files already are the export. Once the database has been
encrypted, the pages move in, in the background.

## Encryption

Lax encrypts its database. You don't turn it on, and there is no setting to
turn it off.

### What is encrypted, and what isn't

Encrypted, in the database `lax.sqlite` in the data folder
(`~/Library/Application Support/io.github.fnyman-ctrl.lax`):

- everything Lax captured, and the index used to search it
- the journal
- the memory's people, projects and organisations, their names and facts, and
  their pages, with everything you wrote on them, and the versions of the blocks
  Lax wrote on pages
- the log of what was sent, and your settings

Not encrypted by Lax:

- **Pages that are still files in `~/Lax`.** Pages move into the database once
  it is encrypted. Until then, and for old files you chose to keep, they are
  plain Markdown files that only FileVault and the folder's permissions
  protect. Lax warns you if FileVault is off.
- **Pages you export.** An exported page is a plain Markdown file in the folder
  you chose. Lax tells you so first.
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

Raw captures expire after a few weeks anyway, but the journal and the memory,
pages included, are only in the database. Export your pages now and then if
they matter to you: an exported copy survives a lost key.

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

It doesn't touch `~/Lax`. Pages that had already moved into the old database
are in the old file, not in the new one. The old file and the old key are kept, so if the right
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
- The database is encrypted with a key that only your login keychain holds. So
  are the memory pages, which you edit in Lax. They leave it only when you
  export them, after a notice that the copy isn't encrypted.
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
