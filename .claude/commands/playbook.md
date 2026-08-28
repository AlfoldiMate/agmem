---
description: Create, review, or prune the project's agent playbooks — the per-role knowledge that accumulates in .claude/playbooks/.
argument-hint: "[show | init <role> | prune [role]]"
allowed-tools: Read, Write, Edit, Bash, Glob, Grep
---

# Playbook

Playbooks live at `.claude/playbooks/<role>.md`, one per agent role (`runner`,
`verifier`, `architect`, `browser`, `tracker`, or any project-specific agent).
Each agent reads its own before starting; nothing else loads them, so they cost
nothing until used.

The review gate is the **checkpoint step**, not git: agents only *propose*
learnings (`LEARNED:` lines), and `/checkpoint` decides what lands. That gate is
what makes self-updating files safe, and it holds whether or not `.claude/` is
tracked. If the repo does track `.claude/`, you get a second layer for free — a
learning arrives as a diff someone can reject. If `.claude/` is gitignored or a
symlink shared across worktrees (both supported setups), a committed rule binds
every branch immediately — which is exactly why the checkpoint gate, not the
commit, is where scrutiny lives.

Act on `$ARGUMENTS` (default `show`).

## show

List every playbook with its line count and the date of its most recent entry.
Flag any over 60 lines, and any whose entries are all older than ~3 months —
both are signs it needs a `prune`. Name the roles that have no playbook yet; that
is fine and common, not a gap to fill preemptively.

## init `<role>`

Create `.claude/playbooks/<role>.md` from this template. Do **not** go exploring
to populate it — a playbook fills up from real runs, and a speculative one is
just guesses with a filename.

```markdown
# <role> playbook — <project>
<!-- Accumulated specifics for the `<role>` agent in this repo.
     Cap 60 lines. Merge, never append. Every entry carries its date and evidence. -->

## Rules
- <imperative, one line> — 2026-08-26, <what proved it>

## Landmarks
- <path> — <what lives here and why you'd go there>

## Dead ends
- <the thing that looks right but isn't> — 2026-08-26, <how it misled>
```

`Dead ends` earns its place: negative knowledge is what agents rediscover most
expensively and record least often.

If the repo already tracks `.claude/`, keep `playbooks/` tracked too — the diff
is a free review layer. If `.claude/` is gitignored or symlinked, leave it be;
do not fight the repo's layout to force tracking.

## prune `[role]`

For each entry in the target playbooks, check it against the repo as it is now:

- Does the path still exist? Does the symbol? Does the rule still describe what
  the code does?
- Is it now obvious — covered by a test, a type, a lint rule, or a comment
  someone since wrote? Obvious knowledge is dead weight.
- Is the reason still true? A rule outlives its reason more often than not.
- Is it duplicated by another entry, or by the agent's generic definition?

Delete what fails. Rewrite what is half-right. Report what you cut and why, in at
most five lines. If a playbook ends up empty, delete the file — an empty file
reads as "checked and there's nothing", which is a claim you'd be making falsely.

Never rewrite an entry's date to today when you only reworded it; the date
records when the evidence was seen, not when the line was last touched.
