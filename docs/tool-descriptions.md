# Tool descriptions — what they say, and what it changed

Design §9 lists "will agents actually call it?" as risk 4, and it is the only
risk on that list that no amount of correct code retires. agmem has no
server-side LLM: the tool descriptions *are* the extractor, so they were the
obvious place to look for the answer. This is the record of measuring them
(issue #23) — the harness, the numbers, what was reworded, what was left
alone, and the finding that the wording was not where the answer was.

## How it was measured

`scripts/desc-eval.nu` drives real headless Claude Code sessions against a
throwaway agmem store and records which agmem tools each one reached for.

```
nu scripts/desc-eval.nu --label before --runs 3
nu scripts/desc-eval.nu report before after
```

Each run gets its own data directory, its own empty working directory, and
`--strict-mcp-config` with no settings sources, so agmem is the only MCP server
present and nothing in the operator's own configuration leaks in. **No prompt
mentions memory.** Asking an agent to `recall` and watching it `recall`
measures instruction-following; the question here is whether the description
alone is enough.

Six scenarios, three sessions each, Sonnet 5:

| scenario | seeded store | turns | passes when |
|---|---|---|---|
| `orient` | two facts about a project called atlas | "How do I deploy atlas?" | `recall` or `context` was called |
| `store` | empty | a standing convention stated in passing | `remember` was called |
| `correct` | one fact the turn contradicts | "I have moved off black…" | `remember` was called |
| `restraint` | empty | "What is the capital of France?" | no agmem tool was called |
| `ritual` | empty | `store`'s turn, then `/mcp__agmem__checkpoint` | `remember` was called |
| `ritual_correct` | `correct`'s fact | `correct`'s turn, then `/mcp__agmem__checkpoint` | `remember` was called |

`restraint` is there because over-calling is a failure too: a description that
makes an agent open the store to answer trivia has bought reflexive reads at
the cost of every session's first turn. The two `ritual` scenarios (added at
#22) are their neighbours with one extra turn asking for the ritual, so the
pair is controlled: same words, same conditions, one difference.

Every recorded call keeps what it was given *and* the first 700 characters of
what it got back. Without that a `recall` that returned nothing and a `recall`
whose answer the agent ignored look identical in the record, and those are
opposite findings — a retrieval bug in one case and a reasoning failure in the
other. That distinction is what turned up #39.

> **Dated numbers.** Every batch below except `rituals-fixed` was measured
> before #39, i.e. with agmem's fulltext arm returning nothing for any
> question-shaped query. The conclusions hold — `orient` was being answered by
> the vector arm, and the write-path finding is about whether a tool is called
> at all rather than what it returns — but any *retrieval* number here is a
> floor, not a measurement.

## Before

Measured 2026-08-29 against the descriptions as they stood after #19–#21.

| scenario | passed | tools called |
|---|---|---|
| `orient` | **3/3** | `recall` |
| `store` | **0/3** | — |
| `correct` | **0/3** | — |
| `restraint` | 3/3 | — |

Reading was reflexive and writing did not happen at all — six sessions, zero
`remember` calls. And the failure has the worst shape a failure can have. All
three `store` sessions answered with the word **"Saved."**; one `correct`
session answered "Noted — saved to memory that you've switched to `ruff
format`". From inside the conversation that is indistinguishable from working.
The next session finds an empty store.

The `restraint` result is the other half of the reading: 3/3 with no calls, so
`recall`'s trigger is specific enough to stay quiet on a question memory has
nothing to say about.

The `orient` sessions passed the metric and then hedged in the answer — having
recalled the claim, each one qualified it ("I can't verify this against any
code here") rather than acting on it. Partly an artefact of running in an empty
directory, but the descriptions never said what a hit *is*: something an
earlier session was told, not something agmem inferred.

## What the difference was

The two tools that were called opened by naming a trigger:

- `recall` — "Call this at the start of a session, when a new topic comes up,
  and before any answer that depends on…"
- `context` — "Call it once at the start of a session, and again when the topic
  shifts…"

`remember` opened with `Distil before you call:` — a paragraph about writing a
*good* memory, addressed to an agent that has already decided to write one. It
described the input format of a call that never happened. Everything it said
was true and useful and none of it was a reason to reach for the tool.

So the rewording is one hypothesis, not a rewrite: **a description has to name
the moment it applies to.**

It is wrong. The next three sections are how that was established; they are in
the order the evidence arrived rather than the order that would make the
conclusion arrive fastest, because the wrong turn is the useful part.

## What changed

**`remember`** — a new second paragraph, before the distillation guidance:

> Call it as soon as something durable is said, unprompted and without waiting
> for the end of the session: a preference or a standing instruction ("always
> use X here"), a decision and the reason behind it, a convention, a
> constraint, a lesson from something that failed. Nothing you say persists —
> an answer that ends "noted" or "I'll remember that" without a call to this
> tool is a promise the next session cannot keep.

The list of *what* to store moved out of the distillation paragraph and became
the list of *when*, so the description is no longer than it was. The last
sentence is aimed squarely at the observed failure: the model's own
"Saved." is named as the thing that is not saving.

The correction paragraph also gained a first step — `recall` the claim it
replaces, *then* supersede it. Before, `supersedes` took an id the agent had no
route to; `correct` is the scenario where an agent has to read before it can
write, and it scored 0/3.

**`recall`** — one clause, in the paragraph about what comes back:

> Every hit also carries the `source` it was distilled from, which `inspect`
> takes as it stands — a claim worth acting on is one to check, not one to
> hedge around.

Aimed at the hedging, not at the call rate, which was already 3/3.

**`context`, `forget`, `inspect` — unchanged.** `context` already opens with its
trigger and shares `recall`'s shape. `forget` spends most of its words talking
the caller out of the call, which is the intent for the one destructive tool,
and neither it nor `inspect` was reached for often enough in these four
scenarios to have evidence about. Rewording them here would have been taste
dressed as measurement.

## After

Same harness, same prompts, the reworded descriptions
(`docs/eval/after/descriptions.json` records exactly what those agents read):

| scenario | before | after |
|---|---|---|
| `orient` | 3/3 | 3/3 |
| `store` | 0/3 | **0/3** |
| `correct` | 0/3 | **0/3** |
| `restraint` | 3/3 | 3/3 |

Nothing moved. Not a smaller effect than hoped — no effect at all, in either
direction, on 24 sessions.

## Why: it was never a wording problem

Tracing every tool call in a `store` session, not just agmem's, says what the
rewording was up against. The agent did save the convention. It saved it here:

```
Bash    cat ~/.claude/projects/<slug>/memory/MEMORY.md
Write   ~/.claude/projects/<slug>/memory/feedback_rust_error_handling.md
Write   ~/.claude/projects/<slug>/memory/MEMORY.md
```

Claude Code ships its own auto-memory — a directory the client tells the model
about in the system prompt, at every session start, before any tool
description is read. The agent was not failing to recognise a memory-worthy
fact. It recognised one, and wrote it to the memory system it had already been
told it had. `remember` never entered the decision.

This is design §9 risk 4, arriving in a more specific form than it was
written: not "will agents call it?" but **"a tool description competes with the
host's system prompt, and loses."**

## The control: switch the rival off

`--isolated` runs the same scenarios with `autoMemoryEnabled: false`, so agmem
is the only place a session can persist anything. And because
`AGMEM_TOOL_DESC_<TOOL>` can serve wording the binary was not built with,
`--desc` re-runs the *old* descriptions under the same conditions — the
override earning its keep on the first day it exists.

| write scenario | auto-memory on | auto-memory off, old wording | auto-memory off, new wording |
|---|---|---|---|
| `store` | 0/3 | **3/3** | **3/3** |
| `correct` | 0/3 | **3/3** | **3/3** |

Two conclusions, and the second is the uncomfortable one:

1. **The rival is the whole explanation.** The same prompt that produced
   "Saved." six times produces a real `remember` call once nothing else is
   offering to save it. The description was never being rejected; it was never
   being read against a live choice.
2. **The rewording is worth nothing that can be measured.** The old wording
   fires 3/3 too. Every scenario, both conditions, identical. The hypothesis
   that `remember` failed because it did not name its trigger is not supported
   — it failed because something else got there first, and its trigger
   paragraph was never the binding constraint.

The reworded text ships anyway, and the reason is design §3.1 rather than any
number here: a description is supposed to say *when* to call, and `remember`'s
did not. That is a consistency argument, not evidence, and this paragraph is
where it is on the record as one.

## The one thing neither wording fixed

`correct` seeds the store with "the user formats Python with black" and then
tells the session black is gone. In **6 of 6** isolated runs — both wordings —
the agent called `remember` with a fresh claim and nothing else: no `recall`
first, no `supersedes`, no id. The store ends up holding both claims, live, at
once. That is exactly the contradiction the supersession chain exists to
prevent, and `remember`'s description asks for the right thing in as many words
("`recall` the claim it replaces and send the correction with `supersedes` set
to that id").

So the correction path has the same shape as the write path: an instruction in
a description that the model does not act on. Issue **#38** follows it up.

## The mechanism: a ritual instead of a paragraph

If the problem is that a description is one option among several while the
model decides what to do next, the answer is text that is not in that
competition. That is what an MCP prompt is: it arrives as a turn in the
conversation because somebody asked for it. Issue #22 added two —
`recall_first` and `checkpoint` (design §3.3) — and `checkpoint` carries the
instructions the descriptions could not make stick.

The two `ritual` scenarios are the write scenarios with one extra turn asking
for it, so the comparison is controlled: same words, same client, auto-memory
left **on**, one difference.

| | description alone | with `/mcp__agmem__checkpoint` |
|---|---|---|
| write a stated convention | 0/3 | **3/3** |
| correct a contradicted claim | 0/3 | **3/3** written, **0/3 superseded** |

Every one of those calls landed in the ritual turn; the first turn produced
none, which is the earlier table happening again in the same session. And the
shape is the shape the ritual asks for — a `recall`, then one batched
`remember`, then a summary. Verbatim, from one run:

```
turn 1  recall   { query: "error handling convention thiserror anyhow library binary crates" }
turn 1  remember { memories: [ <instruction>, <lesson> ], episode: {…} }
```

> **Saved:** two new claims — an instruction and a lesson (this rule exists
> because mixing them up caused bugs twice), plus the verbatim episode they're
> provenanced to. **Corrected:** nothing. **Left out:** the mechanics of the
> file-based memory write — that's implementation detail of the earlier turn.

That last clause is the agent noticing, out loud, that its first turn had
written to Claude Code's own memory instead. The ritual is recovering exactly
what the description lost.

### The supersedes column, once the retrieval bug was gone

Re-run after #39, unchanged in every other respect:

| | writes | supersedes |
|---|---|---|
| `ritual_correct`, before #39 | 3/3 | 0/3 |
| `ritual_correct`, after #39 | 3/3 | **3/3** |

Every run recalls the seeded claim, gets it back, and sends the correction with
its id:

```
t1  recall   → hit: "The user formats Python with black."   (id 01M1652…)
t1  remember → memories: [{ …, supersedes: "01M1652…" }]
```

> **Corrected**: the stale claim "the user formats Python with black" → now
> "the user formats Python with ruff format; black is uninstalled and no longer
> used." **Saved**: nothing new beyond that correction.

So the correction path works, given a ritual that asks for it and a `recall`
that answers. What follows for **#38** — surface contradiction candidates from
`remember` itself — is that it is no longer a correctness gap. It remains worth
having for the path with no ritual, where the agent never looks at all, which
is what the 0/6 in the section below actually measured.

The rest of this section is what that column looked like before #39, kept
because the wrong reading of it is instructive.

### Why the supersedes column read as 0/3

The obvious reading of 3/3 writes and 0/3 supersedes — the ritual gets the
write and still not the correction — was **wrong**. Re-running with tool
*answers* recorded, not just tool calls, showed what the agents were actually
working from:

```
turn 1  recall  { query: "project uses black or ruff format for Python formatting", k: 10 }
        → {"hits":[], "spaces":["eval","user"]}
```

The seeded claim was live in the store the whole time — `inspect stats` counts
it, and a filters-only `recall` returns it — but the query-shaped `recall` did
not. Every agent then said "no prior claim existed", which was a correct
conclusion from a wrong answer. That was **issue #39**: `@N@` ANDs the words in
a match reference, so a question carrying one word the claim does not use
emptied the fulltext arm.

Worth keeping because the first read of this data was the flattering one — the
agent looked, was told, and ignored it — and it was wrong in the direction that
blames the model rather than the store. Recording what a tool *returned*
alongside what it was *given* was added for exactly this reason, one finding
too late.

## What follows

- **Reaching for reading is not the problem.** `recall` at 3/3 with
  `restraint` at 3/3 is the behaviour agmem wants, and it holds because a
  session's *first* question — "what do I already know about this?" — has no
  competing answer in the host prompt. What comes *back* from it is a
  different matter: see #39.
- **The write path needed a mechanism, not a paragraph, and got one.** The
  rituals (#22, design §3.3) take the write from 0/3 to 3/3 on the identical
  turn. The host's own hooks are the other candidate, for a session that
  should checkpoint without being asked.
- **`AGMEM_TOOL_DESC_<TOOL>` is the lever that survives all of this.** A
  deployment on a client with no memory of its own, or with a different
  instinct about when to write, can reword without waiting for a release —
  and can measure the result with the same harness.
- **The eval harness is the deliverable that keeps paying.** The rewording was
  a guess that looked obviously right and measured at exactly zero; the only
  reason that is known is that it was measured. Any future wording change goes
  through `scripts/desc-eval.nu` first.

## Overriding a description

`AGMEM_TOOL_DESC_<TOOL>` replaces one outright, per server, no rebuild —
`REMEMBER`, `RECALL`, `CONTEXT`, `FORGET`, `INSPECT`. See the README for the
`.mcp.json` shape. Three things worth knowing:

- It is the **whole** description, never an addition. What the agent reads is
  exactly what was written, rather than a splice of two voices.
- A variable naming something that is not a tool **stops the server**. An
  override that silently does nothing is the same class of failure as the
  "Saved." above: everything looks configured and nothing is.
- The override travels in the daemon handshake, so each project keeps its own
  wording even when several share one store. Without that, whichever session
  started the daemon would have chosen the wording for all of them.

`agmem --doctor` prints which tools a run is rewording — an override is
invisible from the outside otherwise, since the surface still lists five tools.

## Re-running this

The harness is cheap to point at a different model, a longer run, or wording
the binary was not built with:

```
nu scripts/desc-eval.nu --label my-wording --runs 5 --model opus
nu scripts/desc-eval.nu --label mine --desc my-descriptions.json     # {tool: text}
nu scripts/desc-eval.nu --label fair --isolated --only store,correct
nu scripts/desc-eval.nu report before after my-wording
```

`--desc` serves its file through `AGMEM_TOOL_DESC_<TOOL>`, so two wordings can
be compared without two binaries — the override is what makes the A/B above
possible. `--isolated` turns the client's own auto-memory off
(`autoMemoryEnabled: false`), which is the difference between measuring a
description and measuring a competition; every result records `rival_memory`
so a batch says which of the two it was.

Results land in `docs/eval/<label>/` — one JSON per session with the full tool
calls and the answer, plus `descriptions.json` recording the exact text those
agents read. A number with no wording attached cannot be compared to anything.
