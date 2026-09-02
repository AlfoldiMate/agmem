# Completions and externs

Nushell 0.114.1.

## Custom completions

Two steps: define a command that returns candidates, then attach it to an
argument's type with `@`.

```nu
def animals [] { ["cat" "dog" "eel"] }
def my-command [animal: string@animals] { print $animal }
```

`string@animals` gives the parser both the **type** (for checking) and the
**completer** (for suggestions).

- Return `[]` to suppress completion for an argument.
- Return `null` to fall back to Nushell's file completion.

### Records for descriptions and styling

```nu
def commits [] {
  [
    { value: "5c2464", description: "Add .gitignore", style: red }
    { value: "f3a377", description: "Initial commit",
      style: { fg: green, bg: "#66078c", attr: ub } }
  ]
}
```

Only `value` is inserted; `description` and `style` are display only.

### Per-completer options

Return a record with `completions` and `options` to override global settings:

```nu
def animals [] {
  {
    options: {
      case_sensitive: false
      completion_algorithm: substring    # prefix | substring | fuzzy
      sort: false                        # keep your order (e.g. by date)
      match_description: true            # also match against descriptions
    }
    completions: [cat rat bat]
  }
}
```

`match_description` is what lets you type a person's name and complete to their
opaque email address.

### Context-aware

The completer can accept the command line typed so far, and the cursor position:

```nu
def animal-names [context: string] {
  match ($context | split words | last) {
    cat => ["Missy" "Phoebe"]
    dog => ["Lulu" "Enzo"]
    _ => []
  }
}
def my-command [animal: string@animals, name: string@animal-names] { }

def completer [context: string, position: int] { }   # both, if needed
```

### Put completers in modules

Completers are implementation detail; don't export them.

```nu
module commands {
  def animals [] { ["cat" "dog"] }        # private
  export def my-command [a: string@animals] { }
}
```

Completers attach at **parse time**, so changing a completer requires reparsing
the command that uses it. Re-running `use` does both at once — which is the real
reason modules are the recommended home for them.

## `extern` — signatures for external commands

Declares an external's interface so Nushell can type-check, highlight, and
complete it.

```nu
module "ssh extern" {
  def none [] { [] }
  def identities [] {
    ls ~/.ssh/id_* | where {|f| ($f.name | path parse | get extension) != "pub" } | get name
  }

  export extern ssh [
    destination?: string@none    # Destination host
    -p: int                      # Port
    -i: string@identities        # Identity file
  ]
}
use "ssh extern" ssh
```

`?` marks an optional positional; `...name: type` marks rest args. A trailing
comment becomes the flag's description — **it needs a space before the `#`**.

Without a completer returning `[]`, Nushell falls back to file completion for
that argument, which is usually wrong for things like hostnames.

### Limits

- Cannot express required flag/positional **ordering**.
- Cannot require `=` between a flag and its value.
- Cannot express single-dash long flags (`-long`).
- Does not apply to externals called with `^` (`^ssh`).

## External completers

A global fallback closure, used when Nushell has no completion of its own.
Receives `$spans` (tokens typed so far); returns records or `null`.

```nu
$env.config.completions.external = {
  enable: true
  max_results: 100
  completer: {|spans| carapace $spans.0 nushell ...$spans | from json }
}
```

