# Architecture

Lax is a memory pipeline with four stages. Each stage only reads the output of the
previous one, so they can be built, tested and improved independently.

```
 Capture ──► Summarize ──► Recognize ──► Learn
 (raw text)   (episodes)    (entities)    (Markdown pages)
     └───── encrypted SQLite, pages in pages.text ─────┘
                                                      └── Markdown export
```

The database is encrypted with SQLCipher. Its random key lives in the login
keychain, and Lax opens the database on a thread of its own, so a Keychain
prompt never freezes the window. See
[ADR 0010](adr/0010-encrypted-database.md).

## 1. Capture

- Lax runs in the menu bar. While capture is paused, nothing at all is read.
- Every few seconds, read the focused app, window title and visible text through
  the macOS Accessibility API (`AXUIElement`). This needs the Accessibility
  permission, which the user grants in System Settings after onboarding explains it.
  The order is fixed: pause, app, window title, then the site of each web page,
  and only then any text (`src-tauri/src/capture.rs`). Reading is bounded by
  depth, element count, text length and time, and each request to another app
  times out, so a hung app can't block Lax. All the macOS calls are in
  `src-tauri/src/ax.rs`.
- In browsers only the page is read, never the address field, which holds the
  full address. The page's navigation, header and footer, which repeat on every
  page, are skipped.
- Reading slows down while the user is away from the keyboard or on battery,
  and stops after ten minutes without input until they are back. A window
  whose text keeps changing is stored at most every 30 seconds.
  See [PERFORMANCE.md](PERFORMANCE.md).
- Later: if an app exposes no accessible text, fall back to a screenshot of the
  window (ScreenCaptureKit) and on-device OCR (Apple Vision), with the screenshot
  discarded after OCR. Not built yet.
- Skip unchanged content (hash-based dedup) and anything on the exclude list
  (password managers, private browser windows, user-chosen apps).
- Mask secrets (passwords, keys, tokens, card numbers, personal identity numbers) in
  the text and window title, then store as `captures` rows in SQLite: timestamp, app,
  window title, site (host only, if browser), text.

## 2. Summarize

- Every ~15 minutes (and when idle), group recent captures into time blocks and
  ask the local LLM to write short **episodes**: what was done, in which app, with whom.
  The journal is off until the user turns it on and has chosen a chat model
  (`src-tauri/src/journal.rs`). It skips captures younger than two minutes, sends
  each window once with its latest content, and stops while capture is paused.
  No background work uses the model while the Mac runs on battery; it catches
  up on mains power (`journal::may_use_model`). Questions are not background
  work.
- Episodes are what the rest of the system reads. A raw capture expires only after
  it has been summarized, or at the maximum age (default 90 days, configurable).

## 3. Recognize

- Extract **entities** (people, organisations, projects) from new episodes
  using structured LLM output (JSON). The memory is off until the user turns
  it on, and runs in the journal's loop right after it, but only after two
  minutes without keyboard or mouse input, so the Mac stays quiet while it's
  used (`src-tauri/src/memory.rs`, [ADR 0008](adr/0008-learning-memory.md)).
  Episodes are given their vectors for Ask the same way.
- Resolve them against known entities to avoid duplicates. For now only an
  exact match of the normalized name (case and spacing ignored, diacritics
  kept) joins a known entity; embeddings can suggest merges later, such as
  the same project named in Swedish and in English ("Q3 offert" and "Q3
  proposal").

## 4. Learn

- A new name that isn't seen again fades: not mentioned for 14 days (28 if
  mentioned 3 times or more) before the newest journal entry, it is deleted
  like a forget, but not put on the "don't learn" list
  ([ADR 0013](adr/0013-tidying-the-memory.md)).
- Merge suggestions (`src-tauri/src/store/pairs.rs`, ADR 0013): organisations
  equal once a legal suffix and punctuation are taken off, with the same
  suffix or one without, are listed as obvious pairs, each merged into the
  name with a page, else a kept one. Other pairs with a name signal (one
  name's words inside the other's, a different legal suffix, a letter or two
  apart) and at least one known side are judged by the local model
  (`src-tauri/src/merges.rs`), at most two per round while the user is away,
  from up to three entries and the page facts of each name. Verdicts are kept
  in `pair_verdicts`, go with any change to either name's mentions, and a
  "no" or "unsure" is judged again after 30 days or twice the mentions. The
  user merges; "Not the same" is kept in `not_same` and is final. The Change
  panel's "Same as" list puts the likely names first (`Store::likely_same`).
- Each entity Lax keeps (mentioned on two days, or kept by the user) has a
  page, plus the About me page (`src-tauri/src/pages`). A page is one
  Markdown text in the `text` column of its row in `pages`, in the encrypted
  database, and the user edits it in Lax's Memory view
  ([ADR 0011](adr/0011-pages-in-the-database.md)). Lax writes only between its
  own markers, and versions every block in SQLite.
