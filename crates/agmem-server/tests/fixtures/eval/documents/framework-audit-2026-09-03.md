# ctx-flow framework audit — token cost per session and per turn

Date: 2026-09-03. Read-only; nothing edited. Token estimates are bytes/4.
Measured on this machine: nu 0.114/0.115, rtk 0.47.0, agmem 0.1.10, plugin agmem@agmem 0.1.0,
global `~/.claude/settings.json` (model `fable[1m]`, `defaultMode: auto`), no global
CLAUDE.md, no global skills/ or agents/.

## 1. Inventory and load pattern

### Always in context (every session, every turn — this is the prompt-prefix tax)

| Item | Bytes | ~Tokens | Source |
|---|---:|---:|---|
| `.claude/CLAUDE.md` | 11,564 | 2,890 | project instructions, injected as first user message |
| `output-styles/ctx-flow.md` (body only) | 2,109 | 530 | appended to system prompt (`outputStyle: ctx-flow`) |
| 6 agent `description`s (+ name/tools line in listing) | ~1,750 | 440 | architect 250, scout 313, runner 228, tracker 188, verifier 183, browser 181 B of description; the harness listing adds `(Tools: …)` per agent |
| Local skill descriptions: `ast-grep` 438, `nushell` 511, `rust-expert-developer` ~1,000 (2,150 on disk, truncated in the listing with `…`) | ~1,950 | 490 | skill listing |
| Command descriptions (5, listed as skills): agmem-import 186, ast-grep-it 173, bare-worktree 141, checkpoint 202, ctx-flow-doctor 215 | 917 | 230 | skill listing |
| agmem plugin: skill desc 383 + 3 command descs ~430 | ~810 | 200 | skill listing |
| agmem MCP server instructions | 533 | 135 | `initialize.instructions` |
| agmem MCP tool listing (7 tools; description + inputSchema only) | 23,642 | 5,900 | descriptions 10,452 B + inputSchema 13,190 B (`remember` 6.4 kB, `recall` 4.4 kB, `reflect` 4.3 kB, `forget` 2.7 kB, `consolidate` 2.6 kB). The wire JSON is 65.5 kB because `outputSchema` adds 41 kB, which the harness does not show the model |
| nu MCP server instructions | 11,520 | 2,880 | `initialize.instructions` — one 11.5 kB block, in every system prompt |
| nu MCP tool listing (3 tools) | 6,466 | 1,620 | `evaluate` description alone is 5,247 B |
| agmem SessionStart briefing (`agmem hook session-start`) | 6,643 | 1,660 | measured now for this repo; trimmed to `budget_chars`; ~700 B of it is a fixed trailer paragraph |
| ctx-flow SessionStart layout check | 0 | 0 | prints only on a WORKTREE LAYOUT MISMATCH |
| Auto-memory `MEMORY.md` | 424 | 105 | `~/.claude/projects/-Users-the user-Development-agmem/memory/` (3 topic files, 3 kB, loaded on demand) |
| Global settings `autoMode.environment` block | ~2,500 | ~600 | 25 lines of org/user environment strings, injected under auto mode (harness-side, not the framework's) |
| **Framework + plugins + MCP total** | **~70,000** | **~17,700** | of which nu MCP 4,500 + agmem MCP 6,000 = **~10,500 (60 %)**; CLAUDE.md 2,900 (16 %) |

Not counted: Claude Code's own built-in skill descriptions (code-review, claude-api,
loop, schedule, etc., roughly 3–4 kB) and the base system prompt — not the
framework's to cut.

Caveat on the MCP tool listings: in this audit's own (subagent) prompt the agmem tools
arrived *deferred* (names only, schema fetched via ToolSearch) and the nu instructions
were truncated at ~2.5 kB. If the main thread gets the same deferral, the agmem tool
cost drops to ~100 tokens until first use and the nu instructions to ~600. Verify once
in the main thread (`/context` shows the MCP tool token count) before acting on the
MCP rows.

### Loaded on trigger

