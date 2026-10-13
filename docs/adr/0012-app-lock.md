# 0012. Lock Lax's window with Touch ID or the login password

- **Status:** Proposed
- **Date:** 2026-10-13

## Summary

Lax's window would show nothing until the user confirms with Touch ID or
the login password, through macOS's LocalAuthentication. It locks again
after 5 minutes away, and settings changes, forgets and exports ask again
when they happen. The lock is enforced in Rust: commands that return what Lax
captured or learned refuse while it is locked. Capture, pausing and the
writing assistant keep working in the background, but the writing
assistant uses the memory pages only while Lax is unlocked (open question
1).

The lock guards the window. It doesn't encrypt anything, and it doesn't
change who can read the database key of [ADR 0010](0010-encrypted-database.md).

## Context

The maintainer decided on 2026-10-13 that Lax's window asks for Touch ID
or the login password when it opens, again after 5 minutes away, and
before every settings change, forget or export. Capture and the writing
assistant keep working.

Today anyone at an unlocked, logged-in Mac can open Lax from the menu bar
and read the journal, ask about the user's week, read the captures and the
sent log, and change or delete anything. ADR 0010's threat model counts
this case as one that neither FileVault nor encryption protects: Lax runs,
and the keychain is unlocked. With [ADR 0011](0011-pages-in-the-database.md),
the memory pages are only readable in Lax's window, which makes the window
the one place to guard them.

What the code has to build on:

- **Idle time:** `ax::idle_time` reads how long since the last key press,
  mouse move, scroll or click (`CGEventSourceSecondsSinceLastEventType`,
  never which input, no permission needed).
- **Away, for capture:** after 1 minute without input capture reads less
  often (`capture::AWAY`), and after 10 minutes it reads nothing until the
  user is back (`LONG_AWAY`, `capture::reads`). The capture loop measures
  time on the monotonic clock, which stands still while the Mac sleeps
  (see `capture::Heartbeat`).
- **The window:** closing it only hides it (`lib.rs`, `CloseRequested`);
  "Open Lax" in the menu bar shows it again (`tray::open_window`). Lax has
  no Dock icon (`ActivationPolicy::Accessory`).
- **Commands:** every command the web UI can call is listed in `build.rs`
  and allowed per window in `capabilities/default.json` or
  `capabilities/panel.json`, and `lib::tests::every_command_is_listed_and_allowed`
  checks both. Capabilities are fixed at build time; they can't follow a
  lock that changes while Lax runs.
- **The writing assistant's panel** is its own webview (`summon::PANEL`)
  with its own five commands. `write::write_draft` gives the model the
  facts on About me and on the pages of people named in the instruction or
  the thread's header lines (`ask::known_for`).

## Decision

### What is locked

**Lax starts locked**, every time, and nothing persists "unlocked" across a
restart. Locked, the window shows only the salmon, "Lax is locked" and an
**Unlock** button. It asks at once when the window is shown.

**Unlocking** uses `LAContext` with the `LAPolicyDeviceOwnerAuthentication`
policy: Touch ID where the Mac has it, with "Use Password…" to type the
login password instead, and the login password where it doesn't. Apple's
policy also accepts an Apple Watch that is set up to approve (to verify).
The reason shown in the dialog is a fixed string per action ("open Lax",
"change a setting", "forget the last hour"), never captured content.

**Away.** The user counts as away when:

- there has been no keyboard or mouse input for 5 minutes (`ax::idle_time`),
- the Mac's screen locks, the Mac goes to sleep, or the user switches to
  another account (fast user switching), or
- for the window only: the window has been hidden (closed or minimised)
  for 5 minutes.

The first two **lock Lax**: the window and the panel's use of the pages.
The third **locks the window**: it asks again the next time it is shown,
while the panel keeps what the other two rules allow. Without that split,
the panel, whose use comes while the window is hidden, would need a touch
every 5 minutes.

A screen lock, sleep or account switch locks at once (open question 2), not
after 5 minutes. The capture loop's monotonic clock stands still while the
Mac sleeps, and typing the login password at the lock screen may count as
input (to verify), so neither idle time nor a timer alone can tell a short
sleep from none. For the 5-minute rules, time is checked on both the
monotonic and the wall clock, and either one passing 5 minutes is enough:
the wall clock runs during sleep, and the monotonic clock can't be set back.

