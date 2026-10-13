# Guardrails

These are the rules Lax is built by. They apply to every feature and every pull
request. If a change conflicts with a guardrail, the guardrail wins, or the
guardrail is changed first in its own, openly discussed pull request.

Lax sees everything you do on your computer. That makes it one of the most
sensitive programs you can run. These rules exist so you never have to take
our word for it.

---

## 1. Core promises

1. **Local by default.** A fresh install sends nothing anywhere. All processing
   runs on your machine until you explicitly choose otherwise.
2. **You choose where data goes.** Sending data off the machine (for example to
   a cloud model) is always opt-in, per provider, and visible while it happens.
3. **You can see everything.** Everything that was captured, everything Lax
   believes about you, and everything that was ever sent off the machine can be
   inspected in the app.
4. **You can stop and delete anything.** Pause capture, exclude apps, forget a
   time range, or wipe everything, at any time and with immediate effect.
5. **No telemetry.** No analytics, no crash reporting, no "anonymous usage data". Ever.

## 2. Data classes

| Class | Examples | Sensitivity | May leave the machine? |
|---|---|---|---|
| **Raw captures** | Window text, OCR output, transcripts | Highest | Only to a remote model the user enabled for that task, after redaction |
| **Episodes** | "Worked on Q3 proposal with Anna" | High | Same as above |
| **Memory pages** | `about-me.md`, project and people pages | High | Same as above |
| **Settings** | Excluded apps, model choice | Low | Never |
| **Secrets** | API keys | Critical | Only to their own provider, over TLS |

## 3. Model providers

Lax is **model-agnostic**. All AI work goes through one internal interface,
implemented on top of the **OpenAI-compatible API** (`/v1/chat/completions`,
`/v1/embeddings`). If a provider works with the OpenAI SDK by changing the base
URL, it works with Lax. That one interface covers:

