# Nushell language

Nushell 0.114.1.

## Types

| Type | Literal | Notes |
|---|---|---|
| `int` | `42`, `0x1F`, `0b1010`, `0o755`, `1_000_000` | 64-bit signed |
| `float` | `3.14`, `1e-3`, `inf`, `NaN` | |
| `string` | `"a"`, `'a'`, `` `a` ``, `r#'raw'#` | see below |
| `bool` | `true`, `false` | |
| `nothing` | `null` | absence; `$x?` yields it |
| `duration` | `1ns 1us 1ms 1sec 1min 1hr 1day 1wk` | real arithmetic type |
| `filesize` | `1b 1kb 1mb 1gb 1kib 1mib` | `kb`=1000, `kib`=1024 |
| `datetime` | `2026-07-31`, `2026-07-31T12:00:00+02:00` | |
| `range` | `1..10`, `1..<10`, `1..`, `0..2..10` | `..<` excludes end |
| `binary` | `0x[BE EF]`, `0b[1010]` | |
| `list` | `[1 2 3]`, `[1, 2, 3]` | commas optional |
| `record` | `{a: 1, b: 2}` | |
| `table` | `[[a b]; [1 2] [3 4]]` | a list of records |
| `closure` | `{\|x\| $x + 1 }` | |
| `cell-path` | `$.a.b.0` | first-class value |
| `glob` | `("*.nu" \| into glob)` | distinct from string |
| `semver` | `("1.2.3" \| into semver)` | comparable |

`describe` reports the type; `describe --detailed` gives structure.

### Strings

```nu
"double"          # escapes (\n, \t, \u{1F600}) and interpolation with $"..."
'single'          # fully literal, no escapes
`backtick`        # bare-word-ish; used for paths with spaces
r#'raw string'#   # no escapes at all, for regex and Windows paths
$"($x) items"     # interpolation — parens hold an expression
$'($x) items'     # interpolation without escape processing
```

**Interpolation trap:** inside `$"..."`, `(` opens an expression. Literal
parentheses must be escaped `\(` or the text will be parsed as a command call.
`$"(version).version"` evaluates `version` then appends literal `.version` —
use `$"((version).version)"`.

Multi-line strings are just literals containing newlines. There is no heredoc.

## Operators

Arithmetic `+ - * / // mod **` · Concatenate `++` · Compare `== != < > <= >=`
Regex `=~` (alias `like`) `!~` (alias `not-like`)
Membership `in` `not-in` `has` `not-has`
String `starts-with` `not-starts-with` `ends-with` `not-ends-with`
Logic `and` `or` `xor` `not`
Bitwise `bit-and` `bit-or` `bit-xor` `bit-shl` `bit-shr`
Assign `= += -= *= /= ++=`

`help operators` prints the table with precedence.

`++` concatenates lists/strings/binary. `+` on two lists is an error — this
differs from many languages.

```nu
[1 2] ++ [3]        # [1 2 3]
"a" ++ "b"          # "ab"
$env.PATH ++= ["~/bin"]
```

## Variables

```nu
let x = 5              # immutable (the default; prefer it)
mut y = 5              # mutable
$y += 1
const Z = 5            # parse-time constant — usable by `use`/`source`
```

`const` matters: only constants can be arguments to parse-time keywords.

**Mutable variables cannot cross a closure boundary.** This is a hard rule, not
a lint:

```nu
mut total = 0
[1 2 3] | each {|n| $total += $n }   # ERROR: capture of mutable variable
[1 2 3] | reduce {|it, acc| $acc + $it }   # do this instead
[1 2 3] | math sum                          # or this
```

Scoping is lexical and block-based. `$env` changes are scoped to the block
unless the command is declared `def --env`.

## `$in` and pipelines

`$in` is the pipeline input to the current expression.