[Carapace](https://carapace.sh) covers ~1000 CLIs and is the usual choice.
A more robust version, which falls back to files when carapace errors:

```nu
let carapace = {|spans|
  carapace $spans.0 nushell ...$spans
  | from json
  | if ($in | default [] | where value =~ '^-.*ERR$' | is-empty) { $in } else { null }
}
```

Dispatch to different engines per command:

```nu
let external = {|spans|
  match $spans.0 {
    git => (do $fish_completer $spans)
    _   => (do $carapace_completer $spans)
  }
}
$env.config.completions.external.completer = $external
```

**Alias caveat:** Nushell expands aliases before the completer sees the spans,
while carapace expects the original. Recover the typed text with
`commandline | split words` if this matters.

More recipes: <https://www.nushell.sh/cookbook/external_completers.html>

## Debugging completions without a terminal

`nu --ide-complete <cursor> <file>` asks Nushell what it would offer at a cursor
position, and prints JSON. This is the fastest way to reason about completion
behaviour — no pty, no keypresses, no menu.

```nu
"ps " | save -f /tmp/q.nu
nu --ide-complete 3 /tmp/q.nu
# => {"completions": ["project/", "ps-notes.md", "psfolder/", "readme.md"]}
```

Related: `--ide-hover`, `--ide-goto-def`, `--ide-check`, `--ide-ast`.

## File-path fallback in argument position

Nushell falls back to **file and directory completion** for an argument position
even when the command declares no positional parameters. Verified on 0.114.1:

```
'ps'    → ["ps"]                          # command position, fine
'ps '   → every file and folder in cwd    # despite `ps` taking no positionals
'ps -'  → ["--help", "--long", "-h", "-l"]  # flags are correct
```

`scope commands | where name == ps | get signatures.0` confirms `ps` has only
flags. The path list is a pure fallback, not something the signature asked for.

You rarely notice this with Tab-triggered completion, because you only press Tab
when you want something. With an always-on menu it is visible constantly.

There is no setting to suppress it. For *external* commands you care about,
declare an `extern` and attach a completer that returns `[]` to the positions
that should offer nothing:

```nu
def none [] { [] }
export extern "ps" [ ...args: string@none ]
```

That is the documented technique for suppressing an argument's completions, and
it is the same trick the book's `ssh` example uses to stop hostnames falling
back to filenames.

### Suppressing the fallback

Declaring fewer parameters does **not** stop it. Verified with `--ide-complete`:

```
def foo [] { }                    'foo ' -> every file and folder
def bar [--long] { }              'bar ' -> every file and folder
def qux [...a: string] { }        'qux ' -> every file and folder
def baz [...a: string@none] { }   'baz ' -> []            <- only this works
```

Only a rest parameter carrying a completer that returns `[]` suppresses it. For
a built-in that means shadowing it (see below), and the rest parameter exists
purely to hold the completer — so reject anything actually passed:

```nu
def none [] { [] }
export def ps [--long (-l), ...args: string@none] {
  if ($args | is-not-empty) { error make {msg: "ps takes no positional arguments"} }
  %ps --long=$long
}
```

`%ps ...$args` is not an option: the built-in has no rest parameter, so
spreading into it is a parse error.

**The cost:** a shadowed command appears **twice** in command-position
completion, because the completer lists both the built-in and the custom
declaration — even though `scope commands` shows one.

```
'ps' -> ["ps", "ps"]   shadowed
'ps' -> ["ps"]         stock
```

There is no config-side fix, so this is a trade. Worth it where the gain is real
(`kill` completing PIDs by name); not worth it for commands you rarely pass
arguments to, where you would duplicate every entry for nothing.

### Built-ins: shadow to attach a completer

`extern` only declares *external* commands. A built-in's signature cannot be
annotated from outside, so to give one a completer you shadow it with a custom
command and delegate to the original with `%`:

```nu
export def kill [
  ...pid: int@"nu-complete pids"
  --force (-f)
  --quiet (-q)
  --signal (-s): int
] {
  if $signal != null {
    %kill --signal $signal --force=$force --quiet=$quiet ...$pid
  } else {
    %kill --force=$force --quiet=$quiet ...$pid
  }
}
```

Two rules: a bare `kill` in the body recurses to the recursion limit — `%kill`
is what makes it safe; and switches forward as `--flag=$flag`, not by
re-testing them with `if`.

Match on the description to complete a number by a name:

```nu
export def "nu-complete pids" [] {
  {
    options: { match_description: true, completion_algorithm: substring, sort: false }
    completions: (
      ^ps -Ao pid=,comm=
      | lines
      | parse -r (r#'^\s*(?<pid>\d+)\s+(?<cmd>.+)$'#)
      | each {|r| { value: $r.pid, description: ($r.cmd | path basename) } }
    )
  }
}
```

`kill ghost<TAB>` then inserts Ghostty's PID. Note `^ps` (~25 ms), not Nushell's
`ps` (~115 ms) — the built-in samples CPU over an interval, which is wasted work
when you only need names.

## Overriding a slow external completer

Nushell consults the external completer **only when it has no completion of its
own**. So declaring an `extern` is the way to bypass carapace for a specific
command — useful when carapace is too slow.

Real case, measured on 0.114.1 / carapace 1.7.3:

| Completion | Cost |
|---|---|
| carapace `brew install fo` | **~1650 ms** (2653 results) |
| carapace `brew info fo` | ~127 ms |
| carapace `brew uninstall fo` | ~36 ms |
| carapace `brew ` (subcommands) | ~16 ms |
| reading Homebrew's own name cache | **~4 ms** (16,469 entries) |

With Tab-triggered completion 1.6 s is merely annoying; with an always-on menu
it is unusable.

**zsh is not faster at generating the list — it caches.** `__brew_formulae` in
`/opt/homebrew/share/zsh/site-functions/_brew` calls `brew formulae` once and
stores it with `_store_cache`, re-running only when the cache is two weeks old
or a tap index is newer.

Better still, read whatever cache the tool already maintains, so there is no
second cache to invalidate:

```nu
def read-lines [f: path] {
  if ($f | path exists) { %open --raw $f | lines --skip-empty } else { [] }
}
export def "nu-complete brew all" [] {
  let d = ($env.HOME | path join "Library" "Caches" "Homebrew" "api")
  (read-lines ($d | path join formula_names.txt)) ++
  (read-lines ($d | path join cask_names.txt))
}
export extern "brew install" [ ...packages: string@"nu-complete brew all" ]
```

Three things that matter in that snippet:

- **`lines --skip-empty`, not `where {|l| $l | is-not-empty}`.** Over 16k lines
  the closure filter costs ~12 ms; the built-in flag is free. 15 ms → 4 ms.
- **`%open`**, in case the user has `alias open = ^open` (the macOS fix). A
  module parsed after that alias would otherwise shell out to `/usr/bin/open`.
- **Declare only positionals.** Undeclared flags pass through to the external
  untouched — verified — so there is no need to enumerate every flag, and no
  risk of rejecting a valid one.

Prefer reading files over shelling out. `brew formulae` costs ~97 ms and
`brew list --formula` ~16 ms, both paying Ruby startup; the Cellar directory
listing that answers the same question takes ~1 ms.

Declare externs only for the slow subcommands and let the external completer
keep handling the rest.

## Global completion settings

```nu
$env.config.completions.algorithm = "prefix"   # prefix | substring | fuzzy
$env.config.completions.sort = "smart"         # smart | alphabetical
$env.config.completions.case_sensitive = false
$env.config.completions.quick = true           # auto-accept a lone candidate
$env.config.completions.partial = true         # complete the common prefix
$env.config.completions.use_ls_colors = true
$env.config.completions.external.enable = true # scan PATH for command names
```

## Ready-made completions

[`nu_scripts/custom-completions`](https://github.com/nushell/nu_scripts/tree/main/custom-completions)
has modules for 100+ tools: git, cargo, docker, gh, npm, pnpm, just, make, man,
rg, ssh, tar, curl, kubectl, poetry, zellij, and more.

```nu
use git-completions.nu *      # after placing it on NU_LIB_DIRS
```

On this machine, `nu-fetch-completions <tool>` downloads one into
`~/.nu/completions/`.
