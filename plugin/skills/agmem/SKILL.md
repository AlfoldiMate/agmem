---
name: agmem
description: Judgement for persistent memory — when a moment is worth storing in agmem, when a briefing claim should be corrected rather than worked around, what to leave out, and which seams call for a checkpoint. Use when deciding whether something is worth remembering, when memory and the repo disagree, or at a seam (a push, a decision the user just made, a compaction, a finished task).
---

# Memory judgement

agmem stores what you send and never rewrites it, so the quality of the store
is the quality of your judgement at write time. These are the tests.

## Is it worth storing?

All four, or it is not:

1. **Durable** — true of this project or this person, not of this task.
2. **Non-obvious** — not something the repo, a type, a test, or git history
   already says. Memory is for what a future session would have to *work out
   again*.
3. **Earned** — seen more than once, or once at real cost. A single cheap
   surprise is an anecdote.
4. **Actionable** — it changes what a future session would *do*. "The parser
   is complex" changes nothing; "the parser re-reads the file on every token,
   so batch calls" does.

The reason is the valuable half of a decision. "Used a channel" is worth
little; "used a channel because the mutex version deadlocked under the retry
path" saves the next session from the same detour.

## Correct, never contradict

A briefing claim that turns out wrong is corrected with `remember` and
`supersedes` set to its id — the id ends every briefing line. The old claim
stays readable and dated; only one is live. Storing the new truth *beside*
the old one leaves two live claims that disagree, and the next session picks
one at random.

Before any write, `recall` the topic in the words you would store it in. A
live claim that already says it means nothing to store; one that says
something different means this is a correction and you need its id.

## What kind

| What | kind | decay |
|---|---|---|
| Decision + reason, corrected assumption, map fact | `fact` | default |
| Gotcha, hard-won how-to | `lesson` | slow |
| Standing rule for every future session | `instruction` | pinned — be sparing |
| Branch state (done, next, blocked) | `fact`, `decay_class: fast`, tagged `branch:<slug>` | days |
| A conclusion drawn from stored claims | `reflect`, with `derived_from` set to the claims it rests on | default |

Cite what you drew on. When a conclusion rests on claims that memory put in
front of you — in the briefing or in a `recall` result — `reflect` with those
ids in `derived_from` is the same write with its evidence attached, and it is
how the store learns which of its claims actually help.

## Leave out

Anything git records. Anything the code says plainly. Anything true only for
the current turn. Narrative — memory holds state, not logs. Long output goes
to a file with a one-line pointer claim.

## Seams

A seam is where the reasons are still in context and about to stop being:

- a successful `git push` — work shipped, reasons fresh;
- an answer to a question you asked the user — a decision was just made;
- a compaction — the summary keeps outcomes and drops reasons;
- a finished task, before starting the next.

At a seam, `/agmem:checkpoint`. Saving nothing is the right outcome for a
session that established nothing durable; saving its scratch work is not.
