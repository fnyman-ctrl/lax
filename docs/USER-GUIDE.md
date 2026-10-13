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
Privacy & Security → Accessibility** again.

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

- **Ask:** a conversation about your work. "Vad gjorde jag igår?", "What did I
  work on with Anna last week?", then ask on: "and the day before?". Each answer
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

1. In any text field, write what you want ("svara i min ton och tacka för
   offerten"), or select text you want changed.
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

## Privacy, in short

- Lax reads text through Accessibility, never screenshots, and never password
  fields, password managers or private browser windows.
- Secrets such as passwords, keys and card numbers are masked before anything is
  stored.
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
- **Nothing is captured after an update.** Turn Lax on again under Accessibility.
- **The salmon doesn't come.** Check that the writing assistant is on under
  **Write**, that the app isn't excluded, and that Lax isn't paused. It never
  comes to password fields.
- **The salmon goes to the wrong place.** Some apps report the text cursor
  wrongly; Lax then goes by the mouse pointer. Click in the field first.
- **The fans spin.** The memory's work runs while you're away from the keyboard,
  and never on battery. `npm run perf` shows how Lax is doing.

Found a bug? [Open an issue](https://github.com/fnyman-ctrl/lax/issues), and
leave out anything personal.
