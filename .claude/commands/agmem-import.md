---
description: Migrate a checkout's retired .claude/notes into the agmem store, once — subagent artifacts become documents, a pre-agmem LEDGER.md and branch state files become claims. Showing and tidying the store are the plugin's /agmem:memory.
allowed-tools: Read, Bash, Glob, mcp__agmem__recall, mcp__agmem__remember, mcp__plugin_agmem_agmem__recall, mcp__plugin_agmem_agmem__remember
disable-model-invocation: true
---

# agmem-import

Showing and tidying the store are the plugin's `/agmem:memory show` and
`/agmem:memory tidy`. What this framework still owns is the one-time
migration of the files it used before the store held them: the `.claude/notes/`
dropbox subagents wrote artifacts to, and before that the LEDGER and branch
state files. In a checkout that has already run it (the files carry an
`.imported` suffix) this is a no-op.

Resolve the legacy paths:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/hooks/scripts/ctx-flow-paths.nu"
```

## 1. Artifacts → documents

Deterministic, so a script does it — the kind comes from the filename
(`plan-`/`-plan` → plan, `review-` → review, `research-`/audits → report or
review, `.txt` → probe, else other), the title is the file stem, every
import carries the tag `legacy-notes`:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/import-notes.nu" --dry-run
```

Show the user the table. If a kind looks wrong, `agmem doc put` that file
by hand afterwards with the right `--kind`. Then run it without `--dry-run`;
it renames each imported file to `<name>.imported` and lists what it left
alone — scripts and subdirectories, which the steps below or the user
handle.

## 2. Ledger and state → claims

For whichever of these exist — `LEDGER=` and every file under `NOTES=/state/`:

1. Read the file. Distil each entry into one atomic, third-person claim —
   ledger decisions and map entries → `fact` (set `valid_from` from the
   entry's date where it carries one), gotchas → `lesson`, branch state →
   `fact` with `decay_class:
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
   as the live copy.

## 3. Report, then the directory

Per file: entries seen, claims stored, entries dropped; per document: kind
and id. If nothing under `NOTES=` was importable, say the project has no
pre-documents memory and stop.

The directory itself is meant to be gone — nothing reads it, and
`/ctx-flow-doctor` reports it while it holds files. Deleting is the user's
action: tell them the `rm -r` to run once they have checked the table.
