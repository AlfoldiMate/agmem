---
name: ctx-flow
description: Routing discipline — the main thread holds decisions, everything else holds output
keep-coding-instructions: true
---

The main thread holds decisions; everything else holds output. `.claude/CLAUDE.md`
carries the routing table and the cases these rules do not cover; the reasoning
behind them is in `.claude/README.md`.

## Routing

- Delegate by information ratio, not by task type. Searching, test suites, logs,
  browsers and trackers turn a lot of output into a short conclusion — those go
  to a subagent, always.
- Never delegate the write path. Choosing between designs, or making the edit,
  *is* the accumulated context; a fresh prompt prefix produces a worse change.
- The main thread receives verdicts, never transcripts. Anything whose size you
  cannot predict in advance is a `runner` job; a question that takes many files
  to answer is a `researcher` job. `general-purpose` is never a target, and an
  `Explore` prompt ends with "hard cap 2,000 characters, repo-relative paths;
  anything longer goes in a document".

## Tools

- Structured data — JSON, tables, git plumbing, HTTP responses — goes through
  `mcp__nu__evaluate`. Run a first command uncapped and slice `$history`
  afterwards; the server truncates the reply but keeps the whole result.
- Questions about syntax — callers, definitions, code shapes — go to `ast-grep`.
  `grep` answers questions about text and lies about everything else.
- Shell-driven file edits use nu (`open --raw | str replace | save -f`), never
  sed or python. All three exit 0 when the pattern misses, so confirm the old
  text was present and the new text is.
- Worktrees in bare layouts go through `/bare-worktree` (add, remove, apply),
  never raw `git worktree` — profiles and the `.claude` symlink only exist on
  the scripted path.

## Answers

- Lead with the outcome, the command, or the verdict. Context after, if at all.
- Number multi-step work, one bounded action per step, and end with a single
  concrete next action while anything is open.
- Multi-step work runs through the task list — one entry per bounded step and
  one per dispatched subagent, checked off as each lands. The user follows the
  checklist, not the transcript.
- Answers read one effort level below the work: effort buys deeper checking,
  never a longer or more ceremonious reply.
- When you need a decision and the options are enumerable, ask with
  `AskUserQuestion`. A prose question mid-scroll blocks nothing and gets lost.
