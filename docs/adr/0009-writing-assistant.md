# 0009. A writing assistant called with the Option key

- **Status:** Accepted
- **Date:** 2026-10-11

## Context

What people like most about Goldfish is one key press in any text field: it
drafts a reply, rewrites text or summarizes a thread, with the context it
already has. Lax has that context now: the journal, pages about the people and
projects the user works with, and `about-me.md` (ADR 0008). Issue #63 asks for
the same in Lax.

The maintainer's requirements:

- **Nothing in text fields until called.** Goldfish shows a fish in every text
  field, which is distracting. Lax appears only when the user double-taps
  ⌥ Option, and only in the field they are in.
- Typical use: in a reply, type "write a reply in my tone", press the key, and
  get a proper mail in place of the instruction.

This is the first time Lax would write into another app. Until now the model
can only return text that Lax shows (GUARDRAILS §5: "The model has no tools
that act"). The message being answered is untrusted, and may be written to
steer the draft ("include the recipient's account number").

On a Swedish Mac keyboard Option is used all the time (⌥2 is @, ⌥8 is [), so
the key on its own can't simply be a shortcut.

## Decision

### Calling Lax

- **Double-tap Option on its own**: two taps, the second starting within
  400 ms of the first ending. A tap is pressing and releasing ⌥ with no other
  key in between, within half a second. A single tap, ⌥2, ⌥-click and holding ⌥ do nothing.
- To tell a tap from ⌥2, Lax must notice that *some* key was pressed while ⌥
  was down. macOS delivers every key event with its key code, so Lax can't
  avoid receiving it; what it enforces is that the code goes no further. The
  callback turns each event into "a key went down" (or "modifiers changed")
  before anything else runs; the type it hands on has no key or character
  field; nothing is stored or logged. A test checks that type, and that the
  callback keeps nothing.
- Events are dropped unless the frontmost app is one Lax may read: in an
  excluded app, a password manager or a private window, nothing is looked at.
  Password fields are covered by macOS too: they turn on Secure Event Input,
  which keeps key events from every listener.
- **Permission:** a global key listener needs Accessibility (which Lax has)
  for an `NSEvent` global monitor; a `CGEventTap` would need Input Monitoring,
  a second, more alarming prompt ("receive keystrokes from any
  application"). Lax uses the `NSEvent` monitor, and onboarding says plainly
  that Lax notices when keys are pressed, never which.
- A setting turns the shortcut off, and it does nothing while capture is
  paused or the focused app, window or site is excluded. Password fields and
  private windows never get it.

### What appears

- A small panel by the text cursor: the salmon, the instruction, and the
  draft as it is written. Nothing is inserted yet.
- The instruction is the text the user typed in the field (or selected), or
  what they type into the panel.
- **Enter inserts the draft** in place of the instruction (or selection);
  Escape closes the panel and leaves the field as it was. Lax never presses
  send, never presses Enter in the other app, and never writes anywhere but
  the field the user called it to.

### Context for the draft

- The text of the focused window, read under the same rules as capture
  (exclusions first, secrets redacted), now, not stored.
- `about-me.md` for who the user is and how they write, and the pages of the
  people in the thread (sender and recipients, where the app shows them) and
  the project it's about. Not every name in the window: a hostile message
  could otherwise name people to pull their pages into a reply. The panel
  lists the pages used, and the model is told not to share facts about
  anyone but the people in the thread.
- All of it goes to the model as `Captured` data. The instruction is the
  user's request (`structured::ask_for`). The answer is validated as
  `{"draft": string}`: a length cap, no control or invisible characters.
- The local model, in turn with the journal and memory (`ModelTurn`). Remote
  models only through per-task routing with consent (#16, #20), when that
  exists.

### Inserting

- Text fields that accept it get the draft through Accessibility (replacing
  the selected text). Web views, such as Mail's compose window, get it by
  selecting the instruction and pasting, with the clipboard restored
  afterwards. The pasted item is marked concealed and transient
  (`org.nspasteboard.ConcealedType`, `org.nspasteboard.TransientType`), so
  clipboard managers that honour the convention don't keep it in their
  history. Either way the app's own undo (⌘Z) takes it back.
- Nothing about the draft is stored: not the instruction, not the draft, not
  the window text. Logs hold counts and timings.

## Consequences

- GUARDRAILS §5 changes. "The model has no tools that act" becomes: models
  have no tools; Lax inserts model text only into the field the user called it
  to, after the user confirms, and never sends. The change is made in the PR
  that builds this.
- Listening for "a key was pressed" is close to a key logger in what it could
  do, even if it doesn't. It needs the same care as capture: one module, no key
  identity in its types, excluded apps ignored, and a CLAIMS row with a test.
  The claim is what the code does with events, not that Lax can't see them.
- Clipboard managers that ignore the concealed marker can still keep a pasted
  draft. Fields that accept Accessibility writes avoid the clipboard entirely.
- A preview before inserting costs one key press, but a draft written from an
  untrusted message is never put in a field the user hasn't read.
- Small local models write passable mail in Swedish and English, but "in my
  tone" will be uneven at first. It gets better as `about-me.md` and the pages
  fill up.
- Pasting into web views briefly replaces the clipboard. If the user copies
  something in that moment, it can be lost; the window is a fraction of a
  second.

## Alternatives considered

- **A fixed shortcut such as ⌃⌥Space.** Simpler and no need to detect taps,
  but not what the maintainer asked for. It could be offered as a setting
  later.
- **A single tap of Option**, as Goldfish does. Faster, but easier to set off
  by accident while typing ⌥-characters; the maintainer chose the double tap.
- **Insert directly, without a preview.** One key press fewer, but a draft
  steered by a hostile message would land in the field unread.
- **An indicator in every text field**, as Goldfish does. Rejected by the
  maintainer.

## Decided with the maintainer

The proposal asked four questions, answered on 2026-10-12 (#69):

1. Lax is called by **double-tapping** Option, not a single tap.
2. The draft is shown in the panel first, and Enter inserts it.
3. It works in every app except the exclusions.
4. Nothing about a draft is stored. Learning the user's tone from sent mail can
   come later, through capture.

Later (2026-10-12) the maintainer asked to be able to pick the key, so Lax
doesn't clash with other apps such as dictation apps: ⌥ Option (the default),
only the right ⌥ Option, ⌃ Control, ⌘ Command or ⇧ Shift, double-tapped the same
way. Telling the right ⌥ from the left uses the modifier flags' device bits,
still never which other key was pressed.
