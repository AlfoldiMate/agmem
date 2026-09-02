---
description: Check that ctx-flow's dependencies are installed and working — nu, ast-grep, gh, playwright-cli, rtk, agmem, acli — and that ast-grep can parse this project. Reports exact fixes for anything broken.
allowed-tools: Bash, Read, Glob
---

# ctx-flow doctor

Run the deterministic checks first and use their output as the report's spine:

```bash
nu "${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/doctor.nu"
```

It prints two tables: dependency status with a fix hint per missing row, and
the project's top source extensions with which ast-grep grammar handles each —
built in, `buildable →`, `unregistered →`, or `no grammar`.

Then add the four checks a script cannot do well:

1. **nu MCP server registered?** You can tell from your own tool list: if the
   `mcp__nu__evaluate` tool is available in this session, it is registered and
   working. If not, the fix is
   `claude mcp add nu -- nu --mcp` (add `--scope project` to keep it per-repo).
2. **agmem plugin live?** Same test, two parts. An `agmem:checkpoint` skill
   in your skill list means the plugin is installed; an agmem `context` tool
   (`mcp__agmem__context` or `mcp__plugin_agmem_agmem__context` — the name
   depends on which registration won) means the server is up. Tools without
   the skill is a hand registration and no plugin: the fix is
   `claude plugin marketplace add AlfoldiMate/agmem` then
   `claude plugin install agmem@agmem`. Skill without tools means the
   plugin's server failed to start — `agmem --doctor` says why. An `OLD`
   binary row means the plugin's hooks are silent (they are `agmem hook …`,
   which needs 0.1.9); a `STALE` row means no space derivation, so every
   project reads and writes one shared `default` space, silently.
3. **Hooks live?** Confirm `.claude/settings.json` registers the SessionStart
   (worktree layout check), PostToolUse (idiom nudge), and PreToolUse (rtk +
   worktree guard) hooks and that the nu scripts exist at the paths it names.
   The memory hooks — briefing, seams, recall log — are the plugin's, not
   this file's; a second `agmem context` in settings would brief twice. If `.claude` is a symlink, resolve it
   and confirm the target exists — a dangling symlink is the silent failure
   mode here.
4. **ast-grep actually parses the main language.** Take the top extension from
   the second table, pick one file of it, and run a trivial pattern, e.g.
   `ast-grep run -p '$A' -l <lang> <file> | head -3`. A grammar listed is not a
   grammar that loads — and a pattern with a metavariable is the sharper test,
   since a language whose syntax uses `$` needs an `expandoChar` to parse one.
   Rows reading `buildable →`, `unregistered →` or `registered, unbuilt →`
   carry their own fix, which is `/ast-grep-it <lang>`; offer it. `no grammar`
   means only that this framework has no entry for it — `/ast-grep-it` can still
   try, and the genuine dead end is a language with no tree-sitter grammar at
   all, where the fallback is `rg` plus the LSP the user's editor provides.

## Report

One line per dependency: ok, or the exact command to run. Lead with what is
broken; if everything passes, say so in two lines and stop. Do not install
anything yourself — the fixes touch global state, so present them for the user
to run. `/ast-grep-it` is the exception: it writes only to `~/.cache/ctx-flow`
and the project's `sgconfig.yml`, so run it once the user agrees. A `DOUBLED` rtk row means the hook is registered both here and in
`~/.claude/settings.json`; the fix is removing one, usually the global.
