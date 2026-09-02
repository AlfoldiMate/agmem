---
name: nushell
description: Use when writing, debugging, or configuring Nushell (`nu`) — .nu scripts, modules, plugins, custom completions, `config.nu`/`env.nu`, `$env.config` settings, themes, Reedline keybindings and menus, overlays, hooks, the standard library, or the built-in MCP server (`nu --mcp`); also when converting bash/zsh/PowerShell to Nu, working with Nu's structured-data pipelines (tables, records, lists, cell-paths), or troubleshooting parse-time vs run-time errors. Covers this machine's config at ~/.nu.
---

# Nushell

Nushell is a **structured-data shell with a statically-parsed, typed language**.
It is not POSIX. Almost every mistake made by someone fluent in bash comes from
assuming otherwise.

Verified against **Nushell 0.114.1**. Nushell makes breaking changes at minor
versions — see `references/gotchas.md` § *Version drift* before trusting any
snippet found online.

## Operating rules

1. **Verify against the running binary, not memory.** Nushell's surface changes
   fast. `help <command>`, `config nu --doc`, and `scope commands` are ground
   truth. Signatures shown by `help` include the input/output type table, which
   is usually the fastest way to resolve an error.
2. **Never edit `~/Library/Application Support/nushell` directly** on this
   machine — it is a symlink to `~/.nu`. See `references/this-setup.md`.
3. **Check parse time vs run time first** when a construct "should work" but
   doesn't. This causes more Nushell confusion than everything else combined.
   `references/gotchas.md` § *Parse time* covers it.
4. **Prefer structured output over text.** If a solution reaches for `awk`,
   `sed`, `cut`, or `grep | tr`, there is nearly always a native pipeline that
   keeps the data structured. Text-munging is the bash reflex to unlearn.
5. **Test snippets before delivering them.** `nu -n -c '<code>'` runs in
   isolation with no user config. Being able to run the code is the whole point
   of having the shell installed.
6. **Run Nu through the `nu` MCP server when one is configured**, not through a
   text shell. Results stay structured and stay addressable by
   `history_index`, so a big result is re-sliced rather than re-run — which is
   why capping output inside a first-run pipeline is the wrong reflex there.
   See `references/mcp.md`.

## Orientation

```nu
help <cmd>                 # signature + input/output types + examples
help commands | where category == filters
help operators             # every operator, with precedence
config nu --doc            # every $env.config key, documented inline
$nu                        # all derived paths and runtime facts
scope commands             # everything in scope, incl. modules and plugins
$env.config | table -e     # current settings, expanded
```

Three orthogonal ideas carry most of the language:

- **Everything is a value with a type.** `ls` returns a `table`, not text.
  `describe` tells you what you have; `into <type>` converts.
- **Pipelines pass values, not bytes.** A pipeline element receives `$in`.
  Externals are the boundary where values become strings.
- **Parsing precedes evaluation.** `use`, `source`, `const`, `overlay use`, and
  `plugin use` are resolved before any code runs.

## Reference files

Read the one that matches the task; they are self-contained.

| File | Covers |
|---|---|
| `references/language.md` | Types, operators, variables, closures, control flow, custom commands, error handling, scripts |
| `references/data.md` | Tables/records/lists, cell-paths, filters, strings, `parse`, format conversion, the bash→Nu translation table |
| `references/config.md` | Startup sequence, `$env.config` key-by-key, environment variables, hooks |
| `references/modules.md` | Modules, submodules, `export-env`, overlays, `NU_LIB_DIRS` |
| `references/plugins.md` | Installing, registering, the registry, GC, writing plugins |
| `references/completions.md` | Custom completers, `extern`, external completers (carapace), completion options |
| `references/interface.md` | Themes, `color_config`, shapes, Reedline menus and keybindings, prompts |
| `references/stdlib.md` | `std` submodules and correct import forms |
| `references/gotchas.md` | Parse time, common errors, performance, version drift |
| `references/this-setup.md` | **This machine**: `~/.nu` layout, symlink, conventions |
| `references/mcp.md` | The built-in MCP server: the three tools, `$history`, session state, background jobs |
| `references/ecosystem.md` | `nu_scripts`, `awesome-nu`, nupm, notable plugins |

## Fast answers

**Run something without the user's config:** `nu -n -c '...'`
**Time startup:** `nu -c '$nu.startup-time'`
**Convert a value:** `$x | into int`, `| into datetime`, `| into filesize`
**Inspect mid-pipeline:** `... | inspect | ...`
**Call an external explicitly:** `^git` (bypasses any Nu command of that name)
**Call a shadowed built-in:** `%ls`
**Capture an external fully:** `^cmd | complete` → `{stdout, stderr, exit_code}`
**Escape a value into a string:** `$x | to nuon`
**Re-slice a prior MCP result:** `$history.<index>` — the index from that response