The input rule reads `ax::idle_time`, as capture's 10-minute rule does,
with a 5-minute threshold. It runs in a small watcher of its own (every
few seconds, a reading of idle time and of the window's visibility), so
the lock doesn't depend on capture running or being paused. Screen lock,
sleep and account switch come from `NSWorkspace` notifications
(`willSleep`, `screensDidSleep`, `sessionDidResignActive`) and the
`com.apple.screenIsLocked` distributed notification, which is undocumented
(to verify which of them fire, and when).

**When Lax locks,** it reloads the main webview, so what was on screen
doesn't stay in the page's memory, and the window shows the lock screen.
Anything typed and not saved in the window is lost. The page editor of ADR
0011 saves a few seconds after the user stops typing, so it loses at most
those seconds.

### What else asks each time

Even while unlocked, these ask for Touch ID or the login password again,
at the moment they happen. The command itself asks, in Rust, before it
does anything, so there is no token the web UI could reuse. Forgets and
exports ask every time. For settings, one confirmation covers further
settings changes for 60 seconds, like the padlock in System Settings
(open question 5).

- **Settings changes:** retention, the sent log's retention, adding or
  removing an exclusion, turning a default exclusion on or off, choosing or
  clearing a model (the model provider), turning the journal, the memory or
  the writing assistant on or off, the writing assistant's key, the user's
  other names. (`set_retention_days`, `set_outbox_retention_days`,
  `add_exclusion`, `remove_exclusion`, `set_default_excluded`, `set_model`,
  `clear_model`, `set_journal_enabled`, `set_memory_enabled`,
  `set_writing_enabled`, `set_writing_key`, `set_own_names`.)
- **Forgets:** a time range or app (`forget_captures`), everything
  (`delete_all_data`), an entity (`forget_entity`), everything the memory
  noted (`forget_memory`), the sent log (`delete_outbox`), and allowing a
  name back from the "don't learn" list (`allow_learning`), which loosens
  what Lax may learn.
- **Export** of pages, and deleting the old page files (ADR 0011).
- **"Start over" and "Delete the old file"** when the database can't be
  opened (ADR 0010).

Turning off an exclusion is in the list above. The never tier can't be
turned off at all, as today.

Not asking each time: the light/dark switch, finishing the setup guide,
looking for models on this Mac (`find_local_models`), testing a model, and
the memory's everyday edits: rename, merge, keep, recreate a page, restore
a version, and saving a page (ADR 0011). Those need the window unlocked,
nothing more (open question 6).

### What stays unlocked

- **Capture**, the journal, the memory and the page updates run in the
  background, locked or not, as today.
- **The menu bar:** its status line, all four pauses, "Open Lax" (which
  shows the lock screen) and "Quit Lax". Pausing is the safe direction and
  stays one click.
- **Resuming capture** from the menu bar stays one click too (open
  question 4). It reveals nothing, and the icon shows the state. Asking for
  Touch ID to resume would make pausing cost more, so people would pause
  less. The cost: someone at the Mac could resume a pause the user set.
- **The writing assistant** comes when called, as today. What it may use
  while Lax is locked is open question 1.

### The writing assistant (open question 1)

The panel drafts from the facts on About me and on the pages of the people
in the thread. Someone at an unlocked Mac could open any mail, double-tap
⌥ and type "write down everything you know about Anna Example". The draft
would hold what the memory knows about Anna, without the window ever being
unlocked. ADR 0009 limits which pages are used to the people named in the
instruction or the thread's header, but the instruction is typed by
whoever is at the keyboard.

Recommended:

- **Drafts always work.** Locked, the panel drafts from the instruction and
  the window's text only, and says so under the draft: "Written without
  your memory. Unlock to use it." with an **Unlock** button that asks for
  Touch ID or the password and writes the draft again.
- **Unlocked, the panel uses the pages, as today.** Unlocking from the
  panel unlocks Lax (the first two away rules), not the window, which still
  asks when it is next shown.
- `write_draft` checks the lock in Rust before it gathers any facts.

Weighed honestly:

- In the first 5 minutes after the user leaves an unlocked Mac, the panel
  still uses the pages. So does the window, if it was in use. That is the
  price of the 5 minutes.
- A user who drafts often after a break gets one touch per break. On a Mac
  without Touch ID, that is the login password. Most drafts come right
  after other typing, while the user is present, so most won't ask.
- The other options: keep the panel as it is (simplest, and the hole above
  stays open), or ask for every draft that uses the pages (closes it, but
  costs a touch or a password on almost every draft, and teaches people to
  approve without reading). A fixed window, such as "unlocked within the
  last 60 minutes", is easier to explain but ignores whether the user left.

If this is accepted, it changes what ADR 0009 says drafts draw on. ADR
0009's status line then gets "superseded in part by 0012", as
`docs/adr/README.md` asks.

### Enforced in Rust, failing closed

- **A gate in front of every command.** `lib.rs` wraps the handler from
  `generate_handler!`: before a command runs, the gate looks up its name
  and the calling window in `src-tauri/src/lock.rs`, and refuses with a
  fixed "Lax is locked." unless it is allowed. Tauri 2's `Invoke` carries
  the command's name and the webview it came from (to verify the exact
  API). If that wrapper can't be had, each gated command calls
  `lock::require` first, and a test checks that each one does.
