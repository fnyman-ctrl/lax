# Changelog

What changed in each version of Lax, for people who use it.

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