| Item | Bytes | ~Tokens | When |
|---|---:|---:|---|
| Agent bodies: architect 1,744 / browser 1,618 / runner 1,447 / scout 1,912 / tracker 1,450 / verifier 1,639 | ~9,800 | 2,450 total; ~400 each | per dispatch, into the subagent's prompt |
| `skills:` preload — `ast-grep` SKILL.md (8,979 B body) into architect, scout, verifier | 8,979 | 2,250 | per dispatch of those three |
| Command bodies: checkpoint 2,774 / ctx-flow-doctor 3,337 / ast-grep-it 4,198 / agmem-import 1,770 / bare-worktree 1,696 | 13,775 | 3,440 | when `/name` runs |
| Skill bodies: nushell 4,605 / ast-grep 8,979 / rust-expert-developer 23,215 (+ 526 kB of references opened on demand) | 36,800 | 9,200 | when invoked (nushell + ast-grep list ~180 kB of references) |
| Plugin command bodies (checkpoint 4,940 / doctor 1,355 / memory 2,324), plugin skill body 3,000 | 11,600 | 2,900 | when invoked |
| PostToolUse nudge (ctx-flow) | ≤ 3 × ~420 | ≤ 315 / session | once per idiom per session |
| agmem `post-tool-use` nudge | 438 | 110 | after `git push` / AskUserQuestion / memory writes; 0 otherwise |
| agmem `stop` nudge | ~300 | ~75 | at most once per session (recalled and wrote nothing) |
| PreToolUse worktree guard | 0 (≈ 450 on deny) | 0 | only when denying |

### Never in context

`scripts/*.nu` (51 kB), `hooks/scripts/*.nu` (14.5 kB), `docs/reference.md` (10.7 kB),
`README.md` (19.6 kB), `notes/` (283 kB in 17 files, incl. `agmem-archive/LEDGER.md.imported`
71 kB), `settings*.json`, `.gitignore`, skill `references/` until opened.

## 2. Redundancy

Same rule stated in several always-loaded places (bytes are of the redundant statement):

| Rule | Where it appears | Redundant bytes |
|---|---|---:|
| "Run first nu command uncapped; slice `$history` afterwards" | nu MCP instructions ("THE RULE", ~1.5 kB of the 11.5 kB); `evaluate` tool description (5.2 kB, restates the same); CLAUDE.md Toolbox bullet 2 (~300 B); CLAUDE.md Payload discipline (~120 B); output style Tools bullet 1 (~230 B); nushell skill rule 6 (on trigger) | ~7,000 always-loaded for one rule |
| Delegation by information ratio / never delegate the write path / verdicts not transcripts | CLAUDE.md Delegation (1,707 B) + Payload (408 B); output style Routing (~600 B) | ~600 |
| ast-grep for syntax, grep for text | CLAUDE.md Toolbox bullet 3 (~500 B); output style (~180 B); every agent body (~250 B each, on trigger); PostToolUse nudge text (~420 B, once) | ~700 |
| Edit with nu not sed/python; no-op exits 0 | CLAUDE.md Shell work (1,098 B); output style (~280 B); PostToolUse nudge (~450 B) | ~700 |
| Bare-worktree: never raw `git worktree` | CLAUDE.md Worktrees (688 B); output style (~230 B); PreToolUse deny reason (on trigger) | ~230 |
| Answer shape (lead with outcome, number steps, task list, effort dial, AskUserQuestion) | CLAUDE.md Answer shape (2,427 B); output style Answers (~700 B) | ~700 |
| Correct-never-contradict / `supersedes` | CLAUDE.md Memory bullet 2 (~200 B); agmem MCP instructions (~250 B); briefing trailer (~250 B); agmem skill description (~100 B); agmem skill body (on trigger) | ~600 |
| The routing table rows (Explore, architect, runner, verifier, browser, tracker) | CLAUDE.md table (~900 B) vs the agent descriptions already in the always-loaded agent listing (1,343 B) | ~900 |
| The four "worth storing" tests (durable/non-obvious/earned/actionable) | `commands/checkpoint.md` §2 and the plugin's `skills/agmem/SKILL.md` (both on trigger; `/checkpoint` invokes the plugin skill, so both load in one run) | ~600 on trigger |
| CLAUDE.md vs README | README "What's in it", "Memory", "Playbooks", "Staying ahead of compaction" re-explain CLAUDE.md; README never loads, so no cost — but CLAUDE.md's *rationale* sentences are what README already holds | see §3 |

The output style (2.1 kB) is by design a summary of CLAUDE.md (README says so:
"the cost is duplication"). Every one of its 12 bullets has a CLAUDE.md twin.

## 3. CLAUDE.md: rules vs justification

Section sizes: Toolbox 2,388 · Delegation 1,707 · Payload 408 · Shell work 1,098 ·
Worktrees 688 · Memory 1,905 · Playbooks 625 · Answer shape 2,427 · header 286.

Reasoning/justification that could move to README/docs (≈ 3.9 kB ≈ 1,000 tokens):