- Until the pages have moved in, they are files in `~/Lax`. The move runs
  once, only on an encrypted database, in one transaction
  (`src-tauri/src/pages/moving.rs`). The code that writes pages works on
  either, through `Shelf`: files in `~/Lax` while the database is still
  plaintext, rows after. Nothing is deleted by the move. Lax asks once whether
  to delete the old files, and then removes only those that still hold what
  was moved.
- The editor (`src/PageEditor.tsx`) shows a page as plain text, never as
  HTML, and saves over the text it loaded, so a page that changed meanwhile
  is never overwritten. A page open in it is not written by the background
  round.
- A page leaves the database only through the export
  (`src-tauri/src/pages/export.rs`): Markdown files in a new folder in a
  folder the user picks with macOS's folder picker, chosen in Rust so the
  window never hands Lax a path to write to, after a notice that the copy is
  not encrypted.
- About once an hour, while the user is away, the model updates the facts on
  pages with new mentions (`src-tauri/src/pages/facts.rs`); facts the user
  deletes are never written again. The About me page is about the user:
  who and what they worked with most in the last 30 days, and, once a day,
  what the model learned about their work.
- Pages are updated incrementally: the LLM gets the current page plus new episodes
  and proposes an updated page.
- **User edits win.** If the user edits a page, the edit is treated as ground truth
  and the model is instructed not to contradict it.

## Ask

- A question typed into Lax is answered from the journal by the chat model
  (`src-tauri/src/ask.rs`), as a conversation: the latest turns go along as
  one captured block, and a follow-up with no names or words of its own takes
  the time, names and words of the question before it. The entries are found without a model call: a
  time in the question ("last Tuesday", "last week", and the same in Swedish),
  names the memory knows,
  and a full-text search, within a size budget. The answer names the entries
  it is based on. With an embedding model chosen, episodes get vectors in the
  background (`src-tauri/src/vectors.rs`), and Ask also finds entries close in
  meaning to the question.
- The prompt starts with what changes least: the instructions, what the memory
  knows about the user, then about the people and projects named, the journal
  entries, the conversation so far, and last the question with today's date.
  A server that keeps a prompt cache, as LM Studio does, reads again only
  from the first part that changed: for a follow-up that finds the same
  entries ("and what else?"), only the conversation's new turn and the
  question. Other entries, or another person named, change it earlier.
- The answer is shown as the model writes it: the reply streams from the
  server, the text of its `answer` field goes to the window as plain text
  (`src-tauri/src/models/partial.rs`), and when the reply is complete it is
  validated as always and replaces what was shown, or the error does
  (GUARDRAILS §5).

## Write

- With the writing assistant on, a double tap of ⌥ Option (or the modifier
  the user picks: right ⌥ only, ⌃, ⌘ or ⇧) in a text field calls Lax there ([ADR 0009](adr/0009-writing-assistant.md),
  `src-tauri/src/summon.rs`). To tell the tap from ⌥2, Lax notices that keys
  go down, never which, and only while the user has it on. Before it looks at
  the window, Lax makes the same checks as capture (app, window title, private
  browser window; `capture::admit`), and it never comes to a password field.
  The salmon swims in to the text cursor; if the app names a field that is
  hidden (behind another window or view), it goes by the mouse pointer. The
  instruction is the text selected in the field, or else the paragraph at the
  cursor; the window is read as capture
  reads it, for the app Lax was called to (`src-tauri/src/summon.rs`). On
  Enter, the chat model writes a draft from the instruction, the About me page,
  the pages of people and projects named in the instruction or in the
  thread's header lines at the top of the window (From, To, Cc; not every
  name in the window), and the window's text, all as `Captured` data
  (`src-tauri/src/write.rs`). Secrets are redacted first, and nothing about
  a draft is stored. The user can edit the draft; Enter puts it into the field
  in place of the instruction (after checking it is still there), through
  Accessibility where the app takes it,
  or else pasted with the clipboard marked concealed and transient and put
  back afterwards (`src-tauri/src/insert.rs`). Lax never presses Return or
  Send. On macOS 26 and later the card sits on a native Liquid Glass view
  (`src-tauri/src/glass.rs`).

## Models

| Task | Default | Notes |
|---|---|---|
| Summaries, extraction, page updates | Local 4–8B instruct model, or an opt-in remote provider | Must handle Swedish and English |
| Embeddings | Small local embedding model | For entity resolution and search |
| Audio (later) | Whisper (e.g. KB-Whisper for Swedish) | Following Handy's approach |

All model calls go through one provider interface speaking the OpenAI-compatible
API, so any local server (Ollama, LM Studio, llama.cpp, MLX) or opt-in remote
provider can be used without code changes. See
[ADR 0004](adr/0004-openai-compatible-model-providers.md).

## Privacy and security

See [GUARDRAILS.md](GUARDRAILS.md). Every stage above must follow it.
