# agmem plugin for Claude Code

Persistent memory wired into every session, with nothing to bootstrap by hand:
the plugin registers the agmem MCP server, injects the memory briefing at
session start, ships the checkpoint and tidy commands, and logs which claims
each session recalled and wrote so the store can learn what helped.

Every hook is a subcommand of the `agmem` binary (`agmem hook <event>`), so
the plugin needs nothing beyond agmem itself — no scripting runtime, no `jq`.

## Install

Requires `agmem` 0.1.10 or newer on `PATH` — the Homebrew tap, the shell
installer, or `cargo install --git` as the repository README's Install
section describes (nothing is on crates.io).

```
claude plugin marketplace add AlfoldiMate/agmem
claude plugin install agmem@agmem
```

To try it from a checkout without installing: `claude --plugin-dir ./plugin`.

The plugin's version is the binary's: every release pins both manifests to
the crate version, since the hooks are subcommands of that binary.

## What it adds

| Piece | What it does |
|---|---|
| `.mcp.json` | Registers `agmem` over stdio. The space derives from the repo, so every branch and worktree of a project reads one store. |
| `SessionStart` hook | Injects the briefing (`agmem context`, aimed at the branch and last commit) before the first token, names the branch tag and how many documents carry it, and after a compaction lists the claims the session had recalled so the next checkpoint can cite them. |
| `PostToolUse` hook | Logs the ids each `recall` returned and each `remember`/`reflect` wrote or cited, under `<data dir>/hooks/<session>.jsonl`. Nudges once per session after a successful `git push`, and once after an answered `AskUserQuestion` — both are decision seams. |
| `Stop` hook | Nudges once per session when memory was recalled and nothing was written back. |
| `/agmem:checkpoint` | The distil → recall → remember → reflect ritual. Step 4 (cite with `derived_from`) is byte-identical to the server's own checkpoint prompt; a test in the server crate fails if they drift. |
| `/agmem:memory show\|tidy` | Show the store and judge the briefing; tidy shells out to `agmem consolidate` (the tool is off the default MCP list since #150) and closes with `agmem forget`, merging through `remember` with `supersedes`. Needs an agmem release carrying those subcommands. |
| `/agmem:doctor` | Self-check plus duplicate-registration check. |
| `agmem` skill | The judgement: what is worth storing, correct-never-contradict, kinds, seams. |

## Coexistence

**Already registered agmem yourself** (`claude mcp add agmem --scope user`)?
Both stay configured; Claude Code connects the higher-precedence one (local >
project > user > plugin), so nothing runs twice. It only changes the tool
names — `mcp__agmem__*` for your registration, `mcp__plugin_agmem_agmem__*`
for the plugin's. The plugin's hooks and commands accept both.

**A project whose own hooks already inject `agmem context`** (ctx-flow did,
before the plugin)? Plugin hooks and project hooks both run, so the briefing would
appear twice. Remove the project's SessionStart memory hook and keep the
plugin's.

## The session log

`agmem hook post-tool-use` appends one JSON line per recall and per write to
`<data dir>/hooks/<session_id>.jsonl` (`agmem --doctor` prints the data dir).
Logs untouched for seven days are removed at the next session start. The log
is what makes "recalled-then-cited" countable client-side — the store cannot
keep it, because reinforcing a claim overwrites its last-accessed stamp — and
it is what the post-compaction briefing reads.
