# 0013. Tidying the memory: less noise, suggested merges, projects in parts

- **Status:** Accepted (maintainer, 2026-10-15)
- **Date:** 2026-10-15

## Summary

After a few days of use, the Memory view is a long list to go through by
hand. Most of it is noise, names seen once that don't matter, and projects
named a little differently in different entries. This ADR makes Lax do most
of the tidying:

1. **Less noise at the source.** The model says, for each name, whether the
   user deals with it or it was only mentioned. Only the first kind earns a
   page; names only mentioned wait, folded, under New.
2. **New names fade, and don't ask for attention.** A name seen on one day
   that doesn't come back leaves by itself. The New list is folded away and
   sorted by how often a name is seen.
3. **Lax suggests merges.** Lax finds pairs whose names suggest they are the
   same, and the local model judges each pair from what Lax knows about both.
   The user merges with one click, or says **Not the same**, which sticks.
   Pairs that differ only by a legal suffix ("Nyterio" and "Nyterio AB") are
   listed together, with **Merge all**.
4. **A better merge picker:** the likely names first, and a search.
5. **Projects in parts.** A project can be part of another ("the writing
   assistant" in "Lax"). Lax suggests it, the user accepts, and the part is
   shown under its project; Ask and the writing assistant read the parts'
   pages when the project is named.

Nothing is merged or joined without the user. This builds the merge suggestions
[ADR 0008](0008-learning-memory.md) promised for later, and keeps its rule
that a wrong merge is worse than a duplicate.

Deferred, with reasons below: undoing a merge, and automatic merges.

## Context

The maintainer's first week with the memory (2026-10-15): the list takes
time to go through, and what takes the time is

- **noise in New:** names from a single entry that never matter again, such
  as the author of an article read once, a product on a web page, or a topic
  named as if it were a project; and
- **projects that are similar without being the same:** the same work named
  a little differently in different entries, or a project and its parts.

Duplicates of people were less of a problem, since names are matched on
their normalized form.

The causes, in the code:

- `Entities::TASK` asks for "the people, organisations and projects named
  in the captured text". The entry names what the user read and looked at,
  not only what they work on, so every named thing becomes a candidate.
- A candidate stays in New until it is seen on two days (`DAYS_TO_KEEP`),
  kept or dismissed. Nothing removes a candidate that is never seen again.
- Resolution is exact alias match only (ADR 0008). Anything else is a new
  entity, and merging is the user's job, with a "Same as" list of every
  entity of that kind.
- Merging deleted the merged entity, and its page went with it, the user's
  own text included. Fixed separately before this ADR (#176): the page, or
  the user's text on it, now moves to the kept name's page.

An independent review of the first draft (2026-10-15) changed it: the pair
finder no longer uses episode vectors or names seen together as signals,
the judge reads entries rather than only pages, the model's "no" is not
final, automatic merges and "part of" are deferred, and fading has guards.
Its findings are reflected below.

## Decision

### 1. Less noise at the source

`Entities` asks, for each name, how the user is involved:

```json
{"people": [{"name": "...", "involvement": "works_with" | "mentioned"}],
 "organisations": [...], "projects": [...]}
```

- **works_with:** people the user talks to, writes to, meets or works with;
  organisations they work for, with or sell to; projects that are named
  pieces of work they do or take part in, such as a product, a client
  assignment or an initiative.
- **mentioned:** everything else: authors of things read, people and
  companies in the news, names on a page the user only looked at, topics,
  articles, documents and one-off tasks.

Both kinds are recorded, and each mention keeps its flag. Only `works_with`
mentions count toward a page: a name earns one when it is seen as
`works_with` on two days, as today. A name only ever `mentioned` stays new,
in its own folded group under New (*"Only mentioned"*), can be kept or
merged like any other, and fades like any other new name. A small model
classifies better than it leaves things out, and keeping both lets the
threshold be tuned later without a new prompt. The shape is schema-validated as today, with `deny_unknown_fields`
and the same limits on names.

Measured before it ships:

- the live tests, with synthetic entries holding names the user works with
  and names they only read about, before and after;
- a local, ignored test that replays the maintainer's own recent entries
  through both prompts and prints **counts only** (names found, kept,
  dropped), never names or text, so it can run on real data without
  anything captured reaching the output.

It ships only if it cuts the noise without losing the names that matter.

### 2. New names fade, and don't ask for attention

**Fading.** A candidate fades when all of these hold:

- it is not kept, and was seen as `works_with` on fewer than two days;
- it has **no page** (a name whose episodes were forgotten can drop under
  two days and still have one; it doesn't fade);
- it hasn't been mentioned for **14 days**, or **28 days** if it was
  mentioned 3 times or more. The days are counted back from the **newest
  entry in the journal**, not the clock, so a holiday, a pause or a backlog
  being caught up doesn't make names fade before they had a chance to come
  back.

It is deleted through the same path as a forget (mentions, aliases, the
`not_same` rows below), in one batch per round. It does **not** go on the
"don't learn" list: if the name comes back, it starts over as new.

**The New list.** Folded by default, as *"Lax is watching 23 new names. They
get a page when they come back on another day, and leave after two weeks if
they don't."* Opened, it lists them by mentions, most first, with the names
only mentioned in a group of their own, folded too.

**What fading removes.** The name in the memory: the entity, its names and
which entries mention it. The journal entries themselves stay, so Ask and
search still find the text; only the memory's link from the name to them
goes. A name the user wants to keep, they keep.

### 3. Merge suggestions

**Finding pairs**, without a model, once per memory round, for entities of
the same kind:

- one name contains the other as whole words ("Anna" and "Anna Example",
  "Lax" and "Lax development"), or
- the names are close after normalizing: a small edit distance for typos
  and inflections ("Nyterios" and "Nyterio"), or equal once a legal suffix
  or punctuation is removed (below).

A name signal is required. Two names from the same entries are **not** a
signal: they are usually the different things one article or one meeting
named. Pairs whose mentions are mostly the same entries are skipped. Episode
vectors may later rank pairs, for names with two or more entries, but don't
find them.

**A short name that fits several.** "Anna" contains-matches both "Anna
Example" and "Anna Other": the suggestion shows both, and asks which, or
neither.

**Which pairs are judged.** Only pairs where at least one side is kept or
seen on two days. Pairs of two new names wait: most of them fade. At most a
few pairs per round, the strongest name signal first.

**Judging a pair.** The local chat model gets both names and kind, and for
each up to three recent entry summaries that mention it, plus its page's Lax
block if it has one, all as `Captured` data. It answers
`{"same": "yes" | "no" | "unsure", "reason": string}`, schema-validated.
Only local models (GUARDRAILS §3), only when `may_use_model(When::Later)`,
and not while paused.

**The reason is untrusted text.** It comes from captured text through a
model. At most 200 characters, control and invisible characters removed
(`is_invisible`), shown as plain text, never logged. Injected text in an
entry can at worst flip a verdict; the pair itself comes from Lax, and a
merge always needs the user's click.

**Showing it.** A "yes" becomes a suggestion at the top of the Memory view:
*"Anna and Anna Example look like the same person"*, the reason, the two
pages to open, and **Merge** and **Not the same**.

**Obvious pairs.** Organisations whose names are equal once a legal suffix
or punctuation is removed skip the model and are listed together: *"These
look like the same organisations"*, each with its own **Merge** and
**Not the same**, and **Merge all**. The suffixes: AB, Aktiebolag, HB, KB,
Inc, Ltd, Limited, LLC, GmbH, Oy, Oyj, AS, ASA, ApS, A/S, BV, NV, SA, SAS,
plc, at the end of the name, and only when something is left. Two names with
**different** suffixes ("Example AB" and "Example Oy") are not an obvious
pair: they are often companies in different countries. They go to the
model like any other pair.

**Remembering answers.**

- The user's **Not the same** is final: kept in a `not_same` table (the two
  entity ids), never suggested again.
- The model's "no" and "unsure" are kept with when they were judged and
  how many mentions each side had. The pair is judged again when either side
  has twice the mentions, or after 30 days. A weak model on thin evidence
  must not block a right suggestion for good.
- Forgetting either entity, forgetting the memory, or forgetting a person by
  name deletes their rows (GUARDRAILS §4).

### 4. A better merge picker

The **Same as** list in the Change panel puts the likely names first: the
pairs from point 3 for this entity, then names that share a word with it.
With more than about 15 names, it can be searched.

### 5. Projects in parts

Some projects are neither the same nor unrelated: one belongs to the other,
like "Lax encryption" and "the writing assistant" in "Lax", or "the LinkedIn
series" in "Nyterio". Merging them would mix pages that should stay apart.

**The relation.** A project can be **part of** one other project. One level
only: a project that has parts can't be a part, and a part can't have parts.
Stored as `entities.part_of` (a project's id, `ON DELETE SET NULL`), so
forgetting the whole leaves its parts as projects of their own. People and
organisations have no parts.

**Suggesting it.** Two ways a pair of projects comes up:

- the pair finder of point 3: one name contains the other ("Lax" and "Lax
  encryption");
- for parts named differently, two projects that are both kept or seen on
  two days, and are named in the same entries on **two or more different
  days**. One entry naming both is noise (point 3); the same two names
  together on several days is how a project and its part show up.

For pairs of projects the judge answers
`{"relation": "same" | "part_of" | "neither" | "unsure", "whole": 1 | 2,
"reason": string}`, schema-validated, with the same inputs and the same
untrusted-reason rules as point 3. "same" becomes a merge suggestion;
"part_of" becomes *"The writing assistant looks like part of Lax"*, with
**Yes**, **No** and **They're the same** (which merges). The user's **No** is
final, kept with the pair like **Not the same**; the model's "neither" and
"unsure" are judged again under the rules of point 3.

**By hand.** The Change panel of a project has **Part of** with a picker of
the other projects, and a part has **Take out**.

**Showing it.** In the Memory view, a project's parts are listed under it,
indented. The project's page gets a line in Lax's block, *"Parts: Lax
encryption, the writing assistant."*, and each part's block gets *"Part
of: Lax."* (names defused, as every name in a block).

**Using it.** When a question to Ask, or the text around the writing
assistant, names a project, the pages of up to three of its parts are read
too, the most mentioned first, within the same context budget as today.
When it names a part, the line about its whole is enough; the whole's page
isn't added.

### What stays

- A wrong merge is worse than a duplicate: every merge is the user's click.
- Names from the model are untrusted: shown as text, passed to prompts only
  as `Captured`, and every model answer is schema-validated.
- Logs hold counts (names dropped as mentioned, pairs found, judged,
  suggested, merged, faded), never names or reasons.
- All model calls are local.

## Deferred

- **Undoing a merge (Separate again).** With the user's text now carried on
  merge (#176), a wrong merge loses nothing the user wrote, only the
  separation. A real undo needs a snapshot of the merged entity (name,
  aliases, the mentions that were only its own, the user's text) and can't
  split entries recognized after the merge. Worth its own design once
  suggestions are in use.
- **Automatic merges**, even for legal suffixes. **Merge all** gives the
  same saving without a silent wrong merge, and needs no notice afterwards.

## Rollout

Each step is its own PR, usable on its own:

1. **The involvement flag** (point 1), with the live test and the local
   replay before and after.
2. **Fading and the folded New list** (point 2).
3. **Pairs, the judge and suggestions** for people, organisations and
   projects, with `not_same` and the re-judge rules (point 3).
4. **Obvious pairs and Merge all** (point 3).
5. **The merge picker** (point 4).
6. **Projects in parts:** the relation, Part of and Take out by hand, the
   list and the lines in the blocks (point 5).
7. **Suggesting parts**, and Ask and the writing assistant reading them
   (point 5).
8. **Docs:** USER-GUIDE, ARCHITECTURE, ADR 0008's note, CLAIMS if needed.

Steps 1 and 2 address the noise, the maintainer's biggest cost, and come
first.

## Tests

- Involvement: `mentioned` names are recorded but never earn a page, and
  are listed in their own group; a malformed flag fails
  validation; the old shape fails validation.
- Fading: a candidate past its days goes, with its mentions and `not_same`
  rows; a kept one, one seen on two days, one with a page, or one seen again
  does not; the days count from the newest entry, not the clock; nothing
  goes on the "don't learn" list.
- Pairs: containment as whole words and edit distance find the intended
  pairs; names from the same entries don't pair; kinds never mix; a pair on
  `not_same` is never found again; two new names aren't judged.
- The judge: the answer is schema-validated, and a malformed one counts as
  "unsure"; the reason is capped and cleaned; "no" is judged again after the
  mentions double or 30 days.
- Obvious pairs: each suffix; nothing left after the suffix is no pair;
  different suffixes are no obvious pair.
- Parts: one level only (a whole can't be a part, a part can't have parts);
  only projects; forgetting the whole leaves the parts; a pair named
  together in one entry, or on one day, isn't suggested; on two days it is;
  the lines in the blocks; Ask reads at most three parts' pages.
- Live tests: the prompt's noise before and after; the judge on synthetic
  pairs that are and aren't the same.

## Consequences

- The New list stops asking for attention, and most tidying is a click.
- More model calls in the background: a few pair judgments per round, only
  when idle and on power.
- A new table (`not_same`) and the model's verdicts with their dates.
- A name the user cares about may be flagged `mentioned`. It is still listed,
  folded, and can be kept; the measurements guard against it too.
- A name seen once that the user wanted leaves after two weeks unless they
  keep it.
- Projects get one level of structure, a column, and a little more for Ask
  and the writing assistant to read when a project with parts is named.

## Alternatives considered

- **Merge automatically when the model is sure, with undo.** Less clicking,
  but a wrong merge mixes two people's facts on one page and is hard to
  notice. ADR 0008 rejected it for that reason.
- **Automatic merges for legal suffixes.** Close to safe, but "Example AB"
  and "Example" can still be a parent and a subsidiary, and Merge all costs
  one click.
- **Episode vectors and names seen together as pair signals** (the first
  draft). Names from the same entry share that entry's vector and always
  appear together, so these signals find exactly the unrelated names the
  noise consists of. A name seen in one entry has no vector of its own.
- **Ask the model to leave out names it isn't sure of.** A small model
  classifies more reliably than it omits, and omission can't be tuned
  afterwards.
- **A second prompt that cleans the list ("which of these are noise?").**
  Another model call over many names at once, with an answer that's hard to
  check. Asking better at the source and letting candidates fade is cheaper
  and predictable.
- **Topics as a fourth kind.** Topics are where most of the noise comes
  from; a kind of their own would grow the list, not shrink it.

## Maintainer's answers (2026-10-15)

1. **Fading after 14 days** (28 for names seen 3 times or more), counted
   from the newest entry: **yes**. Fading removes the name from the memory,
   not the journal entries that named it (see point 2).
2. **Names only mentioned:** **kept, folded under New**, rather than dropped
   (point 1 says how).
3. **"Part of" for projects:** **in 0.2** (point 5).
