# Repository settings

GitHub settings that can't be stored in the repository itself. They're documented
here so anyone can verify how the project is configured, and so they can be
restored if needed.

## Branch protection (Settings → Rules → Rulesets)

Ruleset **Protect main**, enforcement *Active*, target: default branch.

- [x] Restrict deletions
- [x] Block force pushes
- [x] Require a pull request before merging
  - Required approvals: 0 while there is a single maintainer (raise to 1 when there are more)
  - [x] Dismiss stale approvals when new commits are pushed
  - [x] Require conversation resolution before merging
- [x] Require status checks to pass
  - [x] Require branches to be up to date before merging
  - Required checks: `Lint workflows`, `Analyze (actions)`
    (more are added with the app scaffold)
- [x] Require linear history
- Bypass list: empty

## Security (Settings → Advanced Security)

- [x] Private vulnerability reporting
- [x] Dependency graph
- [x] Dependabot alerts
- [x] Dependabot malware alerts
- [x] Dependabot security updates
- [x] Secret scanning
  - [x] Push protection
  - [x] Non-provider patterns
- [x] CodeQL: uses the workflow in `.github/workflows/codeql.yml` (don't enable default setup)

## Actions (Settings → Actions → General)

- Actions permissions: **Allow select actions**: GitHub-made actions, plus
  `ossf/scorecard-action@*` and `zizmorcore/zizmor-action@*`.
  Docker actions (`docker://…`) are blocked by this policy. Download tools from
  their official releases and verify a pinned checksum instead.
- [x] Require actions to be pinned to a full-length commit SHA
- Workflow permissions: **Read repository contents** (default `GITHUB_TOKEN` is read-only)
- [ ] Allow GitHub Actions to create and approve pull requests: **off**
- Fork pull request workflows: **Require approval for all external contributors**

## General

- [x] Allow squash merging only (disable merge commits and rebase merging).
  Also set the ruleset's allowed merge method to *Squash* only.
- [x] Automatically delete head branches
- Features: Issues on, Wiki off, Projects as needed
