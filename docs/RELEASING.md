# Releasing

How a version of Lax is released. Lax 0.1 is built from source by its users
(`npm run build:app`); a signed `.dmg` comes later (see the roadmap).

## Before

1. `main` is green, and the issues in the version's milestone are closed or
   moved.
2. The maintainer has used the build daily for at least a week.
3. `npm run perf` is within the budget in [PERFORMANCE.md](PERFORMANCE.md).
4. `CHANGELOG.md` has a section for the version, written for users: what's
   new, what changed, anything they have to do.

## Release

1. Set the version in `package.json`, `src-tauri/Cargo.toml` and
   `src-tauri/tauri.conf.json` (they must match), and run `cargo check` so
   `Cargo.lock` follows. Open a pull request, "Release 0.1.0", and merge it.
2. Tag the merge commit and push the tag:
   ```sh
   git tag -a v0.1.0 -m "Lax 0.1.0"
   git push origin v0.1.0
   ```
3. Create the GitHub release from the tag, with the changelog section as notes:
   ```sh
   gh release create v0.1.0 --title "Lax 0.1.0" --notes-file <(sed -n '/^## 0.1.0/,/^## /p' CHANGELOG.md | sed '$d')
   ```
   GitHub attaches the source archives. Add their SHA-256 checksums to the
   notes, so users can check what they downloaded:
   ```sh
   curl -sL https://github.com/fnyman-ctrl/lax/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
   ```
4. Check that a clean checkout of the tag builds: `git clone --branch v0.1.0
   https://github.com/fnyman-ctrl/lax.git && cd lax && npm run build:app`.

## After

- Move unfinished issues to the next milestone and close the version's one.
- Announce it where the maintainer posts updates.