- **Local:** Ollama, LM Studio, llama.cpp server, MLX servers
- **Remote:** any OpenAI-compatible endpoint the user configures, for example
  [STAIK](https://api.staik.se) (Sweden), [Berget AI](https://berget.ai) (Sweden)
  or Mistral (EU)

Chat and embeddings are configured **separately**, so a user can, for example,
run embeddings locally (cheap, works on almost any machine) and chat remotely.

### Choosing a setup

Onboarding asks about hardware and priorities, not about providers:

| Situation | Suggested setup |
|---|---|
| Capable machine (e.g. Apple Silicon, 16 GB+) | Everything local |
| Weaker machine, privacy is important | Embeddings local, chat via a privacy-focused remote provider the user picks (e.g. one with in-country processing and no retention), with redaction on |
| User doesn't mind cloud processing | Any OpenAI-compatible provider |

Lax can offer **presets** for known providers (base URL, jurisdiction, link to
their privacy policy) to make setup easy. Presets never contain keys and are
never enabled automatically. Lax describes providers by their *stated* policies
and links to the source. It does not vouch for them.

### Rules for every provider

- A provider is **local** only if its endpoint resolves to a loopback address
  (`127.0.0.1`, `::1`, `localhost`). Anything else is treated as **remote**,
  including servers on the local network.
- Lax ships with **no remote provider enabled** and no API keys.
- API keys are stored in the **macOS Keychain**. Never in config files, the
  database or logs.
- Remote endpoints must use **HTTPS**.

### Extra rules for remote providers

- **Explicit consent.** Enabling a remote provider shows a consent screen: who
  the provider is, where it is based (jurisdiction), and its stated retention
  and training policy. The user must confirm.
- **Per-task routing.** The user chooses which tasks may use a remote provider
  (summarizing, entity extraction, page updates, Q&A). Everything else stays local.
- **Redaction first.** Before anything is sent, a local redaction step removes or
  masks emails, phone numbers, Swedish personal identity numbers (personnummer),
  payment card numbers, IBANs, and things that look like passwords, API keys and tokens.
- **Outbox log.** Every remote request is logged locally: when, to which provider,
  for which task, and exactly what was sent (after redaction). The user can browse
  and delete the log.
- **Visible indicator.** The menu bar icon shows when remote processing is enabled.
- **Fail closed.** If the remote provider is unreachable, Lax waits or falls back to
  local. It never falls back to a *different* remote provider.

## 4. Capture

Exclusions come in three tiers:

| Tier | What | Can the user change it? |
|---|---|---|
| **Never** | Password fields (`AXSecureTextField`), private/incognito browser windows, password managers (1Password, Bitwarden, Keychain Access, Passwords…), Lax itself | No. Not even in settings. |
| **Excluded by default** | Banking and payments (e.g. Swedbank, SEB, Handelsbanken, Nordea, Klarna) and health (e.g. 1177) | Yes, but they start excluded |
| **User-defined** | Any app, website or window title pattern, e.g. Messenger or a private chat | Yes |

- **Private browsing is never captured**, whatever is typed or shown in it.
- **Mail and chat are captured by default**, because that's where much of the
  context about people and projects lives. Onboarding lists the mail and chat apps
  that are installed and lets the user exclude any of them up front.
- **Excluded means not read.** Lax checks exclusions *before* reading a window's
  content, never after.
- **Counted, never named.** Lax may count how often it skipped an excluded window
  ("12 private windows skipped today"), but never records which ones.
- **Only the site, not the address.** From browser URLs, Lax keeps only the host
  (`figma.com`), never the path or query, which can contain searches, document
  names, tokens and personal data.
- **Secrets are redacted before they are stored.** Passwords, API keys, tokens,
  private keys, payment card numbers and Swedish personal identity numbers are
  masked at capture time and never reach the database, even though it is local.
  Other personal data (names, emails, phone numbers) is kept locally because the
  memory needs it, and is redacted before anything is sent to a remote provider (§3).
- **Pause** is one click in the menu bar: for 15 minutes, 1 hour, until tomorrow, or
  until resumed. While paused, Lax reads nothing at all, not even app names, and
  stops any pending processing. The menu bar icon shows clearly that Lax is paused.
- **Retention:** Lax is meant to remember everything you do, and it does so in
  episodes and memory pages, which are kept until the user deletes them. Raw
  captures, the most sensitive data, expire automatically: 30 days after they
  were captured once the journal has summarized them, and at the latest at the
  maximum age (default 90 days, configurable), summarized or not. So nothing is
  lost before Lax has learned from it, and raw text doesn't linger after.
- **Forget:** the user can delete a time range, an app or an entity, and the deletion
  includes everything derived from it.
- Screenshots used for OCR are processed in memory and **never written to disk**.

## 5. Prompt injection: captured content is untrusted

Lax reads arbitrary web pages, emails and documents. Any of them can contain text
written to manipulate an AI ("ignore previous instructions and…"). Therefore:

- **The model has no tools that act.** Models in Lax can only return text or JSON.
  They cannot browse, run commands, send messages or call other services.
- **Model text goes only where the user puts it.** The writing assistant
  ([ADR 0009](adr/0009-writing-assistant.md)) inserts a draft only after the user
  has read it and pressed Enter, only into the field they called Lax to, and only
  as text. Lax never presses Return or Send in another app.
- **Structured output is validated.** Model output is parsed against a strict schema;
  anything that doesn't match is discarded.
- **Untrusted content is delimited** in prompts and the model is told it is data,
  not instructions.
- **User edits win.** Text the user wrote or corrected in a memory page cannot be
  overwritten by the model.
- **Memory changes are reviewable.** Page updates are stored as versions, so any
  change can be inspected and rolled back.

## 6. Storage

- All data lives in two folders owned by the user, both with permissions `0700`:
  the data folder (database and logs, in `~/Library/Application Support`) and the
  memory folder (`~/Lax`, the memory's Markdown pages, files `0600`). Lax refuses
  to write pages if the memory folder is inside iCloud Drive or another synced
  folder, since that would copy the memory off the machine (§1).
- Lax relies on FileVault for disk encryption and warns if FileVault is off.
  Database encryption (SQLCipher) is planned.
- **Logs never contain captured content**, only metadata such as counts and timings.
- Everything is stored in open formats (SQLite + Markdown) so the user can export,
  back up or inspect it without Lax.

## 7. Network

The app may only make these network requests:

1. To configured model endpoints (rules above)
2. Model downloads, started by the user
3. Update checks, opt-in

Nothing else. The Tauri content security policy and capability list are kept as
strict as possible, and the web UI cannot make network requests on its own.

## 8. Integrations (e.g. MCP)

Planned, not built. When built:

- Off by default, read-only, local only (no network listener)
- Each client must be approved by the user
- The user is warned that a connected client (e.g. a cloud AI assistant) may send
  what it reads to its own provider

## 9. Development

- **No real personal data** in tests, fixtures, screenshots, issues or bug reports.
  Use synthetic data.
- Databases, models and memory folders are in `.gitignore` and must never be committed.
- Lockfiles (`Cargo.lock`, `package-lock.json`) are committed.
- CI runs `cargo audit` / `cargo deny` and `npm audit`. Dependabot is enabled.
- New dependencies need a reason in the pull request. Fewer is better.
- Pull requests that touch **capture, network, storage or model providers** are
  labelled `security-sensitive` and need extra review against this document.
- **Every guardrail is backed by a test.** [CLAIMS.md](CLAIMS.md) maps each promise
  in this document to the code that keeps it and the test that checks it. A rule
  without a test is a wish, not a guarantee.

## 10. Reporting vulnerabilities

See [SECURITY.md](../SECURITY.md).
