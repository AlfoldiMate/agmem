# ctx-flow reference

Loaded only when needed. Covers return contracts, dispatch prompts, the memory
mapping, playbook rationale, and where MCP servers fit.

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
Anything longer than N lines → pipe it into
`nu "$CLAUDE_PROJECT_DIR/.claude/scripts/doc-put.nu" <agent> <kind> <title>`
and return `DOC: <id> <uri>`.
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
- **Cap the reply of anything without a contract.** The shipped agents carry
  theirs; a built-in (`Explore`) or a one-off agent gets the cap in the
  prompt, verbatim: *"Reply under 2,000 characters. Anything longer goes into
  `nu "$CLAUDE_PROJECT_DIR/.claude/scripts/doc-put.nu" <agent> report <title>`
  and comes back as `DOC: <id> <uri>`."* Measured over 71 sessions,
  `general-purpose` averaged 16k characters back and `Explore` 11k; the
  reports land whole in the main thread. `general-purpose` is not a target
  at all — `researcher` covers what it was used for.
- **Pass the role's lessons.** `runner`, `tracker`, `browser` and
  `researcher` carry no memory tool. When the store holds a `role:<agent>`
  lesson that applies, paste it into the prompt; the agent treats it as an
  appended rule.

## Parallel fan-out

Independent agents in a single message run concurrently. Good fan-out axes:

- **By subsystem** — one `Explore` per layer, when you don't know which layer owns it
- **By hypothesis** — one per candidate cause, when a bug could be several things
- **By file** — one implementer per file with `isolation: "worktree"`, for
  mechanical migrations only

Bad fan-out: several agents on the same question hoping one gets it right. That
costs N prefixes for one answer. Use `verifier` on the single answer instead.

## Memory mapping

Durable state lives in agmem. The space derives from the parent of the repo's
shared git dir (`git rev-parse --git-common-dir`), so every branch and linked
worktree of a project resolves to one store — bare layouts included, and a
fresh worktree is **not** amnesiac. `user` is the reserved cross-project space
for facts about the person rather than the project. The store sits under the
home directory, so none of this is a git or gitignore question.

The lifetime split the two files used to carry maps onto kinds and decay:

| Was | Becomes | Why it fits |
|---|---|---|
| `LEDGER.md` decisions, Map entries, corrected assumptions | `fact` | fades over weeks unless recalled — an unused fact retires itself |
| `LEDGER.md` Gotchas | `lesson` | fades over months; recall keeps the ones that earn it alive |
| a rule binding every future session | `instruction` | pinned, lands in every `context` briefing — be sparing |
| `state/<branch>.md` Done/Next/Blocked | `fact`, `decay_class: fast`, tag `branch:<slug>` | dies in days, as branch state should, with no file to prune |
| verbatim ground truth (an error transcript, a requirement stated exactly) | `episode` on the same `remember` call | every claim stays provenanced to the text it came from |

The slug in `branch:<slug>` is computed inside the agmem binary — its
SessionStart hook announces it and its checkpoint writes it, one rule for
both sides. `hooks/scripts/ctx-flow-paths.nu` (`TAG=`, `--tag`) carries the
same rule for `/agmem-import`, and `scripts/doc-put.nu` reaches it through
the same `_common.nu` — the one place here that must keep matching it.

Writing well is `/checkpoint`'s job, and its order matters: distil → `recall`
the topic → `remember`, with `supersedes` carrying the id of anything now
stale → read the reply's `duplicates` and `related` before reporting. The
store refuses near-duplicates and never rewrites text, so repeated
checkpoints are idempotent and history survives correction — `inspect` shows
any claim's chain and source, and `recall` with `as_of` replays what was
believed at any past instant.

What does **not** go in the store as a *claim*: anything git records,
anything the code says plainly, anything true only for the current turn.
Long tool output is not a claim either, but it does go in the store — as a
**document**, the named, typed artifact tier over the episode table. A
subagent pipes it into `scripts/doc-put.nu <agent> <kind> <title>` and
returns `DOC: <id> memory://<space>/doc/<id>`; the wrapper tags it
`branch:<slug>` (so `agmem doc list --tag branch:<slug>` is this branch's
artifacts) and `agent:<name>` (so `/checkpoint` knows which role's playbook
a proposal inside it joins). The kinds: `plan` (architect), `review`
(verifier), `report` (runner, browser, scout, tracker, researcher), `probe`,
`transcript`, `other`. A lesson accepted from a document is stored with
`reflect` and `derived_from: ["episode:<id>"]`, so the claim cites the
artifact it came from — the provenance the old `.claude/notes/` dropbox
never had. `agmem doc get <id|title> --raw` reads one back; `/agmem-import`
moves a pre-documents dropbox in.

## Staying ahead of compaction

No hook can call `/clear` — the harness owns session control flow, so a fully
automatic checkpoint-clear-reload does not exist. The loop is manual by one
step: `/checkpoint`, `/clear`, and the plugin's `SessionStart` hook puts the
briefing in front of the fresh session before its first token.

