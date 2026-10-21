# Upgrade fixtures

One folder per released version: a database that version made, so every later
version is tested against it (`upgrade::tests`, #170). It is named
`database.fixture`, not `lax.sqlite`, so the rule in `.gitignore` that keeps
databases out of git stays as it is.

- **Synthetic data only.** Anna Example, Exempelbolaget AB, example.com. Never
  real data.
- **The key is a test key**, never a real one: `7e57` ("test") sixteen times,
  in `upgrade.rs`. It only opens these files.
- Each is made with that release's own code, in a worktree of its tag, by the
  `make_fixture.rs` next to it (see `docs/RELEASING.md`). It isn't compiled
  here.
