# ctx-flow

A token-frugal development framework for Claude Code, living as the `.claude`
folder of the [agmem](https://github.com/AlfoldiMate/agmem) repository. It is
the framework agmem is developed with, and the first consumer of agmem's
Claude Code plugin. Copy the folder into your own project to use it.

The premise: **the main thread should hold decisions, and everything else
should hold output.** Build logs, search results, page snapshots and ticket
records are the largest objects in any session and carry the least
information per token. ctx-flow routes each of them into a subagent sized for
the job, and keeps durable memory in agmem, a per-project MCP memory store, so
the main thread's context survives `/clear` and `/compact`.

## Install

Copy this folder to your project's `.claude` directory:

```bash
cp -R /path/to/agmem/.claude /path/to/your-project/.claude
```

Inside the agmem repo itself it is already in place, and it is what every
session here runs, including the sessions that develop the plugin. The
repository README's [Development](../README.md#development) section walks
through the setup from a fresh clone; this section is the same list with the
reasons attached.

### Dependencies

| Tool | Why | Install |
|---|---|---|
| [nu](https://www.nushell.sh) | runs the hooks and scripts; provides one of the two MCP servers the framework wants | `brew install nushell` |
| [agmem](https://github.com/AlfoldiMate/agmem) | persistent cross-session memory, over MCP | `brew install AlfoldiMate/tap/agmem` |
| [ast-grep](https://ast-grep.github.io) | structural (syntax-aware) code search | `brew install ast-grep` |
| [gh](https://cli.github.com) | GitHub, always over the GitHub MCP server | `brew install gh` |
| [rtk](https://github.com/rtk-ai/rtk) | transparently compresses Bash output | `brew install rtk` (hook ships here) |
| [playwright-cli](https://github.com/microsoft/playwright-cli) *(optional)* | browser driving as shell commands, for the `browser` agent | `npm i -g playwright-cli` |
| acli *(optional)* | Jira, for the `tracker` agent | Atlassian's installer |
| tree-sitter-cli *(optional)* | builds grammars ast-grep does not ship | `npm i -g tree-sitter-cli` |

You do not have to *use* Nushell as your shell; the hooks run under `nu`
regardless of what your terminal runs. Register the nu MCP server, and
install the agmem plugin. The plugin registers agmem's server and carries
every memory hook, so nothing about memory needs wiring by hand:

```bash
claude mcp add nu -- nu --mcp
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

Then verify the toolchain from inside a session:

```
/ctx-flow-doctor
```

It checks every row above, both MCP registrations, the hooks, and whether
ast-grep parses the project's languages, and prints the exact fix for
anything broken. This repo's hooks are Nushell, which ast-grep does not ship
a grammar for; `/ast-grep-it nu` builds one into `~/.cache/ctx-flow` and
registers it in a machine-local `sgconfig.yml` at the repo root.

**On MCP:** the framework avoids MCP servers. A CLI beats one wherever a CLI
exists: you choose the fields, the output pipes, no schema in the prompt. Two
exceptions, each holding state no CLI reaches: `nu --mcp` earns its slot by
*being* the shell (structured pipelines, `$history` for re-slicing past
results without re-running, safe uncapped first runs), and `agmem` by *being*
the memory, cross-session state that has to outlive every process.

**On agmem:** install the current release; the plugin's version is pinned to
the binary's, because its hooks are `agmem hook <event>`, subcommands of the
binary itself. An older binary answers them with a usage error the session
never sees, and memory simply fails to arrive: hooks need 0.1.10, documents
(`agmem doc`, which `scripts/doc-put.nu` calls) need 0.2.0. `/ctx-flow-doctor`
flags an old, stale or duplicated binary and a missing plugin. If you had
registered agmem by hand before (`claude mcp add --scope user agmem`), remove
it (`claude mcp remove agmem -s user`): Claude Code connects only one, yours
winning, and that changes the tool names to `mcp__agmem__*`. This framework's
agents name the plugin's (`mcp__plugin_agmem_agmem__*`), so under a by-hand
registration they start without memory. `/agmem:doctor` flags the duplicate.
The store is one directory (`~/Library/Application Support/dev.agmem.agmem`
on macOS, `~/.local/share/agmem` on Linux); back it up or delete it as a
unit. There is no server-side LLM: the session distils, the store never
rewrites what it holds.

**On rtk:** the framework's `settings.json` registers its hook (`rtk hook
claude`). Install only the binary, and do **not** also run `rtk init -g`, or
every Bash call is rewritten twice (`/ctx-flow-doctor` flags this as
DOUBLED). As of rtk 0.46 the hook only rewrites the command
(`updatedInput`), leaving your permission prompts intact; older versions also
emitted `permissionDecision: "allow"`. Telemetry is on by default
(`rtk telemetry disable`).

### Worktrees and profiles

The agmem checkout runs as a bare repository with one sibling directory per
branch, and `/bare-worktree` is the only way worktrees are made there:

```
proj/                  the container ("root")
├── .bare/             the one real git dir
├── .git               a file: "gitdir: ./.bare", so git works from root
├── .claude/           optional: symlinked into every worktree on apply
├── .profiles/         the gitignored files each worktree needs
│   ├── .state/        one manifest per worktree, recording what apply did
│   ├── dflt/          the default profile, always applied first
│   └── <name>/        any other directory is a named profile
└── <worktree>/        one sibling directory per branch
```

Every file in a profile directory is symlinked into the worktree at the same
relative path: `settings.local.json`, `sgconfig.yml`, a machine-local skill,
whatever a session needs but git must not track. Raw `git worktree add`
skips all of it, and a `PreToolUse` hook denies it in this layout. `init`
transforms an existing clone; `add`, `remove`, `apply`, `discard` and `which`
do the rest; the script's header comment documents the profile format and
its hooks. A plain clone needs none of this. Everything here is symlink-safe,
and agmem derives its memory space through the shared git dir regardless, so
a worktree made either way is never amnesiac.

## What's in it

### CLAUDE.md, the system prompt

The routing discipline lives in `CLAUDE.md`, loaded automatically every
session. It used to be a skill; it isn't one any more, deliberately. A skill
must be *discovered*, a description line the model may or may not act on,
and routing that only sometimes applies is routing that silently doesn't.
Rules that apply to every session belong in the file that loads every
session.

It carries: the delegation rule (route by **information ratio**, not task
type), the routing table, payload discipline, the shell and worktree rules,
what the framework adds to the plugin's memory rules, and the answer-shape
cases the output style leaves out. It is rules only, held under 6 kB. On
2026-09-04 it was 11.6 kB, a third of it justification, and at ~2.9k tokens
on every turn it was 16% of the fixed prefix. The justification lives here.

Why the shell rules exist: auto mode's preamble asks for `cat`, `sed -n` and
`sed`. That chooses the tool, not the language, and the language is nu. A
windowed `sed -n 'a,bp'` read is Read offset/limit spelled in shell and
passes the read guard, but a `sed` *edit* is regex always over source that
carries `$ { [ ? |` on nearly every line, so it corrupts quietly rather than
failing, and every edit tool exits 0 on a missed pattern. Settled 2026-09-04
when the two rules collided (#146).

Why the answer-shape rules exist: output is shaped so the reader can *act*
on it, not just read it. Working memory is small, starting is the hardest
step, and buried wins do not register. So the first line is the outcome or
the next action, multi-step work is numbered and tracked, and a rule is
broken only when it would delete the answer itself.

Why the memory section is short: the plugin's briefing footer already states
the rules every session (the briefing is established fact, `recall` in words,
correct with `supersedes`), and the same rule used to be repeated in the
agmem instructions, the skill description and two CLAUDE.md bullets. One
copy, the plugin's; CLAUDE.md keeps only what the framework adds.

### Seven agents

| Agent | Model / effort | Absorbs |
|---|---|---|
| `runner` | haiku / low | builds, suites, linters; returns the failure signature |
| `verifier` | sonnet / high | adversarial check of one claim; defaults to refuting |
| `architect` | fable / high | design of a non-trivial change; read-only, never edits |
| `browser` | sonnet / medium | `playwright-cli` sessions; snapshots stop here |
| `tracker` | haiku / low | Jira/GitHub via `gh`/`acli`; never raw records |
| `scout` | haiku / medium | where a symbol lives, which files match a shape; paths and line refs, never bodies, under 3k chars |
| `researcher` | sonnet / medium | a bounded question that takes many files, docs or the web; under 2k chars back, the rest as a document |

Every one ends with a **mandatory return contract**: fixed keys, hard caps,
and an explicit forbidden list. An unschematized subagent writes an essay; a
schematized one writes 200 tokens. Tiers are picked by *consequence of being
wrong*, not output size: `runner` misses cost one re-dispatch; a bad
`architect` plan is discovered late, after the code exists.

Broad codebase exploration stays with Claude Code's built-in `Explore`, with
the same cap pasted into its prompt; `scout` is the cheaper cousin for a
question with an enumerable answer (where is X, which files do Y) that
should come back as refs, not prose. `researcher` is the target for what
used to go to `general-purpose`, which over 71 audited sessions returned 16k
characters on average and is no longer a target at all.

What each agent preloads is sized to what it returns. `architect`, `verifier`
and `scout` carry the agmem `recall` tool for their `role:` lessons; `runner`,
`tracker`, `browser` and `researcher` do not. The schema was ~6k tokens per
dispatch, runner alone ran 157 times, and the dispatcher pastes any
applicable lesson into the prompt instead. The four ast-grep users preload
`skills/ast-grep-lite`, a 2 kB card; the full `ast-grep` skill stays for the
main thread.

### Five commands

- **`/checkpoint`** runs the agmem plugin's checkpoint ritual (recall first,
  so corrections land as `supersedes`; `reflect` with `derived_from` for
  conclusions) so you can `/clear` instead of letting auto-compaction fire,
  then applies the gate that accepts or drops agents' proposed learnings, the
  one step that is this framework's own.
- **`/agmem-import`** moves a pre-agmem `LEDGER.md` and branch state files
  into the store, once. Showing and tidying the store are the plugin's
  `/agmem:memory show` and `/agmem:memory tidy`.
- **`/ctx-flow-doctor`** checks every dependency above, the hooks, both MCP
  registrations, and whether ast-grep actually parses this project's
  languages; prints the exact fix for anything broken, including a stale
  agmem binary, which fails silently otherwise.
- **`/ast-grep-it [lang]`** teaches ast-grep a language it does not ship.
  Called bare, it sets up whatever this project most needs. ast-grep loads
  any tree-sitter grammar from a dynamic library, so "not supported" is
  usually a missing binary, not a missing capability, Nushell being the case
  in point. Finds the grammar repo, compiles it into
  `~/.cache/ctx-flow/grammars` (shared by every project on the machine),
  registers it in the project's `sgconfig.yml`, and verifies it against real
  files. `sgconfig.yml` is machine-local; gitignore it.
- **`/bare-worktree`** is `init`, `add`, `remove`, `apply`, `discard`, `which`
  for the layout above; the only sanctioned way to make a worktree here.

### The hooks

Deterministic work that costs zero tokens and happens *every* time, which no
prompt instruction achieves. Six are `.nu` scripts under `hooks/scripts/`,
sharing `_common.nu` and the `ctx-flow-paths.nu` resolver; the seventh is
rtk's. Their cases live in `hooks/tests/`. The memory hooks (briefing
injection, the recall log, the seam nudges) are the agmem plugin's
(`agmem hook …`), not this folder's.

| Event | Script | Does |
|---|---|---|
| `UserPromptSubmit` | `prompt-context-nudge.nu` | The session-length nudge: reads the context size the last turn was served with (the API `usage` on the transcript's last assistant line, a `tail -c`, no parse of the file) and, from 120k tokens, says once "/checkpoint then /clear", then again only per further 40k. CLAUDE.md's "prefer several short sessions" rule had no enforcement; the audit's two largest sessions ran 400+ turns at ~267k each and never cleared. Knobs `CTX_FLOW_CONTEXT_NUDGE_TOKENS` / `_STEP` |
| `SessionStart` | `session-start-layout.nu` | The worktree layout check: in a bare layout the real `.claude` is symlinked into each worktree, and one carrying its own copy has silently diverged, a fact about this checkout the plugin cannot know. The briefing, the branch tag and the post-compaction warning arrive through the plugin's own SessionStart hook |
| `SessionStart` | `session-start-notebook.nu` | Prints `notebook.md` into context whole, verbatim, no budget. If it outgrows a session, that is a finding for Claude to act on by pruning, not for a hook to hide by summarising |
| `PreToolUse` (Bash) | `rtk hook claude` | Transparently rewrites Bash commands so their output arrives compressed |
| `PreToolUse` (Bash) | `pre-worktree-guard.nu` | Denies raw `git worktree add/remove/move` in a bare layout, since they skip the profiles and the `.claude` symlink |
| `PreToolUse` (Bash, Read) | `pre-read-guard.nu` | Denies a whole-file read: a bare `Read` with no `offset`/`limit`, or `cat`/`head`/`tail`/`sed` printing more than 300 lines of a file over 12 kB, and asks for a window instead. The 2026-09-03 token audit found those two shapes were 46% of every tool-result character this repo's sessions ever paid for. Any windowed call passes, including one whose `limit` covers the whole file; `cat big \| wc -l`, heredocs and redirects pass. Knobs `CTX_FLOW_READ_MAX_LINES` / `CTX_FLOW_READ_SMALL_BYTES` |
| `PostToolUse` (Bash) | `post-bash-nudges.nu` | One nudge, fired at most once per session and unable to block: when a Bash call reached for `sed`/`python`/`grep` where nu or ast-grep is the house tool. It exists because a rule in an always-loaded file is a rule you stop seeing; this repo's own transcripts showed CLAUDE.md losing to habit on 18% of Bash calls. The `git push` checkpoint nudge moved to the plugin |

### The notebook

`notebook.md` is Claude's own, not the project's, and the undistilled
counterpart to agmem: open questions, changed minds, taste, complaints,
drafts, each entry dated. agmem holds claims; the notebook holds what is not
a claim yet. It is written the moment something is noticed, mid-task, not
held for a checkpoint. What firms up moves to the store; the file is pruned
by hand, on a reread every week or so. It lives at the shared root in a bare
layout so every worktree sees one copy, and the hook loads it whole because
nothing should rewrite it.

### Where the rules live

`CLAUDE.md` is injected as a user message *after* the system prompt; an
output style is appended *to* it. So the two split by how much they need to
survive momentum, not by topic:

| File | Holds |
|---|---|
| `output-styles/ctx-flow.md` | the ~25 lines that must not be forgotten mid-task: routing, tool choice, answer shape |
| `CLAUDE.md` | the routing table, the shell/worktree/memory rules, and every case the style does not cover |
| this README | the reasoning behind both, one `Read` away, never on the prefix |
| `docs/reference.md` | the return-contract template, the memory mapping, playbook guards; loaded on demand when dispatching a custom agent |

The cost is duplication: change a rule in one and it drifts from the other.
Keep the style terse enough that it is obviously a summary. Set via the
`outputStyle` field in `settings.json`; it is read once, so it takes effect
after `/clear` or a new session, and it does not reach subagents. The
sections CLAUDE.md dropped in favour of the style (delegation prose, the
numbered answer shape) do not matter to a subagent, which has a return
contract instead.

Three commands (`/agmem-import`, `/ast-grep-it`, `/bare-worktree`) carry
`disable-model-invocation: true`: only you run them, so their description
lines leave the model's skill listing. The machine-local
`rust-expert-developer` skill's description is ~300 bytes for the same
reason; the listing truncates at ~1 kB, so a longer trigger list never
reached the model anyway.

## Scoping capability to agents

The main thread never loads what only one role needs: the same routing rule,
applied to configuration.

### MCP servers: agent-scoped by default

A globally registered MCP server loads its schema into *every* session, which
is exactly the cost this framework exists to avoid. When a server is genuinely
needed, declare it on the one agent that uses it. It starts only when that
agent runs, and the main session never sees a token of it:

```yaml
---
name: db-inspector
description: Answers questions against the staging database, returns verdicts
mcpServers:
  - postgres:
      type: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/staging"]
tools: Read, mcp__postgres__*
---
```

`mcpServers` takes inline definitions (same schema as `.mcp.json`; stdio and
http/sse both work) or the bare name of an already-registered global server.
Declaring the server does not grant its tools: list `mcp__<server>__*` (or
individual `mcp__<server>__<tool>` entries) in `tools:` as well. Two servers
stay global: `nu --mcp` (it is the shell, and every session wants the shell)
and `agmem` (it is the memory; the main thread writes it, and the agents
whose lessons pay for the schema declare read-only access to recall them).

### Skills: who gets to see one

The levers, ordered from fully documented to verify-once:

- **Guarantee a subagent has a skill.** The agent-frontmatter `skills:` field
  preloads the full skill content when the agent starts, no discovery step.
  Plugin skills are referenced by bare name, same as local ones:

  ```yaml
  ---
  name: migrator
  description: Plans and applies database migrations
  skills:
    - db-migrations        # local or plugin-shipped, bare name either way
  ---
  ```

- **Keep the model from invoking a skill anywhere.** Set
  `disable-model-invocation: true` in the skill's own frontmatter; only you
  can trigger it, via `/name`. This also removes it from the preload pool
  ("preloading draws from the same set of skills Claude can invoke"), so it
  cannot combine with the lever above.

- **Hide a local skill by config.** `skillOverrides` in `settings.json`;
  values `"off"`, `"name-only"`, `"user-invocable-only"`, `"on"`:

  ```json
  { "skillOverrides": { "db-migrations": "off" } }
  ```

  Explicitly does **not** cover plugin skills: "Plugin skills are not
  affected by `skillOverrides`. Manage those through `/plugin` instead."

- **Hidden in main + preloaded into one agent.** No *documented* combo, but
  two mechanically sound ones: preload is startup injection, not a Skill-tool
  call, and the documented preload exclusions are only
  `disable-model-invocation` skills and the bundled `/verify`; neither hiding
  mechanism below is on that list.

  | Skill origin | Hide in main via | Grant to the agent via |
  |---|---|---|
  | local | `skillOverrides: "off"` | `skills:` preload |
  | plugin | permissions deny rule `Skill(<name>)` | `skills:` preload |

  Caveats: a deny rule blocks *invocation*, so the skill's description line
  may still occupy main-prompt tokens; and both rows rest on undocumented
  interactions, so verify once in a scratch session (spawn the agent, have it
  quote a marker line from the skill) and re-verify after Claude Code
  upgrades.

- **The zero-dependency fallback.** Don't make it a skill. Only
  `.claude/skills/` is discovered, so keep the folder at
  `.claude/docs/skills/<name>/` (for a plugin skill: vendor the folder out of
  the plugin instead of installing it) and have the agent's definition or
  playbook `Read` the `SKILL.md` at startup. Nothing else ever sees it; it is
  the same move playbooks already make.

## Memory

Durable state lives in **agmem**, outside both the window and the repo. The
space derives from the repo's shared git dir: every branch and worktree of a
project reads one store, and the reserved `user` space follows you across
projects. This is the answer to "how do I keep context without keeping it in
context": you don't hold it, you *address* it. The store holds the claims; the
window is a working set.

The loop: the briefing arrives with the session (the plugin's `SessionStart`
hook injects it; call the agmem `context` tool yourself on a topic shift, or
when no block opened the session); `recall` before assuming; `/checkpoint` at
seams, decisions with reasons, corrected assumptions, gotchas, with `recall`
before every write so a correction lands as `supersedes` rather than a
contradiction. The old claim stays readable and dated; only one is live.
Three kinds carry the lifetime split the old ledger/state files used to:
`fact` (fades over weeks unless used), `lesson` (fades over months),
`instruction` (pinned into every briefing). Branch state is a `fact` with
`decay_class: fast` tagged `branch:<slug>`; it dies in days, as branch state
should, with no file to prune.

Artifacts live there too, since agmem v0.2.0: a subagent's long output (a
plan, a review, a test log) is a **document**, written through
`scripts/doc-put.nu` (which tags it with the branch and the role) and handed
back as `DOC: <id> memory://…`. `/checkpoint` reads a `LEARNED:` proposal
out of the document and stores the accepted lesson citing it. The old
`.claude/notes/` dropbox is retired; `/agmem-import` moves one in and
`/ctx-flow-doctor` flags one that comes back.

## Playbooks: knowledge that accumulates

A role's project specifics live in the store as `lesson`s tagged
`role:<agent>`. `architect`, `verifier` and `scout` recall their own tag
before starting (read-only wiring, declared per agent); the other four carry
no memory tool and receive their lessons in the dispatch prompt. Nothing else
loads them, so an unused playbook costs nothing. Rules **append** to the
agent's definition and never override it; on conflict the agent file wins.

The guards the file version needed are mostly the store's behaviour now:
near-duplicates are refused at write time, unused rules fade by decay instead
of accumulating, and `/agmem:memory tidy` merges what still piles up. What
remains yours is the structural guard: **proposing is not committing**.
Agents end with `LEARNED: <claim> — <evidence>`, and `/checkpoint` applies
the four tests (durable, non-obvious, earned, actionable) before anything
lands. The agent proposing a rule is often the cheapest thing in the system,
and rules bind every future run; dropping proposals is the normal outcome.
Scripts never self-modify at all.

## Git and gitignore

Memory is not a git question: the store lives under your home directory, not
in the repo, and so do subagent artifacts. What remains in `.claude/` is the
framework itself, tracked, with its machine-local parts listed in
`.claude/.gitignore`: `settings.local.json`, the private
`rust-expert-developer` skill, and the retired `notes/` (kept ignored so a
regressed agent can never commit a blob). `sgconfig.yml` is ignored at the
repo root. When you write `.gitignore` rules for `.claude`, use `.claude/*`
(contents), never `.claude/` (directory): git will not descend into an
excluded directory, so `!` negations under it would be silently dead.

## Staying ahead of compaction

No hook can call `/clear`; the harness owns session control flow. The loop:

1. `/checkpoint` at a natural seam (a `git push`, an answered question, or a
   turn that recalled memory and wrote none; the plugin's hooks nudge at
   each).
2. `/clear`.
3. The plugin's `SessionStart` hook puts the briefing in front of the fresh
   session before its first token, and this folder's puts the notebook
   beside it.

Leave auto-compaction enabled as the backstop: disabling it trades a lossy
summary for a hard `prompt_too_long` failure. When it fires, the restart
warning says so, and memory is one `context` call away.

Prefer several short sessions chained through memory over one long one. A
600k-token session produces worse output than a 100k one even when it never
compacts; the token saving is a side effect of the quality win.

## Layout

```
.claude/                    this folder, copied into your project
├── CLAUDE.md               the routing discipline; loads every session
├── README.md               this file: the reasoning, one Read away
├── notebook.md             Claude's undistilled notes, loaded whole each session
├── settings.json           hook registration and the output style
├── .gitignore              the machine-local parts: settings.local.json, one private skill, the retired notes/
├── agents/                 runner, verifier, architect, browser, tracker, scout, researcher
├── commands/               checkpoint (wraps the plugin's), agmem-import, ctx-flow-doctor, ast-grep-it, bare-worktree
├── docs/reference.md       contracts, memory mapping, playbook guards; loaded on demand
├── hooks/scripts/          the six hooks, _common.nu and the ctx-flow-paths.nu resolver
├── hooks/tests/            their cases: read-guard, context-nudge, notebook-load
├── output-styles/          ctx-flow.md: the hard rules, appended to the system prompt
├── scripts/                doctor.nu, doc-put.nu (subagent artifacts → documents), import-notes.nu, build-grammar.nu, the grammars.nu registry, bare-worktree.nu
├── skills/nushell/         deep Nushell reference, loaded when writing nu
├── skills/ast-grep/        rule-writing workflow, loaded when a query needs more than a pattern
├── skills/ast-grep-lite/   the 2 kB card preloaded into scout, verifier, architect, researcher
└── skills/                 a machine-local skill dropped in here stays untracked; list it in .gitignore
```

MIT.
