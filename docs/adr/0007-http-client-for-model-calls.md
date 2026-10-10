# 0007. reqwest with macOS TLS for model calls

- **Status:** Accepted
- **Date:** 2026-10-11

## Context

Stage 2 adds the model layer (ADR 0004): Lax calls OpenAI-compatible servers
over HTTP, local ones on loopback by default and, later and only with consent,
remote ones over HTTPS (GUARDRAILS §3). That needs an HTTP client, and HTTPS
needs TLS. AGENTS.md asks for few dependencies, each with a reason.

## Decision

- **reqwest** (async, on Tauri's tokio runtime) with only the `json` and
  `native-tls` features. No HTTP/2, no cookies, no compression, no system
  proxy, no redirects to other hosts.
- **TLS through macOS** (`native-tls`, which uses Apple's Security framework),
  so certificates are checked against the system's trust store, the same way
  Safari does, and Lax ships no TLS stack of its own.
- All requests go through one module (`src-tauri/src/models`), which decides
  what may be called: loopback addresses without TLS, everything else only
  over HTTPS and only once remote providers exist with consent (#16).

## Consequences

- reqwest was already in `Cargo.lock` through Tauri, but Tauri only builds it
  on other platforms. On macOS this adds about 35 crates to the build, mostly
  hyper, tower and tokio parts, plus native-tls and security-framework.
  `cargo deny` accepts all of them. The lockfile also gains openssl and
  schannel entries, which only build on Linux and Windows.
- Certificate handling follows macOS, including enterprise roots the user
  trusts. A compromised system trust store affects Lax as it affects Safari.
- Async fits Tauri commands, and a request is cancelled by dropping it.

## Alternatives considered

- **rustls:** TLS in Rust, independent of the OS, but more new crates (rustls,
  a crypto provider such as ring or aws-lc) and its own root store to keep
  current.
- **ureq:** small and blocking, but a crate not yet in the tree, and blocking
  calls would tie up threads that Tauri commands share.
- **Hand-written HTTP over `std::net`:** no dependencies for loopback, but TLS
  can't be hand-written safely, and remote providers need it.
