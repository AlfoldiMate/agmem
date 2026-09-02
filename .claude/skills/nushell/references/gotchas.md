# Gotchas, errors, and version drift

Nushell 0.114.1.

## Parse time vs run time

The root cause of most confusing Nushell behaviour.

Nushell **parses a whole file before evaluating any of it.** These are
*parse-time keywords*, resolved during that pass:

`use` · `source` · `source-env` · `overlay use` · `module` · `const` ·
`export` · `plugin use` · `def` · `extern` · `alias` · `hide`

Their arguments must be knowable at parse time.

```nu
# ✅ $nu.* is constant
source ($nu.default-config-dir | path join 'x.nu')

# ❌ runtime variable
let p = "x.nu"
source $p
# Error: nu::parser::parse_mismatch — "expected constant"
```

Consequences that look like bugs:

```nu
# ❌ `plugin use` resolves before `plugin add` runs
plugin add ./nu_plugin_foo
plugin use foo
# → use `nu --plugins '[./nu_plugin_foo]'` instead

# ❌ a module's export-env is only evaluated when `use` is EVALUATED,
#    and a top-level `use` inside another module is only PARSED
use my-utils
export def --env go [] { cd $env.FROM_MY_UTILS }   # not found at runtime

# ❌ conditional import — the `use` runs at parse time regardless
if $cond { use a.nu } else { use b.nu }
```

`const` participates in parsing, which is why `const NU_LIB_DIRS` in `config.nu`
is visible to files parsed afterwards.

## Frequent errors

**`Cannot find column`** — a missing record key. Use `?`:
`$env.FOO?`, `$data | get x?.y?`

**`capture of mutable variable`** — a `mut` crossed a closure boundary. Use
`reduce`, or a built-in aggregator:

```nu
mut n = 0; [1 2 3] | each {|x| $n += $x }   # ❌
[1 2 3] | reduce --fold 0 {|it, acc| $acc + $it }   # ✅
```

**`nu::shell::recursion_limit_reached`** — a `def` shadowing a built-in calls
itself. Back it up with `alias original = cmd` first, or call `%cmd`.

**`Command 'x' not found` inside a string** — a bare `(` in `$"..."` opens an
expression. Escape it `\(` or restructure.
`$"(version).version"` → `$"((version).version)"`

**`command not found` for something on PATH** — the parser decided the token was
not a command. Force it with `^name`.

**External didn't fail** — externals return exit codes, not Nu errors. Use
`^cmd | complete` and check `.exit_code`, or `$env.LAST_EXIT_CODE`.

**Alias won't take a pipeline** — aliases are textual substitution. Use `def`:

```nu
alias u = uuidgen | tr A-F a-f      # ❌
def u [] { ^uuidgen | tr A-F a-f }  # ✅
```

**Config change had no effect** — either it was set in a script/`-c` (which load
no config), or a whole record was assigned and reset its siblings, or it is a
startup-only key (`history.*`).

**`$env` change vanished** — env is block-scoped. The command needs `def --env`.

**macOS: `open` doesn't open anything** — Nushell's `open` parses files into
data; it does not launch a file the way `/usr/bin/open` does. The built-in
`start <path>` does that. Aliasing over `open` is the tempting fix and the wrong
one — see below:

```nu
alias open = ^open   # don't
```

### Aliases leak into modules parsed after them

Aliasing a built-in has a sharp edge. **A module inherits whatever names were in
scope when it was parsed** — so a module `use`d from a file that runs *after*
`alias open = ^open` will resolve a bare `open` to `/usr/bin/open`, not the
built-in. The failure is remote from its cause and reads as nonsense:

