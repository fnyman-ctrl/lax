# Changelog

What changed in each version of Lax, for people who use it.

## Unreleased

- **Fixed: models that think too long wrote nothing.** Some models, such as
  Qwen 3.5, think before every answer until they run out of room, so the
  journal and the memory got no answer at all. When that happens, Lax now
  asks once more without thinking, and gets one.
- **Fixed: the journal could stop writing.** A part of your day the model
  fails to summarize three times is set aside until Lax restarts. Once about
  200 captures were set aside, the journal stopped writing altogether, even
  about the captures after them. Now it goes on with the rest.
- **Brief me.** Before a call or a reply, press **Brief me** on a name in
  Memory: Ask tells you who they are to you, what you discussed lately and
  what is still open, with its sources.
- **Copy details for a bug report**, under Models: versions, your Mac, the
  models, what's on and counts, shown before you paste it. Nothing Lax has
  read, and nothing is sent.
- **Journal on battery.** The journal now writes on battery too, about once an
  hour, and waits for power only below 20 %. Before, it waited for power
  however long you were unplugged. The Journal and Memory tabs say what
  waits for power.
- **Memory: new names fade.** A new name that doesn't come back on another
  day leaves the memory two weeks after your newest journal entry (four weeks
  if it was mentioned three times or more). The journal entries stay, and the
  name can come back. New names are folded away under **New** until then.
- **Memory: obvious duplicates.** Organisations whose names differ only by a
  legal suffix or punctuation, such as "Example" and "Example AB", are
  suggested at the top of Memory, with **Merge**, **Not the same** and
  **Merge all**. Your "Not the same" is final.
- **Memory: suggested merges.** Other names that may be the same, such as
  "Anna" and "Anna Example", are judged by the model on your Mac while you're
  away, and suggested with its reason. Lax never merges on its own.
- **Memory: Same as.** The list in a name's Change panel puts the likely
  names first, and can be searched when there are many.
- **Memory: projects in parts.** A project can be part of another, one level
  deep, set by you or suggested by the model on your Mac. Parts are listed
  under their project, and the pages name them. Ask and the writing
  assistant read up to three parts' pages when you name the project.

## 0.1.0 (2026-10-16)

The first version. Everything runs on your Mac; nothing leaves it.

**What you need:** a Mac, the Xcode Command Line Tools, rustup and Node.js to
build Lax, and a local model server ([LM Studio](https://lmstudio.ai) or
[Ollama](https://ollama.com)) with a chat model for the AI parts. An embedding
model is optional: with one, Ask also finds entries by meaning.
[MODELS.md](docs/MODELS.md) suggests models for your Mac's memory.
See [Install](docs/USER-GUIDE.md#install).

- **First start:** Lax explains what it reads and never reads, helps you turn
  on Accessibility, finds the models on your Mac, and lets you choose what to
  turn on. The journal, the memory and the writing assistant are all off until
  you do.
- **Capture:** reads the text of the window you're working in through
  Accessibility. Never password fields, password managers or private browser
  windows; banking and health excluded from the start; your own exclusions;
  secrets masked; pause from the menu bar; nothing read while you're away.
- **Journal** (opt-in): a few lines about what you did, every 15 minutes or so.
- **Memory** (opt-in): the people, projects and organisations you work with,
  each with a page you edit in Lax, plus an About me page, with history. The
  pages are kept in the encrypted database (see below). Forgetting a name deletes
  its page, after showing you what you wrote on it; merging two keeps what you
  wrote on both.
- **Ask:** a conversation about your work, answered from the journal and the
  memory, with the entries each answer is based on.
- **Writing assistant** (opt-in): double-tap ⌥ Option (or a key you pick) in any
  text field; the pixel salmon swims to your cursor and drafts the text with
  you. Enter puts it in the field; Lax never sends.
- **Forget and expiry:** forget a time, an app or a person; raw text goes 30
  days after it's summarized, and at 90 days at the latest.
- **Encrypted database:** everything Lax keeps in its database is encrypted with
  a random key that only your login keychain holds. A new database is encrypted
  from the start; an existing one is converted once, after a notice (if the disk
  is too full, Lax says so and tries again at the next start). The
  conversion is one-way: an older version of Lax can't open the encrypted
  database. Backups and snapshots made before the conversion still hold the old,
  unencrypted file. After every build, macOS asks once for your login password
  so the new build may read the key. There is no recovery key: keep a copy of
  the key in your password manager. See [Encryption](docs/USER-GUIDE.md#encryption).
- **Pages in the database:** the memory pages move from Markdown files in
  `~/Lax` into the encrypted database, once, when the database is encrypted
  (if it stays plaintext, the pages stay files). You edit them in Lax, and
  export one page or all pages as Markdown files, after a notice that the copy
  is not encrypted. After the move, Lax asks once whether to delete the old
  files; it deletes only files that still hold what was moved. Deleting can't
  reliably erase them from the disk (see
  [Your memory pages](docs/USER-GUIDE.md#your-memory-pages)). The move is
  one-way, like the encryption: an older version of Lax can't read pages that
  have moved. There is no app lock yet; it comes after 0.1.
- **Install:** build from source with `npm run build:app`.

### Known limits

- **No app lock yet.** Anyone at your unlocked Mac can open Lax's window and
  read the journal, the memory and the pages. A lock with Touch ID or your
  login password is planned for 0.2.
- **Every build is a new app to macOS.** After you build Lax again, remove it
  under **Privacy & Security → Accessibility** with − and add it again with +,
  and type your login password once so the new build may read its key. A
  signed app comes later.
- **Private windows:** checked on a real Mac only in Safari. Chrome, Firefox,
  Edge and Arc are recognised by their window titles, which haven't been
  checked on a real Mac yet. Exclude them under **Exclusions** if you want to
  be sure.
- **Meetings you only listen to** stop being read after ten minutes without
  keyboard or mouse, like any time you're away.
- **The memory's list grows long.** You keep, merge and forget names by hand.
  0.2 is planned to tidy it for you.
- **English and Swedish** are the languages Lax is tested with.
