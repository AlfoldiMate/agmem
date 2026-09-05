# agmem plugin — context-token audit (2026-09-03)

Read-only audit of `plugin/` and the `agmem` server on branch `feat/135-doc-cli`
(binary measured: `target/debug/agmem` 0.1.11, built from this tree). Tokens
are chars/4 throughout. Measurements come from the wire, not from reading Rust:
`tools/list`, `resources/templates/list`, `prompts/list` over stdio against a
scratch `--data` dir, and each `agmem hook <event>` run with a synthetic payload.

## 1. Cost table

"cc chars" for a tool = description + input schema JSON + the
`mcp__plugin_agmem_agmem__<name>` tool name, which is what Claude Code renders
into the system prompt. Output schemas are listed separately because Claude
Code does not forward them to the model (assumption — see §5).

| Item | When loaded | Chars | Est. tokens |
|---|---|---|---|
| MCP server `instructions` (service.rs `INSTRUCTIONS`) | once per session | 533 | 133 |
| tool `remember` (desc 2062 + schema 5922) | once per session | 8017 | 2004 |
| tool `recall` (desc 1159 + schema 3256, 11 params) | once per session | 4446 | 1112 |
| tool `reflect` (desc 1734 + schema 2546) | once per session | 4312 | 1078 |
| tool `inspect` (desc 1044 + schema 2392) | once per session | 3468 | 867 |
| tool `forget` (desc 1678 + schema 1450) | once per session | 3159 | 790 |
| tool `consolidate` (desc 2455 + schema 367) | once per session | 2858 | 715 |
| tool `context` (desc 912 + schema 823) | once per session | 1767 | 442 |
| **tools subtotal** (desc 11044, schemas 16756) | | **28027** | **7007** |
| output schemas, all 7 tools (inspect 14.6k, consolidate 12.6k, recall 11.3k) | not sent to the model by Claude Code (assumed) | 49303 | (12326 if a client sends them) |
| resource template `memory://{space}/{id}` | only on `resources/templates/list`; not in the system prompt | 302 | ~0 |
| MCP prompts `checkpoint` / `recall_first` (desc 143 / 120) | listed as slash commands; ~0–70 in prompt | 263 | ≤70 |
| SessionStart briefing — `agmem context` block, budget 6000 default; measured 5969 on this repo (trimmed) | once per session start, and again on resume/clear/compact/fork | 5969 | 1492 |
| SessionStart trailer (`FOOTER`, 457) + branch note (~120) + JSON envelope (~80) | with every briefing | ~657 | ~164 |
| SessionStart post-compaction addition (`COMPACTED` 350 + up to 40 recalled ids ≈ 1150) | only `source: compact` | 494–1500 | 124–375 |
| PostToolUse — plain `Bash`, agmem `recall`/`remember`/`reflect` | every matching call | 0 | 0 |
| PostToolUse — successful `git push` (`PUSH_NUDGE`) | once per session | 360 | 90 |
| PostToolUse — `AskUserQuestion` (`DECISION_NUDGE`) | **every** answered question, no once-gate | 327 | 82 each |
| Stop (`STOP_NUDGE`) | at most once per session, only if recalled and never wrote | 268 | 67 |
| skill `agmem:agmem` description | once per session (skills list) | 381 | 95 |
| command `agmem:checkpoint` description | once per session | 101 | 25 |
| command `agmem:memory` description | once per session | 174 | 44 |
| command `agmem:doctor` description | once per session | 141 | 35 |
| skill `agmem:agmem` body | on invocation | 2962 | 740 |
| command `agmem:checkpoint` body (+ `git branch` output) | on invocation | 4510 | 1128 |
| command `agmem:memory` body | on invocation | 1686 | 422 |
| command `agmem:doctor` body (+ `agmem --doctor`, `which -a`, `claude mcp list` output) | on invocation | 1276 + shell output | 319 + |
| MCP prompt `checkpoint` / `recall_first` text | on invocation | 2311 / 1018 | 578 / 255 |
| `plugin.json` / `hooks.json` descriptions | marketplace/UI only, not the model | 220 / 204 | 0 |

## 2. Totals

**Fixed per session (all tools loaded):** 133 + 7007 + 1492 + 164 + ~200 (skill/command
descriptions) + ≤70 (prompt listing) ≈ **9,050 tokens** (~9.1k). A resumed or
`/clear`ed session pays the briefing again (+1.65k); a compaction pays the
briefing plus up to +375.

**Fixed per session when the harness defers MCP tools** (this session listed
the seven agmem tools under ToolSearch — names only until fetched): ≈ 133 +
~60 (names) + 1492 + 164 + ~270 ≈ **2,100 tokens**, with the 7k schema bill
paid only on first use of each tool.

**Per-turn recurring context cost: 0 tokens.** No hook prints on an ordinary
turn. The recurring costs are process spawns, not tokens:

