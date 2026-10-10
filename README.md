# Lax 🐟

[![CI](https://github.com/fnyman-ctrl/lax/actions/workflows/ci.yml/badge.svg)](https://github.com/fnyman-ctrl/lax/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/fnyman-ctrl/lax/badge)](https://scorecard.dev/viewer/?uri=github.com/fnyman-ctrl/lax)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

**A private, fully local memory for your Mac that learns who you are and what you work on.**

Lax quietly follows what you do on your computer and turns it into a living memory:
which projects you're working on, who you work with, and what happened when.
Everything runs on your machine by default: no cloud and no API keys unless you
choose a remote model, and no telemetry, ever. Nothing leaves your computer unless
you decide it should.

> Salmon (*lax* in Swedish) find their way back to the exact river they were born
> in, years later and thousands of kilometres away. That's the kind of memory we're after.

> ⚠️ **Status: early development.** Lax runs in the menu bar and capture can be paused,
> but it doesn't capture anything yet. See the [roadmap](docs/ROADMAP.md), follow along
> or help out!

## Why

Personal memory apps have shown how useful it is to have a computer that remembers
what you did. But many of them send your screen content to cloud AI providers, even
when the data itself is stored locally. Your screen is the most private thing you have. Lax uses local models by
default, so the privacy promise is simple: **your data stays on your Mac unless you
decide otherwise.**

## What it will do

1. **Capture**: read the text of the active window (app, window title, content)
   using macOS Accessibility APIs, with on-device OCR as fallback.
2. **Summarize**: every few minutes a local LLM turns raw activity into short,
   readable episodes ("Worked on the Q3 proposal in Google Docs, emailed Anna about delivery").
3. **Recognize**: extract projects, people and organisations from those episodes.
4. **Learn**: maintain a page per project and person, plus an *About me* page,
   kept as plain **Markdown files** you can read, search and edit. When you
   correct a page, Lax learns from the correction.

Later: search and Q&A over your history, meeting transcription (Whisper),
time tracking, and a local MCP server so other local tools can use your memory.

## Principles

- **Local by default.** Out of the box, all models run on your machine (via
  [Ollama](https://ollama.com) or similar) and nothing is sent anywhere.
- **Your choice of model.** Lax works with any OpenAI-compatible API. If your
  computer can't run a local model, you can pick a privacy-focused provider
  instead. That's opt-in, data is redacted first, and every request is logged
  for you to inspect.
- **Your data, open formats.** Memory is stored as Markdown + SQLite in a folder you own.
- **Transparent.** You can always see what was captured, what Lax believes about you,
  and what was sent off your machine. You can pause, exclude apps or delete anything.
- **No telemetry.** Ever.

These principles are spelled out as concrete rules in [docs/GUARDRAILS.md](docs/GUARDRAILS.md),
and [docs/CLAIMS.md](docs/CLAIMS.md) shows the test behind each one.

## Tech stack

- [Tauri](https://tauri.app) (Rust backend, React/TypeScript UI)
- SQLite (FTS5 full-text search, `sqlite-vec` for semantic search)
- Any OpenAI-compatible model API: local (Ollama, LM Studio…) by default, remote if you choose
- Apple Vision OCR, ScreenCaptureKit, Accessibility API
- Whisper for audio, building on the approach of [Handy](https://github.com/cjpais/Handy)

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details, and
[docs/adr/](docs/adr/) for why things are built the way they are.

## Contributing

Ideas, questions and feedback are very welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).
To report a security or privacy problem, see [SECURITY.md](SECURITY.md).

## Acknowledgements

Lax builds on ideas and code from [Handy](https://github.com/cjpais/Handy) by CJ Pais
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
