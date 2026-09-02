# Modules and overlays

Nushell 0.114.1.

## Defining a module

A module is a file of `export` statements. Two forms:

- `foo.nu` — **file form**, module name is the filename
- `foo/mod.nu` — **directory form**, module name is the directory

Behaviour once imported is identical; only the path differs.

```nu
# math-utils.nu
export def double []: int -> int { $in * 2 }
export alias d = double
export const VERSION = "1.0"
export extern "mytool" [ --flag ]

def helper [] { }        # no `export` → private to the module
```

Exportable: `export def`, `export alias`, `export const`, `export extern`,
`export module`, `export use`, `export-env`.

### `main`

An export cannot share the module's name. Name it `main` and it takes the
module's name on import:

```nu
# increment.nu
export def main []: int -> int { $in + 1 }
export def by [n: int]: int -> int { $in + $n }
```

```nu
use increment.nu
5 | increment        # main
5 | increment by 3   # subcommand
```

`main` is imported by `use <mod>`, `use <mod> *`, or `use <mod> main` — but not
by `use <mod> <other>`.

## Importing

```nu
use foo.nu           # commands as `foo <sub>`, plus `foo` if it has main
use foo.nu *         # definitions directly into scope, unprefixed
use foo.nu [a b]     # only these
use foo.nu bar       # one definition
hide foo             # remove from scope
```

For the standard library **always use the slash form.** `use std log` and
`use std *` parse the entire library first; `use std/log` parses only that
submodule. Worth tens of milliseconds at startup.

### Module search path

For a relative path, Nushell searches: current directory → each `$NU_LIB_DIRS`
entry (const) → each `$env.NU_LIB_DIRS` entry (deprecated). First match wins.

```nu
const NU_LIB_DIRS = [
  ($nu.default-config-dir | path join 'modules')
]
use mymod.nu *      # resolves from anywhere
```

The const version is visible to files parsed afterwards, including the autoload
files — verified on 0.114.1.

## Submodules

```nu
# my-utils/mod.nu
export module ./increment.nu       # submodule keeps its own namespace
export use ./range-tools.nu        # re-exports members into the PARENT
```

`export module` → `my-utils increment by 3`
`export use` → members become members of `my-utils` itself

Only `export use` can export selectively:

```nu
export module go {
  export use ./go.nu [home, modules]   # these two, under a `go` prefix
}
```

`module` without `export` defines a local module only.

## `export-env`

Runs when the module is imported, merging into the caller's environment.

```nu
export-env {
  $env.MY_TOOL_HOME = ($nu.default-config-dir | path join "mytool")
}
```

**The trap:** `export-env` runs only when the `use` is *evaluated*. A `use` at
the top of another module is only *parsed* when that module is imported, so the
environment never materialises.

```nu
# go.nu — BROKEN
use my-utils                        # parsed, not evaluated
export def --env modules [] { cd $env.NU_MODULES_DIR }   # not found
```

Two fixes:

```nu
# 1. import inside the command that needs it
export def --env modules [] { use my-utils; cd $env.NU_MODULES_DIR }

# 2. re-import the environment inside your own export-env
use my-utils
export-env { use my-utils [] }      # [] = environment only
```

This bites most often with `std/log`, which exports environment.

## Constraints

- A file cannot share its parent directory's name (`spam/spam.nu`).
- An export cannot share the module's name — use `main`.
- Use forward slashes in module paths even on Windows.

## Overlays

Overlays are activatable/deactivatable *layers* of definitions — Nushell's
answer to virtualenv. They build on modules.

```nu
overlay use spam            # activate (also runs export-env)
overlay use spam as eggs    # rename
overlay use --prefix spam   # keep definitions behind `spam <cmd>`
overlay list                # active, in order; `zero` is the base
overlay hide spam           # deactivate
overlay hide                # deactivate the most recent
overlay new scratch         # create an empty one
```

Properties worth knowing:

- **Recordable.** New definitions land in the last active overlay and are
  remembered across hide/use cycles.
- **Scoped.** An overlay activated inside a block disappears at the end of it.
- **Stacked.** Later overlays shadow earlier ones; `overlay use zero` promotes
  the base back to the top.
- `overlay hide --keep-custom` keeps definitions you added.
- `overlay hide --keep-env [VAR]` keeps selected env vars.

Typical use — activate a project environment on `cd`:

```nu
$env.config.hooks.env_change.PWD ++= [{
  condition: {|_, after| $after | path join 'project.nu' | path exists }
  code: "overlay use project.nu"
}]
```

Python venvs ship an `activate.nu` designed for exactly this.

## Distribution

There is no mandatory package manager. Options, in order of prevalence:

1. **Copy the file** into a `NU_LIB_DIRS` directory. Most common.
2. **`git clone`** into a vendored directory that is on `NU_LIB_DIRS`.
3. **[nupm](https://github.com/nushell/nupm)** — the official (still
   experimental) package manager. Packages declare a `nupm.nuon`.

`nu_scripts` uses `nupm.nuon` files but is equally usable by copying.
