# Plan — #136: retire `.claude/notes/`; agents write documents, checkpoint gates learnings from them

Read-only design pass, 2026-09-04, branch `feat/136-retire-notes` at 68f74e4 (#135 merged).
This is the last plan written to `.claude/notes/`; step 8 imports it as the `plan` document
the acceptance cycle's lesson cites.

## What the code actually gives us (verified)

- `agmem doc put --kind <plan|review|report|probe|transcript|other> --title <t> [--tag T]... [--mime M] [--space S]`
  reads the document on **stdin** (`doc.rs:37`) and prints exactly one line: `<ULID> memory://<space>/doc/<ULID>`
  (`doc.rs:81`). `--tag` is repeatable; `--mime` defaults to `text/markdown`. Cap: 100 000 chars
  (`remember.rs:446 MAX_EPISODE_CHARS`). Tags on a document are *any-of* filters in `list` (`read.rs:199`).
- `agmem doc list [--kind K]... [--tag T]... [--json]` → one line per doc, newest first:
  `<id>  <kind>  <chars> chars  cited <n>  <created_at>  <title>` (`doc.rs:160`); `--json` is the `inspect` docs answer.
- `agmem doc get <id|title> --raw` prints the whole document (`limit` defaults to the cap, `doc.rs:117`).
- `remember` has **no `derived_from`** (`remember.rs:48-90`); only `reflect` does, and it accepts
  `episode:<id>` refs (`reflect.rs:354`). So "a lesson citing its report" is a `reflect` call, not `remember`.
- The one-shot route attaches to the shared daemon (`doc.rs:233`) — a subagent's `agmem doc put` works
  while the session's daemon owns the store. It needs the `doc` subcommand, which is **not in v0.1.11**
  (release a2bc2ff predates #132/#134/#135) — a brew binary today answers `agmem doc` with a usage error.
- The branch slug has one rule in two places already: `hook.rs:528 slug` and `_common.nu:66 slug`;
  `ctx-flow-paths.nu` prints `TAG=branch:<slug>` (empty on detached HEAD). Subagents have no session id
  (Claude Code only hands it to hooks), so the branch tag is the only grouping key a subagent can compute.
- Briefing footer: `hook.rs:308-327` (`FOOTER` const + `branch_note`); `context.rs:render` is the block itself.
- `.claude/notes` today: 15 `.md`, 2 `.txt` eval logs, 1 `.nu` script, `LEDGER.md.imported`, empty `state/`,
  `agmem-archive/{LEDGER.md.imported, state/main.md.imported}`. Largest: `token-analysis-2026-09-03.md`
  97 162 bytes — under the 100k-char cap (chars ≤ bytes), everything else far below.
- Already ignored: `.claude/.gitignore` has `notes/`. Existing single check hook: `session-start-layout.nu`.

## APPROACH

Route every artifact through `agmem doc put` via one tiny wrapper, `.claude/scripts/doc-put.nu`, that
resolves the branch tag with the same `_common.nu` rule the hooks use, probes that the binary has `doc`,
and prints the `<id> <uri>` line — so the six agent files change one sentence each and never learn
shell quoting. `/checkpoint` lists this branch's documents, greps their trailing `LEARNED:` line, and
stores an accepted one with `reflect` + `derived_from: ["episode:<doc id>"]`. A deterministic
`import-notes.nu` moves the existing corpus in by filename rule; `.claude/notes` is then deleted and
`/ctx-flow-doctor` + the SessionStart layout hook warn if it ever holds files again. **Zero Rust in this PR.**

REJECTED: agents calling `agmem doc put --tag "$(nu ctx-flow-paths.nu ...)"` inline — an empty
expansion on a detached HEAD becomes `--tag ""` or a clap error, and a haiku runner will get the heredoc
wrong; the wrapper is 25 lines and makes the tag rule un-driftable. Also rejected: adding the
"N documents this branch" line to `hook.rs` here — it is a second daemon round trip on every SessionStart
and #151 is rewriting that footer; it goes there (see Decisions).

## Tagging convention (proposed)

Every agent-written document carries two tags, set by the wrapper, never by the agent:
- `branch:<slug>` — same rule/tag as branch state, so `agmem doc list --tag branch:<slug>` is "this branch's
  documents" and the plugin's SessionStart note already announces the tag. Omitted on a detached HEAD.
- `agent:<name>` — which role wrote it, so the gate knows which `role:<name>` playbook an accepted
  proposal joins. Deliberately **not** `role:<name>`: that tag namespace is the lesson playbook, and
  whether `recall tags:[...]` reaches episode chunks is not something I could confirm from `recall.rs`.
Title convention: `<kind>-<topic>[-<date>]`, e.g. `plan-136-retire-notes`, `report-cargo-test-2026-09-04`.
A second put under the same title is a new version (`config.rs:175`) — a re-run overwrites nothing.
"This session's" documents = the ids agents returned in-context, with `doc list --tag branch:` as the
fallback after a compaction (the `created_at` column tells today's from older).

## The agent-side call

Wrapper (new): `nu "$CLAUDE_PROJECT_DIR/.claude/scripts/doc-put.nu" <agent> <kind> <title> [--mime M]`
— content on stdin. What it runs, literally:

    agmem doc put --kind <kind> --title <title> --tag branch:<slug> --tag agent:<agent> [--mime M]

and prints what agmem printed: `01M1ABC… memory://agmem/doc/01M1ABC…` (space = this repo's, `agmem`).
Exit 2 with `agmem has no \`doc\` subcommand (needs 0.1.12+) — write to /tmp and return the path` when
`agmem doc --help` fails, so an old binary degrades loudly, not silently.

Per agent (file → kind → when):
- `architect.md` → `plan` — **always**: store the full reply as `plan-<name>` before returning; first line
  of the reply becomes `DOC: <id> <uri>`; the ≤60-line return contract is otherwise unchanged. Heredoc,
  not Write (the agent has no Write tool — memory 01M1HNF20GDECBRCX8PG69BJXR).
- `runner.md` → `report` — only when the full log matters (as today); title `report-<command>-<date>`.
- `browser.md` → `report` — snapshots/DOM/console logs; `ARTIFACTS:` becomes `DOC: <id> <uri>`; a
  non-markdown blob gets `--mime text/plain`.
- `scout.md`, `tracker.md` → `report` — when over the 8-hit / 20-line cap.
- `verifier.md` → `review` — when the evidence exceeds the 3-line contract (new, one sentence).
- All six: if the run ends with a `LEARNED:` line, the **document ends with the same line** under a
  `## Learned` heading, so the proposal survives the reply scrolling out or compacting.

## How the checkpoint gate reads proposals (`.claude/commands/checkpoint.md` §2)

1. Tag: the `branch:<slug>` the plugin announced at session start, else `nu …/ctx-flow-paths.nu --tag`.
2. `agmem doc list --tag <tag>` (Bash; allowed already) — keep rows created this session (ids the agents
   returned; `created_at` for the post-compaction case).
3. Per document: `agmem doc get <id> --raw | rg '^LEARNED:'` — the doc stays in the shell, only the
   proposal line reaches the thread. Union with `LEARNED:` lines still in context (an agent that wrote no
   document proposes only in its reply).
4. Four tests as now. Accepted **with a document** → `reflect { insight, kind: "lesson",
   tags: ["role:<agent>"], derived_from: ["episode:<doc id>"] }` (the `agent:` tag on the doc names the role).
   Accepted **without one** → `remember` as today. `recall` the `role:` tag first; contradiction → `supersedes`.
5. Report: "N proposals seen (M from documents), K kept" and the doc ids.
§3 "Legacy files" loses the literal path: "if `/ctx-flow-doctor`'s `notes` row is not ok, run `/agmem-import`".

## Import design (`.claude/scripts/import-notes.nu`, run by `/agmem-import`)

Deterministic — no distillation, so a script, not a prompt. `--dry-run` prints the table first.
- Dir: `paths.notes` from `_common.nu` (the shared root's `.claude/notes`). Files only, skip `*.imported`,
  skip anything over 100 000 chars with a printed reason.
- Kind from the name, first rule wins: prefix `plan-` → plan; `review-` → review; `research-` → report
  (#154's wording); suffix `-plan` → plan (`embedder-required-plan.md`); name contains `audit` or
  `analysis` → review (`framework-audit-…`, `plugin-audit-…`, `token-analysis-…`); `.txt` → probe with
  `--mime text/plain` (the two `82-desc-eval*` logs); otherwise `other` (`checkpoint-2026-09-02-takeover`,
  `issue-retire-race-2026-09-02`, `phase8-issues`).
- Title = file stem. Tags: `legacy-notes` (so the batch is one `doc list --tag legacy-notes`), no branch tag.
- On success rename `<file>` → `<file>.imported` (the existing ledger convention: idempotent, reversible).
  Print `file  kind  chars  id  uri` per row.
- Not imported: `LEDGER.md.imported`, `agmem-archive/`, `state/` — their claims are already in the store
  with the raw text as episodes (`agmem-import.md` step 2), and `state/` is empty. `rm -r` them.
- `token-analysis-2026-09-03.nu`: not a document — a probe script. Move to `docs/eval/token-analysis.nu`
  and commit (the repo's convention for probes and their scripts, per the role:architect lesson on
  `docs/eval/`). It writes its `.md` next to itself; adjust the one path line or leave it as a
  dry-run-only artifact — user's call (Decision 4).
- The three 2026-09-03 audits (#154): import them as `review` **now** — `agmem doc get
  token-analysis-2026-09-03 --raw` is the new path and it survives `rm -r .claude/notes`. Leaving three
  files behind keeps the directory alive and the doctor warning permanent. Then one comment on #154 with
  the path → id map; optionally `gh issue edit` the `Source:` line on #145–#153 (Decision 3).
- Last step of `/agmem-import`: tell the user to `rm -r .claude/notes` — deletion stays the user's action.

## BUILD SEQUENCE (each step leaves the tree building; steps 1–3 are nu, the rest markdown)

1. `.claude/hooks/scripts/_common.nu` — add `notes: ($root | path join $NOTES_REL)` to `paths`; add
   `export def notes-check [root] -> any` (warning text when the dir holds files, else null; `.imported`
   files count too — the dir is meant to be gone); reword the comments at 75, 97–102, 110–113 to
   "profiles" only, dropping "artifact dropbox". `NOTES_REL` stays: the single literal both the import
   script and the doctor resolve through.
2. `.claude/hooks/scripts/ctx-flow-paths.nu` — `--tag` flag printing only the tag (nothing on detached);
   `NOTES=` appears automatically since keys print from the record. New: `.claude/scripts/doc-put.nu`
   as specified above (`use _common.nu [paths]`; `^agmem doc --help | complete` probe; `^agmem doc put`).
3. `.claude/scripts/import-notes.nu` — new, as specified; `.claude/scripts/doctor.nu` — `agmem-row`
   probes `agmem doc --help` the way it probes `agmem hook --help` (status `OLD`, fix `brew upgrade agmem
   — without \`agmem doc\` (0.1.12+) subagents cannot write documents`); new `notes-row` from
   `notes-check`; `.claude/hooks/scripts/session-start-layout.nu` — also emit `notes-check` (it is the
   framework's one "about our files" hook, and this is what makes a returning dropbox non-silent).
4. `.claude/agents/{architect,runner,browser,scout,tracker,verifier}.md` — the one-sentence swap per
   agent above; return contracts say `DOC: <id> <uri>` where they said "path"; `## Learned` gains
   "the same line closes the document, if you wrote one".
5. `.claude/docs/reference.md` — template line 35 → "Anything longer than N lines → `nu
   .claude/scripts/doc-put.nu <agent> <kind> <title>` and return `DOC: <id> <uri>`"; lines 96–99 →
   documents, with the kind vocabulary and the two tags; the slug paragraph (line 88) now also names
   `doc-put.nu` as a consumer. `.claude/CLAUDE.md:148` → "Subagents write long output as agmem documents
   and return the id + `memory://` URI — the store holds claims and the artifacts they cite"; Toolbox
   agmem line: "needs ≥ v0.1.12 (`agmem doc`)". `.claude/README.md` 300–302, 326, 361, 371 → same
   story; layout tree drops the `notes/` leaf and adds `scripts/doc-put.nu`, `scripts/import-notes.nu`.
6. `.claude/commands/checkpoint.md` — §2 as specified; §3 as specified; `allowed-tools` unchanged (Bash
   is already unrestricted there). `.claude/commands/agmem-import.md` — new first step "documents:
   `nu …/import-notes.nu --dry-run`, show the table, then run it"; ledger/state steps unchanged but
   reached through `NOTES=`/`LEDGER=` from `ctx-flow-paths.nu`, no literal path; closing line about
   `rm -r`.
7. `docs/design.md:1260` — "moves off its `.claude/notes/` dropbox" → "moves off its notes dropbox
   (this file's documents)" so the acceptance `rg` is clean; `.claude/.gitignore` comment → "retired
   dropbox — anything landing here is invisible to memory; /ctx-flow-doctor flags it" (keep `notes/`
   ignored: a regressed agent must not be able to commit blobs). Check `plugin/` needs nothing: the
   plugin's checkpoint and doctor are generic (see Decision 2).
8. Run it: `nu .claude/scripts/import-notes.nu --dry-run`, then for real (this plan imports as `plan`);
   `mv .claude/notes/token-analysis-2026-09-03.nu docs/eval/`; `rm -r .claude/notes`; `nu
   .claude/scripts/doctor.nu` shows `notes ok`; comment on #154; acceptance `rg -n '\.claude/notes'
   .claude plugin docs` → `_common.nu` (NOTES_REL) only.

## Acceptance check (fresh session)

architect dispatch → reply starts `DOC: <id> memory://agmem/doc/<id>` → implement → `/checkpoint` →
`agmem doc list --tag branch:<slug>` shows the plan → a kept `LEARNED:` lands via `reflect` → `inspect
<lesson id>` shows `derived_from: [episode:<id>]`; `agmem doc list` shows `cited 1` on the plan.

## RISKS

- **Binary too old.** brew `agmem` is 0.1.11 without `doc`; every agent write fails until v0.1.12 ships.
  Tell early: `doctor.nu`'s new probe; the wrapper's exit 2 message. Merge after the release, or accept
  that this branch only works with a `cargo install --path` binary until then.
- **Old daemon, new CLI.** A 0.1.12 CLI attaching to a still-running 0.1.11 daemon gets `remember`
  without `doc_kind` → "remember answered without an episode id; agmem versions disagree?" (`doc.rs:77`).
  Tell early: the same message; fix is letting the daemon retire (release takeover) — one-time.
- **Documents leaking into `recall`.** Plan chunks now compete with claims in the Relevant section and in
  `recall tags:["branch:…"]`; #137 measures this and has the three one-line mitigations. Tell early:
  a branch-resume recall whose top hits are plan slices.
- **Tag drift.** `hook.rs slug` vs `_common.nu slug` are two implementations of one rule; the wrapper
  adds a third *consumer*, not a third implementation — fine as long as it goes through `paths`.
- **Cap.** A future runner log over 100k chars is refused; the wrapper should say so and fall back to
  `/tmp/<title>.log` + path, not truncate silently.
- **Losing the audits' addressability.** Nine open issues cite `.claude/notes/token-analysis-…` by path;
  after step 8 those paths are dead unless the #154 comment / body edits land.

## DECISIONS for the user

1. **Release first?** #136 says it depends on #135 shipping. Cut v0.1.12 before merging (recommended), or
   merge and let `doctor.nu`'s `OLD` row + the wrapper's exit 2 carry the gap.
2. **`agmem --doctor` warning on `.claude/notes`.** Recommended: **no** — the binary is generic (the issue's
   own note), and a ctx-flow path check belongs in `/ctx-flow-doctor` + the SessionStart hook (step 3).
   Alternatives: an inline `!ls .claude/notes` line in `plugin/commands/doctor.md` (Claude-Code-specific,
   still no Rust), or a `--doctor` line in `doctor.rs`.
3. **The three audits and the nine issues.** Recommended: import now as `review`, one comment on #154
   with the path → `agmem doc get <title>` map, and edit the `Source:` line in #145–#153 (9 × `gh issue
   edit`). Alternative: leave the three files until Phase 11 closes (#154's original wording) — costs a
   permanent doctor warning and a lingering directory.
4. **`token-analysis-2026-09-03.nu`.** Recommended: commit to `docs/eval/token-analysis.nu` (probe
   scripts live there). Alternative: import as a `probe` document with `--mime text/x-nushell`.
5. **Briefing line "N documents this branch".** Recommended: split into #151 (footer rewrite, payload
   audit) — a `doc::list` call from `hook.rs::session_start` is ~15 lines but is a second daemon session
   on every start. Alternative: do it here as the only Rust change.
6. **Wrapper vs raw CLI in agent files.** Recommended: the wrapper (`doc-put.nu`) for the detached-HEAD
   and version-probe cases. Alternative: raw `agmem doc put` with the tag inline, accepting those edges.

## UNKNOWNS

- Whether `recall`'s episode lane honours the `tags` filter (decides if `agent:` vs `role:` matters at all).
- Exact `created_at` format in `doc list` lines (RFC3339 assumed) — affects only the post-compaction filter.
- Whether the ctx-flow `.claude/` is mirrored to a separate context-flow repo that needs the same PR.