```
open: unrecognized option `--raw'
```

Inside a module, force the built-in with the `%` sigil, which bypasses both
alias and `def` shadowing:

```nu
%open --raw $path
```

This class of bug hides from testing in a specific way worth knowing: a script
run as `nu script.nu`, and `nu -c`, **loads no user config**, so the alias does
not exist and the code works. It only fails where autoload *does* run: an
interactive session, and `nu --mcp`. The MCP server is the nastiest of the
three, because it is exactly where you reach for `open f | from json`, and the
error it returns ("External command had a non-zero exit code") never names the
alias. Test module code both ways.

Names worth defending in shared modules: `open`, `ls`, `rm`, `cp`, `mv`,
`cd` — all commonly aliased, all built-ins.

## Performance

- `use std/<sub>`, never `use std` or `use std *`.
- `nu -n --no-std-lib -c '...'` for scripted invocations in a loop.
- `par-each` only for genuinely parallel, non-trivial work — thread setup is not
  free, and it does not preserve order.
- `$env.config.completions.external.enable = false` if PATH spans a slow
  network filesystem.
- `highlight_resolved_externals = true` costs a PATH lookup per token.
- Profile with `$nu.startup-time`, `timeit { ... }`, `debug profile { ... }`,
  and `view files`.

## Version drift

Nushell makes breaking changes at **minor** versions. Snippets found online are
frequently stale. Known changes worth recognising:

| Change | Since |
|---|---|
| `let-env FOO = x` → `$env.FOO = x` | 0.83 |
| `build-string` removed → string interpolation | 0.83 |
| `str collect` → `str join` | 0.79 |
| `fetch` / `post` → `http get` / `http post` | 0.80 |
| `register` → `plugin add` + `plugin use` | 0.93 |
| `$env.config = { ... }` monolith → per-key assignment | 0.87+ |
| `env.nu` / `config.nu` no longer generated with full defaults | 0.101 |
| `$env.NU_LIB_DIRS` deprecated → `const NU_LIB_DIRS` | 0.101 |
| `date now` formatting → `format date` | 0.60s |
| `into decimal` → `into float` | 0.83 |
| `--numbered` flags removed → `enumerate` | 0.80s |
| `size` → `str stats` | 0.90s |
| `std/iters` documented but module is `std/iter` | as of 0.114.1 |

**When unsure, ask the binary:**

```nu
version
help <command>              # signature + input/output types
scope commands | where name =~ 'pattern'
config nu --doc | find <key>
```

Release notes — always worth skimming after an upgrade —
<https://www.nushell.sh/blog/>.

## Nushell is not POSIX

Things that simply do not exist:

- `eval` of shell strings. Tool integrations must ship `.nu` files.
- `&&` / `||` — Nushell aborts on error, so `;` is the sequencer.
  (`and`/`or` are boolean operators, not command chaining.)
- Word splitting and glob expansion of variables. A variable holding
  `"a b"` is one argument, always. This removes a whole class of quoting bugs.
- `$?` → `$env.LAST_EXIT_CODE`
- Heredocs → multi-line string literals
- `set -e` → the default
- Subshell `$(...)` → `(...)`
- Backreferences and lookaround in regex — Nushell uses Rust's `regex` crate.

## Debugging toolkit

```nu
nu --ide-complete <pos> f.nu  # what would complete here — no pty needed
nu --ide-check 100 f.nu       # diagnostics
nu --ide-ast f.nu             # parse tree
$x | describe               # type
$x | describe --detailed    # structure
... | inspect | ...         # peek mid-pipeline, pass through
ast "1 + 1"                 # parse tree
view source <command>       # source of a custom command
view files                  # everything parsed this session
view span $start $end
debug profile { ... }       # per-element timing
timeit { ... }
metadata $x                 # provenance and span
scope commands / scope modules / scope variables / scope aliases
nu-check file.nu            # syntax check without running
```

### `nu-check` and `NU_LIB_DIRS`

`nu-check` runs without your config, so a file containing `use foo.nu` or
`source foo.nu` that relies on `NU_LIB_DIRS` reports `false` even when it is
perfectly valid. Observed on 0.114.1: passing `-I <dir>` fixes `source` but not
`use` in default (script) mode; `nu-check --as-module` resolves it.

```nu
nu -n -I ~/.nu/modules -c "nu-check --as-module ~/.nu/autoload/80-modules.nu"
```

So a `false` from `nu-check` on a config file is not by itself evidence of a
syntax error. Confirm by actually starting a shell.
