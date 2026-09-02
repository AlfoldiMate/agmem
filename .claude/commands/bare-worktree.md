---
description: Manage the bare-repo + sibling-worktrees layout — init/transform a repo, add/remove worktrees, apply/discard/inspect profiles.
argument-hint: "<init|add|remove|apply|discard|which> [args]"
allowed-tools: Bash, Read, AskUserQuestion
---

# bare-worktree

Everything is implemented in the backing script; this command runs it and
interprets the result. The script is standalone nu — the layout, the profile
format, and every subcommand are documented in its header comment:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/bare-worktree.nu" $ARGUMENTS
```

Run it from the directory the user means — subcommands resolve the layout from
the cwd (root vs. inside a worktree changes what `apply`, `add`, and `which`
do). With no arguments it prints usage; show that rather than guessing a
subcommand.

Care points, in order of severity:

- **`init` on an existing repo is a transformation** — history moves into
  `.bare`, the working tree is re-created as a worktree, gitignored files move
  into `.profiles/dflt`. Confirm with the user before running it on a repo
  that was not just created for this. It refuses a dirty tree; relay that
  refusal as-is, do not stash or commit on the user's behalf to get past it.
- **After a transform, relay the junk warning verbatim** (node_modules and
  friends landing in `.profiles/dflt`) and offer to delete those — they are
  rebuildable and only bloat the profile.
- **`remove` refuses when untracked files remain**; the error explains
  `--force`. Ask before re-running with `--force` — those files are usually
  rebuildable junk, but it is the user's call.
- **`discard` keeps copies that diverged from their source** and says so; a
  kept file is the user's work, never delete it manually to "finish the job".
- Warnings about entries that are not gitignored mean the user's `.gitignore`
  needs a pattern fix (symlinks don't match `dir/` patterns) — surface them.
