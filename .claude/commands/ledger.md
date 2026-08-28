---
description: Show, initialise, or prune the project ledger and the current branch's state file.
argument-hint: "[show | init | prune]"
allowed-tools: Read, Write, Edit, Bash, Glob
---

# Ledger

Operate on both memory files according to `$ARGUMENTS` (default: `show`).

Resolve both paths first, and use exactly what it prints:

```bash
nu "$CLAUDE_PROJECT_DIR/.claude/hooks/scripts/ctx-flow-paths.nu"
```

This is the same resolver `SessionStart` uses to read the files back, so the two
sides cannot drift. The durable ledger lands in the main worktree (found via
`git rev-parse --git-common-dir`, so it is shared by every worktree and branch);
branch state sits beside it under `state/`, keyed by a sanitised branch name.
`STATE=` is empty on a detached HEAD.

**show** — print both, with line counts, and say which branch you're on. If
either is missing, say so and offer to `init`. Add one line of assessment: is
anything here stale, wrong, or contradicted by what the repo says right now?

**init** — create from the templates in
`$CLAUDE_PROJECT_DIR/.claude/docs/reference.md`, pre-filled from what you can
determine cheaply: project name, the goal if the README states it, and a Map of
the three or four entry-point files. Do not go exploring to fill it in — it fills
up as work happens. If the repo tracks `.claude/` (check with
`git check-ignore .claude/notes`), add `.claude/notes/state/` and
`.claude/notes/*.log` to `.gitignore`, leaving `LEDGER.md` tracked. If `.claude/`
is gitignored or a symlink, skip the gitignore step silently — memory works from
disk either way.

**prune** — cut the ledger back under ~150 lines and branch state under ~60.
Delete anything git records, anything now obvious from the code, anything
resolved, and any entry whose reason has evaporated. Verify Map paths still
exist. Also delete state files for branches that no longer exist
(`git branch --list` to check). Report what you cut in at most five lines.

Never delete either file outright without confirming.
