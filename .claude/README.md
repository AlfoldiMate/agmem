# ctx-flow

A token-frugal development framework for Claude Code, shipped as the contents
of a `.claude` folder — this repo *is* the folder; mount it as one.

The premise: **the main thread should hold decisions, and everything else should
hold output.** Build logs, search results, page snapshots and ticket records are
the largest objects in any session and carry the least information per token.
ctx-flow routes each of them into a subagent sized for the job, and keeps a
durable ledger so the main thread's context survives `/clear` and `/compact`.

## Install

Make this repo your project's `.claude` directory — copy it, or symlink it:

```bash
ln -s /path/to/context-flow /path/to/your-project/.claude
```

For linked git worktrees, symlink the main worktree's `.claude` into each one;
everything here is symlink-safe, and memory resolves through the main worktree
regardless (`git rev-parse --git-common-dir`), so a fresh worktree is never
amnesiac either way.

Then verify the toolchain:

```
/ctx-flow-doctor
```

### Dependencies

| Tool | Why | Install |
|---|---|---|
| [nu](https://www.nushell.sh) | runs the hooks; provides the one MCP server the framework wants | `brew install nushell` |
| [ast-grep](https://ast-grep.github.io) | structural (syntax-aware) code search | `brew install ast-grep` |
| [gh](https://cli.github.com) | GitHub — always over the GitHub MCP server | `brew install gh` |
| [playwright-cli](https://github.com/microsoft/playwright-cli) | browser driving as shell commands | `npm i -g playwright-cli` |
| [rtk](https://github.com/rtk-ai/rtk) | transparently compresses Bash output | `brew install rtk` (hook ships here) |
| acli *(optional)* | Jira | Atlassian's installer |
| tree-sitter-cli *(optional)* | builds grammars ast-grep does not ship | `npm i -g tree-sitter-cli` |

You do not have to *use* Nushell as your shell; the hooks run under `nu`
regardless of what your terminal runs. Register the nu MCP server with
`claude mcp add nu -- nu --mcp`.

**On MCP:** the framework avoids MCP servers — a CLI beats one wherever a CLI
exists (you choose the fields, the output pipes, no schema in the prompt). The
single exception is `nu --mcp`, which earns its slot by *being* the shell:
structured pipelines, `$history` for re-slicing past results without
re-running, safe uncapped first runs.

**On rtk:** the framework's `settings.json` registers its hook (`rtk hook
claude`) — install only the binary, and do **not** also run `rtk init -g`, or
every Bash call is rewritten twice (`/ctx-flow-doctor` flags this as DOUBLED).
As of rtk 0.46 the hook only rewrites the command (`updatedInput`), leaving
your permission prompts intact — older versions also emitted
`permissionDecision: "allow"`. Telemetry is on by default
(`rtk telemetry disable`).

## What's in it

### CLAUDE.md — the system prompt

The routing discipline lives in `CLAUDE.md`, loaded automatically every
session. It used to be a skill; it isn't one any more, deliberately. A skill
must be *discovered* — a description line the model may or may not act on —
and routing that only sometimes applies is routing that silently doesn't.
Rules that apply to every session belong in the file that loads every session.

It carries: the delegation rule (route by **information ratio**, not task
type), the routing table, payload discipline, the memory loop, and the answer
shape — output led by the next action, numbered steps, restated state, no
preamble, questions asked through `AskUserQuestion` rather than prose.

### Five agents

| Agent | Model / effort | Absorbs |
|---|---|---|
| `runner` | haiku / low | builds, suites, linters — returns the failure signature |
| `verifier` | sonnet / high | adversarial check of one claim — defaults to refuting |
| `architect` | opus / high | design of a non-trivial change — read-only, never edits |
| `browser` | sonnet / medium | `playwright-cli` sessions — snapshots stop here |
| `tracker` | haiku / low | Jira/GitHub via `gh`/`acli` — never raw records |

Every one ends with a **mandatory return contract**: fixed keys, hard caps, and
an explicit forbidden list. An unschematized subagent writes an essay; a
schematized one writes 200 tokens. Tiers are picked by *consequence of being
wrong*, not output size — `runner` misses cost one re-dispatch; a bad
`architect` plan is discovered late, after the code exists.

Codebase search is **not** here: Claude Code ships `Explore`, which already
does it. Route search there.

### Four commands

- **`/checkpoint`** — writes durable state to the ledger so you can `/clear`
  instead of letting auto-compaction fire; also the gate that accepts or drops
  agents' proposed playbook learnings.
- **`/ledger`** — `show`, `init`, or `prune` the two memory files.
- **`/playbook`** — `show`, `init <role>`, or `prune` the agent playbooks.
- **`/ctx-flow-doctor`** — checks every dependency above, the hooks, the nu
  MCP registration, and whether ast-grep actually parses this project's
  languages; prints the exact fix for anything broken.
- **`/ast-grep-it [lang]`** — teaches ast-grep a language it does not ship.
  Called bare, it sets up whatever this project most needs.
  ast-grep loads any tree-sitter grammar from a dynamic library, so "not
  supported" is usually a missing binary, not a missing capability — Nushell
  being the case in point. Finds the grammar repo, compiles it into
  `~/.cache/ctx-flow/grammars` (shared by every project on the machine),
  registers it in the project's `sgconfig.yml`, and verifies it against real
  files. `sgconfig.yml` is machine-local — gitignore it.

### Four hooks

Deterministic work that costs zero tokens and happens *every* time, which no
prompt instruction achieves. Three are `.nu` scripts under `hooks/scripts/`;
the fourth is rtk's.

| Event | Does |
|---|---|
| `SessionStart` | injects both memory files — the repo ledger and the current branch's state — which is what makes `/clear` cheap; after a compaction (`source: "compact"`) it also warns that context was truncated and remembered line numbers are unreliable |
| `PreCompact` | stamps the ledger with a running count, so silent context loss becomes a visible pattern |
| `PreToolUse` | `rtk hook claude` — transparently rewrites Bash commands so their output arrives compressed |
| `PostToolUse` | two nudges, each fired at most once per session and neither able to block: after a successful `git push`, that a checkpoint seam has arrived; and when a Bash call reached for `sed`/`python` where nu is the house tool. The second exists because a rule in an always-loaded file is a rule you stop seeing — this repo`s own transcripts showed CLAUDE.md losing to habit on 18% of Bash calls |

### Where the rules live

`CLAUDE.md` is injected as a user message *after* the system prompt; an output
style is appended *to* it. So the two split by how much they need to survive
momentum, not by topic:

| File | Holds |
|---|---|
| `output-styles/ctx-flow.md` | the ~25 lines that must not be forgotten mid-task — routing, tool choice, answer shape |
| `CLAUDE.md` | the reasoning behind them, the tables, and every case they do not cover |

The cost is duplication: change a rule in one and it drifts from the other.
Keep the style terse enough that it is obviously a summary. Set via the
`outputStyle` field in `settings.json`; it is read once, so it takes effect
after `/clear` or a new session, and it does not reach subagents.

## Scoping capability to agents

The main thread never loads what only one role needs — the same routing rule,
applied to configuration.

### MCP servers: agent-scoped by default

A globally registered MCP server loads its schema into *every* session, which
is exactly the cost this framework exists to avoid. When a server is genuinely
needed, declare it on the one agent that uses it — it starts only when that
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
Declaring the server does not grant its tools — list `mcp__<server>__*` (or
individual `mcp__<server>__<tool>` entries) in `tools:` as well. The one
server that stays global is `nu --mcp`: it is the shell, and every session
wants the shell.

### Skills: who gets to see one

The levers, ordered from fully documented to verify-once:

- **Guarantee a subagent has a skill** — the agent-frontmatter `skills:` field
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

- **Keep the model from invoking a skill anywhere** — set
  `disable-model-invocation: true` in the skill's own frontmatter; only you
  can trigger it, via `/name`. This also removes it from the preload pool
  ("preloading draws from the same set of skills Claude can invoke"), so it
  cannot combine with the lever above.

- **Hide a local skill by config** — `skillOverrides` in `settings.json`;
  values `"off"`, `"name-only"`, `"user-invocable-only"`, `"on"`:

  ```json
  { "skillOverrides": { "db-migrations": "off" } }
  ```

  Explicitly does **not** cover plugin skills — "Plugin skills are not
  affected by `skillOverrides`. Manage those through `/plugin` instead."

- **Hidden in main + preloaded into one agent** — no *documented* combo, but
  two mechanically sound ones: preload is startup injection, not a Skill-tool
  call, and the documented preload exclusions are only
  `disable-model-invocation` skills and the bundled `/verify` — neither hiding
  mechanism below is on that list.

  | Skill origin | Hide in main via | Grant to the agent via |
  |---|---|---|
  | local | `skillOverrides: "off"` | `skills:` preload |
  | plugin | permissions deny rule `Skill(<name>)` | `skills:` preload |

  Caveats: a deny rule blocks *invocation* — the skill's description line may
  still occupy main-prompt tokens; and both rows rest on undocumented
  interactions, so verify once in a scratch session (spawn the agent, have it
  quote a marker line from the skill) and re-verify after Claude Code
  upgrades.

- **The zero-dependency fallback** — don't make it a skill. Only
  `.claude/skills/` is discovered, so keep the folder at
  `.claude/docs/skills/<name>/` (for a plugin skill: vendor the folder out of
  the plugin instead of installing it) and have the agent's definition or
  playbook `Read` the `SKILL.md` at startup. Nothing else ever sees it; it is
  the same move playbooks already make.

## Memory

Two files, split by **lifetime**, both reloaded at every session start:

| File | Holds | Lifetime |
|---|---|---|
| `<main worktree>/.claude/notes/LEDGER.md` | Goal, Map, Gotchas, decisions with reasons | outlives the branch |
| `<main worktree>/.claude/notes/state/<branch>.md` | Done, Next, Blocked | dies with the branch |

Both resolve through `git rev-parse --git-common-dir`, so every linked worktree
shares one copy — symlinked `.claude` or not, the same files are found. This is
the answer to "how do I keep context without keeping it in context": you don't
hold it, you *address* it. The filesystem is the store; the window is a working
set.

## Playbooks — knowledge that accumulates

`.claude/playbooks/<role>.md` holds a project's specifics for one agent role.
Each agent reads its own before starting; nothing else loads them, so an unused
playbook costs nothing. A playbook **appends** to its agent's definition and
never overrides it; on conflict the agent file wins.

They grow by accretion, under guards: cap 60 lines and merge-never-append
(against unbounded growth); an evidence threshold of twice-or-once-at-real-cost
(against overfitting); date + evidence on every entry (against unverified
claims compounding); `/playbook prune` re-checks entries against the repo
(against staleness).

The structural guard is **proposing is not committing**: agents end with
`LEARNED: <claim> — <evidence>`, and `/checkpoint` decides what lands. The
agent proposing a rule is often the cheapest thing in the system, and rules
bind every future run — dropping proposals is the normal outcome. This gate is
what makes self-updating files safe. Scripts never self-modify at all; the one
thing that varies is an environment variable.

## Git, gitignore, and worktrees

The framework does not assume `.claude/` is tracked. Three setups all work:

- **Tracked** — commit `LEDGER.md` and `playbooks/`, gitignore `notes/state/`.
  You gain a review layer: a learning arrives as a diff someone can reject.
  Use `.claude/*` (contents), never `.claude/` (directory), in `.gitignore` —
  git will not descend into an excluded directory, so `!` negations under it
  would be silently dead.
- **Gitignored** — everything works from disk; memory is local to the clone.
- **Symlinked into worktrees** — one `.claude` shared by all worktrees; memory
  and playbooks converge on the same files the hooks already resolve to.

In the untracked and symlinked setups a committed playbook rule binds every
branch immediately — which is exactly why the checkpoint gate, not git, is
where scrutiny lives.

## Staying ahead of compaction

No hook can call `/clear` — the harness owns session control flow. The loop:

1. `/checkpoint` at a natural seam (a `git push` triggers a nudge automatically).
2. `/clear`.
3. `SessionStart` reloads both memory files.

Leave auto-compaction enabled as the backstop — disabling it trades a lossy
summary for a hard `prompt_too_long` failure. When it fires, `PreCompact`
stamps the ledger, so "this keeps happening, checkpoint earlier" is visible
rather than silent.

Prefer several short sessions chained through the ledger over one long one. A
600k-token session produces worse output than a 100k one even when it never
compacts; the token saving is a side effect of the quality win.

## Tuning

Environment variables over built-in defaults, and nothing else — no config
file, by design: a hook that parses config gains three new ways to break the
session it exists to help.

| Env var | Default | Effect |
|---|---|---|
| `CTX_FLOW_LEDGER_MAX_LINES` | `400` | cap on the injected ledger |

## Layout

```
context-flow/               mounted as your project's .claude
├── CLAUDE.md               the routing discipline — loads every session
├── settings.json           hook registration
├── agents/                 runner, verifier, architect, browser, tracker
├── commands/               checkpoint, ledger, playbook, ctx-flow-doctor, ast-grep-it
├── docs/reference.md       contracts, templates, rationale — loaded on demand
├── hooks/scripts/          the three hooks + shared _common.nu + paths resolver
├── output-styles/          ctx-flow.md — the hard rules, appended to the system prompt
├── scripts/                doctor.nu + build-grammar.nu + grammars.nu registry
├── skills/nushell/         deep Nushell reference, loaded when writing nu
├── playbooks/<role>.md     accumulates per project    (created by use)
└── notes/                  LEDGER.md + state/<branch> (created by use)
```

MIT.
