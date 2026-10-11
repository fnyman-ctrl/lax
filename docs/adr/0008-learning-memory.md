# 0008. How the memory learns, and how user edits are protected

- **Status:** Proposed
- **Date:** 2026-10-11

## Context

Stage 4 is the core of Lax: a memory that learns who the user is, what they work
on and who they work with. [ADR 0005](0005-memory-as-markdown.md) settled that
this memory is a folder of Markdown pages, one per project, person and
organisation, plus `about-me.md`, and that user edits are ground truth.
[ARCHITECTURE.md](../ARCHITECTURE.md) sketches the pipeline: episodes are
recognised into entities, and entities are learned into pages.

What is still open is how that works in practice:

- Episodes name many things that aren't worth a page: a person in a news
  article, a project mentioned once. A memory full of noise is a memory nobody
  trusts.
- The same entity shows up under different names ("Anna", "Anna Example",
  "anna@example.com"; "Q3 offert", "Q3 proposal").
- GUARDRAILS §5 promises that text the user wrote or corrected "cannot be
  overwritten by the model". A prompt telling the model not to is not a
  guarantee. It has to hold even when the model ignores its instructions, for
  example because a captured page told it to.
- Pages are files the user can edit at any time, in any editor, while Lax runs.
- A small local model (Gemma 4 E4B on a 16 GB Mac takes about 35 seconds per
  journal entry) can't rewrite dozens of pages every few minutes.
- Forgetting must reach the memory: GUARDRAILS §4 says deleting a time range,
  an app or an entity deletes everything derived from it.

## Decision

### Recognize: entities in SQLite, from episodes only

- After each new episode, the memory asks the chat model for the people,
  organisations and projects it names, using the existing `Entities` shape in
  `src-tauri/src/models/structured.rs`. It reads the episode summary, not the
  raw captures: the summary has already filtered out what wasn't the user's own
  work, and keeps each step reading only the previous step's output.
- Entities live in SQLite: `entities` (kind, display name, state), `aliases`
  (every name an entity has been seen under) and `mentions` (entity, episode).
  Names are matched on a normalized form: case-folded, whitespace collapsed,
  diacritics **kept**, so "Åsa" and "Asa" stay two people.
- **Resolution is conservative.** A name joins an existing entity only on an
  exact alias match within the same kind. Anything else becomes a new entity.
  Duplicates are cheaper to fix than wrong merges: merging is one click in the
  Memory view, and adds the other names as aliases, so it sticks. Suggesting
  merges with embeddings comes later, with Stage 5's local embeddings, and stays
  a suggestion.
- **An entity earns a page.** A new entity is a *candidate* until it has been
  mentioned in episodes on at least two different days. Only then does Lax write
  its page. The user can promote a candidate early, or dismiss it.
- **Dismissed and forgotten names are remembered as names only**, so they
  aren't recreated: a dismissed name is never shown again, a forgotten entity's
  aliases are kept as a short block list that the user can see and clear.
- The user is not an entity. Lax asks for the user's name (and other names
  they go by) when the memory is turned on, and those names go to `about-me.md`
  instead.

### Learn: Lax writes only inside its own block

Each page is a Markdown file:

```markdown
# Anna Example

Anything here is the user's. Lax never changes it.

<!-- lax:begin: written by Lax. Edit freely; your edits are kept. -->
- Works at Exempelbolaget AB on the Q3 launch.
- Usually in touch over Slack and Mail.
<!-- lax:end -->
```

- **Lax only ever replaces the text between its two markers.** Everything
  outside them belongs to the user. This is enforced by the code that writes
  the file, not by the prompt: the writer reads the file, swaps the block and
  checks that every byte outside the block is unchanged before it writes. A
  test feeds it hostile model output to prove it.
- **When the user edits Lax's block, the edit becomes the user's.** Lax keeps a
  hash of each block it wrote. If the block on disk no longer matches, Lax moves
  the edited text out of its markers into the user's part of the page and
  starts an empty block below it. From then on that text is ground truth, and
  the model only adds what it doesn't already say.
- **Updates are batched.** Mentions mark a page as stale. Once an hour, while
  the Mac is idle and capture isn't paused, Lax updates at most a few stale
  pages, most-mentioned first. The model gets the user's text (as ground truth
  it must not contradict), its current block and the new episodes, and returns
  the new block as JSON (`{"facts": [string]}`). The answer is validated like
  every other model answer: a capped number of short lines, no control or
  invisible characters, no HTML, no marker text. Anything else is discarded.
