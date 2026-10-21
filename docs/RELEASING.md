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
5. `upgrade::tests` passes: every earlier release's database (in
   `src-tauri/fixtures/`) opens with this version, with everything in it.

## Release

1. Set the version in `package.json`, `src-tauri/Cargo.toml` and
   `src-tauri/tauri.conf.json` (they must match), and run `cargo check` so
   `Cargo.lock` follows. In `CHANGELOG.md`, replace "(unreleased)" in the
   version's heading with the date. Open a pull request, "Release 0.1.0", and
   merge it.
2. Tag the merge commit and push the tag:
   ```sh
   git tag -a v0.1.0 -m "Lax 0.1.0"
   git push origin v0.1.0
   ```
3. Create the GitHub release from the tag, with the changelog section as notes:
   ```sh
   gh release create v0.1.0 --title "Lax 0.1.0" --notes-file <(awk '/^## /{f=/^## 0\.1\.0( |$)/; next} f' CHANGELOG.md)
   ```
   GitHub attaches the source archives. Add their SHA-256 checksums to the
   notes, so users can check what they downloaded:
   ```sh
   curl -sL https://github.com/fnyman-ctrl/lax/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
   ```
4. The **Release check** workflow runs on the tag: it checks that the three
   versions match the tag, builds and installs Lax as a user would
   (`npm run build:app`), and verifies the app's signature. It can also be
   run by hand from the Actions tab before tagging. Wait for it to pass
   before announcing the release.

## After

- Add the release's own upgrade fixture, so every later version is tested
  against it (#170). In a worktree of the tag, copy the previous release's
  `make_fixture.rs` into `src-tauri/src/`, add `mod make_fixture;` to
  `lib.rs` there, adjust it to anything new the release stores, and run it:
  ```sh
  git worktree add --detach /tmp/lax-fixture v0.2.0
  cd /tmp/lax-fixture/src-tauri
  FIXTURE_OUT=/tmp/lax-fixture-out cargo test make_upgrade_fixture -- --ignored
  ```
  Copy `lax.sqlite`, renamed `database.fixture`, and the maker to
  `src-tauri/fixtures/0.2.0/` on `main`,
  add `"0.2.0"` to `RELEASES` in `src-tauri/src/upgrade.rs`, and extend
  `check` for what the release added. Synthetic data only; the key is the
  test key.

- Move unfinished issues to the next milestone and close the version's one.
- Announce it where the maintainer posts updates.
