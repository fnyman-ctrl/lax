# Performance

Lax runs all day in the background, so it has to stay out of the way. This
page sets the budget, says what keeps Lax within it, and how to check.

## Budget

| Resource | Budget (average over a workday) |
|---|---|
| CPU | Under 2% of one core |
| Memory | Under 150 MB |
| Disk | Raw captures only grow until retention deletes them (90 days by default) |
| Energy | "Low" in Activity Monitor's Energy tab; less on battery |

These are first targets from #11. They are checked by measuring a normal
workday on a real Mac (see below), and the numbers here are updated when
they change.

## What keeps Lax within it

- **Reading is bounded.** One read of a window looks at no more than 3,000
  elements, 60 levels deep, 20,000 characters, and stops after one second.
  Every request to another app times out after 250 ms, so a hung app can't
  stall Lax (`src-tauri/src/capture.rs`, `Limits`).
- **Unchanged content isn't stored.** A window whose text didn't change since
  it was last stored is skipped. A window whose text keeps changing (a chat,
  a document being typed) is stored at most every 30 seconds, with its latest
  content (`Throttle`).
- **Reading slows down when you're away, and stops when you've left.** After
  a minute without keyboard or mouse input, Lax reads every 15 seconds instead
  of every 5. After ten minutes nothing is read until you're back, and the
  first input wakes it up. Only the time since the last input is read, never
  the input itself (`pace`, `reads`, `ax::idle_time`).
- **Reading slows down on battery.** On battery, every wait is twice as long
  (`ax::on_battery`).
- **The model works in the background only when it doesn't disturb.** On
  battery, the journal writes about once an hour, everything that waited in
  one go, and waits for power below 20 %; nothing else in the background uses
  the model. On power, the journal writes as captures settle; the memory's
  reading, page facts and Ask's vectors wait until two minutes without
  keyboard or mouse input
  (`journal::may_use_model`). A question in Ask always goes first.
- **An encrypted database keeps temporary data in memory.** SQLCipher encrypts
  the database, its journal and its write-ahead log, but not the temporary
  files SQLite makes for sorts and temporary indexes. So the encrypted database
  uses `temp_store = MEMORY`, and those files never reach the disk
  ([ADR 0010](adr/0010-encrypted-database.md)).
- **Forgetting doesn't copy the database.** `VACUUM` builds a full copy, which
  with temporary data in memory takes about as much RAM as the file is large
  (296 MB for a 246 MB file in the probe, against a budget of 150 MB). An
  encrypted database is laid out for `auto_vacuum = INCREMENTAL`, and
  forgetting frees pages with `PRAGMA incremental_vacuum` instead (10 MB peak in
  the same probe). Deleted text is still zeroed by `secure_delete`.
- **Migrations that build indexes on large tables must be measured.** Building
  an index sorts in memory now. Before such a migration ships, run it on a
  database as large as a few months of captures and check the peak memory.
- **Expensive setup happens once.** Chromium and Electron apps are asked to
  build their accessibility tree once per process, not on every read.

## How to check

`npm run perf` does most of it (`scripts/perf-report.sh`): the running Lax's
average CPU since it started and its memory now, the data folder's size, and a
day-by-day summary of the log below. It reads only Lax's own log and `ps`.
Several evenings of normal use count as well as one workday.

By hand:

1. Run Lax for a normal workday.
2. In Activity Monitor, find Lax: CPU (the % CPU column and CPU Time),
   Memory, and Energy (Avg Energy Impact).
3. Check how much the data folder grew:
   `du -sh ~/Library/Application\ Support/io.github.fnyman-ctrl.lax`
4. Every ten minutes the log
   (`~/Library/Application Support/io.github.fnyman-ctrl.lax/logs/lax.log`)
   has a `capture.summary` line with counts and timings only: rounds,
   captures stored, unchanged, paused, the largest window walked, rounds while
   away, gone (nothing read) and on battery, and the slowest round. A `slowest` near one second or
   a `largest_window_elements` at 3,000 point to a window that hits the limits.

## Measured

**2026-10-15, the maintainer's MacBook, a normal workday** (`npm run perf`
after 10.6 hours, with the journal, memory, Ask and the writing assistant on):

| Resource | Measured | Budget |
|---|---|---|
| CPU | 0.13% of one core, average | Under 2% |
| Memory | 39 MB | Under 150 MB |
| Disk | 25 MB data folder after a week | Grows until retention |
| Energy | 2.29, "12 hr Power" in Activity Monitor | "Low" |

Capture over the week (2026-10-10 to 10-15): up to about 2,900 reads and 670
stored captures a day, with most rounds while away reading nothing. The
slowest round was about one second and the largest window 3,000 elements on
most days: some windows hit the limits above, which is what the limits are
for. Nothing suggests the reading itself needs to change yet.

Ideas if a later measurement doesn't meet it: watch for accessibility notifications instead of
reading on a timer, read only the part of a window that changed, and skip
reading entirely while the screen is locked or asleep.