- **Every change is versioned** in SQLite: each block Lax writes, and each user
  edit it notices, is stored as a version of the page. The Memory view shows
  the history and can restore any version. Restoring writes a new version, so
  undo can be undone.
- **Files are written safely.** Lax writes to a temporary file and renames it
  over the page. Just before the rename it checks that the page hasn't changed
  since it read it, and if it has, it tries again next round.
- **Deleting a page file forgets the entity.** If the user deletes a page
  outside Lax, Lax treats it as "forget this", the same as the button.
- **Names and paths.** Pages live in `people/`, `organisations/` and
  `projects/`, named by a slug of the display name that keeps Swedish letters
  (`åsa-lind.md`). Lax tracks pages by an ID in SQLite, so the user can rename a
  file and Lax follows it.

### What the memory may not record

Like the excluded-by-default sites, some facts don't belong in the memory even
though Lax could infer them: health, religion or beliefs, political views,
sexual orientation, ethnicity and union membership (the special categories in
GDPR art. 9). The prompts tell the model so, and the Memory view makes any
line easy to remove. Prompts can't guarantee this, so it is a rule for the
model, not a promise to the user, and the docs say so.

### The memory folder

- The memory lives in `~/Lax/`, created with permissions `0700` like the data
  folder, and Lax checks it isn't inside iCloud Drive, Dropbox, Google Drive or
  OneDrive before using it. A synced folder would send the memory off the Mac,
  which GUARDRAILS §1 doesn't allow without consent. Choosing another folder,
  and syncing it knowingly, can come later with its own consent screen.
- GUARDRAILS §6 says "one folder". It becomes two, both `0700`: the data folder
  (database, logs) and the memory folder (pages). The guardrail is updated in
  the same pull request that creates the memory folder.

### Forgetting reaches the memory

- Forgetting a time range or an app deletes its episodes (as today), and with
  them their mentions. Pages that drew on a deleted episode are marked for
  rebuild: Lax rewrites their block from the episodes that remain, and deletes
  the versions written from the forgotten episodes, so what was forgotten
  doesn't live on in the history. The user's own text is never touched by a
  forget.
- Forgetting an entity deletes its page, its versions, its mentions and its
  aliases (keeping the names on the block list above), and removes it from
  other pages' blocks at their next update.

### Remote models

Entity recognition and page updates are model tasks like summarizing. They run
on the local model until remote providers and per-task routing exist (#16,
#20), and then follow the same rules: opt-in per task, redacted, and logged in
the outbox.

### Build order

Each step is usable and reviewable on its own:

1. Recognize: tables, extraction after each episode, resolution, forget
   cascades. No pages yet.
2. Memory view: entities and candidates, with merge, rename, promote, dismiss
   and forget.
3. Page files: the block writer, user-edit detection, versions, the folder
   checks. Tested without a model.
4. Page updates with the model, batched and validated.
5. `about-me.md`.
6. History and undo in the Memory view.

## Consequences

- The guarantee that user text survives doesn't depend on the model behaving.
- Pages are plain Markdown that any editor, Obsidian or git can use. The marker
  comments are visible in editors that don't hide HTML comments.
- Conservative resolution means some duplicates until the user merges them, or
  until embedding suggestions arrive.
- The two-day threshold means a new colleague gets a page on the second day
  they show up, not the first.
- Moving user-edited text out of the block means Lax can't later correct that
  text itself. That is the point, but a page edited often ends up mostly user
  text, with a small Lax block of new facts below it.
- Hourly, capped updates keep the load low, so pages lag the journal by up to a
  few hours on a busy day.
- Two `0700` folders instead of one, and a GUARDRAILS change to match.

## Alternatives considered

- **Let the model rewrite the whole page, told to keep the user's text.** Simple,
  but the guarantee would rest on the model following instructions, which is
  exactly what prompt injection breaks.
- **Detect user edits with a diff and keep them line by line.** Finer-grained,
  but fragile when the user reorders or rewrites lines, and hard to explain.
  Moving the whole edited block is coarse but predictable.
- **Versions in a git repository inside `~/Lax/`.** Familiar, but needs `git` on
  the Mac or a new dependency, and a `.git` folder the user might push somewhere.
  SQLite versions are already backed up with the rest of the data. Users can
  still put the folder under git themselves.
- **Extract entities from raw captures.** More names, mostly noise: everything
  on every page the user looked at.
- **Merge entities automatically with embeddings.** Wrong merges mix two
  people's facts on one page, which is worse than a duplicate.
- **Pages in the app's data folder.** One folder, as GUARDRAILS §6 says today,
  but hidden in `~/Library`, which defeats ADR 0005's point of a memory the user
  can see and edit with their own tools.