- Header: "These rules load in every session by design — they used to be a skill…" (~200 B).
- Toolbox: the agmem bullet's version history and hook explanation (~350 B); the
  ast-grep bullet's "A language it does not ship a grammar for is usually one build
  away…" (~300 B); `rtk`, `gh`, `playwright-cli`, `acli` rows are for the doctor, not
  the model (~250 B); "Avoid MCP servers" rationale paragraph (~450 B — the model cannot
  register servers anyway).
- Delegation: the two "ratio" explanatory paragraphs (~700 B) — the table is the rule.
- Shell work: the harness-routing preamble (~250 B) and the `grep -c` BRE explanation (~300 B).
- Worktrees: layout explanation (~350 B); the hook already denies the raw command.
- Memory: "The space derives from the repo's shared git dir…" (~300 B), kinds table
  duplicates the agmem skill (~350 B), "Prefer several short sessions…" (~200 B).
- Playbooks: entire section (625 B) is mechanism the main thread never executes —
  `/checkpoint` and the agents carry it.
- Answer shape: "Working memory is small, starting is the hardest step…" and "Break the
  rules when…" (~600 B) — the second is arguably a rule; the first is not.

Sections the output style already enforces (could be cut from CLAUDE.md, or the
style dropped): Delegation core, Payload, Shell work core, Worktrees core, Answer
shape items 1–3, 5, effort note, AskUserQuestion — ~2.5 kB overlap.

Conflict worth noting: global `defaultMode: auto` injects "While auto mode is active:
do your work through the Bash tool … make file changes with sed, heredocs, or short
scripts, rather than … Read, Edit, Write". This directly contradicts CLAUDE.md "Shell
work" (nu, never sed) and the PostToolUse nudge then fires on the sed the harness told
the model to use. Transcript sample: 14 nudge fires in one session (`1a496abd`), 8 in
another — the "once per session per idiom" marker is keyed by `session_id`, and the
count above suggests either several sessions share a transcript (resume/fork) or the
temp-dir marker was cleared. Either way the nudge is firing more than designed.

## 4. Hooks

| Hook | Event | Runs | stdout → context | Latency (measured) |
|---|---|---|---|---|
| `session-start-layout.nu` | SessionStart (startup/resume/clear/compact/fork) | 2× `git rev-parse` | 0 B normally; ~400 B on mismatch | ~40 ms |
| `rtk hook claude` | PreToolUse:Bash | rtk binary | none (`updatedInput` only, rtk 0.47) | ~10 ms |
| `pre-worktree-guard.nu` | PreToolUse:Bash | nu startup + regex; `git rev-parse` only if regex matches | 0 B; ~450 B deny reason when tripped | ~40 ms |
| `post-bash-nudges.nu` | PostToolUse:Bash | nu startup + 3 regexes + temp-file marker | 0 B, or one ~420 B note per idiom, once per session | ~40 ms |
| plugin `agmem hook post-tool-use` | PostToolUse: Bash / AskUserQuestion / recall / remember / reflect | agmem binary (opens store log line) | 438 B after `git push`; 0 otherwise | ~10 ms |
| plugin `agmem hook stop` | Stop | agmem | ~300 B at most once per session | <10 ms |
| plugin `agmem hook session-start` | SessionStart | agmem, embeds a query | 6.6 kB briefing | ~60 ms |

Per Bash call: rtk 10 + guard 40 + nudge 40 + agmem 10 ≈ **100 ms, 0 tokens** in the
common case. Bare `nu -n -c null` is 20 ms; `--stdin script` with `use _common.nu` is
40 ms. Not worth optimising — the token cost is nil and 100 ms is below the Bash
command itself. The two nu hooks could be merged into one PostToolUse script only if
the guard gave up its deny power (deny must be PreToolUse), so keep both.

Transcript sample (last 15 sessions): 0–107 Bash calls per session (median ~4),
0–89 `mcp__nu__evaluate` calls. Hook wall-clock per session ≤ 11 s worst case.

## 5. Agents

| Agent | desc B | body B | model / effort | Return contract | Playbook recall | `skills:` preload |
|---|---:|---:|---|---|---|---|
| architect | 250 | 1,744 | fable / high | yes (≤60 lines, ≤8 steps) | recall + context | ast-grep (+9 kB) |
| browser | 181 | 1,618 | sonnet / medium | yes (4+5 caps) | recall | — |
| runner | 228 | 1,447 | haiku / low | yes (≤5 causes) | recall | — |
| scout | 313 | 1,912 | haiku / medium | yes (≤8 hits) | recall | ast-grep (+9 kB); also nu MCP (+18 kB instructions+tools) |
| tracker | 188 | 1,450 | haiku / low | yes | recall | — |
| verifier | 183 | 1,639 | sonnet / high | yes (≤3 refs) | recall | ast-grep (+9 kB) |

