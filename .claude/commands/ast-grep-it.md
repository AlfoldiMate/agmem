---
description: Teach ast-grep a language it does not ship — builds the tree-sitter grammar, registers it in sgconfig.yml, and verifies it against real files in this project.
argument-hint: [language] [--repo <url>] [--subdir <dir>] [--expando <char>]
allowed-tools: Bash, Read, Glob, AskUserQuestion
disable-model-invocation: true
---

# ast-grep it

Make `ast-grep -l $ARGUMENTS` work in this project. ast-grep loads any
tree-sitter grammar from a dynamic library, so "not supported" is usually a
missing binary rather than a missing capability.

Target language and any flags: **$ARGUMENTS**.

**With no argument, just run it** — the script picks this project's biggest
language that ast-grep cannot yet handle and sets it up:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/build-grammar.nu"
```

It ends in one of three states, all of them final: it built something, or every
language here is already handled (it prints the table — report that and stop),
or the languages left have no registry entry, in which case it names them and
you continue from step 2 with the biggest one. `--list` prints the registry
alone without touching the project.

Everything below is for when a language *was* named.

## 1. Is it already handled?

Cheapest first, and stop at the first yes:

```bash
printf '' > /tmp/agit.<ext> && ast-grep run -p '$A' -l <lang> /tmp/agit.<ext>
```

Exit 0 or 1 means ast-grep already parses it — **stop and say so**. `invalid
value ... is not supported` means it does not. Note this checks the *language
name*: if the language works but your files use an odd extension, the fix is
`languageGlobs` in `sgconfig.yml`, not a grammar build.

## 2. Find the grammar

If the language is in the registry (`--list` above), skip straight to step 3 —
the repo is already known and verified.

Otherwise find it, preferring the `tree-sitter` and `tree-sitter-grammars` orgs,
which are the maintained homes for most grammars:

```bash
gh api -X GET search/repositories -f q='tree-sitter-<lang> in:name' \
  --jq '.items[] | "\(.full_name)  \(.stargazers_count)★  \(.description // "")"'
```

Take the user's `--repo` over anything you find. When two candidates are close,
**ask with `AskUserQuestion`** rather than guessing — a fork can be years stale,
and the wrong grammar fails as a confusing pattern error much later. If the repo
is a monorepo whose `grammar.js` sits one level down (as
`tree-sitter-typescript` does), pass `--subdir`.

## 3. Build and register

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/build-grammar.nu" <lang> --repo <url> [--subdir <dir>]
```

It clones to `~/.cache/ctx-flow/grammars`, compiles, and merges one entry into
the project's `sgconfig.yml` — creating it if absent, and printing the snippet
to paste rather than reformatting a config that has comments in it. Registry
languages need no flags at all.

Read what it prints. If it reports that `` `$A` does not parse ``, the language
gives `$` its own meaning and needs an expando char — re-run with
`--expando _ --force`. This is the single most common reason a correctly built
grammar still looks broken.

## 4. Verify against this project, not a toy

The script proves `$A` resolves. That is not the same as the grammar being
useful, so confirm on real code — pick an actual file of that language and run
both shapes, since they fail for different reasons:

```bash
ast-grep run --kind <node-kind> -l <lang> <real file>   # structure
ast-grep run -p '<a real call in that file>' -l <lang> <real file>   # metavariables
```

`ast-grep run -p '$A' -l <lang> --debug-query=ast <file>` dumps node kinds when
you need to find the right `--kind`. A pattern that fails *after* `$A` works is
a pattern-shape problem, not a setup problem — say which one it is rather than
re-running the build.

## 5. Land what you learned

For a language not already in the registry, the script prints a `LEARNED:` line.
Propose it for `.claude/scripts/grammars.nu` — `/checkpoint` decides whether it
lands, same gate as any proposed learning. Include the expando char and any
`--subdir`, because those are the two facts nobody rediscovers cheaply.

## Report

Lead with the verdict: the language now works, was already supported, or has no
usable grammar. Then, in at most three lines: the repo used, the `sgconfig.yml`
entry, and one command the user can paste to see it working on their own code.
If a pattern shape failed in step 4, name it as a pattern limit and move on —
do not present it as a broken install.
