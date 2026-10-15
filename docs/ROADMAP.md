# Roadmap

Lax is built in stages. Each stage delivers something usable on its own, and the
maintainer uses Lax daily from Stage 1 on, so problems surface early.

Each stage is a [GitHub milestone](https://github.com/fnyman-ctrl/lax/milestones)
with issues for the concrete tasks. Issues for later stages are added when we get
closer to them.

| Stage | Goal | Status |
|---|---|---|
| 0 | Foundation | ✅ Done |
| [1](https://github.com/fnyman-ctrl/lax/milestone/1) | Capture | 🚧 Almost done: a workday measurement is left (#11). Reads nothing while you're away |
| [2](https://github.com/fnyman-ctrl/lax/milestone/2) | Model layer | 🚧 Local models done, quiet in the background and on battery; remote providers in 0.3 |
| [3](https://github.com/fnyman-ctrl/lax/milestone/3) | Journal | ✅ Done, opt-in; raw text goes 30 days after it's summarized |
| [4](https://github.com/fnyman-ctrl/lax/milestone/4) | Learning memory ⭐ | ✅ First version: entities, pages with facts and history in the encrypted database, an About me page, edited and exported in Lax; a week of use to judge |
| [5](https://github.com/fnyman-ctrl/lax/milestone/5) | Ask your memory | ✅ First version: a conversation, answered from the journal and the memory, found by time, name, words and meaning |
| — | Writing assistant | ✅ First version, pulled forward ([ADR 0009](adr/0009-writing-assistant.md)): call Lax to any text field for a draft |
| [6](https://github.com/fnyman-ctrl/lax/milestone/6) | Release 0.1 | Planned |
| [7](https://github.com/fnyman-ctrl/lax/milestone/8) | Release 0.2: Safe to share | Planned |
| [8](https://github.com/fnyman-ctrl/lax/milestone/7) | Release 0.3: Remote providers | Planned |

---

## Stage 0: Foundation ✅

Repository, license, [guardrails](GUARDRAILS.md), [architecture decisions](adr/),
security policy and hardened CI, all in place before the first line of app code.

## Stage 1: Capture

**Goal:** A menu bar app that reads the text of the active window and stores it
locally. No AI yet.

- Menu bar app with pause (15 minutes, 1 hour, until tomorrow, until resumed)
- Accessibility permission onboarding that explains what is and isn't read
- Capture app, window title, visible text and the site of browser windows (host only)
- Never capture password fields, password managers or private windows; banking and
  health excluded by default; user-defined exclusions
- Secrets masked before anything is stored
- SQLite storage with full-text search
- Retention and "forget" controls
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
- Raw captures expire only once summarized, or at the maximum age (default 90 days)

**Done when:** the journal for a normal workday is accurate and useful to read back.

## Stage 4: Learning memory ⭐

**Goal:** The core of Lax: a memory that learns who you are, what you work on and
who you work with.

- Recognize projects, people and organisations in episodes, without duplicates
- A Markdown page per entity, plus an About me page ([ADR 0005](adr/0005-memory-as-markdown.md)),
  kept in the encrypted database, edited in Lax and exported as Markdown on
  request ([ADR 0011](adr/0011-pages-in-the-database.md))
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
- A conversation: follow-up questions ("and the day before?") with the turns before

## Writing assistant (pulled forward)

**Goal:** Lax's context where you write ([ADR 0009](adr/0009-writing-assistant.md)).
The maintainer chose to build it before the release, since it is what makes Lax
useful every day.

- Double-tap ⌥ Option (or a key the user picks) in a text field; nothing shows
  in text fields until then
- A small pixel salmon swims to the text cursor; the card sits on Liquid Glass
  on macOS 26 and later
- The instruction is the selection or the paragraph at the cursor; the draft is
  written from it, the About me page, the pages of people named in it and the window
- The user reads and edits the draft; Enter puts it in place of the instruction,
  and Lax never sends

Since then: the pages of the people in a mail thread (#113) and pasting
through the app's own menu, for every keyboard layout (#112). Next: Pages
(#111).

## Stage 6: Release 0.1

**Goal:** The first version others can install. Decided with the maintainer
(2026-10-13): 0.1 is local only, without remote providers, and is built from
source (`npm run build:app`), so it needs no Apple account and opens without
Gatekeeper warnings. A signed and notarized `.dmg` comes later.

- `npm run build:app`: build from a tagged release and install in ~/Applications
- First-run onboarding: hardware check, finding a local model server, permissions
- User documentation ([USER-GUIDE.md](USER-GUIDE.md))
- Release process documented ([RELEASING.md](RELEASING.md), [CHANGELOG.md](../CHANGELOG.md))
- `npm run perf` for the performance budget (#11), measured over a few evenings
- The database encrypted ([ADR 0010](adr/0010-encrypted-database.md)), and the
  memory pages kept in it, edited and exported in Lax
  ([ADR 0011](adr/0011-pages-in-the-database.md)): built
- Later: signed and notarized `.dmg` built by CI with build provenance
- In 0.2: the app lock with Touch ID ([ADR 0012](adr/0012-app-lock.md))

## Stage 7: Release 0.2: Safe to share

**Goal:** A version a few friends can use every day, and trust with their
data. Decided with the maintainer (2026-10-15): small, still built from
source, and a memory that tidies itself, since the list of names was the
biggest chore in the first week. Remote providers come in 0.3.

- The app lock with Touch ID or the login password ([ADR 0012](adr/0012-app-lock.md), #158)
- A memory that tidies itself ([ADR 0013](adr/0013-tidying-the-memory.md)):
  names the user works with told from names only mentioned (#178), new names
  that fade and a folded New list (#179), suggested merges (#180), and
  projects in parts (#181)
- Upgrades keep the data: a database made by each release is opened by the
  next one in CI (#170)
- The paths for databases from before 0.1 removed: plaintext conversion,
  pages as files in `~/Lax`, and moving them in (#169)
- Help for testers: a setup check before the build and a guide for testers
  (#172), and **Copy details** for a bug report, without anything captured (#171)
- Loose ends from 0.1: deleting the old database after Start over (#173), and
  noticing when Allow was chosen instead of Always Allow (#174)
- Whatever the testers find

**Done when:** 3–5 testers have used it for two weeks, upgraded from 0.1
without losing anything, and no bug for 0.2 is open.

Not in 0.2: the signed `.dmg` (it needs an Apple Developer account; later),
and other new features such as meetings or time tracking.

## Stage 8: Release 0.3: Remote providers

**Goal:** The model layer's remote half (Stage 2): opt-in providers with a
consent flow and presets (#16), API keys in the Keychain (#17), redaction and
the outbox, and per-task routing with a remote indicator (#20). The first time
data may leave the Mac, so it is a release of its own.

## Later

Ideas for after 0.1, roughly in order of interest. None of them are committed.

- **Meetings:** local transcription with Whisper (e.g. KB-Whisper for Swedish)
- **Time tracking:** time per project, derived from the journal
- **MCP server:** read-only access to your memory for other local tools
- **OCR fallback** for apps that don't expose text through Accessibility
- **Windows and Linux** support