- Every agent makes one `recall(tags: role:<x>)` MCP call on start: ~1 round trip plus
  the agmem tool listing (~6 k tokens) in *its* prompt, plus the 533 B instructions. For
  `runner` on haiku that listing is larger than the agent body. Usage sample: runner 13,
  general-purpose 5, architect 4, scout 3, Explore 2, tracker 1 dispatches over 15
  sessions — runner is the workhorse, and it carries the full agmem schema every time
  for a playbook that is usually empty.
- `scout` preloads the nu MCP server (18 kB of instructions + tool schemas) *and*
  ast-grep (9 kB) — ~7 k tokens of prefix on a haiku agent whose contract is 8 lines.
  Its overlap with the built-in `Explore` is near-total (both read-only search; Explore
  has no return contract, scout does). Keep scout for the contract; drop the nu server
  from it (ast-grep `--json | jq` or plain `Bash` suffice for a path list).
- `architect` vs built-in `Plan`: same job; Plan has no model pin, no contract, no
  playbook. No overlap cost (Plan's description is the harness's). `general-purpose` was
  used 5× in the sample — it has `*` tools and no contract; each of those was a
  candidate for scout/runner/verifier.
- The "Learned" trailer (~280 B) and the "Brief yourself from project memory" paragraph
  (~420 B) are identical across all six bodies — ~4.2 kB total, fine on trigger.

## 6. Skills

| Skill | Description B (always) | Body B (trigger) | Notes |
|---|---:|---:|---|
| `rust-expert-developer` | 2,150 on disk (~1,000 shown, truncated with `…`) | 23,215 + 526 kB refs | machine-local (gitignored). Its description is 2× every other skill's combined and gets cut mid-sentence by the listing — the trailing trigger list never reaches the model |
| `nushell` | 511 | 4,605 + 86 kB refs | fine |
| `ast-grep` | 438 | 8,979 + 10.7 kB ref | generic upstream text (JS examples); preloaded whole into 3 agents. A 2 kB trimmed version would serve the preload |
| `checkpoint` (local command) | 202 | 2,774 | wraps `agmem:checkpoint` (plugin, desc 118 B, body 4,940) — two `checkpoint` entries in every listing; the local one adds only the LEARNED gate |
| `ctx-flow-doctor` | 215 | 3,337 | alongside plugin `agmem:doctor` (desc ~150) — overlapping doctors |
| `agmem-import` | 186 | 1,770 | one-shot migration already done here (`LEDGER.md.imported` exists); listed every session anyway |
| `ast-grep-it`, `bare-worktree` | 173, 141 | 4,198, 1,696 | fine; `disable-model-invocation: true` would drop them from the model's listing while keeping `/name` |

## 7. Settings

