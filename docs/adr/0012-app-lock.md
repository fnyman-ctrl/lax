# 0012. Lock Lax's window with Touch ID or the login password

- **Status:** Accepted. Built after release 0.1 (maintainer, 2026-10-15; issue #158)
- **Date:** 2026-10-13

## Summary

Lax's window would show nothing until the user confirms with Touch ID or
the login password, through macOS's LocalAuthentication. It locks again
after 5 minutes away. Ordinary settings need only the unlocked window.
Forgets, exports and every change that loosens privacy (an exclusion
turned off, a name allowed back, a model that doesn't run on this Mac)
ask again every time. The lock is enforced in Rust: commands
that return what Lax captured or learned refuse while it is locked.
Capture, pausing and the writing assistant keep working in the background,
but the writing assistant uses the memory pages only while Lax is
unlocked.

The lock guards the window. It doesn't encrypt anything, and it doesn't
change who can read the database key of [ADR 0010](0010-encrypted-database.md).

## Maintainer's answers (2026-10-13)

The maintainer answered the open questions late on 2026-10-13. The
questions at the end keep their reasoning, with each answer added. The
Decision below follows the answers.

- **The writing assistant while locked (question 1): as recommended.**
  Drafts always work, but use the memory pages only while Lax is unlocked.
  Locked, the panel offers Unlock and then writes the draft again.
- **Settings changes (question 5): changed from the recommendation, then
  settled on Claude's recommendation.** Not 60 seconds, and no separate
  confirmation for ordinary settings: unlocking the window is enough, and
  they stay open while the window is unlocked, under the same away rules.
  Every change that **loosens privacy** asks for Touch ID or the password
  every time, like forget and export. The reason: someone at an unlocked
  window can already see everything, so the real risk is a lasting change
  that weakens privacy after they leave. See "What else asks" for the
  list. The list comes from reading the commands in `src-tauri/src`.
  Retention, the user's other names, and turning on the journal, the
  memory or the writing assistant are judgement calls, for the maintainer
  to confirm.
- **Away (questions 2 and 3): as recommended.** A screen lock, sleep or
  account switch locks at once. No input for 5 minutes locks Lax, and a
  window hidden for 5 minutes locks the window.
- **The rest, as recommended:** resuming capture needs no lock
  (question 4); the memory's everyday edits don't ask, while
  `allow_learning` does (question 6); Lax fails closed, with no setting to
  turn the lock off (question 7); the bindings come from the
  `objc2-local-authentication` crate (question 8); the 5 minutes can't be
  changed for now (question 9); and the window is locked during the setup
  guide too (question 10).

**Status.** The LocalAuthentication spike is done (see "The
LocalAuthentication spike"): `evaluatePolicy` works for an ad-hoc signed
app like Lax. Its other findings are rules in the Decision now. Accepted by
the maintainer on 2026-10-13.

## Context

The maintainer decided on 2026-10-13 that Lax's window asks for Touch ID
or the login password when it opens, again after 5 minutes away, and
before every settings change, forget or export. Capture and the writing
assistant keep working. Later the same day he settled how often settings
ask (see "Maintainer's answers").

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
The spike showed this works for an ad-hoc signed app with no entitlements
and no Dock icon (see "The LocalAuthentication spike").

**The reason** shown in the dialog is a fixed string per action, never
captured content. macOS puts it into its own sentence, "<App> is trying to
<reason>." (in Swedish, "<App> försöker att <reason>."), and shows the
rest of the dialog in the system language. So each reason is a lowercase
verb phrase that fits that sentence ("open your memory", "turn off an
exclusion", "forget the last hour"), localized to the system language,
Swedish and English at least.

**A new `LAContext` after every lock.** The spike showed that a second
`evaluatePolicy` on the same context succeeds at once, without asking. So
`lock.rs` throws its context away whenever Lax or the window locks, and
after every change that asks, and makes a new one for the next request. Reusing a context would silently skip
the prompt.

**Only when the user asked.** Lax asks only in answer to something the
user just did in Lax: showing the window, pressing **Unlock**, or making a
change that asks. It never asks on its own in the background, not even
when the window locks while it is visible; then it shows the lock screen
and waits for **Unlock**. The spike showed the dialog appears on top of
whatever app the user is in, and a prompt over another app teaches people
to approve prompts they didn't cause.

**Cancel** leaves Lax locked, quietly: the lock screen stays, with no
error message. Other failures say briefly what happened.

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

A screen lock, sleep or account switch locks at once (question 2), not
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

### What else asks

The rule (question 5): **ordinary settings need only the unlocked window.
Every change that loosens privacy asks for Touch ID or the login password
every time**, at the moment it happens, like forgets and exports. Someone
at an unlocked window can already see everything; what the lock must stop
is a lasting change that weakens privacy after they leave. The command
itself asks, in Rust, before it does anything, so there is no token the
web UI could reuse, and no "confirmed" state to keep.

**Asks every time:**

- **Forgets:** a time range or app (`forget_captures`), everything
  (`delete_all_data`), an entity (`forget_entity`), everything the memory
  noted (`forget_memory`), and the sent log (`delete_outbox`).
- **Export** of pages, and deleting the old page files (ADR 0011).
- **"Start over" and "Delete the old file"** when the database can't be
  opened (ADR 0010).
- **Removing one of the user's own exclusions** (`remove_exclusion`).
- **Allowing a name back** from the "don't learn" list (`allow_learning`),
  which lets Lax learn something it was told not to.
- **Retention**, for raw captures (`set_retention_days`) and for the sent
  log (`set_outbox_retention_days`). A longer time keeps more of what was
  captured or sent. A shorter one deletes at once, so it is a forget.
- **The user's other names** (`set_own_names`). Adding a name forgets the
  people the memory noted under it, and removing one lets Lax learn a
  person by that name again.

**Asks when the change loosens privacy.** The command looks at its
arguments and the current setting, in Rust, and asks only in the
loosening direction:

- **Turning off a banking or health default** (`set_default_excluded`
  with `excluded: false`). Turning one back on doesn't ask.
- **Choosing a model that doesn't run on this Mac**, or routing a task to
  one (`set_model`, when the endpoint's locality isn't local). Today
  `set_model` refuses such endpoints anyway; this is the rule for when
  remote providers come (#16). A model on this Mac doesn't ask.
- **Turning on the journal, the memory or the writing assistant**
  (`set_journal_enabled`, `set_memory_enabled`, `set_writing_enabled`
  with `true`). Each makes Lax do more with what it reads: keep summaries
  longer than the raw captures, learn about people, or read the focused
  window when called, and send it to the chosen model. Turning them off
  doesn't ask.
- **Anything added later** that widens what is captured, what is kept or
  what may leave the Mac, such as loosening an exclusion tier. The never
  tier can't be turned off at all, as today.

**Doesn't ask, once the window is unlocked** (question 6): adding an
exclusion, clearing a model, choosing a model on this Mac, looking for
models on this Mac (`find_local_models`), testing a model, the call key
(`set_writing_key`), the light/dark switch, finishing the setup guide, and
the memory's everyday edits: rename, merge, keep, recreate a page, restore
a version, and saving a page (ADR 0011).

**One exception, kept on purpose:** resuming capture from the menu bar
widens what is captured, but stays one click (question 4). See "What stays
unlocked".

### What stays unlocked

- **Capture**, the journal, the memory and the page updates run in the
  background, locked or not, as today.
- **The menu bar:** its status line, all four pauses, "Open Lax" (which
  shows the lock screen) and "Quit Lax". Pausing is the safe direction and
  stays one click.
- **Resuming capture** from the menu bar stays one click too
  (question 4). It reveals nothing, and the icon shows the state. Asking for
  Touch ID to resume would make pausing cost more, so people would pause
  less. The cost: someone at the Mac could resume a pause the user set.
- **The writing assistant** comes when called, as today. While Lax is
  locked, it drafts without the memory pages (question 1).

### The writing assistant (question 1)

The panel drafts from the facts on About me and on the pages of the people
in the thread. Someone at an unlocked Mac could open any mail, double-tap
⌥ and type "write down everything you know about Anna Example". The draft
would hold what the memory knows about Anna, without the window ever being
unlocked. ADR 0009 limits which pages are used to the people named in the
instruction or the thread's header, but the instruction is typed by
whoever is at the keyboard.

Decided, as recommended:

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

When this ADR is accepted, it changes what ADR 0009 says drafts draw on.
ADR 0009's status line then gets "superseded in part by 0012", as
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
  unlock, and the window says so, with a link to System Settings
  (question 7). There is no bypass, not even in debug builds; tests use a
  fake authenticator behind a trait.

### Implementation sketch

- `src-tauri/src/lock.rs`: the lock's state (locked; unlocked, with when;
  window hidden since), the away rules, the command lists, the check that
  tells whether a call loosens privacy, an `Authenticator` trait with the
  macOS one and a fake for tests, and the watcher.
- `LAContext`: a new context for each request, and the old one dropped
  whenever Lax or the window locks. A used context doesn't ask again (the
  spike), so an earlier success must never stand in for a later one.
  `evaluatePolicy` with `LAPolicyDeviceOwnerAuthentication`, the answer
  awaited on a channel. The dialog comes to the front by itself, also for
  an `Accessory` app (the spike), so Lax doesn't need to activate itself
  first.
- The reasons: one localized string per action, Swedish and English at
  least, each a lowercase verb phrase that fits "<App> is trying to
  <reason>.", chosen by the system language.
- Errors: `LAError.userCancel` (−2) leaves the lock screen as it is, with
  no message. Other errors leave Lax locked and say briefly why.
- Bindings: `objc2-local-authentication`, a new crate from the same
  project as `objc2-app-kit` (MIT, Apache-2.0 or Zlib), rather than a few
  `msg_send!` calls on `objc2` with the framework linked by hand
  (question 8). The notifications need features of `objc2-foundation` and
  `objc2-app-kit` that Lax doesn't turn on yet.
- **Still to verify:** how the dialog behaves on a Mac without Touch ID
  and with an Apple Watch; whether typing the password at the lock screen
  resets idle time; which notifications fire on screen lock, sleep and
  account switch; and that reloading the webview drops what it showed.
  None of them was in the spike. They are checked while building.

### The LocalAuthentication spike

The maintainer ran it on 2026-10-13 (`local/touchid-spike`), on an M1 Pro
with Touch ID, macOS 26, with the system in Swedish. The spike is a small
AppKit app, "Lax Lock Spike.app", built like Lax: ad-hoc signed
(`codesign -s -`), no entitlements, not sandboxed, `LSUIElement` and the
`.accessory` activation policy. It calls `LAContext.evaluatePolicy` with
`.deviceOwnerAuthentication` and the reason "open your memory".

- **It works.** The system dialog appeared with the fingerprint icon. It
  named the app by its bundle name and used our reason, in the system
  language: "Lax Lock Spike försöker att open your memory. Använd Touch ID
  eller ange ditt lösenord för att fortsätta med Lax Lock Spike.", with
  "Använd lösenord…" and "Avbryt". So LocalAuthentication works for an
  ad-hoc signed app with no entitlements and no Dock icon.
- **The wording.** macOS builds the sentence "<App> is trying to
  <reason>." around the reason, in the system language. Hence the rule for
  reasons under "What is locked".
- **In front.** The dialog came to the front, over the app's own window.
- **A used context doesn't ask again.** A second `evaluatePolicy` on the
  same `LAContext` returned success at once, without a dialog. Hence the
  rule that every lock drops the context.
- **Over another app.** Started 10 seconds later while the maintainer was
  in another app, the dialog still appeared, on top of that app. Hence the
  rule that Lax asks only when the user just did something in Lax.
- **Cancel** returned `LAError.userCancel` (−2, "Authentication
  canceled") after 1.9 seconds.

Not tested: an Apple Watch, idle time after the lock screen, and the
lock, sleep and account-switch notifications (see "Still to verify").

### Relation to ADR 0010

- **The lock gates the window. The database key is separate.** Lax reads
  the key from the login keychain once, at start, without a prompt except
  once after each update, which Lax announces with its own note first
  (ADR 0010's Keychain spike), and keeps the database open while it runs,
  because capture writes to it. So, locked or not, the key is in Lax's
  memory and the database is open. The lock encrypts nothing, and doesn't stop another program from
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
| That person forgets, exports or loosens privacy in those 5 minutes: turns off an exclusion, allows a name back, lengthens retention, turns on the journal, the memory or the writing assistant, or picks a model off this Mac | Can | Asked for Touch ID or the login password, every time (question 5) |
| That person changes an ordinary setting in those 5 minutes: the theme, the call key, a model on this Mac, a page edit, a rename or merge | Can | **Can**, without asking, and their own input keeps the window from locking. None of these weakens privacy after they leave (question 5) |
| That person double-taps ⌥ and asks for what Lax knows about someone | Gets a draft with the facts | No facts unless Lax is unlocked (question 1); within the 5 minutes, yes |
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
- `lock::tests::a_lock_drops_the_context`: after an unlock, every way of
  locking (idle, screen lock, sleep, account switch, the window hidden)
  drops the `LAContext`, and the next request makes a new one and asks.
  The fake authenticator counts the contexts and the requests.
- `lock::tests`: nothing asks without a user action. Locking while the
  window is visible shows the lock screen and makes no request until
  **Unlock**. A cancel (`userCancel`) leaves Lax locked with no error
  message; other errors leave it locked with one.
- `lock::tests::every_reason_is_localized`: each action's reason exists in
  Swedish and English, is not empty, and starts with a lowercase letter.
- `lock::tests` for asking: an ordinary setting changes with the window
  unlocked and no request; each `every_time` command asks on every call,
  also twice in a row; each `loosening` command asks in the loosening
  direction and not in the other (`set_default_excluded` with `false` and
  `true`, `set_model` with an endpoint off this Mac and on it, each
  `set_*_enabled` with `true` and `false`).
- `lib::tests::every_command_has_a_lock_rule`: every command in
  `generate_handler!` is in exactly one of the lists in `lock.rs`: `open`
  (allowed while locked), `none` (needs the unlocked window, nothing
  more), `every_time`, `loosening` or `panel`. It sits next to
  `every_command_is_listed_and_allowed`.
- `lock::tests::locked_commands_are_refused`: the gate refuses every
  command not open while locked, and every main-window command from the
  panel.
- For each command that asks, a test that a failed authentication changes
  nothing (with the fake authenticator).
- `write::tests::no_page_facts_while_locked`.
- Live, opt-in, on a real Mac (`#[ignore]`): `live_unlock`.
- A Vitest test that the lock screen shows no content. It can't show the
  Rust gate works; the tests above do.

### CLAIMS.md

New rows, updated as each part lands:

- "Lax's window shows nothing until you confirm with Touch ID or your login
  password, and locks again after 5 minutes away."
- "While Lax is locked, no command returns what it captured or learned."
- "Forgets, exports and every change that loosens your privacy ask for
  Touch ID or your login password every time."
- "Locked, the writing assistant doesn't use your memory pages."

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

All answered on 2026-10-13 (see "Maintainer's answers"). Question 5 was
answered differently from the recommendation. The Decision above follows
the answers.

1. **What may the writing assistant use while Lax is locked?** Flagged: the
   panel can write out what Lax knows about a person, without the window.
   Recommended: drafts always work, but use the memory pages only while Lax
   is unlocked; locked, the panel offers Unlock and writes the draft again.
   See "The writing assistant". **Answered: as recommended.**
2. **Does a screen lock, sleep or account switch lock Lax at once, or after
   5 minutes?** Recommended: at once. The clocks can't tell a short sleep
   from none, and if the Mac's own password is set to "after 1 hour", a
   Mac that slept briefly wakes without one. **Answered: at once.**
3. **Does a hidden window count as away?** Recommended: yes, for the window
   only, after 5 minutes. It closes the common case of a user who closed
   Lax an hour ago and stepped away without locking. Asking at every
   showing would ask several times an hour for nothing. **Answered: yes,
   for the window only, after 5 minutes.**
4. **Does resuming capture need the lock?** Recommended: no. See "What
   stays unlocked". **Answered: no.**
5. **Every settings change, or one authentication for a minute of
   settings changes?** The recommendation was one authentication for 60
   seconds of settings changes, so editing a list of exclusions doesn't
   ask five times. **Answered differently, on Claude's later
   recommendation:** ordinary settings need only the unlocked window, with
   no separate confirmation, and stay open while it is unlocked. Every
   change that loosens privacy asks every time, like forgets, forget-all
   and exports. See "What else asks".
6. **Do the memory's everyday edits ask each time?** Recommended: no
   (rename, merge, keep, recreate, restore, saving a page); the unlocked
   window is enough. `allow_learning` does ask, since it loosens.
   **Answered: as recommended.**
7. **No login password, or LocalAuthentication unavailable?** Recommended:
   fail closed. The window explains how to set a password. No setting turns
   the lock off. **Answered: fail closed, with no off switch.**
8. **`objc2-local-authentication` or hand-written `msg_send!`?**
   Recommended: the crate. Typed bindings from the family Lax already uses
   are easier to review than hand-written selectors in `unsafe` code. Its
   licence is AGPL-compatible. **Answered: the crate.**
9. **Can the user change the 5 minutes?** Recommended: not now. One fixed
   value is easier to test and to explain. **Answered: not now.**
10. **Is the window locked during the setup guide too?** Recommended: yes.
    It is the first time the user sees Touch ID for Lax, and there is no
    special case to get wrong. **Answered: yes.**

## Consequences

- Lax's window, with the journal, Ask and, after ADR 0011, the pages, is
  closed to someone who sits down at an unlocked Mac after the user has
  been gone for 5 minutes, or after it slept or locked.
- Forgets, exports and changes that loosen privacy can't be done by
  someone in those 5 minutes without the user's finger or password.
  Ordinary settings can.
- The lock is checked by the commands, not drawn over the UI. A command
  added later is refused until it is classified.
- Touch ID prompts: one on opening after a break, one per forget, export
  or change that loosens privacy, and perhaps one in the panel after a
  break. On Macs without Touch ID, each is the login
  password.
- A new module, a watcher thread, two commands, and probably one new
  crate.

### What we give up

- One-click access to Lax's window. Every break costs a touch.
- Quick forgets, exports and changes that loosen privacy. Removing three
  exclusions costs three touches.
- Writing-assistant drafts with the pages right after a break, without a
  touch (question 1).

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

0. **GUARDRAILS §6**, in its own PR, once this ADR is accepted. The
   **spike** is done (`local/touchid-spike`, not merged); its results are
   in "The LocalAuthentication spike".
1. **`lock.rs`** with the state, the away rules, the authenticator trait
   and the fake, and their tests. No change in behaviour.
2. **The gate** and the command lists, `lock_status`, `unlock`, the lock
   screen, the reload on lock, `every_command_has_a_lock_rule`.
3. **Asking again**, every time, for forgets, exports and changes that
   loosen privacy, with the `every_time` and `loosening` lists.
4. **The panel:** pages only while unlocked, and its Unlock.
5. **Docs:** USER-GUIDE, ARCHITECTURE, PERFORMANCE, onboarding, CLAIMS.