- **Default deny.** `lock.rs` lists the few commands the main window may
  call while locked: `lock_status`, `unlock`, `accessibility_status`,
  `open_accessibility_settings`, `filevault_enabled` and `capture_status`
  (none returns captured or learned content). Every other main-window
  command needs Lax unlocked and the window unlocked. A command added
  later and forgotten in `lock.rs` is refused, not let through.
- **The panel's commands** (`panel_call`, `write_draft`, `fit_panel`,
  `insert_draft`, `close_panel`) are allowed from the panel window only, as
  their capability already says; `write_draft` checks the lock itself for
  the pages.
- **New commands** `lock_status` and `unlock` (in the main window and the
  panel), and an event that tells the UI when Lax locks. Each is listed in `build.rs`
  and `capabilities/`, which `every_command_is_listed_and_allowed` checks.
  A UI test with mocked commands can't catch a missing entry there, so the
  Rust tests carry these claims.
- **Fails closed.** Cancel, a wrong finger or password, a timeout, a
  dialog the system took away, LocalAuthentication not being available, or
  any error: Lax stays locked. If the Mac has no login password, Lax can't
  unlock, and the window says so, with a link to System Settings (open
  question 7). There is no bypass, not even in debug builds; tests use a
  fake authenticator behind a trait.

### Implementation sketch

- `src-tauri/src/lock.rs`: the lock's state (locked; unlocked, with when;
  window hidden since), the away rules, the command lists, an
  `Authenticator` trait with the macOS one and a fake for tests, and the
  watcher.
- `LAContext`: a new context for each request, so an earlier success
  can't stand in for a later one (whether a used context asks again is to
  verify), `evaluatePolicy` with
  `LAPolicyDeviceOwnerAuthentication`, the answer awaited on a channel.
  Lax activates itself first, so the dialog isn't behind other windows (to
  verify for an `Accessory` app).
- Bindings: `objc2-local-authentication`, a new crate from the same
  project as `objc2-app-kit` (MIT, Apache-2.0 or Zlib), or a few
  `msg_send!` calls on `objc2` with the framework linked by hand (open
  question 8). The notifications need features of `objc2-foundation` and
  `objc2-app-kit` that Lax doesn't turn on yet.
- **To verify in a spike** on a real Mac, alongside ADR 0010's Keychain
  spike: that `evaluatePolicy` works for an ad-hoc signed, non-sandboxed
  app with no entitlements (we expect it to, and know of no entitlement it
  needs on macOS); that the dialog names Lax and comes to
  the front; how it behaves on a Mac without Touch ID and with an Apple
  Watch; whether typing the password at the lock screen resets idle time;
  which notifications fire on screen lock, sleep and account switch; and
  that reloading the webview drops what it showed.

### Relation to ADR 0010

- **The lock gates the window. The database key is separate.** Lax reads
  the key from the login keychain once, at start, normally without a
  prompt (ADR 0010), and keeps the database open while it runs, because
  capture writes to it. So, locked or not, the key is in Lax's memory and the database is
  open. The lock encrypts nothing, and doesn't stop another program from
  asking the keychain for the key (the Keychain prompt in ADR 0010's
  malware row).