Take the checkpoint at a **seam in the work**, not at a token count. A decision
made, an assumption corrected, a subsystem understood — those are what a fresh
session needs, and they are finished at moments that have nothing to do with how
full the window is. A successful `git push` is the canonical seam — work
shipped, reasons still in context — and the plugin's `PostToolUse` hook
nudges a checkpoint after every one, as it does after an answered
`AskUserQuestion`; its `Stop` hook adds one nudge per session when memory
was recalled and nothing written back. There was a `Stop` hook here once that measured
context from the transcript and blocked the turn past a threshold; it fired
reliably and it fired mid-investigation, which is precisely when there is least
worth writing down.

Leave auto-compaction enabled as a backstop. Disabling it entirely
(`DISABLE_AUTO_COMPACT`) trades a lossy summary for a hard `prompt_too_long`
failure, which is worse. When it fires, the SessionStart restart warning names
it — and a session that keeps seeing that warning is a session checkpointing
too late.

## Playbooks: knowledge that accumulates

### Why not a skill, and why not files

A skill is a file plus a description line in the always-loaded listing. That line
exists so the model can *discover* the skill — and the listing goes to everything
holding the `Skill` tool, subagents included. Role-specific knowledge needs no
discovery: the agent definition names its own `role:<agent>` tag, and the recall
costs nothing until that agent actually runs — where the agent carries the
`recall` tool at all. It costs ~6k tokens of schema per dispatch, so only
`architect`, `verifier` and `scout` carry it; the dispatcher pastes lessons
into a `runner`, `tracker`, `browser` or `researcher` prompt instead, which
is the same knowledge at zero standing cost. (The same reasoning is why the
routing rules themselves moved from a skill into `CLAUDE.md`: rules that apply
to every session shouldn't need discovering, and rules that need discovering
sometimes aren't discovered.)

The file version — `.claude/playbooks/<role>.md` — is retired: tags give the
same role-scoping inside the store, and most of the guards a self-updating file
needed are the store's native behaviour.

### The guards, and why each exists

Self-improving memory fails silently. The playbook drifts subtly wrong,
everything downstream degrades slightly, and nobody notices because nobody reads
it any more. The guards, each against a specific failure:

| Failure | Guard |
|---|---|
| Append-only growth — the playbook becomes the token problem it was solving | Decay: a rule nothing recalls fades instead of taxing every future run; `/agmem:memory tidy` merges what still accumulates. |
| Overfitting — "ALWAYS check X" from one weird bug, taxed forever | Evidence threshold, applied at the `/checkpoint` gate: twice, or once at real cost. |
| Unverified claims compound — a guess becomes load-bearing three sessions later | Provenance: evidence inside the claim, `valid_from` dating it, `inspect` showing source and correction chain. |
| No negative signal — nothing reports a rule gone stale | `consolidate` surfaces contradictions and stale claims for judgement; a wrong rule is closed by `supersedes`, keeping its history. |
| A rule loosens the role it was written for | Recalled rules append, never override. The agent file wins on conflict. |

And the structural one: **proposing is not committing.** Agents emit
`LEARNED: <claim> — <evidence>`; `/checkpoint` decides. A haiku·low agent
editing its own standing instructions is the failure mode in miniature. The
store lives outside git entirely, so this gate is not backed by a diff-review
layer — it is the only scrutiny there is, which is exactly why it is the
load-bearing one and why agents ship with read-only wiring.

Negative knowledge — the thing that looks right but isn't — is what agents
rediscover most expensively and record least often. It makes the best
`role:`-tagged lessons; phrase it as "X looks right here and is wrong because Y".

### Scripts vary by environment, not by self-edits

A self-modifying script is far harder to review than a self-updating claim
store, and its blast radius is larger. Code stays fixed. There are currently no
tunables at all: the last env var, `CTX_FLOW_LEDGER_MAX_LINES`, existed to
bound file injection and died with the files. What genuinely varies by project
is knowledge, and that lives in the store where `/agmem:memory show` reads it.

## Where MCP servers fit

Almost nowhere — that is the position, not an accident. A CLI beats an MCP
server wherever one exists: `gh` over the GitHub server, `acli` over Atlassian,
`playwright-cli` over the Playwright server. You choose the fields, the output
pipes, and no tool schema loads into any prompt.

- **This framework wants exactly two global servers.** `nu --mcp` earns its
  slot because it is not a wrapper around a CLI — it *is* the shell, with
  structured pipelines, `$history` for re-slicing past results without
  re-running, and server-side truncation that makes uncapped first runs safe.
  `agmem` earns its slot the same way: it *is* the memory — cross-session
  state that must outlive every process, which no CLI invocation holds. Both
  hold state; that is the test, and a stateless wrapper fails it.
- **If a project genuinely needs another server** (brokered OAuth, a live
  stateful session no CLI exposes), scope it in that project's `.mcp.json`,
  never globally, and trim it with its capability flags — cutting a 90-tool
  server to 8 improves tool choice everywhere.
- **Reach MCP results only from a dedicated agent.** They are the largest
  objects in the system and the best delegation candidates you have. (agmem is
  the exception by design: its replies are claims, already distilled, and the
  agents that recall carry their own read-only wiring.)
