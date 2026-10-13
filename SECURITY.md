# Security policy

Lax reads what is on your screen and keeps a memory of your work, so a security
or privacy problem in it can expose very sensitive data. Reports are welcome and
taken seriously.

## Supported versions

Lax is in early development. Only the latest code on `main` and the latest
release get security fixes.

## Reporting a vulnerability

**Please don't open a public issue.** Report it privately through GitHub's
private vulnerability reporting:

**https://github.com/fnyman-ctrl/lax/security/advisories/new**

(or the repository's **Security** tab → **Report a vulnerability**). Only the
maintainer can see the report.

Please include:

- what you found, and what an attacker could do with it,
- how to reproduce it (steps, a proof of concept, the version or commit),
- anything you know about a fix.

Never include real personal data: use synthetic data such as "Anna Example" and
`example.com`.

## What happens next

- **Within 7 days:** an acknowledgement that the report arrived.
- **Within 14 days:** an assessment: whether it's a vulnerability, how severe,
  and a plan.
- **Within 90 days:** a fix in a release, for confirmed vulnerabilities. Severe
  ones (data leaving the Mac, excluded content captured) come first.

Lax is a volunteer project, so a fix can take longer; you'll hear how it's going
either way.

## Disclosure

We follow coordinated disclosure: please keep the vulnerability private until a
fix is released, or until 90 days after your report, whichever comes first. The
fix is announced in a GitHub security advisory and the changelog, crediting you
if you like.

## Scope

Especially interesting:

- Data leaving the Mac without the user's consent
- Capturing content that should be excluded (password fields, private windows,
  excluded apps or sites)
- Prompt injection that changes the memory, a draft, or what Lax does, in ways the
  user didn't intend
- The writing assistant writing anywhere but the field the user called it to, or
  pressing anything but paste
- Key events, captured text or memory showing up in logs
- Secrets exposed outside the macOS Keychain

[docs/GUARDRAILS.md](docs/GUARDRAILS.md) has the rules Lax is meant to follow, and
[docs/CLAIMS.md](docs/CLAIMS.md) the test behind each one.

Out of scope: problems that need an attacker who already controls the user's
account on the Mac, and the security of the model server the user chose.