- `settings.json`: only `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, three hooks, `outputStyle`.
  No `MAX_THINKING_TOKENS`, no `CLAUDE_CODE_*` token env, no `disableAllHooks`, no
  subagent model default, no `effort`. Agents pin model/effort per file.
- `settings.local.json`: deny list of 6 tools; `skillOverrides` turn off 3 artifact
  skills and demote `design`/`dataviz` — those already save listing tokens.
  `enableAllProjectMcpServers: true` is harmless (no `.mcp.json`).
- No `permissions.allow` list: under `defaultMode: auto` (global) prompts are handled
  by the auto-mode classifier, so an allowlist saves nothing here.
- Global: `model: fable[1m]`, `modelSettings` effort high for fable-5 / medium for
  fable-5-1 / xhigh for opus-5. The `autoMode.environment` 25-line block (~2.5 kB) is
  injected every session and is almost entirely "None configured".
- `rust-analyzer-lsp` plugin: no hooks, no skills, LICENSE+README only — zero prompt cost.

## 8. nu MCP server

- Instructions: **11,520 B (~2,880 tokens)** in every system prompt (main thread, and
  every agent declaring the server — scout). Content: THE RULE (~1.5 kB), response
  format, `$history` (~1.3 kB), env limits, structured output, string literals table,
  Running externals, Filesystem, Polars, background jobs (~1.5 kB), tips.
- Tools: `evaluate` (desc 5,247 B — restates THE RULE and the job-promotion text),
  `list_commands` (201), `command_help` (182). Total 6,466 B (~1,600 tokens).
- Whether all three are needed: `list_commands`/`command_help` cost ~400 B combined
  and replace `evaluate("help x")` — cheap, keep. The cost is the instruction block and
  `evaluate`'s description, which are nushell's (`nu --mcp`), not editable from here
  except by `NU_MCP_*` env (limits only) or by wrapping the server. Transcript sample:
  nu evaluate used 0–89× per session; in 7 of 15 sessions it was used 0×, i.e. the
  4,500 tokens bought nothing in half the sessions.

## 9. Other observations

- `notes/`: 17 files, 283 kB, growing (five 20–29 kB research/review files this week).
  Never loaded, but `Read` of one is 5–7 k tokens; #136 (retire notes into agmem
  documents) is the planned fix.
- Auto-memory: `MEMORY.md` 424 B + 3 files ≈ 3.4 kB; negligible. Two of its three
  entries duplicate agmem lessons in spirit (nu gotchas) — the harness's own memory and
  agmem now overlap as two stores.
- Briefing: 6.6 kB, `_Trimmed to fit budget_chars_` — the two `instruction`s (~900 B)
  are pinned and the 700 B trailer is fixed text that restates CLAUDE.md Memory bullets
  1–2 and the checkpoint rule.
- Output style is 2.1 kB and does not reach subagents; it is the right size.

## 10. Ranked savings

| # | Change | Saves (tokens) | Risk to quality |
|---|---|---:|---|
| 1 | Register `nu` MCP only on agents that need it (or a wrapper that overrides `instructions` to ~1.5 kB and trims `evaluate`'s description); keep the server for the main thread only if `/context` shows it is not deferred | 3,000–4,500 / session | Loses `$history` re-slicing in the main thread on the days it is used; half of sampled sessions never called it. Medium |
| 2 | Drop `mcpServers: plugin:agmem:agmem` + `recall` from `runner`, `tracker`, `browser` (playbooks are near-empty; `LEARNED` still proposed) — keep on architect/verifier/scout | ~6,000 / dispatch × 13 runner dispatches per 15 sessions ≈ 5,000 / session | A runner rule stored under `role:runner` stops loading; store it as a `fact` the main thread passes in the prompt instead. Low |
| 3 | Move justification out of CLAUDE.md to README/docs (§3 list) | ~1,000 / session, every turn | Rules stay; reasons live one `Read` away. Low |
| 4 | Cut the CLAUDE.md sections the output style already enforces (Delegation prose, Shell work explanation, Answer shape 1–3/5/effort), keeping the table and the exceptions | ~600 / session | The style does not reach subagents, but neither does CLAUDE.md's Answer shape matter there. Low |
| 5 | Shorten `rust-expert-developer` description to ~300 B (it is truncated to ~1,000 B anyway, so the trigger list is already lost) | ~200 / session | None; the listing currently drops the trailing triggers. None |
| 6 | Remove `nu` from `scout`'s `mcpServers`/tools; trim the `ast-grep` SKILL.md to a ~2 kB house version for the three preloads | ~4,500 / scout dispatch, ~1,700 / architect+verifier dispatch | Scout loses `$history`; a haiku agent returning 8 paths does not need it. Low |
| 7 | Mark `agmem-import`, `ast-grep-it`, `bare-worktree` `disable-model-invocation: true` (user-invoked only) and fold `ctx-flow-doctor` into a note in `agmem:doctor` or vice-versa | ~400 / session | The model can no longer suggest running them by name; CLAUDE.md still names them. Low |
| 8 | Ask agmem to drop the fixed 700 B briefing trailer when CLAUDE.md already carries the Memory rules (plugin option), or cut CLAUDE.md Memory bullets 1–2 | ~180 / session | One copy remains either way. None |
| 9 | Resolve the auto-mode ↔ "nu not sed" conflict: either drop the Shell-work rule and the `edit-via-shell`/`search-via-grep` nudges, or add `Read/Edit` to auto-mode's allowed path | ≤ 300 / session + fewer contradictory instructions | Removing the nudge loses the 18 % habit correction it was written for. Medium |
| 10 | Prune `notes/` (LEDGER.md.imported 71 kB; five ≥20 kB research files) into agmem documents once #135/#136 land | 0 / session; 5–7 k per accidental `Read` | None |

Total achievable without touching the plugin or nushell: items 3, 4, 5, 7, 8 ≈
**2,400 tokens/session (14 % of the framework's prefix)**; items 1, 2, 6 are where the
real mass is (≈ 60 % of the prefix and most of the per-dispatch cost) and need either
a wrapper for `nu --mcp` or an agmem plugin change.
