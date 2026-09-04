---
description: Inspect or tidy this project's agmem memory — show what the store holds and whether the briefing is still right, or consolidate duplicates, contradictions and stale claims.
argument-hint: "[show | tidy]"
allowed-tools: Read, Bash(agmem consolidate:*), Bash(agmem forget:*), Bash(agmem doc forget:*), mcp__agmem__context, mcp__agmem__recall, mcp__agmem__inspect, mcp__agmem__remember, mcp__agmem__reflect, mcp__plugin_agmem_agmem__context, mcp__plugin_agmem_agmem__recall, mcp__plugin_agmem_agmem__inspect, mcp__plugin_agmem_agmem__remember, mcp__plugin_agmem_agmem__reflect
---

# agmem memory

Operate on this project's memory store according to `$ARGUMENTS` (default:
`show`). The space derives from the repo automatically; you never name it
except where noted.

## show

`inspect` with `ref: "stats"` for the counts, then `context` with no query for
the general briefing. Report: live claims by kind for this project's space and
`user`, the briefing itself, and one line of assessment — is anything in it
stale, wrong, or contradicted by what the repo says right now? A claim worth
doubting gets `inspect` on its id (provenance and correction history), not a
hedge.

## tidy

Run `agmem consolidate` in the shell — the tool is off the default MCP list
(#150) and the CLI is its door; it prints the tool's JSON — then judge each
list. It decides nothing itself, and empty lists are the healthy outcome, not
a failure:

- **near_duplicates** — merge a group with one `remember`: the wording worth
  keeping, `supersedes` set to every other member's id. Check
  `min_similarity` first: it is the weakest pair anywhere in the group, so a
  low value means the cluster chained through a middle claim and may be two
  claims, not one. Never `forget` a duplicate — that deletes the history the
  merge exists to keep.
- **contradictions** — read both; nothing has judged that they disagree. When
  one is wrong, send the right one with the wrong one's id in `supersedes`.
  When both are right (scope differs), leave them.
- **stale_contexts** — a `fast` claim recall kept alive: if it proved durable,
  re-store it with a slower `decay_class` (superseding the fast one); if it
  was scaffolding, close it with `agmem forget <id>`.
- **over_full_tags** — merge the way a duplicate group merges: one `remember`
  with the wording worth keeping and the absorbed lessons' ids in `supersedes`.
- **orphan_documents** — distil what one still says and `remember` it citing
  the document, or, once the user confirms, `agmem doc forget <id> --purge`.

Merges and corrections go through MCP `remember`/`reflect` with `supersedes`;
only closing and purging shell out. Report what you merged, corrected, and
expired, in at most five lines.

Never purge (`agmem forget --purge`, `agmem doc forget --purge`) during any
of these modes without the user confirming first.
