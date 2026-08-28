# Ecosystem

## Official sources

| Resource | URL |
|---|---|
| The Book | <https://www.nushell.sh/book/> |
| Command reference (~690 commands) | <https://www.nushell.sh/commands/> |
| Cookbook | <https://www.nushell.sh/cookbook/> |
| Language guide | <https://www.nushell.sh/lang-guide/> |
| Contributor book (incl. plugin protocol) | <https://www.nushell.sh/contributor-book/> |
| Blog / release notes | <https://www.nushell.sh/blog/> |
| Main repo | <https://github.com/nushell/nushell> |
| Docs source | <https://github.com/nushell/nushell.github.io> |
| Discord | <https://discord.gg/NtAbbGn> |

The docs site is a VitePress build of `nushell.github.io`. Cloning that repo
gives the full markdown — better than scraping when you need exact detail.

**Release notes matter more than usual here.** Nushell breaks things at minor
versions; the blog post for each release has a migration section.

## `nu_scripts`

<https://github.com/nushell/nu_scripts> — the community collection, and the
first place to look before writing something yourself.

| Directory | Contents |
|---|---|
| `themes/nu-themes/` | 450+ colour themes, module style |
| `custom-completions/` | 100+ tools: git, cargo, docker, gh, npm, pnpm, just, make, man, rg, ssh, tar, curl, kubectl, poetry, zellij, jj, bat, eza, … |
| `modules/` | git, kubernetes, docker, aws, nix, prompt, formats, fuzzy, network, system, virtual_environments, … |
| `aliases/` | alias sets per tool |
| `sourced/` | standalone scripts |
| `benchmarks/`, `make_release/` | project tooling |

Usable two ways: copy individual files onto `NU_LIB_DIRS`, or clone the repo and
add the relevant subdirectory to the path. Files carry `nupm.nuon` metadata but
do not require nupm.

On this machine: `nu-fetch-completions <tool>` and `nu-fetch-theme <name>`.

## `awesome-nu`

<https://github.com/nushell/awesome-nu> — curated plugins, modules, and tools.
`plugin_details.md` lists each plugin with the `nu-plugin` version it was built
against; **check that column before installing**, since the protocol is
versioned.

## Package management

There is no mandatory package manager. In practice:

1. **Copy the file** into a `NU_LIB_DIRS` directory — by far the most common.
2. **`git clone`** into a vendored dir on the path, update with `git pull`.
3. **[nupm](https://github.com/nushell/nupm)** — official, still experimental.
   Packages declare `nupm.nuon`; supports `nupm install`, `nupm test`.

Nushell configs are usually managed as plain dotfiles (git, stow, chezmoi,
home-manager) rather than through a package manager.

## Notable third-party plugins

`nu_plugin_highlight` syntax highlighting · `nu_plugin_clipboard` ·
`nu_plugin_dns` · `nu_plugin_hashes` · `nu_plugin_compress` (zstd/gzip/bzip2/xz) ·
`nu_plugin_image` · `nu_plugin_kdl` · `nu_plugin_regex` · `nu_plugin_skim`
(fuzzy finder) · `nu_plugin_port_list` · `nu_plugin_desktop_notifications` ·
`nu_plugin_bin_reader` · `nu_plugin_from_beancount` · `nu_plugin_dbus`

## Tools with first-class Nu support

| Tool | Integration |
|---|---|
| starship | `starship prompt` from `PROMPT_COMMAND` |
| zoxide | `zoxide init nushell` |
| atuin | `atuin init nu` |
| carapace | `carapace _carapace nushell` — completions for ~1000 CLIs |
| direnv | PWD hook + `direnv export json` |
| oh-my-posh | `oh-my-posh init nu` |
| mise / asdf | `mise activate nu` |
| zellij | ships Nu completions |

All of these emit a `.nu` file rather than shell script, because Nushell cannot
`eval` shell strings. Convention is to write it into `vendor/autoload/`.

## Editor support

- **VS Code** — `nushell-lang` extension; Nushell ships an LSP (`nu --lsp`)
- **Neovim** — `nu` treesitter grammar; LSP via `nu --lsp`
- **Zed** — Nu extension available
- **Helix** — treesitter + LSP configurable

Nushell also has an MCP mode (`nu --mcp`) — visible in the feature list of
`version` on this build. It is a first-class server, not a demo: three tools,
a persistent stack, and a server-side result history. See `mcp.md`.

## Related projects

`nana` (experimental GUI) · `nu_plugin_*` template repos ·
[`nutest`](https://github.com/vyadh/nutest) test runner ·
[showcase](https://github.com/nushell/showcase) for talks and posts
