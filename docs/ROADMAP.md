# Roadmap

Lax is built in stages. Each stage delivers something usable on its own, and the
maintainer uses Lax daily from Stage 1 on, so problems surface early.

Each stage is a [GitHub milestone](https://github.com/fnyman-ctrl/lax/milestones)
with issues for the concrete tasks. Issues for later stages are added when we get
closer to them.

| Stage | Goal | Status |
|---|---|---|
| 0 | Foundation | ✅ Done |
| [1](https://github.com/fnyman-ctrl/lax/milestone/1) | Capture | 🚧 Next |
| [2](https://github.com/fnyman-ctrl/lax/milestone/2) | Model layer | Planned |
| [3](https://github.com/fnyman-ctrl/lax/milestone/3) | Journal | Planned |
| [4](https://github.com/fnyman-ctrl/lax/milestone/4) | Learning memory ⭐ | Planned |
| [5](https://github.com/fnyman-ctrl/lax/milestone/5) | Ask your memory | Planned |
| [6](https://github.com/fnyman-ctrl/lax/milestone/6) | Release 0.1 | Planned |

---

## Stage 0: Foundation ✅

Repository, license, [guardrails](GUARDRAILS.md), [architecture decisions](adr/),
security policy and hardened CI, all in place before the first line of app code.

## Stage 1: Capture

**Goal:** A menu bar app that reads the text of the active window and stores it
locally. No AI yet.

- Accessibility permission onboarding that explains what is and isn't read
- Capture app, window title, visible text and browser URL
- Never capture password fields, password managers or private windows; user-defined exclusions
- SQLite storage with full-text search
- Pause, retention, and "forget" controls
- A view of everything captured, with search
- A logging policy that keeps captured content out of logs

**Done when:** Lax runs through a full workday without being noticed, stays within
its performance budget, and never captures excluded content.

## Stage 2: Model layer

**Goal:** One way to talk to models, local or remote, under the rules in
[GUARDRAILS §3](GUARDRAILS.md#3-model-providers).

- OpenAI-compatible provider interface for chat and embeddings
- Local setup (Ollama, LM Studio) and a recommended default model for 16 GB Macs
- Opt-in remote providers with a consent flow and presets (e.g. STAIK, Berget AI)
- API keys in the macOS Keychain
- Redaction before anything is sent remotely
- Outbox log of everything that left the machine
- Per-task routing, and a menu bar indicator when remote processing is on
- Schema-validated model output

**Done when:** the same feature works with a local model and a remote provider,
and every remote request is redacted and visible in the outbox.

## Stage 3: Journal

**Goal:** Turn raw captures into a readable timeline of your day.

- Every ~15 minutes (and when idle), summarize recent captures into short episodes
- A daily timeline view
- Raw captures can expire once summarized

**Done when:** the journal for a normal workday is accurate and useful to read back.

## Stage 4: Learning memory ⭐

**Goal:** The core of Lax: a memory that learns who you are, what you work on and
who you work with.

- Recognize projects, people and organisations in episodes, without duplicates
- A Markdown page per entity, plus `about-me.md` ([ADR 0005](adr/0005-memory-as-markdown.md))
- Incremental page updates; user edits are ground truth and never overwritten
- Version history for every page, with undo
- Views to browse and edit the memory

**Done when:** after a week of use, the project and people pages are accurate
enough that you'd trust them, and corrections stick.

## Stage 5: Ask your memory

**Goal:** Get answers from your history.

- Semantic search over episodes and pages (local embeddings)
- Questions like "What did I do on Tuesday?" or "What's the status of project X?",
  with links to the sources

## Stage 6: Release 0.1

**Goal:** The first version others can install.

- Signed and notarized macOS app, built by CI with build provenance
- First-run onboarding: hardware check, local or remote setup, permissions
- User documentation
- Release process documented

## Later

Ideas for after 0.1, roughly in order of interest. None of them are committed.

- **Writing assistant:** press a key in any text field to draft replies with context from your memory
- **Meetings:** local transcription with Whisper (e.g. KB-Whisper for Swedish)
- **Time tracking:** time per project, derived from the journal
- **MCP server:** read-only access to your memory for other local tools
- **OCR fallback** for apps that don't expose text through Accessibility
- **Encrypted database** (SQLCipher)
- **Windows and Linux** support
