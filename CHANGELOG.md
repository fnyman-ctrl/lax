# Changelog

What changed in each version of Lax, for people who use it.

## 0.1.0 (unreleased)

The first version. Everything runs on your Mac; nothing leaves it.

- **Capture:** reads the text of the window you're working in through
  Accessibility. Never password fields, password managers or private browser
  windows; banking and health excluded from the start; your own exclusions;
  secrets masked; pause from the menu bar; nothing read while you're away.
- **Journal** (opt-in): a few lines about what you did, every 15 minutes or so.
- **Memory** (opt-in): the people, projects and organisations you work with,
  each with a Markdown page in `~/Lax` you can edit, plus `about-me.md`, with
  history.
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
  unencrypted file. The memory pages in `~/Lax` are not part of the database
  yet. After every build, macOS asks once for your login password so the new
  build may read the key. There is no recovery key: keep a copy of the key in
  your password manager. See [Encryption](docs/USER-GUIDE.md#encryption).
- **Install:** build from source with `npm run build:app`.
