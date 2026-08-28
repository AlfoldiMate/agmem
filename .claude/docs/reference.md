# ctx-flow reference

Loaded only when needed. Covers return contracts, dispatch prompts, the ledger
format, playbook rationale, and where MCP servers fit.

## Why return contracts matter

A subagent with no specified output shape writes for a human reader: preamble,
restatement, findings, caveats, next steps. That is 1–3k tokens carrying maybe
150 tokens of information. The same agent given a rigid shape returns the 150.

Three properties make a contract work:

1. **Fixed keys.** The caller can parse it without reading it.
2. **Hard caps.** "At most 5 entries", "under 12 words" — an unbounded list is
   an invitation to pad.
3. **An explicit forbidden list.** "No preamble, no code blocks, no
   recommendations" does more work than any positive instruction, because the
   default behaviour is exactly those things.

Put the contract at the **end** of the agent file. It's the last thing read and
the first thing obeyed.

## Contract template

```
VERDICT_KEY: <enum of 2-4 values>

<SECTION>:
- <shape of one entry>     (at most N)

<CLOSING_KEY>: <one sentence>

Forbidden: <the specific padding this role tends to produce>
Anything longer than N lines → write to .claude/notes/ and return the path.
```

## Dispatching well

The quality of a subagent's answer is bounded by how well the question was
posed. When you dispatch:

- **Give it the conclusion you need, not the task.** "Find where session expiry
  is enforced" beats "look into the auth code".
- **Give it what you already know** — the two files you've ruled out, the
  symbol name, the error text. Otherwise it re-derives it and you pay twice.
- **Don't give it your reasoning.** It doesn't need your hypothesis; a
  hypothesis biases a search agent toward confirming it.
- **One question per agent.** Two questions in one prompt produce one good
  answer and one lazy one. Dispatch two agents in the same message instead.

## Parallel fan-out

Independent agents in a single message run concurrently. Good fan-out axes:

- **By subsystem** — one `Explore` per layer, when you don't know which layer owns it
- **By hypothesis** — one per candidate cause, when a bug could be several things
- **By file** — one implementer per file with `isolation: "worktree"`, for
  mechanical migrations only

Bad fan-out: several agents on the same question hoping one gets it right. That
costs N prefixes for one answer. Use `verifier` on the single answer instead.

## Memory layout

Two files, split by **lifetime**. Both are injected at every session start, so
both compete with real context — prune ruthlessly. A stale ledger is worse than
no ledger.

| File | Holds | Lifetime | Cap |
|---|---|---|---|
| `<main worktree>/.claude/notes/LEDGER.md` | Goal, Map, Gotchas, architectural decisions | outlives the branch | ~150 lines |
| `<main worktree>/.claude/notes/state/<branch>.md` | Done, Next, Blocked, in-flight decisions | dies with the branch | ~60 lines |

The durable file resolves through `git rev-parse --git-common-dir`, which points
at the one real `.git` from anywhere in the repo. That makes it shared by every
linked worktree — a fresh worktree is **not** amnesiac — and it holds whether
the worktree's `.claude` is a symlink to the main worktree's or its own
directory: both resolve to the same files.

Deciding which file an entry belongs in: **would this still be true after the
branch merges?** Yes → ledger. No → branch state. When unsure, the ledger — a
wrong entry there gets pruned, a lost one is gone.

Git tracking is optional. If the repo tracks `.claude/`, commit `LEDGER.md` and
gitignore `notes/state/` — durable knowledge is worth sharing with anyone who
clones. If `.claude/` is gitignored or symlinked, memory works identically from
disk; you only lose the diff-review layer, and the checkpoint gate (below)
carries the scrutiny instead. When writing a `.gitignore` rule, use `.claude/*`
rather than `.claude/` — git does not descend into an excluded directory, so
`!` negations under `.claude/` would be silently dead.

### LEDGER.md

```markdown
# Ledger — <project>

## Goal
<what we're actually trying to achieve, 1-3 lines — outlives any single task>

## Decisions
- 2026-08-26 — chose X over Y because <reason that isn't obvious from the code>

## Map
- path/to/thing — what it is and why you'd open it

## Gotchas
- <the thing that cost an hour and would cost it again>
```

### state/<branch>.md

```markdown
# <branch>

## State
- Done: <shipped and verified>
- Next: <the immediate next action>
- Blocked: <what's stuck and on what>

## In flight
- <decision made for this branch only, with its reason>
```

What does **not** go in either: anything git records, anything the code says
plainly, anything true only for the current turn. Absolute dates, never
"yesterday".

## Staying ahead of compaction

No hook can call `/clear` — the harness owns session control flow, so a fully
automatic checkpoint-clear-reload does not exist. The loop is manual by one
step: `/checkpoint`, `/clear`, and `SessionStart` reloads both files.