- `agmem hook post-tool-use` is spawned after every `Bash`, `AskUserQuestion`
  and agmem `recall`/`remember`/`reflect` call (hooks.json matcher is anchored,
  so not every tool). Measured ~13 ms each (5 runs: 67 ms). It touches only
  `<data dir>/hooks/<session>.jsonl` — never the daemon. Silent except the two
  nudges above.
- `agmem hook stop` is spawned at the end of every turn; file-only; silent
  except once per session.
- `agmem hook session-start` attaches to the daemon (or starts it) and calls
  `context`; with `--no-daemon` it loads the embedding model in-process (~5 s
  measured on a cold scratch store). Timeout is 15 s.

Exceptions to "0 per turn": every answered `AskUserQuestion` costs 82 tokens
(not once-gated), and the first successful push 90.

## 3. Ranked savings

Gate column: desc-eval (`scripts/desc-eval.nu`) drives real headless sessions
with agmem as the only MCP server and no settings sources, counting which
tools were called and whether writes landed. Any change to tool descriptions,
input-schema field docs, or the `instructions` string changes that surface and
is gated. Hook text and plugin skill/command text are invisible to the eval
(it runs without the plugin's hooks), so those are not gated.

| # | Change | Saves | Memory-quality risk | desc-eval gated |
|---|---|---|---|---|
| 1 | **Serve `consolidate` and `forget` only on demand.** Both are maintenance verbs used from `/agmem:memory tidy`; neither is something a working session should reach for unprompted. Options: an opt-in tool group (`AGMEM_TOOLS=core|all`, default core) that `/agmem:memory` tells the user to enable, or CLI-only (`agmem consolidate --json` via Bash from the command). Where the harness defers MCP tools this is already ~free, but the plugin cannot assume that. | ~6,000 chars ≈ **1,500 tok/session** | Low. `remember`+`supersedes` stays for corrections; the destructive path becoming harder to reach is a feature. The `consolidate` eval scenario would need `all`. | Yes (a scenario seeds `consolidate`). |
| 2 | **Slim the `remember` input schema** (5,922 chars, the single largest item). `$defs`: `EpisodeInput` 1,254, `DocKind` 826 (document typing, only for `episode`), `Kind` 693, `DecayClass` 507, `MemoryInput.supersedes` 446-char description. Keep enum values, move the per-variant prose into the description's existing paragraphs or drop it; document `episode`/`DocKind` in `inspect`/`agmem doc` instead. Target ≤ 3,000. | ~2,900 chars ≈ **700 tok/session** | Low–medium: the `supersedes` and `kind` docs steer correct writes; keep one sentence each. | Yes (schema field docs are wire wording). |
| 3 | **Cut the `consolidate` description** (2,455 chars): the five per-list playbooks duplicate `plugin/commands/memory.md` §tidy almost verbatim. Return that guidance in-band (a `how_to` string per non-empty list) and keep the description to what/when (~700 chars). Same for `forget` (1,678): the dry-run two-step is enforced server-side and its refusal message can carry the instruction. | ~2,700 chars ≈ **670 tok/session** (subsumed by #1 if taken) | Low: guidance moves to the moment it is used. | Yes. |
| 4 | **Aim and cap the briefing's Relevant section.** Measured block: 2 Instructions + 8 Relevant, Lessons starved to zero and the trim note emitted — with no query, "Relevant" is just recent high-strength facts (`RELEVANT_K = 10`) and it eats the section that holds the hard-won lessons. Cap Relevant at 5 when there is no query, and have `session-start` pass a query (branch slug + last commit subject) so the section is aimed. Optionally lower the hook's default budget to 4,000. | Budget 6000→4000: ~500 tok/session; cap alone: ~0 tok but Lessons come back | The briefing is the main quality lever; today the loss is *quality* (no Lessons) not just tokens. Capping Relevant without a query raises quality. | No (hook/`context` assembly; the `context` description's "6000 by default" sentence would change if the default does). |
| 5 | **Make the `AskUserQuestion` nudge once-per-session** (`first_time("decision")`, like push and stop), or once per N answers. It is the only nudge without a gate. | 82 tok × (answers − 1) per session; a planning session with 6 questions pays ~400 | Low: a repeated identical nudge is ignored anyway. | No. |
| 6 | **Shrink the trailer `FOOTER` (457 chars) to one sentence.** It restates the server `instructions`, the `context` description and — for this project — the CLAUDE.md Memory section (1,908 chars); the model sees the rule three times. Keep: "Briefing from agmem — established fact; a stale line is corrected with `remember` + `supersedes` (id ends each line); `/agmem:checkpoint` writes back." Also fold the branch note into it. | ~350 chars ≈ **90 tok/session** | Low. | No. |
| 7 | **Collapse `recall`'s time-travel params.** `as_of`, `since`, `until`, `changed_since`, `include_invalidated` are 5 of 11 properties and ~1,300 chars, all rarely used from a working session; nest them under one optional `history` object with one description, or leave them documented on `inspect`. | ~900 chars ≈ **225 tok/session** | Low: the audit path stays reachable. | Yes. |
| 8 | **Tighten `reflect` (4,312 total).** Description 1,734 and schema 2,546 both explain `derived_from`; the `supersedes` field doc (277) is a copy of `remember`'s. One explanation, in the description. | ~1,000 chars ≈ **250 tok/session** | Low. | Yes. |
| 9 | **Shorten the `agmem:agmem` skill description** (381 chars; the longest always-loaded line of the four) to ~180, and consider dropping `agmem:doctor` from the skills list into `agmem --doctor` docs only. | ~250 chars ≈ **60 tok/session** | None. | No. |
| 10 | **Trim `instructions` to its first paragraph** (533 → ~200); the distil/supersedes paragraph is repeated in `remember`'s description. | ~330 chars ≈ **80 tok/session** | Low–medium: instructions is the cheapest steering text there is; measure. | Yes. |

Taking #1, #2, #4 (budget), #5, #6 gives roughly **2,800–3,000 tokens per
session** off a ~9k fixed bill (~30%), before any wording change to the four
tools that stay. Under tool deferral the fixed bill is already ~2.1k and the
briefing (#4, #6) is the whole lever.

Not worth doing:

- **Resource template** — 302 chars, never enters the system prompt in Claude
  Code. No weight to cut.
- **`agmem hook post-tool-use` printing** — already silent for the logging
  path; nothing to fix.
- **`HEADER` fallback in hook.rs** — unreachable in practice: an empty store
  still yields a non-empty block (`_Nothing stored for these spaces yet._`), so
  the trailer path always runs. Dead text, not a cost.

## 4. Hook process behaviour

| Hook | Fires | Spawns binary | Talks to daemon | Prints |
|---|---|---|---|---|
| SessionStart | startup/resume/clear/compact/fork | yes | yes (attach or start; in-process + model load only with `--no-daemon`) | always (briefing + trailer) |
| PostToolUse | Bash, AskUserQuestion, agmem recall/remember/reflect | yes, ~13 ms | no (appends to `hooks/<session>.jsonl`; reads it back for the once-gates) | push (once), AskUserQuestion (every time); otherwise nothing |
| Stop | every turn end | yes | no | once per session at most |

The per-Bash spawn is the only latency worth naming: ~13 ms × every Bash call.
It re-reads the whole session log each time for `first_time`, which is O(log
size) per call — fine for a session, a small cost for a very long one.

## 5. Assumptions to verify before acting

- Claude Code renders MCP tools as name + description + `inputSchema` and
  omits `outputSchema`. If any client forwards output schemas, they are the
  largest item by far (49 kB, ~12k tokens) and #1 above becomes "strip output
  schemas from the wire" instead.
- Claude Code's MCP tool deferral (ToolSearch) was active in the auditing
  session; whether it is on for the user's ordinary sessions decides whether
  the 7k schema bill is paid up front.
- Whether MCP prompts (`checkpoint`, `recall_first`) are listed in the system
  prompt at all; if not, the ≤70 tokens attributed to them are zero.

## 6. Sources measured

- `~/Development/agmem/plugin/hooks/hooks.json` — matchers and events.
- `~/Development/agmem/crates/agmem-server/src/hook.rs` — `HEADER`, `FOOTER` (457), `COMPACTED`, `PUSH_NUDGE`, `DECISION_NUDGE`, `STOP_NUDGE`; once-gates at `first_time`; `COMPACT_RECALLED_CAP = 40`.
- `~/Development/agmem/crates/agmem-server/src/service.rs` — `INSTRUCTIONS`, all seven `#[tool]` descriptions, two `#[prompt]`s.
- `~/Development/agmem/crates/agmem-server/src/tools/context.rs` — `DEFAULT_BUDGET_CHARS = 6_000`, `MIN_BUDGET_CHARS = 200`, `RELEVANT_K = 10`, `LESSONS_K = 5`, `TRIMMED`, `NOTHING`.
- `~/Development/agmem/crates/agmem-server/src/oneshot.rs` — daemon vs direct route for the hook's `context` call.
- `~/Development/agmem/crates/agmem-server/src/resources.rs` — one template, `memory://{space}/{id}`.
- `~/Development/agmem/docs/design.md` §2.3 (decay), §3.2 (briefing assembly and priority order).
- `~/Development/agmem/scripts/desc-eval.nu` header — what the eval gates.
- Wire captures: `/tmp/agmem-audit-tools.jsonl` (tools/templates/prompts), `/tmp/agmem-audit-briefing.md` (this repo's live briefing, 5,969 chars).
