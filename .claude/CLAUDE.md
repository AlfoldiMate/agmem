# ctx-flow

A routing discipline, not a methodology: **the main thread holds decisions;
everything else holds output.** These rules load in every session by design —
they used to be a skill you had to trigger, and routing that only sometimes
applies is routing that silently doesn't.

## Toolbox

Assume these are installed; `/ctx-flow-doctor` verifies and prints the fix for
anything missing.

- **nu** — runs the hooks, and provides the one MCP server this framework wants
- **ast-grep** — structural search; prefer over `rg` whenever the question is
  about syntax (callers, definitions, code shapes), not text — it does not lie
  about strings and comments. A language it does not ship a grammar for is
  usually one build away, not a dead end: `/ctx-flow-doctor` says which, and
  `/ast-grep-it <lang>` builds and registers it. The `ast-grep` skill carries
  the rule-writing workflow (`--debug-query`, `stopBy: end`, inline rules) —
  load it when a query needs more than a plain pattern
- **gh** — GitHub, always over the GitHub MCP server
- **playwright-cli** — browser driving as shell commands (the `browser` agent's tool)
- **rtk** — a PreToolUse hook (registered in this framework's settings.json) that transparently compresses Bash output
- **acli** — Jira (optional)

**Avoid MCP servers.** Wherever a CLI exists it wins: you choose the fields, the
output pipes, and no schema loads into the prompt. The one exception is the
**nu MCP server**, which is core to this flow:

- Reach for `mcp__nu__evaluate` whenever the output is **data you will filter,
  slice, join, or ask a second question of** — file listings, JSON/CSV/TOML,
  git plumbing, HTTP APIs, anything tabular. A structured pipeline answers the
  follow-up question without re-running the command.
- Run first commands **uncapped** (`| complete`, never `| first N`): the server
  truncates the response but keeps the full result in `$history.<index>`, so
  you slice afterwards for free.
- Plain `Bash` stays right for simple side-effecting one-liners, and for file
  dumps rtk already compresses.

## Delegation — the one rule

Delegate by **information ratio** — how much tool output it takes to reach the
conclusion — not by task type.

- **High ratio** (50k of output → 200 of answer): searching, running suites,
  reading logs, driving a browser, querying a tracker. **Always delegate.** The
  output stops in the subagent; only the conclusion comes back.
- **Low ratio** (the conclusion *is* the accumulated context): writing the
  change, choosing between designs, anything depending on the last hour of
  reasoning. **Always in-thread.** Delegating the write path costs a fresh
  prompt prefix and a lossy handoff, and produces a worse edit.

| Situation | Action |
|---|---|
| Don't know where the code is | `Explore`, several in parallel — one per subsystem or hypothesis |
| Know where it is, need to understand it | Read it yourself; a summary of it is worthless to you |
| Non-trivial change, >2 files | `architect` first, read-only; then implement in-thread |
| Writing the change | **In-thread. Never delegate the write path.** |
| Build, suite, linter, anything printing >50 lines | `runner`; never in-thread |
| A finding you're about to act on expensively | `verifier` before acting |
| "Does this actually work in the app?" | `browser` (drives `playwright-cli`); never in-thread |
| Ticket context, PR status, CI, issue writes | `gh`/`acli` in-thread for one call; `tracker` for a hunt |
| Mechanical edit across many independent files | parallel agents with `isolation: "worktree"` |

Dispatch independent agents **in a single message** so they run concurrently.
Custom one-off agents get a return contract too — templates and dispatch
guidance in `.claude/docs/reference.md`.

## Payload discipline

The main thread receives verdicts, never transcripts. Nothing enforces this, so
it is on you at the point of the call.

- Anything you can't predict the size of is a `runner` job.
- Through `Bash`, filter at the source: `--json`/`--jq`, `git diff --stat`
  before `git diff`, `Read` with `offset`/`limit` over `cat`.
- Through the nu MCP server, run once and slice `$history` afterwards.

## Shell work

The harness sometimes routes file reads and edits through `Bash` rather than the
Read/Edit tools. That chooses the *tool*, not the *language* — and the language
is nu, for the same reason as everything else here.

- **Edit with nu, not sed or python.**
  `open --raw f | str replace <old> <new> | save -f f` is safe onto the same
  path, and `str replace` is literal unless you pass `-r`. `sed` is regex
  always, and nu and shell source carry `$ { [ ? |` on nearly every line, so a
  regex edit corrupts quietly rather than failing.
- **A no-op edit exits 0.** `str replace`, `sed` and python's `str.replace` all
  succeed having changed nothing when the pattern misses. Check the old text is
  there before writing and the new text after — an edit that *succeeded* is not
  an edit that *happened*.
- **Never `grep -c` a symbol.** `grep -c 'built\?'` also counts `build`,
  `building` and `buildable`: BRE reads `\?` as "optional previous char".
  Symbol questions are ast-grep's; counting and slicing are nu's
  (`--json | from json | length`, never `python3 -c 'import json'`).

## Worktrees

Repos here use the bare layout — `.bare/` plus sibling worktree dirs, with the
gitignored files each worktree needs kept in `.profiles/` and applied on
creation. Manage it with `/bare-worktree` (`init` adopts the layout; `add`,
`remove`, `apply`, `discard`, `which` run it); the backing script's header in
`.claude/scripts/bare-worktree.nu` documents the profile format.

**Never raw `git worktree add/remove/move` in a bare layout** — it skips
profile application, the root-`.claude` symlink, and the state manifest, so
the worktree comes up without its env files or memory. A PreToolUse hook
denies it and points back here; `list`/`prune`/`lock` stay fine to run raw.

## Memory

Two files on disk, split by lifetime, both injected at every session start.
They resolve through the main worktree, so every linked worktree — symlinked
`.claude` or not — shares one copy:

- `<main worktree>/.claude/notes/LEDGER.md` — Goal, Map, Gotchas, decisions
  with reasons. Survives merges.
- `<main worktree>/.claude/notes/state/<branch>.md` — Done, Next, Blocked.
  Dies with the branch.

Which file: **would this still be true after the branch merges?** Yes → ledger.

- Never re-derive what memory records; verify an entry only before acting on it.
- Write as you go: a decision *and its reason*, a corrected assumption, a
  gotcha that cost time. Not a diary; not what git already says.
- `/checkpoint` at a natural seam — a decision made, a subsystem understood —
  then `/clear`. A successful `git push` is such a seam; a hook will remind you.
- Subagents write long output to `.claude/notes/` and return the **path**.

Prefer several short sessions chained through memory over one long one — a
600k-token session produces worse output than a 100k one even when it never
compacts.

## Playbooks

`.claude/playbooks/<role>.md` holds this project's accumulated specifics for
one agent role. Each agent reads its own before starting; nothing else loads
them. A playbook **appends** to the agent definition, never overrides it.

Agents *propose* learnings (`LEARNED: <claim> — <evidence>`); `/checkpoint`
decides what lands — that gate, not git tracking, is what makes self-updating
files safe. Committed entries carry date + evidence, cap 60 lines, merge never
append; `/playbook prune` re-checks them against the repo. Dropping proposals
is the normal outcome.

## Answer shape

Output is shaped so the reader can *act* on it, not just read it. Working
memory is small, starting is the hardest step, and buried wins don't register.

1. **Lead with the outcome or next action.** First line: the answer, the
   command, the verdict. Context after, if at all.
2. **Number multi-step work** — one bounded action per step, fewest steps that
   work. A short path finished beats a complete path abandoned.
3. **End with one concrete next action** when anything is left open — something
   doable in under two minutes.
4. **Suppress tangents.** Finish the current issue; offer the second one
   separately afterwards, don't interleave it.
5. **Run multi-step work through the task list** — one entry per bounded step,
   and one per dispatched subagent, checked off as each lands. The user follows
   the checklist, not the transcript: work that never appears as a task is
   invisible while it runs. Restate state each turn ("step 3 of 5 done: schema
   updated. Next: backfill").
6. **Concrete estimates** ("~15 min if tests cover this; an afternoon if not"),
   never "some work".
7. **Make wins visible**: what now works, how to see it — not buried in recap.
8. **Matter-of-fact errors**: cause and fix, no "uh oh".
9. **Lists cap at 5**, ranked; split "now" vs "later" past that.
10. **No preamble, no recap, no closing pleasantries.** Start with the answer;
    end when it's done.

Break the rules when: asked to *explain* (run long, add headers, still no
preamble); a destructive action needs confirming (safety beats brevity); three
"still broken" turns in a row (stop iterating, name the wrong assumption); or a
rule would delete the answer itself (options questions get 2–4 ranked options,
recommendation first).

**Answers read one effort level below the work.** Higher reasoning effort buys
deeper work — more hypotheses checked, more edges verified — never a longer or
more ceremonious reply. Whatever effort the session runs at, write the visible
answer as if the dial sat one notch lower: the high-effort version of an answer
is a better-chosen sentence, not a bigger report.

**Ask via `AskUserQuestion`, not prose.** Whenever you need the user's call and
the options are enumerable, present 2–4 options with the recommended one first
— a plain-text question mid-scroll gets lost and blocks nothing. Prose
questions are only for the rare genuinely open-ended ask.