- **Later, with the signed `.dmg`:** the key can move to the data
  protection keychain with `SecAccessControl` and `userPresence`, so reading
  it needs Touch ID or the password. An `LAContext` that has just been
  evaluated can be handed to the Keychain (`kSecUseAuthenticationContext`),
  so one touch at start could both read the key and unlock the window. It
  only helps at start: after that the key is in memory, and the window lock
  is the one in this ADR. It also means capture waits for that touch after
  every restart. That is a later step, with its own ADR, together with ADR
  0010's open question 4.
- **Words:** ADR 0010's screen while the store opens says "Opening the
  database…", so "Unlock" means only this lock.

### Threat model

| Situation | Without the lock | With the lock |
|---|---|---|
| Someone opens Lax at an unlocked Mac after the user has been gone 5 minutes, or after the Mac locked or slept | Sees the journal, Ask, pages, captures and sent log, and can change or delete anything | Sees the lock screen. Needs Touch ID or the login password |
| The same, while the user was present but the window had been hidden for 5 minutes | Sees everything | Asked for Touch ID or the login password |
| The same, in the first 5 minutes after the user left, with the window in use | Sees everything | **Sees everything.** The cost of not asking all the time |
| That person changes a setting, forgets or exports in those 5 minutes | Can | Asked for Touch ID or the login password: for every forget and export, and for settings unless the user confirmed one in the last 60 seconds (question 5) |
| That person double-taps ⌥ and asks for what Lax knows about someone | Gets a draft with the facts | Recommended answer to question 1: no facts unless Lax is unlocked; within the 5 minutes, yes |
| Someone who knows the login password (family, a colleague who saw it typed) | Sees everything | **Sees everything.** The password is the fallback |
| Another program of the user reads Lax's files | Not addressed here; ADR 0010 and 0011 | Not addressed here |
| A program asks the unlocked keychain for the database key | Not addressed here; ADR 0010's Keychain prompt | **Not addressed.** The lock doesn't guard the key |
| Malware running as the user | Not protected | **Not protected.** It can get the key as ADR 0010 says, read Lax's window through Accessibility while it is unlocked, wait for the user, or watch the screen |
| Screen sharing, or someone watching while the window is open | Not protected | Not protected |
| A Mac that is off, or lost | FileVault and ADR 0010 | The same. The lock adds nothing |

### Tests

- `lock::tests`: Lax starts locked; only the authenticator's success
  unlocks; cancel, failure, "not available" and errors leave it locked; 5
  minutes without input locks (with a fake idle source); screen lock, sleep
  and account switch lock at once; sleep counts, by the wall clock, and a
  clock set back doesn't extend the time; the window locks after 5 minutes
  hidden while the panel stays unlocked; each request uses a new context.
- `lib::tests::every_command_has_a_lock_rule`: every command in
  `generate_handler!` is in exactly one of the lists in `lock.rs` (open
  while locked, needs unlocked, asks each time, panel), next to
  `every_command_is_listed_and_allowed`.
- `lock::tests::locked_commands_are_refused`: the gate refuses every
  command not open while locked, and every main-window command from the
  panel.
- For each command that asks each time, a test that a failed
  authentication changes nothing (with the fake authenticator).
- `write::tests::no_page_facts_while_locked`.
- Live, opt-in, on a real Mac (`#[ignore]`): `live_unlock`.
- A Vitest test that the lock screen shows no content. It can't show the
  Rust gate works; the tests above do.

### CLAIMS.md

New rows, updated as each part lands:

- "Lax's window shows nothing until you confirm with Touch ID or your login
  password, and locks again after 5 minutes away."
- "While Lax is locked, no command returns what it captured or learned."
- "Settings changes, forgets and exports ask for Touch ID or your login
  password when they happen" (with question 5's answer).