```nu
ls | $in | length              # explicit
[1 2 3] | each {|x| $x * 2 }   # closure param
[1 2 3] | each { $in * 2 }     # $in inside a closure = current item
def double []: int -> int { $in * 2 }   # $in in a command = pipeline input
```

`$in` is consumed once; bind it if used twice:

```nu
def f [] { let x = $in; $"($x) and ($x)" }
```

## Control flow

Everything is an expression and returns a value.

```nu
let s = if $n > 10 { "big" } else if $n > 5 { "mid" } else { "small" }

match $x {
  0 => "zero",
  1..5 => "small",
  {name: $n} => $"record with ($n)",     # destructuring
  [$a, $b] => $"pair ($a) ($b)",
  $o if $o > 100 => "huge",              # guard
  _ => "other",
}

for i in 1..3 { print $i }     # statement; cannot produce a value
while $c < 5 { $c += 1 }
loop { if $done { break } }
```

Prefer `each`/`where`/`reduce` over `for`: they stream, compose, and return
values. `for` exists for side effects.

## Custom commands

```nu
# Documentation comment becomes `help greet`.
def greet [
  name: string              # positional, typed
  --greeting (-g): string = "Hello"   # flag with short form and default
  --loud                    # boolean switch
  ...rest: string           # rest args
]: nothing -> string {      # input -> output type signature
  let msg = $"($greeting), ($name)!"
  if $loud { $msg | str upcase } else { $msg }
}
```

- A `--flag` named `--dry-run` is read as `$dry_run`.
- Type signature `input -> output` is checked; use `nothing -> ...` when the
  command takes no pipeline input. Multiple signatures: `[int -> string, list -> table]`.
- `def --env` lets the command mutate the caller's environment.
- `def --wrapped` passes unknown flags through to an external.
- Subcommands are names with spaces: `def "git cleanup" [] { ... }`.
- `export def` makes it a module member.

Shadowing a built-in with `def` of the same name recurses. Back it up with
`alias original = cmd` **first**, or call the built-in via `%cmd`.

## Errors

```nu
try { risky } catch {|e| print $e.msg }

error make {
  msg: "invalid input"
  label: { text: "here", span: (metadata $x).span }
  help: "try a positive number"
}
```

External commands do not raise by default — check `$env.LAST_EXIT_CODE`, or:

```nu
let r = (^cmd arg | complete)   # {stdout, stderr, exit_code}
if $r.exit_code != 0 { ... }
```

## Externals and redirection

```nu
^git status              # force external even if a Nu `git` exists
git status               # external if no Nu command shadows it
%ls                      # force the built-in even if shadowed

cmd o> out.txt           # stdout to file
cmd e> err.txt           # stderr to file
cmd o+e>| next           # merge both into a pipe
cmd e>| next             # stderr into a pipe
cmd | ignore             # discard output
```

`$env.PATH` is a list inside Nushell and is converted back to a string when an
external is invoked. Env values that are not strings need an `ENV_CONVERSIONS`
entry or the external will not see them correctly.

## Scripts

```nu
#!/usr/bin/env nu

# Called with the script's CLI arguments.
def main [name: string, --verbose] {
  print $"hi ($name)"
}

# Subcommands: `script.nu build --release`
def "main build" [--release] { ... }
```

`main` receives argv. Anything defined but not called still needs `main` to
produce behaviour.

**A script does not load user config** (see `config.md`). No aliases, no
modules, no hooks. Import what you need explicitly.

Useful inside scripts:

- `$env.CURRENT_FILE` — this file's path (also `path self`)
- `$env.FILE_PWD` — its directory
- `$env.PROCESS_PATH` — how the script was invoked

## Parallelism

```nu
ls | par-each {|f| expensive $f }        # order NOT preserved
ls | par-each --threads 4 {|f| ... }
```

Use `par-each` only for genuinely independent, non-trivial work; thread setup
costs more than it saves on small inputs. Reorder afterwards with `sort-by` if
order matters.

Background jobs: `job spawn { ... }`, `job list`, `job kill <id>`.
