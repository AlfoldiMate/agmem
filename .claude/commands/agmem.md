---
description: Migrate a pre-agmem checkout's file memory — LEDGER.md, branch state files, playbooks — into the agmem store, once. Showing and tidying the store are the plugin's /agmem:memory.
argument-hint: "import"
allowed-tools: Read, Bash, Glob, mcp__agmem__recall, mcp__agmem__remember, mcp__plugin_agmem_agmem__recall, mcp__plugin_agmem_agmem__remember
---

# agmem import

`show` and `tidy` live in the agmem plugin as `/agmem:memory show` and
`/agmem:memory tidy`; if `$ARGUMENTS` asks for either, say so and stop. What
this framework still owns is the one-time migration of the file memory it
used before agmem existed.

Resolve the legacy paths:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/hooks/scripts/ctx-flow-paths.nu"
```

Then, for whichever of these exist — `LEDGER=`, every file under
`.claude/notes/state/`, and every `.claude/playbooks/<role>.md`:

1. Read the file. Distil each entry into one atomic, third-person claim —
   ledger decisions and map entries → `fact` (set `valid_from` from the
   entry's date where it carries one), gotchas → `lesson`, playbook rules →
   `lesson` tagged `role:<role>`, branch state → `fact` with `decay_class:
   fast` tagged with that branch's `branch:<slug>` tag (`TAG=` for the
   current branch; the same slug rule for others). A rule the ledger states
   as binding every session → `instruction`, sparingly.
2. Send each file's claims in one `remember` call with the raw file text as
   the `episode`, so every imported claim stays provenanced to what it came
   from. Read the reply: `duplicates` means the store already knew it — fine,
   move on.
3. Skip what the repo already records (git history, code, CLAUDE.md) and any
   entry whose reason has evaporated — import is also a prune, and dropping
   entries is normal.
4. Rename each imported file to `<name>.imported` in place — keep it on disk;
   the `.imported` suffix is what stops a second import and marks the store
   as the live copy. Deleting is the user's call, later.

Report per file: entries seen, claims stored, entries dropped. If none of the
legacy files exist, say the project has no pre-agmem memory and stop.