- "Locked, the writing assistant doesn't use your memory pages" (with
  question 1's answer).

### Docs

- **GUARDRAILS:** one line in §6 on the lock, in its own PR first, as the
  preamble requires: what it guards and that it encrypts nothing.
- **USER-GUIDE:** a "Locking" section: when Lax asks, what keeps running
  while locked, what the lock doesn't protect against.
- **ARCHITECTURE:** a short "Lock" section pointing here.
- **PERFORMANCE:** the watcher's cost (a reading every few seconds).
- **Onboarding:** one line that Lax will ask for Touch ID or the password
  when it opens.

## Open questions for the maintainer

The Decision above assumes the recommended answers.

1. **What may the writing assistant use while Lax is locked?** Flagged: the
   panel can write out what Lax knows about a person, without the window.
   Recommended: drafts always work, but use the memory pages only while Lax
   is unlocked; locked, the panel offers Unlock and writes the draft again.
   See "The writing assistant".
2. **Does a screen lock, sleep or account switch lock Lax at once, or after
   5 minutes?** Recommended: at once. The clocks can't tell a short sleep
   from none, and if the Mac's own password is set to "after 1 hour", a
   Mac that slept briefly wakes without one.
3. **Does a hidden window count as away?** Recommended: yes, for the window
   only, after 5 minutes. It closes the common case of a user who closed
   Lax an hour ago and stepped away without locking. Asking at every
   showing would ask several times an hour for nothing.
4. **Does resuming capture need the lock?** Recommended: no. See "What
   stays unlocked".
5. **Every settings change, or one authentication for a minute of
   settings changes?** Recommended: one authentication covers settings
   changes for 60 seconds, like the padlock in System Settings, so editing
   a list of exclusions doesn't ask five times. Forgets and exports ask
   every time. This relaxes the maintainer's "every", so it is his call.
6. **Do the memory's everyday edits ask each time?** Recommended: no
   (rename, merge, keep, recreate, restore, saving a page); the unlocked
   window is enough. `allow_learning` does ask, since it loosens.
7. **No login password, or LocalAuthentication unavailable?** Recommended:
   fail closed. The window explains how to set a password. No setting turns
   the lock off.
8. **`objc2-local-authentication` or hand-written `msg_send!`?**
   Recommended: the crate. Typed bindings from the family Lax already uses
   are easier to review than hand-written selectors in `unsafe` code. Its
   licence is AGPL-compatible.
9. **Can the user change the 5 minutes?** Recommended: not now. One fixed
   value is easier to test and to explain.
10. **Is the window locked during the setup guide too?** Recommended: yes.
    It is the first time the user sees Touch ID for Lax, and there is no
    special case to get wrong.

## Consequences

- Lax's window, with the journal, Ask and, after ADR 0011, the pages, is
  closed to someone who sits down at an unlocked Mac after the user has
  been gone for 5 minutes, or after it slept or locked.
- Settings, forgets and exports can't be changed by someone in those 5
  minutes without the user's finger or password.
- The lock is checked by the commands, not drawn over the UI. A command
  added later is refused until it is classified.
- Touch ID prompts: one on opening after a break, one per minute of
  settings changes (question 5), one per forget or export, and
  perhaps one in the panel after a break. On Macs without Touch ID, each is
  the login password.
- A new module, a watcher thread, two commands, and probably one new
  crate.

### What we give up

- One-click access to Lax's window. Every break costs a touch.
- Quick settings changes and forgets.
- Writing-assistant drafts with the pages right after a break, without a
  touch (with question 1's recommended answer).

## Alternatives considered

- **A UI overlay only.** Easy, but the web UI could still call every
  command, so any bug in the UI, or a page script, would reach the data.
  The lock has to be in the commands.
- **A Lax password of its own.** One more secret to remember and to store,
  and no Touch ID. The login password is what macOS already protects.
- **Relying on the Mac's own screen lock.** It covers a locked Mac, not an
  unlocked one, and not a Mac whose password is asked only after an hour.
- **A user-presence key now, instead of LocalAuthentication.** Needs the
  data protection keychain, so the signed build, and only guards opening
  the database at start (see "Relation to ADR 0010").
- **Lock only the pages and Ask, not the rest of the window.** The
  journal, captures and sent log are as sensitive as the pages
  (GUARDRAILS §2), and one rule is easier to explain.

## Rollout

Each PR green on its own, labelled `security-sensitive`.

0. **GUARDRAILS §6**, in its own PR, once this ADR is accepted. Alongside
   it, the **spike** (not merged), whose results go into this ADR.
1. **`lock.rs`** with the state, the away rules, the authenticator trait
   and the fake, and their tests. No change in behaviour.
2. **The gate** and the command lists, `lock_status`, `unlock`, the lock
   screen, the reload on lock, `every_command_has_a_lock_rule`.
3. **Asking each time** for settings, forgets and exports.
4. **The panel:** pages only while unlocked, and its Unlock.
5. **Docs:** USER-GUIDE, ARCHITECTURE, PERFORMANCE, onboarding, CLAIMS.
