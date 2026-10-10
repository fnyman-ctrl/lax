# Contributing to Lax

Thanks for your interest! Lax is in early development. Right now the most useful
help is opening an issue with ideas, questions or feedback.

## Before you start

Read [docs/GUARDRAILS.md](docs/GUARDRAILS.md). Lax sees everything a user does on
their computer, so privacy and security rules come before features. For larger
changes, open an issue first so we can agree on the approach.

## How we work

- `main` is protected. All changes go through pull requests, including the maintainer's.
- One concern per pull request. Small PRs get reviewed faster.
- CI must pass. Don't disable or weaken checks to get a green build.
- Significant or hard-to-reverse decisions get an
  [Architecture Decision Record](docs/adr/README.md).
- Write commit messages in the imperative ("Add capture exclusions", not "Added…").

## AI-assisted contributions

Using AI tools is fine. You are responsible for every line you submit: you must
understand it, have tested it, and be able to explain it in review. Mention in
the pull request if a significant part was AI-generated.
[AGENTS.md](AGENTS.md) contains the rules we give AI assistants.

## Privacy in issues and pull requests

Never include real personal data: no screenshots of your own activity, no real
captured text, no database files. Use synthetic examples.

## Security issues

Don't open a public issue. See [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contribution is licensed under the project's
[AGPL-3.0 license](LICENSE). Code taken from MIT-licensed projects (such as Handy)
must keep its original copyright notice.

## Getting started

### Requirements

- macOS on Apple Silicon (Intel may work but isn't tested)
- Xcode Command Line Tools: `xcode-select --install`
- [rustup](https://rustup.rs). The exact Rust version is pinned in `rust-toolchain.toml`
  and installed automatically.
- Node.js (version in `.nvmrc`)

### Run

```bash
npm ci
npm run tauri dev
```

npm install scripts are disabled in `.npmrc`. Don't turn them on.

### Checks

CI runs these on every pull request. Run them locally before pushing:

```bash
npm run typecheck
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo deny check   # brew install cargo-deny
```