Take the checkpoint at a **seam in the work**, not at a token count. A decision
made, an assumption corrected, a subsystem understood — those are what a fresh
session needs, and they are finished at moments that have nothing to do with how
full the window is. A successful `git push` is the canonical seam — work
shipped, reasons still in context — and the `PostToolUse` hook nudges a
checkpoint after every one. There was a `Stop` hook here once that measured
context from the transcript and blocked the turn past a threshold; it fired
reliably and it fired mid-investigation, which is precisely when there is least
worth writing down.

Leave auto-compaction enabled as a backstop. Disabling it entirely
(`DISABLE_AUTO_COMPACT`) trades a lossy summary for a hard `prompt_too_long`
failure, which is worse. `PreCompact` stamps the ledger with a running count
when it fires, so "this keeps happening" is visible rather than silent.

## Playbooks: knowledge that accumulates

### Why not a skill

A skill is a file plus a description line in the always-loaded listing. That line
exists so the model can *discover* the skill — and the listing goes to everything
holding the `Skill` tool, subagents included. Role-specific knowledge needs no
discovery: the agent definition already names the file. Making it a skill would
therefore charge every subagent for knowledge only one of them ever reads.

So: plain markdown at `.claude/playbooks/<role>.md`, named by the agent that
reads it. (The same reasoning is why the routing rules themselves moved from a
skill into `CLAUDE.md`: rules that apply to every session shouldn't need
discovering, and rules that need discovering sometimes aren't discovered.)

### The guards, and why each exists

Self-improving memory fails silently. The playbook drifts subtly wrong,
everything downstream degrades slightly, and nobody notices because nobody reads
the file any more. The guards, each against a specific failure:

| Failure | Guard |
|---|---|
| Append-only growth — the playbook becomes the token problem it was solving | Cap 60 lines. Merge, never append. |
| Overfitting — "ALWAYS check X" from one weird bug, taxed forever | Evidence threshold: twice, or once at real cost. |
| Unverified claims compound — a guess becomes load-bearing three sessions later | Provenance: date + evidence on every entry. |
| No negative signal — nothing reports a rule gone stale | `/playbook prune` re-checks entries against the repo. |
| A rule loosens the role it was written for | Playbooks append, never override. The agent file wins on conflict. |

And the structural one: **proposing is not committing.** Agents emit
`LEARNED: <claim> — <evidence>`; `/checkpoint` decides. A haiku·low agent
editing its own standing instructions is the failure mode in miniature. This
gate is what makes self-updating files safe — git tracking, where the repo has
it, is a second layer, not the load-bearing one, because `.claude/` may be
gitignored or symlinked across worktrees and the framework must hold either way.

### Sections that earn their place

`Rules` (imperative, dated), `Landmarks` (paths worth knowing), and `Dead ends` —
the thing that looks right but isn't. Negative knowledge is what agents
rediscover most expensively and record least often, and it is the section most
worth keeping.

An empty playbook is a fine outcome. Delete one that prunes to nothing rather
than leaving an empty file, which reads as "checked, nothing here" — a claim you
would be making falsely.

### Scripts vary by environment, not by self-edits

A self-modifying script is far harder to review than a self-modifying markdown
file, and its blast radius is larger. Code stays fixed; the only thing that
varies per project or per session is an environment variable:

| Env var | Default | Effect |
|---|---|---|
| `CTX_FLOW_LEDGER_MAX_LINES` | `400` | cap on the injected ledger |

There used to be a config file under this, resolving **env > config > default**.
It is gone. A config file makes a hook parse, validate, and fall back silently
on malformed input — three new ways for a hook to break the session it exists to
help — and every threshold it held was already reachable through the env var
that overrode it. What genuinely varies by project is knowledge, and that
belongs in a playbook where a human reads it.

## Where MCP servers fit

Almost nowhere — that is the position, not an accident. A CLI beats an MCP
server wherever one exists: `gh` over the GitHub server, `acli` over Atlassian,
`playwright-cli` over the Playwright server. You choose the fields, the output
pipes, and no tool schema loads into any prompt.

- **The one MCP server this framework wants is `nu --mcp`.** It earns the slot
  because it is not a wrapper around a CLI — it *is* the shell, with structured
  pipelines, `$history` for re-slicing past results without re-running, and
  server-side truncation that makes uncapped first runs safe. Use it for
  anything data-shaped and queryable; use plain `Bash` for simple side effects.
- **If a project genuinely needs another server** (brokered OAuth, a live
  stateful session no CLI exposes), scope it in that project's `.mcp.json`,
  never globally, and trim it with its capability flags — cutting a 90-tool
  server to 8 improves tool choice everywhere.
- **Reach MCP results only from a dedicated agent.** They are the largest
  objects in the system and the best delegation candidates you have.
