# Standard library

Nushell 0.114.1. Written in Nu, loaded into a virtual filesystem at startup but
**not** imported — you must `use` what you need.

## Import forms, and why they matter

```nu
use std/log          # ✅ parses ONLY the log submodule
use std log          # ❌ parses ALL of std, then imports log
use std *            # ❌ parses and imports everything
```

The slash form is the only one that avoids parsing the whole library. In startup
config the difference is easily tens of milliseconds. Audit with:

```nu
view files | enumerate | flatten
| where filename !~ '^std' and filename !~ '^entry'
| where {|f| (view span $f.start $f.end) =~ 'use\W+std[^\/]' }
```

## Submodules (verified on 0.114.1)

`assert` `bench` `config` `dirs` `dt` `formats` `help` `input` `iter` `log`
`math` `random` `testing` `xml`

> The book still documents this as `std/iters`. On 0.114.1 the module is
> **`std/iter`** (singular). Check with `use std; scope modules | where name == std`.

### Import without a glob — `use std/<mod>`

Commands keep their prefix.

| Module | Commands |
|---|---|
| `std/assert` | `assert`, `assert equal`, `assert not equal`, `assert less`, `assert greater`, `assert greater or equal`, `assert less or equal`, `assert length`, `assert str contains`, `assert error`, `assert not`, `assert compare` |
| `std/bench` | `bench` |
| `std/dirs` | `dirs`, `dirs add`, `dirs drop`, `dirs goto`, `dirs next`, `dirs prev` (+ aliases `enter`, `shells`, `n`, `p`, `g`, `dexit`) |
| `std/input` | `input display`, `input list`, `input listen` |
| `std/help` | `help`, `help commands`, `help modules`, `help operators`, `help escapes`, `help pipe-and-redirect` — completion-aware replacement for built-in `help` |
| `std/iter` | `iter filter-map`, `iter find`, `iter find-index`, `iter flat-map`, `iter intersperse`, `iter scan`, `iter zip-into-record`, `iter zip-with` |
| `std/log` | `log critical`, `log error`, `log warning`, `log info`, `log debug`, `log custom`, `log set-level`, `log log-level`, `log log-prefix`, `log log-ansi` |
| `std/math` | `math abs`, `math round`, `math floor`, `math ceil`, `math sqrt`, `math ln`, `math log`, `math exp`, `math sin`…`math tanh`, `math avg`, `math median`, `math mode`, `math stddev`, `math variance`, `math product`, `math sum`, `math min`, `math max` |
| `std/random` | `random`, `random bool`, `random int`, `random float`, `random chars`, `random binary`, `random dice`, `random uuid`, `random pass` |

### Import with a glob — `use std/<mod> *`

Definitions land in scope unprefixed.

| Module | Provides |
|---|---|
| `std/dt *` | `datetime-diff`, `pretty-print-duration` |
| `std/formats *` | `from jsonl`, `from ndjson`, `from ndnuon`, `to jsonl`, `to ndjson`, `to ndnuon` |
| `std/math *` | Bare constants: `$PI`, `$E`, `$TAU`, `$PHI`, `$GAMMA` |
| `std/xml *` | `xaccess`, `xinsert`, `xupdate`, `xtype` |
| `std/config` | `dark-theme`, `light-theme`, `env-conversions` |
| `std/util` | `path add`, `repeat`, `null-device` |

## The ones you will actually use

```nu
# PATH manipulation — prepends, so later calls win
use std/util "path add"
path add "~/.local/bin"
path add ($env.CARGO_HOME | path join "bin")

# Logging — honours $env.NU_LOG_LEVEL
use std/log
log info "starting"
log error "failed"

# Assertions, for tests
use std/assert
assert equal (1 + 1) 2
assert error {|| 1 / 0 }

# Benchmark
use std/bench
bench { 1..1000 | math sum } --rounds 50

# Directory stack
use std/dirs
enter ~/projects; n; p; dexit

# Themes
use std/config [dark-theme light-theme]
$env.config.color_config = (dark-theme)
```

`$env.NU_LOG_LEVEL` controls `std/log` output. It is unrelated to
`nu --log-level`, which logs Nushell's internal Rust commands.

## `std/log` exports environment

Which means it hits the `export-env` trap: importing it at the top of *your*
module parses but never evaluates it, so its environment is missing at runtime.
See `modules.md` § *`export-env`*. Fix by importing inside the command, or:

```nu
use std/log
export-env { use std/log [] }
```

## `std-rfc`

Staging ground for future stdlib additions, shipped alongside `std`.

```nu
use std-rfc/<module>
```

Contents change between releases and commands can be removed. Do not depend on
it in anything you care about. Contribute candidates by PR to the
[`nu-std` crate](https://github.com/nushell/nushell/tree/main/crates/nu-std).

## Testing

```nu
use std/assert
def "test addition" [] { assert equal (1 + 1) 2 }
```

Run test suites with `std/testing`, or the more capable third-party
[nutest](https://github.com/vyadh/nutest).

## Disabling

```nu
nu --no-std-lib             # ~4x faster startup for throwaway invocations
nu -n --no-std-lib -c "…"   # the floor
```
