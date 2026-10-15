# 0011. Keep the memory pages in the encrypted database

- **Status:** Accepted
- **Date:** 2026-10-13

## Summary

The memory pages, and the About me page, would move from Markdown files in
`~/Lax` into Lax's database, which [ADR 0010](0010-encrypted-database.md)
encrypts. Each page keeps the same Markdown text, the same block of Lax's
between two markers, and the same rule: user edits win. Pages are read and
edited in Lax's Memory view, behind the app lock of
[ADR 0012](0012-app-lock.md). Existing pages are moved in once and checked,
and Lax then asks one time whether to delete the old files. An explicit
export writes pages as Markdown to a folder the user picks, after a notice
that the copy is not encrypted.

This supersedes part of [ADR 0005](0005-memory-as-markdown.md) and part of
[ADR 0008](0008-learning-memory.md). "Relation to ADR 0005 and 0008" below
says which parts.

## Maintainer's answers (2026-10-13)

The maintainer answered every open question as recommended, late on
2026-10-13. The questions at the end keep their reasoning, with each answer
added. He chose these three explicitly:

- **A plain-text editor**, with no Markdown rendering (question 2).
- **Forgetting an entity deletes the user's own text on its page too**,
  after the forget dialog shows that text and offers "Export it first"
  (question 5).
- **The old files:** Lax asks once, with "Delete the old files" as the
  default, and deletes only files that still hold exactly what was moved
  (question 6).

The other seven (1, 3, 4, 7, 8, 9 and 10) are answered as recommended. The
Decision below follows the answers.

**Status.** Accepted by the maintainer on 2026-10-13, together with [ADR 0010](0010-encrypted-database.md). Pages only ever move into an encrypted database.

### 2026-10-15: before release 0.1, without the app lock

