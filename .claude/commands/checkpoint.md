---
description: Write the session's durable state to the ledger so you can safely /clear instead of letting /compact fire.
argument-hint: "[optional: what to emphasise]"
allowed-tools: Read, Write, Edit, Bash, Glob, Grep
---

# Checkpoint

Write memory to disk so that a fresh session — with none of this context — could
pick the work up without asking a single question.

## Where things go

Two files, split by **lifetime**, not by location. Resolve both first:

```bash
nu "$CLAUDE_PROJECT_DIR/.claude/hooks/scripts/ctx-flow-paths.nu"
```

It prints `ROOT=`, `LEDGER=`, `BRANCH=` and `STATE=`. **Use those paths.** Do not
re-derive them: this is the same resolver `SessionStart` uses to read the files
back, and a second implementation of the branch-name rule drifts from it
silently — the checkpoint lands in a file nothing ever loads.

Under the hood it resolves the main worktree through `git rev-parse
--git-common-dir`, which points at the one real `.git` from anywhere in the
repo, so the ledger is shared by every worktree and every branch while branch
state sits beside it keyed by a sanitised branch name. `STATE=` comes back empty
on a detached HEAD — then there is no branch file, and everything goes in the
ledger.

| File | Holds | Lifetime |
|---|---|---|
| `LEDGER.md` | Goal, Map, Gotchas, architectural decisions | outlives the branch — survives the merge |
| `state/<branch>.md` | Done, Next, Blocked, in-flight decisions | dies with the branch, and should |

The test for which file: **would this still be true after the branch merges?**
Yes → ledger. No → branch state. When unsure, prefer the ledger; a wrong entry
there gets pruned, a lost one is gone.

## What to record

Only what is durable and **non-obvious**:

- **Decisions and their reasons.** The reason is the valuable half. "Used a
  channel instead of a mutex" is worthless; "used a channel because the mutex
  version deadlocked under the retry path" is the whole point.
- **Corrected assumptions.** What you believed at the start of the session that
  turned out to be wrong. The highest-value entry type there is, because a fresh
  session will make the same wrong assumption.
- **Map entries** for files whose purpose isn't obvious from their path.
- **Gotchas**: the thing that cost real time and would cost it again.
- In branch state: what is done *and verified*, what is next, what is blocked on
  what.

## What to leave out

Anything git records. Anything the code says plainly. Anything true only for the
current turn. Narrative of what happened — these are state files, not logs.

## How to write it

**Merge, don't append.** Rewrite entries that are now wrong; delete ones that are
now irrelevant. A file that only grows becomes noise and gets ignored. Absolute
dates. Create either file from the templates in
`$CLAUDE_PROJECT_DIR/.claude/docs/reference.md` if missing.

**Re-read immediately before writing, and edit — never overwrite.** The ledger
and the playbooks are one physical file shared by every worktree, and another
session may have checkpointed since you last read them. `Read` the file right
before changing it, then apply each change with `Edit`: string-level edits leave
a concurrent session's additions elsewhere in the file intact, while a
whole-file `Write` silently discards them. `Write` is only for creating a file
that does not yet exist.

Keep `LEDGER.md` under ~150 lines and branch state under ~60 — both are injected
at every session start, so they compete with real context. If a topic needs more
room, give it its own file under `.claude/notes/` and leave a one-line pointer.

If `$ARGUMENTS` is non-empty, make sure that topic is covered explicitly.

## Proposed learnings

Subagents may end a response with `LEARNED: <claim> — <evidence>`. Those are
**proposals**. This command is where they are accepted or dropped — that
separation is deliberate: the agent proposing a rule is often the cheapest, least
informed thing in the system, and rules it writes bind every future run.

For each proposal from this session, apply all four tests. Commit it to
`.claude/playbooks/<role>.md` only if it passes all of them:

1. **Durable** — true of this project, not of this task.
2. **Non-obvious** — not something the repo, a type, or a test already says.
3. **Earned** — seen more than once, or once at real cost. A single cheap
   surprise is an anecdote.
4. **Actionable** — it changes what a future run would *do*. "The parser is
   complex" changes nothing.

Write it with its provenance, so a later prune can judge it:

```
- <imperative rule, one line> — <date>, <what proved it>
```

Then check the whole playbook: does this contradict an existing entry? If so,
resolve it now rather than leaving both — a playbook with two opposing rules is
worse than one with neither. Keep each playbook under 60 lines; if committing
this pushes it over, prune something rather than growing the file.

**Dropping a proposal is the common outcome and needs no justification.** Say how
many you saw and how many you kept, and move on.

## Then

Report in three lines: what you added, what you pruned, and the line count of
each file. Then tell the user they can safely `/clear` — and that this is better
than letting auto-compaction fire, because the memory is now on disk.
