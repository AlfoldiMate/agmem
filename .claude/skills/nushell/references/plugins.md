# Plugins

Nushell 0.114.1.

Plugins are **separate executables** that speak the `nu-plugin` protocol over
stdin/stdout or local sockets, using JSON or MessagePack. They add commands that
behave like built-ins.

## The version rule

> The protocol is versioned. A plugin must be built against the same `nu-plugin`
> version as the running Nushell.

After **every** Nushell upgrade, re-run `plugin add` for each plugin (and
rebuild third-party ones). A stale plugin fails to load, sometimes noisily,
sometimes by silently missing its commands.

## Three steps

Install → register → import.

```nu
# 1. install: package manager, `cargo install nu_plugin_x --locked`, or a build
# 2. register (once; writes signatures into the registry)
plugin add nu_plugin_polars          # resolved via $NU_PLUGIN_DIRS
plugin add ~/.cargo/bin/nu_plugin_x  # or an absolute path
# 3. import into the current session (or just restart)
plugin use polars
```

Naming is inconsistent by design and trips people up:

- `plugin add` takes the **filename** — `nu_plugin_polars`, with `.exe` on Windows
- `plugin use` / `plugin rm` take the **plugin name** — `polars`

The file must begin with `nu_plugin_`; that prefix is how Nushell identifies it.

`plugin use` is a **parse-time keyword**, so `plugin add` followed by
`plugin use` cannot work in one script — the `use` resolves before the `add`
runs. At the REPL, on separate lines, it is fine. For scripts:

```nu
nu --plugins '[./nu_plugin_cool]' script.nu
```

## The registry

`$nu.plugin-path` → `plugin.msgpackz`, a compressed cache of plugin signatures.
Every registered plugin is loaded automatically at startup; **you do not add
`plugin use` lines to `config.nu`.**

```nu
plugin list                       # name, version, status, pid, filename, commands
plugin list | where status == running
plugin stop query                 # stop a running plugin
plugin rm gstat                   # unregister (commands persist until session end)
scope commands | where type == 'plugin'
```

## Search path

`$NU_PLUGIN_DIRS` (const) and `$env.NU_PLUGIN_DIRS` (deprecated), consulted by
`plugin add` only. Defaults: `<config-dir>/plugins` and the directory holding
the `nu` binary.

```nu
const NU_PLUGIN_DIRS = [
  ($nu.default-config-dir | path join 'plugins')
  ($nu.current-exe | path dirname)          # where core plugins live
]
```

## Core plugins

Shipped and maintained with Nushell, normally installed beside the binary:

| Plugin | Provides |
|---|---|
| `polars` | DataFrames via Polars — very fast columnar operations |
| `formats` | `from`/`to` for EML, ICS, INI, plist, VCF |
| `gstat` | Git status as structured data |
| `query` | `query json`, `query web`, `query xml`, `query webpage-info` |
| `inc` | Increment values and semver strings |

Also shipped, for plugin developers: `example`, `custom_values`,
`stress_internals`.

Most package managers install these automatically — Homebrew does, into
`/opt/homebrew/bin`. `cargo install nu` does **not**; install each with
`cargo install nu_plugin_<name> --locked`.

Being installed is not the same as being registered. Check with `plugin list`.

## Garbage collection

Plugins stay resident while in use and stop after an idle period (10s default).

```nu
$env.config.plugin_gc = {
  default: { enabled: true, stop_after: 10sec }
  plugins: {
    gstat: { stop_after: 1min }
    polars: { enabled: false }        # never auto-stop; keeps state warm
  }
}
```

Disable GC for plugins that hold expensive state (`polars`) or are called in
tight loops.

## Per-plugin configuration

```nu
$env.config.plugins.<name> = { ... }    # key must match the plugin name
```

## Writing one

Official examples: `nu_plugin_example` (Rust) and `nu_plugin_python` in the
Nushell repo. The Rust path:

```toml
[dependencies]
nu-plugin = "0.114"
nu-protocol = "0.114"
```

Implement `Plugin` + `PluginCommand`, then `serve_plugin(&MyPlugin, MsgPackSerializer)`.

Debugging: print to **stderr** — plugin stderr is forwarded to the user. Capture
the full protocol stream with the [`trace_nu_plugin`](https://crates.io/crates/trace_nu_plugin)
wrapper, and remember to remove it afterwards (trace files grow without bound).

Details: the [plugin chapter of the contributor book](https://www.nushell.sh/contributor-book/plugins.html)
and the [protocol reference](https://www.nushell.sh/contributor-book/plugin_protocol_reference.html).

## Finding plugins

[awesome-nu](https://github.com/nushell/awesome-nu/blob/main/plugin_details.md)
lists ~100, with the `nu-plugin` version each was built against — check that
column before installing. Notable ones: `nu_plugin_highlight`,
`nu_plugin_clipboard`, `nu_plugin_dns`, `nu_plugin_hashes`,
`nu_plugin_compress`, `nu_plugin_image`, `nu_plugin_kdl`, `nu_plugin_regex`,
`nu_plugin_port_list`, `nu_plugin_skim`.

Third-party plugins run arbitrary code on your machine. Confirm the source.