The maintainer decided to build this ADR before release 0.1, and the app
lock ([ADR 0012](0012-app-lock.md), issue #158) after it. Until the lock is
there, the places below that say "behind the app lock" or "needs Touch ID
or the login password" work like this:

- **The editor** (`page_text`, `save_page`) is open whenever Lax's window
  is, like the rest of the Memory view today.
- **Export** and **Delete the old files** ask for a confirmation in a dialog
  that says what happens, every time, as forgets do now.

When ADR 0012 is built, these three get the lock as this ADR describes.

### 2026-10-15: built

Steps 0 to 4 of "Rollout" are in (#160 to #164): GUARDRAILS, the rows and the
block writer on them, the editor, the move with its one-time question, and
the export. Step 5 is split. The docs are done in the change that adds this
note. The removal of the folder code waits: it stays while a database can
still be plaintext (a conversion postponed for lack of disk space or a
failed check, see the USER-GUIDE's "Encryption"), because the pages stay
files in `~/Lax` until the database is encrypted. It goes once a plaintext
database can no longer happen.

## Context

[ADR 0005](0005-memory-as-markdown.md) put the memory in Markdown files in
a folder the user owns, so they could read, edit, search, back up and sync
it with any tool. It rejected a database-only memory as "opaque and locked
into Lax".

[ADR 0008](0008-learning-memory.md) worked out how that is kept safe:

- **User edits win**, enforced by the code that writes, not by the prompt.
  Lax writes only between its two markers (`<!-- lax:begin <id>: … -->`
  and `<!-- lax:end -->`). `block::update` in `src-tauri/src/pages/block.rs`
  checks, before returning anything to write, that the user's text outside
  the block is unchanged. Lines the user adds or changes inside Lax's block
  are moved above it as theirs; facts they delete from it are kept as
  rejected facts and never written again (`Store::reject_fact`).
- **A block writer that manages blocks inside files.** `pages::sync` in
  `src-tauri/src/pages/mod.rs` scans the folder for page ids in the begin
  markers (so renamed and moved files are found), writes to a temporary file
  and renames it over the page, only if the page is unchanged since it was
  read (`pages::write`). A page no file carries is marked missing; a file
  without markers is detached. Neither is recreated or forgotten without
  asking.
- **Versions are already in SQLite:** `page_versions` keeps each block Lax
  wrote and each edit it adopted, linked to the episodes it drew on
  (`migrations/0008_pages.sql`). The facts are in `pages.facts`, and
  rejected facts in `rejected_facts` (`migrations/0009_page_facts.sql`).
- **The folder is private:** `~/Lax` is created `0700`, pages `0600`.
  `prepare_folder` and `write_inside` refuse a folder, or a subfolder
  reached through a symbolic link, inside iCloud Drive,
  `~/Library/CloudStorage`, Dropbox, Google Drive or OneDrive, since syncing
  would copy the memory off the Mac (GUARDRAILS §1).

What that leaves open: permissions keep other users out, not other
programs. Every program the user runs can read `~/Lax`, and so can backup
tools, sync agents, indexers and anything that scrapes files. Spotlight
indexes the pages unless the user excludes the folder, and editors keep
their own copies (history, caches, swap files). The pages are the most
condensed thing Lax holds about a person: what they work on, how to reach
them, what the user noted about them. With ADR 0010, the journal and the
facts would be encrypted while the same facts sit in plaintext on the
pages next to it.

The maintainer considers this a real risk and decided on 2026-10-13 that
the pages move into the encrypted database and are edited inside Lax.

Today, the user's own text on a page (everything outside Lax's block) is
only in the file. Ask and the writing assistant don't read it: they use
`pages.facts` (`ask::known`, `ask::known_for`). The fact updates do: the
model gets "the user's own notes" from the file (`facts::notes_of`).

## Decision

### Pages are rows

- Each page, About me included, is a row in `pages`, as now, with one new
  column, `text`: the page's whole Markdown, exactly as it would be in the
  file. A new migration adds it; no migration file changes.
- The text has the same structure as today's files: a title, the user's
  text, and one block of Lax's between the same two markers, with the
  page's id in the begin marker. `block.rs` keeps working on that text
  unchanged: `update`, `removed_lines`, `without_block`, `defuse` and their
  tests carry over as they are.
- **User edits win, as before.** Lax changes a page's text only through
  `block::update`, so the check that the user's text is unchanged runs
  before every write, whatever the model answered. Lines the user adds in
  Lax's block become theirs, and facts they delete become rejected facts.
- **Writes compare the old text.** Where Lax renamed a temporary file over
  the page only if the file was unchanged, it now runs one `UPDATE pages
  SET text = ?new … WHERE id = ?id AND text = ?read`. If the user saved in
  the meantime, nothing is written and Lax tries again next round, as it
  does now when a file changed.
- **Versions and history stay as they are:** `page_versions`, the sources
  that tie them to episodes, the 20 kept versions of Lax's, "restore" and
  "undo the restore", and About me keeping only its latest version.
- **The user's own edits are versioned too**, as `by = 'user'` versions:
  one each time the user opens a page and changes it (not one per save),
  the last 20 kept. They are the user's text, so a forget of episodes
  doesn't remove them; forgetting the entity does.
- The page's `path` column is no longer a place on disk. The export names
  its files from it (`people/anna-example.md`).

### Editing in the Memory view

- The Memory view gets a page editor: the page's Markdown as plain text,
  in a monospace text field. Lax's block is shown as it is, markers
  included, with a line under the editor saying what they mean. No new npm
  dependency.
- **Shown as text, never as HTML.** Lax's block is written from captured
  text, so rendering the Markdown as HTML would put captured content in the
  DOM as markup. The Memory view keeps the rule the viewer has
  (`viewer::tests::the_web_ui_never_renders_text_as_html`).
- The editor saves a few seconds after the user stops typing, and when
  they leave the page, so a lock (ADR 0012) loses at most those seconds.
- New commands: `page_text` (one page's text) and `save_page` (the page's
  id, the text the editor loaded and the new text). `save_page` refuses a
  text over 1 MB, the cap `MAX_PAGE_BYTES` has today, and refuses if the
  page changed since the editor loaded it; the editor then shows both
  texts so nothing the user wrote is lost. Both commands are listed in
  `build.rs` and `capabilities/default.json`, and both refuse while Lax is
  locked (ADR 0012). `open_memory_folder` is removed from `lib.rs`,
  `build.rs` and the capabilities.
- **While a page is open in the editor, Lax doesn't write it.** The
  background round skips it, so a save rarely finds the page changed.
- A user who removes the markers detaches the page, as today: Lax stops
  writing to it, and the Memory view offers "Recreate page", which now
  adds a new block of Lax's below the user's text on the same page,
  instead of writing a new file next to the old one.
- The facts' notes come from the row (`facts::notes_of` reads `text`
  instead of the file). What Ask and the writing assistant read doesn't
  change.

### Forgetting

- Forgetting a time range or an app works as today: the versions written
  from forgotten episodes go, and the blocks of pages that drew on them are
  rewritten at once from the episodes that remain, also while paused, as
  `pages::sync_now` does now. Until then the old block is still in the
  page's text, as it is in the file today. The user's text is not touched.
- Forgetting an entity deletes its page, in the same transaction as the
  rest of the forget, so nothing is left to retry. ADR 0008 says so too,
  but with files the code takes only Lax's block out and keeps a file that
  holds anything of the user's. A row can't stay behind like a file can,
  where nobody would see it. So the forget dialog shows the user's text on
  that page, says it is deleted with the page, and offers "Export it first"
  next to "Forget" (question 5, answered).
- "Forget everything the memory noted" deletes all pages, including the
  user's text on them, after the confirmation says so.
- Deleted page text is handled like all deleted text: `secure_delete`, and
  the scrub of ADR 0010.

### Export

- An action in the Memory view: "Export pages…", for one page, a choice of
  pages, or all.
- Lax shows a folder picker. The folder is chosen in Rust (AppKit's
  `NSOpenPanel`, through `objc2-app-kit`, which Lax already uses; a new
  feature of it, no new crate; to verify), so the web UI never hands Lax a
  path to write to.
- Before writing, a notice: "These pages will be saved as plain Markdown
  files, not encrypted. Every program you run can read them, Spotlight may
  index them, and backups and sync copy them. Forgetting someone in Lax
  later doesn't reach this copy." If the chosen folder is inside iCloud
  Drive or another synced folder (the check that `synced` in
  `pages/mod.rs` does today), the notice also says the copy will leave
  this Mac, and the user confirms that separately.
- Export needs Touch ID or the login password at that moment (ADR 0012).
- Lax writes a new folder, `Lax pages <date>` (`0700`), inside the chosen
  one, with the layout the pages have today (`people/`, `organisations/`,
  `projects/`, `about-me.md`), files `0600`, each page's whole text with
  its markers. It never overwrites: if the folder exists, it gets a number.
  Logs hold the count, never names or paths.
- Lax doesn't keep track of exported copies, and doesn't read them back.
  Importing is not part of this ADR.

**Relation to ADR 0010, open question 9** ("A plaintext export?",
recommended "not now", answered as recommended). Moving the pages in
without a way out would break the reason for ADR 0005 and GUARDRAILS §6's
promise that the user can export their data. So the export of pages
belongs in this ADR, and ships with it. An export of the journal, captures
or anything else waits for its own decision, as ADR 0010's answer to
question 9 says.

### Moving the existing pages in

This runs once, in the background, only on a database that is already
encrypted (ADR 0010). Pages never move into a plaintext database: until
the conversion is done, they stay in `~/Lax` and Lax works as today.

1. **Stop writing files.** The background round leaves pages alone while
   the move runs.
2. **Find each page's file** by the rules Lax uses now: the file at the
   page's `path` if it carries the page's id, else the first other file
   that does.
3. **Read it whole**, including every line the user added, and keep it as
   the page's `text`, unchanged. Edits the user made in Lax's block are
   adopted at the next round, as they would be today.
4. **Check each page:** read the file again and compare it, byte for byte,
   with the row as written, read back inside the transaction of step 5. A
   file that changed in between is read again. A file that can't be read, isn't UTF-8 or is
   over 1 MB is treated as missing (below), and left where it is.
5. **Switch over in one transaction:** all the texts, and a setting that
   says pages now live in the database. If Lax stops before the commit,
   nothing changed and the move runs again at the next start. After it,
   Lax reads and writes only the rows.

ADR 0008's rules carry over:

- **Missing** (no file carries the page's id): the page stays missing, with
  "Recreate page" and "Forget". It is never recreated or forgotten without
  asking.
- **Detached** (the file at its path has no markers): its text is moved in
  as the user's, and the page stays detached. Lax doesn't write to it.
- **Copies** (two files with the same id): the one chosen in step 2 is
  moved in. The others are left in place and listed.
- **Files Lax doesn't know** (no page id, or the id of a page that is gone):
  never moved and never deleted. They are the user's.

**Then Lax asks, one time**, in the Memory view: "Your pages are now in
Lax's encrypted database. Delete the old files in ~/Lax?" with **Delete the
old files**, the default button, and **Keep them**. The text says:

- Edits to the old files are no longer read.
- Deleting doesn't reliably erase the old text from the disk: on APFS and
  SSDs, old blocks can stay until they're reused. FileVault, if it's on,
  keeps them unreadable. Time Machine backups and local snapshots keep the
  files until they expire.
- If they keep the files, they stay readable by every program, as now.

"Delete" removes only files that were moved in and still hold exactly the
text that was moved, then the folders left empty, then `~/Lax` if it is
empty. A file changed since the move is kept and listed. Deleting needs
Touch ID or the login password (ADR 0012), like any forget. After "Keep
them", the Memory view shows a quiet line with "Delete the old files", so
the user can still do it later; Lax doesn't ask again.

### What changes in the code

- `pages::sync` and `refresh` work on rows instead of files. `scan`,
  `ScanCache`, `write`, `write_inside`, `inside`, `free_path`,
  `prepare_folder`, `folder`, `folder_of` and `folder_problem` go, except
  what the move needs, which stays in a module of its own until the move
  has been in a release long enough to remove it. The `missing` state stays
  for pages that were missing at the move.
- **The symbolic-link and sync-folder checks on pages go**, because no page
  is written to a folder any more. What replaces them: the pages are in the
  data folder's database, `0700`, encrypted (ADR 0010), and the only way
  out is the export, which repeats the sync-folder check on the folder the
  user picks and asks before writing there.
- `MemoryStatus` loses `folder` and `folder_problem`; the Memory view drops
  "Open folder" and the folder warning.
- `forget::delete_all_data` and the forgets in `memory.rs` no longer need
  `pages::sync_now` to reach files.

### GUARDRAILS §6

Changed in its own pull request first, as the guardrails' preamble
requires:

- "All data lives in two folders owned by the user …" becomes one folder
  again: the data folder (database and logs), `0700`. The memory's pages
  are in the database. Pages the user exports go where they choose, after a
  notice that they are not encrypted.
- "Lax refuses to write pages if the memory folder is inside iCloud Drive or
  another synced folder" becomes: Lax warns, and asks again, before
  exporting into a synced folder.
- "Everything is stored in open formats (SQLite + Markdown) so the user can
  export, back up or inspect it without Lax" becomes: the database is
  SQLite (SQLCipher, ADR 0010), the pages are Markdown inside it, and Lax
  exports them as Markdown files on request.
- §2's data classes: "Memory pages: `about-me.md`, project and people
  pages" loses the file name.

### CLAIMS.md and tests

Rows that change:

- **"Text the user writes in a memory page is never changed by Lax":** the
  same claim. The code column names rows instead of files, and the update
  that compares the old text instead of `pages::write`.
  `pages::tests::a_page_changed_while_being_written_is_not_overwritten`
  becomes a test on rows. New: `an_edit_saved_while_lax_writes_is_kept`, and
  a page open in the editor is skipped.
- **"The memory's pages stay on this Mac, private, and are never forgotten
  or recreated without asking":** becomes "The memory's pages are kept in
  the encrypted database, leave it only when the user exports them, and are
  never forgotten or recreated without asking". The tests on the folder go:
  `synced_folders_are_refused`, `a_symlinked_subfolder_is_not_written_to`,
  `paths_stay_inside_the_folder`, `a_renamed_page_is_followed`,
  `a_copied_page_doesnt_take_over`, `unchanged_files_are_not_read_again`,
  and the private folder and file checks. Tests on the move and the export
  take their place.
- **"Facts the model writes on a page …":** `about-me.md` becomes the About
  me page; the notes come from the row.
- **"Nothing about a writing-assistant draft is stored …":** `about-me.md`
  becomes the About me page.

New rows:

- **"Moving the pages in keeps every line the user wrote, and the old files
  are deleted only if the user agrees and they still hold what was moved."**
  Tests: user lines inside and outside the block survive; missing, detached,
  copied and unknown files follow ADR 0008's rules; a file changed after
  the move is not deleted; a stop before the commit changes nothing; the
  move doesn't run on a plaintext database.
- **"Exported pages go only to the folder the user picked, after a notice
  that they aren't encrypted."** Tests: nothing is written without the
  confirmed notice and the lock's authentication; nothing is overwritten;
  folders `0700`, files `0600`; a synced folder needs the second
  confirmation; the export command takes no path from the web UI; logs hold
  only counts.

ADR 0010's encryption test adds a page's text, with a line of the user's
own, to what must not be readable on disk.

### Docs

USER-GUIDE ("Your memory in ~/Lax" becomes "Your memory pages": the
editor, export, and the one-time question), ARCHITECTURE (the diagram's
`~/Lax/` and the Learn section), README (step 4, "plain Markdown files in
`~/Lax`"), the site (`site/index.html`), ROADMAP (stage 4's row) and
CHANGELOG. ADR 0009 mentions `about-me.md`; it is an accepted ADR and
isn't rewritten.

## Relation to ADR 0005 and 0008

When this ADR is accepted, the status lines of 0005 and 0008 become
"Accepted; superseded in part by [0011](0011-pages-in-the-database.md)",
as `docs/adr/README.md` asks. Nothing else in them changes.

Superseded:

- **0005:** "plain Markdown files in a folder the user owns", and the
  consequence that users can edit, search, back up and sync the pages with
  any tool.
- **0008:** that pages are files any editor can change while Lax runs;
  "Files are written safely"; "Names and paths" (finding a page by its id
  in the folder); "The memory folder" in full; answer 1 of "Decided with
  the maintainer" (`~/Lax`, two folders); and the consequences "plain
  Markdown that any editor, Obsidian or git can use" and "two `0700`
  folders".

Kept:

- **0005:** Markdown as the pages' format, user edits as ground truth, and
  every model change versioned and reversible.
- **0008:** recognition, the two-day rule, the block and its markers, user
  edits win and the check that enforces it, rejected facts, batched
  updates, versions in SQLite, the special categories, forgetting, and
  "never forgotten or recreated without asking".

ADR 0005 rejected "database only" because the memory would become opaque
and locked into Lax. Here it is not opaque: the Memory view shows and edits
every page, whole. And it is not locked in: the export writes the same
Markdown files as today, on request.

## Open questions for the maintainer

All answered on 2026-10-13, as recommended (see "Maintainer's answers").
The Decision above follows the answers.

1. **One Markdown text per page, with the markers, or the user's text and
   Lax's block in two columns?** Recommended: one text. `block.rs` and its
   tests, including the hostile-input ones, carry over unchanged, and the
   move is a plain copy. Two columns would make the boundary structural (no
   markers to break), but rewrite the part that carries the guarantee. That
   can come later, in its own ADR. **Answered: one text.**
2. **A plain text editor, with no Markdown rendering?** Recommended: yes.
   Rendered Markdown means HTML from captured text. A preview that renders
   only headings, lists and emphasis as text styles could come later.
   **Answered: yes, plain text, no Markdown rendering.**
3. **Should Lax skip a page that is open in the editor?** Recommended: yes,
   and a save that still finds the page changed shows both texts, never
   drops the user's. **Answered: yes.**
4. **Keep versions of the user's own edits too?** Today only Lax's blocks
   and adopted edits are versioned; the user's text had their editor's
   undo, Time Machine or git. Recommended: yes, as `by = 'user'` versions,
   one per time the user opened the page and changed it (not one per
   save), the last 20 kept. They are the user's text, so a forget of
   episodes doesn't remove them; forgetting the entity does. **Answered:
   yes.**
5. **What happens to the user's text when an entity is forgotten?** Today
   the file stays as theirs. Recommended: it is deleted with the page, and
   the forget dialog shows it first, with "Export it first" next to
   "Forget". The user asked to forget that person; a row nobody sees
   shouldn't keep them. **Answered: it is deleted with the page, after the
   dialog shows it and offers "Export it first".**
6. **Delete or keep the old files by default?** Recommended: ask, with
   "Delete the old files" as the default button, and delete only files that
   still hold what was moved. **Answered: ask once, Delete as the default,
   and delete only files that still hold exactly what was moved.**
7. **Export with the markers?** Recommended: yes. The files look like the
   pages users know, and an import, if one comes, can read them back.
   **Answered: yes.**
8. **Export in this ADR, or later?** Recommended: in this ADR, pages only
   (see "Relation to ADR 0010"). Everything else in question 9 of ADR 0010
   stays open. **Answered: in this ADR, pages only.** An export of anything
   else waits for its own decision.
9. **Exporting into a synced folder: warn or refuse?** Recommended: warn
   and confirm separately. It is the user's explicit choice, and that is the
   consent GUARDRAILS §1 asks for. **Answered: warn and confirm
   separately.**
10. **Move the pages only on an encrypted database?** Recommended: yes.
    Moving them into a plaintext database gives up the editors and gains
    nothing. **Answered: yes.**

## Consequences

- The pages are encrypted with the database, from the move on. Programs
  that read files, backup tools without encryption, sync agents and
  Spotlight no longer have them. Old copies stay in free disk blocks,
  snapshots and backups until those are gone; FileVault covers the blocks.
- Forgetting an entity reaches its page in the same transaction, with no
  retries and no file left half-cleaned.
- The block writer's guarantee keeps its code and its tests.
- A lost database key (ADR 0010) now loses the pages too, including the
  user's own notes. The export is the user's own copy. ADR 0010's open
  question 2 ("No recovery key") was answered with this in mind: no
  recovery key, for 0.x.
- The move is one-way. It runs only on an encrypted database, which an
  older Lax can't open anyway (ADR 0010). The CHANGELOG says so.
- New code: the editor, the export, the move and its one-time question.

### What we give up

- **Editing in other editors.** No Obsidian, VS Code or TextEdit on the
  pages; only Lax's Memory view, while Lax is unlocked.
- **Search outside Lax.** No `grep` and no Spotlight over pages. Ask
  doesn't search page text either; a search in the Memory view could come
  later.
- **The user's own git.** ADR 0008 noted users could put `~/Lax` under
  git. Versions in Lax, and the export, are what's left.
- **Seeing the memory without Lax.** Reading pages now needs Lax, or
  SQLCipher and the key (ADR 0010), or an export.

## Alternatives considered

- **Keep the files, encrypt each one** (for example with a key from the
  Keychain): no editor could open them either, so the reason for files is
  gone. It adds crypto code of our own, and every edit would need a
  plaintext copy on disk.
- **An encrypted disk image** (`hdiutil`) mounted at `~/Lax`: editors keep
  working, but Lax writes pages in the background, so it would be mounted
  whenever Lax runs, and while mounted every program of the user can read
  it, and Spotlight indexes it. It protects copies and a Mac that is off,
  which FileVault and ADR 0010 already cover. ADR 0010 rejects the same for
  the data folder.
- **Keep plaintext files, with a warning.** Keeps the editors, git and
  Spotlight, and needs no new code. But it leaves the risk the maintainer
  wants closed, and the most condensed text Lax has would sit in plaintext
  next to an encrypted database holding the same facts.
- **Pages in the database, with a live Markdown mirror in `~/Lax`.** A
  mirror is the plaintext files again.

## Rollout

Each PR green on its own, labelled `security-sensitive`, after ADR 0010's
conversion is in.

0. **GUARDRAILS §2 and §6**, in their own PR, once this ADR is accepted.
1. **The `text` column and the block writer on rows**, with the folder code
   kept for pages that haven't moved. Tests on rows.
2. **The editor:** `page_text` and `save_page`, behind the app lock, and
   versions of the user's edits (question 4).
3. **The move** and the one-time question, with its tests. The
   maintainer's pages first, with a copy kept by hand.
4. **Export**, with the notice, the synced-folder check and the lock.
5. **Removing the folder code**, the folder's CLAIMS tests and
   `open_memory_folder`. Docs: USER-GUIDE, ARCHITECTURE, README, the site,
   ROADMAP, CHANGELOG.

The export (4) ships in the same release as the move (3), so pages are
never in the database without a way out.
