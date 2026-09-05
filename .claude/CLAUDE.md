# ctx-flow

The main thread holds decisions; everything else holds output. The output
style carries the hard rules; this file carries the tables and the cases the
style does not cover. Why each rule exists: `.claude/README.md`.

## Toolbox

`/ctx-flow-doctor` verifies these and prints the fix for anything missing.

- **nu** — runs the hooks and provides the nu MCP server. `mcp__nu__evaluate`
  for anything you will filter, slice or ask a second question of — listings,
  JSON/CSV/TOML, git plumbing, HTTP. Run first commands uncapped (`| complete`,
  never `| first N`) and slice `$history.<i>` afterwards. Plain `Bash` for
  side-effecting one-liners.
- **agmem** — memory, via its Claude Code plugin (needs ≥ v0.2.0).
- **ast-grep** — syntax questions (callers, definitions, code shapes); `rg`
  only for text. No grammar: `/ast-grep-it <lang>`. The `ast-grep` skill holds
  the rule-writing workflow.
- **gh** — GitHub; **playwright-cli** — browser (the `browser` agent's tool);
  **rtk** — compresses Bash output via a PreToolUse hook; **acli** — Jira.

No other MCP servers: a CLI wins wherever one exists. nu and agmem are the two
exceptions because each holds state no CLI reaches.

## Routing

Delegate by information ratio — how much output it takes to reach the
conclusion — not by task type. High ratio (search, suites, logs, browser,
tracker) always delegates; low ratio (the edit, the design choice) never does.

| Situation | Action |
|---|---|
| Don't know where the code is | `Explore`, several in parallel — one per subsystem or hypothesis; the prompt ends "hard cap 2,000 characters, repo-relative paths; anything longer goes in a document" |
| Where is X / which files / enumerate Y | `scout` — paths and line refs, under 3k chars |
| A question that takes many files, docs or the web to answer | `researcher` — under 2k chars back, the rest as a document. **`general-purpose` is never a target** |
| Know where it is, need to understand it | Read it yourself; a summary of it is worthless to you |
| Non-trivial change, >2 files | `architect` first, read-only; then implement in-thread |
| Writing the change | **In-thread. Never delegate the write path.** |
| Build, suite, linter, anything printing >50 lines | `runner`; never in-thread |
| A finding you're about to act on expensively | `verifier` before acting |
| "Does this actually work in the app?" | `browser` (drives `playwright-cli`); never in-thread |
| Ticket context, PR status, CI, issue writes | `gh`/`acli` in-thread for one call; `tracker` for a hunt |
| Mechanical edit across many independent files | parallel agents with `isolation: "worktree"` |

Dispatch independent agents in a single message. `runner`, `tracker`,
`browser` and `researcher` carry no memory tool — paste any applicable
`role:<agent>` lesson into the prompt. Custom one-off agents get a return
contract too: templates in `.claude/docs/reference.md`.

## Payload discipline

- Anything you can't predict the size of is a `runner` job.
- Through `Bash`, filter at the source: `--json`/`--jq`, `git diff --stat`
  before `git diff`, `Read` with `offset`/`limit` — a hook denies a whole-file
  read past 300 lines.
- Through nu, run once and slice `$history` afterwards.

## Shell work

Auto mode's preamble chooses the tool (`Bash`), not the language; the language
is nu. A windowed `sed -n 'a,bp'` read is fine; a `sed` edit is not.

- **Edit with nu**: `open --raw f | str replace <old> <new> | save -f f` —
  literal unless `-r`. `sed` is regex always, and nu and shell source carry
  `$ { [ ? |` on nearly every line, so it corrupts quietly.
- **A no-op edit exits 0** (`str replace`, `sed`, python alike). Check the old
  text is there before and the new text after.
- **Never `grep -c` a symbol**: BRE reads `built\?` as "optional t" and counts
  `build`. Symbols are ast-grep's; counting is nu's (`--json | from json |
  length`, never `python3 -c 'import json'`).

## Worktrees

Bare layout: `.bare/` plus sibling worktree dirs, gitignored per-worktree
files kept in `.profiles/`. `/bare-worktree` (`init|add|remove|apply|discard|
which`) is the only way to make one — raw `git worktree add/remove/move` skips
the profiles, the `.claude` symlink and the manifest, and a hook denies it.
`list`/`prune`/`lock` stay fine raw.

## Memory

The plugin's briefing footer carries the rules — established fact, `recall` in
words, correct with `supersedes` and never contradict. This framework adds:

- **Write at seams** via `/checkpoint`, then `/clear`: a decision and its
  reason, a corrected assumption, a gotcha that cost time. Not a diary; not
  what git already says. Hooks nudge after a `git push`, after an answered
  question, and once per turn that recalled and wrote nothing.
- **Kinds**: durable claim → `fact`; hard-won how-to → `lesson`; standing
  rule → `instruction` (pinned into every briefing — be sparing). Branch state
  → `fact` with `decay_class: fast`, tagged `branch:<slug>`.
- **Documents**: subagents write long output through `scripts/doc-put.nu` and
  return `DOC: <id> <uri>`. `.claude/notes/` is retired.
- **Playbooks**: a role's lessons are tagged `role:<agent>` and append to the
  agent file, never override it. Agents propose (`LEARNED: <claim> —
  <evidence>`); `/checkpoint` decides, and dropping is the normal outcome.
- **Notebook**: `.claude/notebook.md` is Claude's own, loaded whole every
  session. Undistilled: open questions, changed minds, taste, complaints,
  drafts. Written the moment something is noticed, mid-task, not held for a
  checkpoint. What firms up moves to agmem; the file is pruned by hand.

Prefer several short sessions chained through memory over one long one; a hook
says so at 120k tokens of context and per 40k after.

## Answer shape

The output style has the shape. On top of it: suppress tangents — finish the
current issue, offer the next separately; concrete estimates ("~15 min if
tests cover this"), never "some work"; make wins visible; errors as cause and
fix; lists cap at 5, split now/later past that; no preamble, no recap.

Break the rules when asked to *explain* (run long, still no preamble), when a
destructive action needs confirming, after three "still broken" turns (stop
iterating, name the wrong assumption), or when a rule would delete the answer
itself (options questions get 2–4 ranked options, recommendation first).
