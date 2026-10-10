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
- **Reading slows down when you're away.** After a minute without keyboard
  or mouse input, Lax reads every 15 seconds instead of every 5; after ten
  minutes, every minute. Only the time since the last input is read, never
  the input itself (`pace`, `ax::idle_time`).
- **Reading slows down on battery.** On battery, every wait is twice as long
  (`ax::on_battery`).
- **Expensive setup happens once.** Chromium and Electron apps are asked to
  build their accessibility tree once per process, not on every read.

## How to check

1. Run Lax for a normal workday.
2. In Activity Monitor, find Lax: CPU (the % CPU column and CPU Time),
   Memory, and Energy (Avg Energy Impact).
3. Check how much the data folder grew:
   `du -sh ~/Library/Application\ Support/io.github.fnyman-ctrl.lax`
4. Every ten minutes the log
   (`~/Library/Application Support/io.github.fnyman-ctrl.lax/logs/lax.log`)
   has a `capture.summary` line with counts and timings only: rounds,
   captures stored, unchanged, paused, the largest window walked, rounds while
   away and on battery, and the slowest round. A `slowest` near one second or
   a `largest_window_elements` at 3,000 point to a window that hits the limits.

## Not yet measured

The budget hasn't been checked against a full workday yet. Until it has, treat
it as a target, not a promise.

Ideas if it isn't met: watch for accessibility notifications instead of
reading on a timer, read only the part of a window that changed, and skip
reading entirely while the screen is locked or asleep.
