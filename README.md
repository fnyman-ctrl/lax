<img src="src-tauri/icons/128x128@2x.png" width="96" alt="The Lax app icon: a pixel-art silver salmon">

# Lax

[![CI](https://github.com/fnyman-ctrl/lax/actions/workflows/ci.yml/badge.svg)](https://github.com/fnyman-ctrl/lax/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/fnyman-ctrl/lax/badge)](https://scorecard.dev/viewer/?uri=github.com/fnyman-ctrl/lax)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

**A private, fully local memory for your Mac that learns who you are and what you work on.**

🐟 **[fnyman-ctrl.github.io/lax](https://fnyman-ctrl.github.io/lax/)**

Lax quietly follows what you do on your computer and turns it into a living memory:
which projects you're working on, who you work with, and what happened when.
Everything runs on your machine by default: no cloud and no API keys unless you
choose a remote model, and no telemetry, ever. Nothing leaves your computer unless
you decide it should.

> Salmon (*lax* in Swedish) find their way back to the exact river they were born
> in, years later and thousands of kilometres away. That's the kind of memory we're after.

> ⚠️ **Status: early development.** Lax captures the text of the active window, lets you
> see, search and forget everything it captured, writes a journal of your day, learns who
> and what you work with, answers questions about your work, and drafts text in any text
> field, all with AI models on your Mac. The journal, the memory and the writing
> assistant are off until you turn them on. See the [roadmap](docs/ROADMAP.md), follow
> along or help out!

## Get Lax

**Build it yourself** (works today). You need macOS, the Xcode Command Line
Tools (`xcode-select --install`), [rustup](https://rustup.rs) and
[Node.js](https://nodejs.org). Then:

```sh
git clone https://github.com/fnyman-ctrl/lax.git
cd lax
npm run build:app
open ~/Applications/Lax.app
```

A Lax you build yourself opens without warnings and needs no Apple account.
Lax then asks for the Accessibility permission and explains what it reads. For
the AI parts, run a local model server such as [LM Studio](https://lmstudio.ai)
or [Ollama](https://ollama.com) and pick its models under **Models**
([docs/MODELS.md](docs/MODELS.md)). After building a new version, macOS may ask
you to turn Lax on under Accessibility again, since each build has a new
signature.

**Download for Mac** (later): one `.dmg`, signed and notarized by Apple, so it
opens like any other app.

[docs/USER-GUIDE.md](docs/USER-GUIDE.md) explains the first start, each
section, the writing assistant and what to do when something doesn't work.

## Why

Personal memory apps have shown how useful it is to have a computer that remembers
what you did. But many of them send your screen content to cloud AI providers, even
when the data itself is stored locally. Your screen is the most private thing you have. Lax uses local models by
default, so the privacy promise is simple: **your data stays on your Mac unless you
decide otherwise.**

## What it does

1. **Capture** (works today): reads the text of the active window (app, window
   title, content) through the macOS Accessibility API. Password fields, private
   browser windows and password managers are never read, banking and health are
   excluded from the start, and you can exclude any app, website or window.
   On-device OCR as a fallback comes later.
2. **Summarize** (works today, opt-in): every few minutes a model on your Mac turns
   raw activity into short, readable episodes ("Read Anna's mail about the Q3 offer
   and updated the roadmap"). See [docs/MODELS.md](docs/MODELS.md) for the
   recommended local models.
3. **Recognize** (works today, opt-in): picks out the projects, people and
   organisations in those episodes. You can rename, merge, keep or forget them,
   and tell Lax never to learn about someone.
4. **Learn** (works today, opt-in): keeps a page per project and person, plus an
   *About me* page, as plain **Markdown files** in `~/Lax` that you can read and
   edit. What you write there is never overwritten, and facts you delete stay
   deleted.
5. **Ask** (works today): a conversation about your work. Ask "What did I do
   yesterday?" or "What did I work on with Anna last week?", then ask on ("and
   the day before?"). Answers come from your journal and memory, with the entries
   they're based on. Ask in English or Swedish.
6. **Write** (works today, opt-in): double-tap ⌥ Option (or a key you pick, so it
   doesn't clash with other apps) in any text field, and a small pixel salmon
   swims to your cursor. Tell it what to write ("reply in my tone and thank her
   for the offer"), or select a paragraph and ask it to make it shorter. It
   drafts the text from what Lax knows and the window you're in, on Liquid Glass
   on macOS 26 and later. You read and edit it; Enter puts it in the field.
   Lax never sends anything.

Lax keeps your Mac quiet while you use it: it reads nothing once you've left the
computer for ten minutes, the memory's model work waits until you're away from
the keyboard, and nothing runs in the background on battery.

Later: meeting transcription (Whisper), time tracking, and a local MCP server so
other local tools can use your memory.

## Principles

- **Local by default.** Out of the box, all models run on your machine (via
  [LM Studio](https://lmstudio.ai), [Ollama](https://ollama.com) or similar) and
  nothing is sent anywhere. Lax 0.1 is local only.
- **Your choice of model.** Lax works with any OpenAI-compatible API. After 0.1,
  if your computer can't run a local model, you'll be able to pick a
  privacy-focused provider instead: opt-in, redacted first, and every request
  logged for you to inspect.
- **Your data, open formats.** Memory is stored as Markdown + SQLite in a folder you own.
- **Transparent.** You can always see what was captured, what Lax believes about you,
  and what was sent off your machine. You can pause, exclude apps or delete anything.
- **No telemetry.** Ever.

These principles are spelled out as concrete rules in [docs/GUARDRAILS.md](docs/GUARDRAILS.md),
and [docs/CLAIMS.md](docs/CLAIMS.md) shows the test behind each one.

## Tech stack

- [Tauri](https://tauri.app) (Rust backend, React/TypeScript UI)
- SQLite with FTS5 full-text search, and episode vectors for search by meaning
- Any OpenAI-compatible model API: local (LM Studio, Ollama…), remote after 0.1
- The macOS Accessibility API to read windows and write drafts; AppKit for the
  writing assistant's panel and Liquid Glass
- Rust tests and live model tests, Vitest for the window
- Later: Apple Vision OCR and ScreenCaptureKit as a fallback for apps without
  text, and Whisper for audio, following [Handy](https://github.com/cjpais/Handy)

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details, and
[docs/adr/](docs/adr/) for why things are built the way they are. The look and the mascot are
described in [docs/BRAND.md](docs/BRAND.md).

## Contributing

Ideas, questions and feedback are very welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).
To report a security or privacy problem, see [SECURITY.md](SECURITY.md).

## Acknowledgements

Lax builds on ideas from [Handy](https://github.com/cjpais/Handy) by CJ Pais
(MIT), a great offline speech-to-text app, and on ideas from
[Mason](https://github.com/Baltsar/mason) by Gustaf Garnow (MIT), a local memory
of how you build with AI agents.

## License

Lax is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0).

You're free to use, study, change and share Lax. If you distribute a modified
version, or run one as a network service, you must make your source code
available under the same license. Lax is about keeping your data in your hands,
and the license keeps the code that way too.

Parts derived from [Handy](https://github.com/cjpais/Handy) remain under their
original MIT license, with the copyright notice kept in those files.

Lax is built with [SQLCipher](https://www.zetetic.net/sqlcipher/) by Zetetic
LLC, under a BSD-style license. Its notice is in
[`src-tauri/licenses/SQLCipher.txt`](src-tauri/licenses/SQLCipher.txt) and in
every Lax.app.
