---
description: Store the session's durable state in agmem so you can safely /clear instead of letting /compact fire — the agmem plugin's ritual, plus this framework's gate on agents' proposed learnings.
argument-hint: "[optional: what to emphasise]"
allowed-tools: Read, Bash, Glob, Grep, Skill, mcp__agmem__recall, mcp__agmem__remember, mcp__agmem__reflect, mcp__agmem__inspect, mcp__plugin_agmem_agmem__recall, mcp__plugin_agmem_agmem__remember, mcp__plugin_agmem_agmem__reflect, mcp__plugin_agmem_agmem__inspect
---

# Checkpoint

Store memory so that a fresh session — with none of this context — could pick
the work up without asking a single question. The ritual itself ships with
agmem's Claude Code plugin; this command runs it and adds the one step that
is this framework's own.

## 1. The ritual

Invoke the **`agmem:checkpoint`** skill, passing `$ARGUMENTS` through. It
carries the measured order — distil, `recall` before every write so a
correction lands as `supersedes`, `remember` in one batch, `reflect` with
`derived_from` for a conclusion drawn from stored claims, branch state as
fast-decay facts under the branch tag — and reports what it stored.

If no `agmem:checkpoint` skill exists in this session, the plugin is not
installed: say so, point at `/ctx-flow-doctor` for the fix, and stop. Do not
improvise the ritual from memory — its wording is the measured part.

## 2. Gate proposed learnings

Subagents may end a response with `LEARNED: <claim> — <evidence>`. Those are
**proposals**. This is where they are accepted or dropped — that separation
is deliberate: the agent proposing a rule is often the cheapest, least
informed thing in the system, and rules it writes bind every future run.

Collect them from two places. The replies still in context, and this
branch's **documents** — an agent that wrote one closed it with the same
line, and that copy survives compaction. List them with the branch tag the
plugin announced at session start (or `nu
"${CLAUDE_PROJECT_DIR:-.}/.claude/hooks/scripts/ctx-flow-paths.nu" --tag`):

```bash
agmem doc list --tag branch:<slug>
agmem doc get <id> --raw | rg '^LEARNED:'      # per document written this session
```

Keep the documents in the shell; only the proposal lines reach the thread.
The `agent:<name>` tag on a document names the role a proposal belongs to.

For each proposal, apply all four tests. Store it only if it passes all of
them:

1. **Durable** — true of this project, not of this task.
2. **Non-obvious** — not something the repo, a type, or a test already says.
3. **Earned** — seen more than once, or once at real cost. A single cheap
   surprise is an anecdote.
4. **Actionable** — it changes what a future run would *do*. "The parser is
   complex" changes nothing.

An accepted proposal becomes a `lesson` tagged `role:<agent>`, with the
evidence in the claim itself ("… — proved by the 2026-08-30 retry-path
deadlock"), because a rule whose reason is invisible cannot be pruned honestly
later. One that came out of a document is stored with `reflect`, not
`remember`, with `derived_from: ["episode:<doc id>"]` — the lesson then cites
the report it was earned in, and `inspect` can open it. A proposal that
binds *every* session, not just one role, becomes an `instruction` — rare,
and worth a second look before pinning.

Before storing, `recall` with the same `role:` tag: if an existing rule
contradicts the new one, resolve it now with `supersedes` — two opposing rules
are worse than neither.

**Dropping a proposal is the common outcome and needs no justification.** Say
how many you saw (and how many of those came from documents) and how many
you kept, and move on.

## 3. Legacy files

If `/ctx-flow-doctor`'s `notes` row is not `ok`, this checkout still carries
the retired `.claude/notes/` dropbox — a pre-agmem ledger, or artifacts from
before documents. Run `/agmem-import` once to move them into the store.

## Then

One line on top of the ritual's report: proposals seen vs kept. Then tell the
user they can safely `/clear` — better than letting auto-compaction fire,
because the memory is now in the store and the plugin's SessionStart hook
puts it in front of the next session.
