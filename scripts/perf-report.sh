#!/bin/sh
# How Lax is doing against its performance budget (docs/PERFORMANCE.md, #11).
# Run with: npm run perf  (or: sh scripts/perf-report.sh [days], default 7)
#
# Reads only Lax's own log (counts and timings, never content) and the Lax
# process's CPU time and memory from `ps`. Nothing is sent anywhere.
set -eu

days="${1:-7}"
data="$HOME/Library/Application Support/io.github.fnyman-ctrl.lax"
log="$data/logs/lax.log"

echo "Lax performance report"
echo

# The running Lax: average CPU since it started, and memory now.
pid="$(pgrep -x lax | head -n 1 || true)"
if [ -n "$pid" ]; then
  ps -o etime=,time=,rss= -p "$pid" | awk '
    function seconds(t,   n, p, s, d) {
      d = 0
      if (index(t, "-")) { split(t, p, "-"); d = p[1]; t = p[2] }
      n = split(t, p, ":"); s = 0
      for (i = 1; i <= n; i++) s = s * 60 + p[i]
      return d * 86400 + s
    }
    {
      up = seconds($1); cpu = seconds($2)
      printf "Running for     %.1f hours\n", up / 3600
      avg = 0
      if (up > 0) avg = 100 * cpu / up
      printf "Average CPU     %.2f%% of one core (budget: under 2%%)\n", avg
      printf "Memory now      %d MB (budget: under 150 MB)\n", $3 / 1024
    }'
else
  echo "Lax isn't running, so CPU and memory can't be measured now."
fi

# The data folder.
if [ -d "$data" ]; then
  printf "Data folder     %s\n" "$(du -sh "$data" | cut -f1)"
fi
echo

# What capture did, per day, from the summary logged every ten minutes.
if [ ! -f "$log" ]; then
  echo "No log yet at $log"
  exit 0
fi
since="$(date -u -v-"${days}"d +%Y-%m-%d)"
echo "Capture per day since $since (from capture.summary):"
grep "capture.summary" "$log" | awk -v since="$since" '
  substr($1, 1, 10) >= since {
    day = substr($1, 1, 10)
    for (i = 4; i <= NF; i++) {
      split($i, kv, "=")
      v = kv[2]; sub(/ms$/, "", v)
      if (kv[1] == "ticks") ticks[day] += v
      if (kv[1] == "stored") stored[day] += v
      if (kv[1] == "away") away[day] += v
      if (kv[1] == "gone") gone[day] += v
      if (kv[1] == "battery") battery[day] += v
      if (kv[1] == "slowest" && v + 0 > slowest[day]) slowest[day] = v + 0
      if (kv[1] == "largest_window_elements" && v + 0 > largest[day]) largest[day] = v + 0
    }
    days[day] = 1
  }
  END {
    printf "  %-10s %7s %7s %7s %7s %8s %9s\n", "day", "reads", "stored", "away", "gone", "slowest", "largest"
    n = asorti_days()
  }
  function asorti_days(   d, list, i, j, t, n) {
    n = 0
    for (d in days) list[++n] = d
    for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++) if (list[j] < list[i]) { t = list[i]; list[i] = list[j]; list[j] = t }
    for (i = 1; i <= n; i++) {
      d = list[i]
      printf "  %-10s %7d %7d %7d %7d %6dms %9d\n", d, ticks[d], stored[d], away[d], gone[d], slowest[d], largest[d]
    }
    return n
  }'
echo
echo "A slowest read near 1000 ms, or a largest window at 3000 elements, points"
echo "to a window that hits the limits. Energy: see Activity Monitor's Energy tab."
