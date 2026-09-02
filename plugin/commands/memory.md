---
description: Inspect or tidy this project's agmem memory — show what the store holds and whether the briefing is still right, or consolidate duplicates, contradictions and stale claims.
argument-hint: "[show | tidy]"
allowed-tools: Read, mcp__agmem__context, mcp__agmem__recall, mcp__agmem__inspect, mcp__agmem__consolidate, mcp__agmem__remember, mcp__agmem__reflect, mcp__agmem__forget, mcp__plugin_agmem_agmem__context, mcp__plugin_agmem_agmem__recall, mcp__plugin_agmem_agmem__inspect, mcp__plugin_agmem_agmem__consolidate, mcp__plugin_agmem_agmem__remember, mcp__plugin_agmem_agmem__reflect, mcp__plugin_agmem_agmem__forget
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

Run `consolidate`, then judge each list — it decides nothing itself, and empty
lists are the healthy outcome, not a failure:

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
  was scaffolding, `forget` it.

Report what you merged, corrected, and expired, in at most five lines.

Never `forget` with `purge` during any of these modes without the user
confirming first.
