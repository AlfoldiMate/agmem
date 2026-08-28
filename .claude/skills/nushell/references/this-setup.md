# This machine's Nushell setup

macOS (arm64), Nushell **0.114.1** via Homebrew, login shell `/bin/zsh`,
terminal Ghostty, prompt Starship, theme Catppuccin Macchiato.

## `~/.nu` is the single source of truth

```
~/Library/Application Support/nushell  →  ~/.nu     (symlink)
```

Every Nushell path derives from the config dir, so this one link redirects all
of them. No environment variables, no launch flags — works identically for
Ghostty, VS Code, Zed, `ssh`, `cron`, and anything that shells out to `nu`.

**Never edit `~/Library/Application Support/nushell` directly** — edit `~/.nu`.

`~/.nu` is a git repository. Backup of the pre-migration config:
`~/Library/Application Support/nushell.pre-migration-2026-07-31`.

## Layout

```
~/.nu/
├── config.nu              search paths only (NU_LIB_DIRS / NU_PLUGIN_DIRS)
├── env.nu                 legacy slot, intentionally empty
├── login.nu               login-shell env (inactive — login shell is zsh)
├── autoload/              the real config, alphabetical
│   ├── 00-env.nu          PATH, EDITOR, env vars, ENV_CONVERSIONS
│   ├── 10-ui.nu           tables, banner, history, errors, vi mode
│   ├── 20-theme.nu        colours
│   ├── 30-prompt.nu       starship + indicators
│   ├── 40-line-editor.nu  Reedline menus + keybindings
│   ├── 50-completions.nu  completion behaviour, external completer
│   ├── 60-aliases.nu      aliases and small commands
│   ├── 70-integrations.nu zoxide/atuin, command_not_found, fzf
│   ├── 80-modules.nu      `use` statements
│   └── 90-local.nu        machine-local, gitignored
├── modules/               nu-manage.nu, integrations.nu
├── themes/                catppuccin-macchiato.nu + nu-themes/ alternates
├── completions/  scripts/  overlays/  vendored/     all on NU_LIB_DIRS
├── plugins/                                         on NU_PLUGIN_DIRS
├── vendor/autoload/       generated integrations (gitignored)
├── docs/startup-order.md
├── CLAUDE.md              instructions for Claude Code in this repo
└── .claude/skills/nushell this skill (symlinked into ~/.claude/skills/nushell)
```

`completions/` is currently empty — a documented slot, not a dead directory.

`NU_LIB_DIRS` covers `modules themes completions scripts overlays vendored`, so
a bare filename resolves from anywhere:

```nu
use nu-manage.nu *
source catppuccin-macchiato.nu
```

## Where to change what

| Want to change | Edit |
|---|---|
| PATH, env vars, `$EDITOR` | `autoload/00-env.nu` |
| Table style, banner, vi mode, history | `autoload/10-ui.nu` |
| Colours / theme | `autoload/20-theme.nu` |
| Prompt | `autoload/30-prompt.nu` (or `~/.config/starship.toml`) |
| Keybindings, menus | `autoload/40-line-editor.nu` |
| Completion behaviour | `autoload/50-completions.nu` |
| Aliases | `autoload/60-aliases.nu` |
| Tool integrations, hooks | `autoload/70-integrations.nu` |
| Loading a new module | `autoload/80-modules.nu` |
| Anything machine-specific/private | `autoload/90-local.nu` (gitignored) |
| Search paths | `config.nu` |

## Commands (from `modules/nu-manage.nu`)

`nu-doctor` · `nu-startup-time [n]` · `nu-loaded-files` ·
`nu-plugins-available` · `nu-plugins-add-core` · `nu-fetch-completions <tool>` ·
`nu-fetch-theme <name>` · `nu-update-vendored` · `nu-edit` · `nu-root`

From `modules/integrations.nu`:
`integrations status` · `integrations setup` · `integrations remove <tool>`

`nu-doctor` is the first thing to run when something looks wrong — it verifies
the symlink, prints every derived path, lists autoload order, and shows plugin
and tool status.

## Current configuration

- **Edit mode** vi; cursor `line` in insert, `block` in normal
- **Buffer editor** `["zed", "--wait"]` (Ctrl+O)
- **Tables** `psql`, index always, footer at 25 rows
- **Banner** off
- **History** plaintext, 100k entries (switch to `sqlite` in `10-ui.nu` for
  timestamps, cwd and session isolation)
- **Menus** `ide_completion_menu` — Ctrl+L
- **Keybindings** Ctrl+L completion menu, Ctrl+Y history menu (both vi modes)
- **Theme** Catppuccin Macchiato; also sets `highlight_resolved_externals` and
  `explore` colours
- **`open`** left alone — it is Nushell's parser. Launch a file in its default
  app with the built-in `start <path>`

## Installed and available

Core plugins present at `/opt/homebrew/bin` but **not yet registered**:
`polars` `formats` `gstat` `query` `inc` → `nu-plugins-add-core`

Tools present on PATH: starship, zoxide, atuin, vivid, fzf, eza, bat, rg, fd,
delta, gh, nvim, zed, cargo. Also installed but **deliberately not wired into
this config**: carapace, direnv.

## Tool integrations

Generated into `vendor/autoload/` by `integrations setup`
(`modules/integrations.nu`, or `scripts/setup-integrations.nu` standalone).

Active: **zoxide** (`z`/`zi` + PWD hook) · **atuin** (Ctrl+R history) ·
**vivid** (`LS_COLORS`).

The rule is *installed → generated; absent → pruned*. Nothing is commented out;
presence on PATH is the switch, gated by the registry in
`modules/integrations.nu`. Idempotent, so re-run after installing or upgrading
a tool.

Two details specific to this setup:

- **vivid is baked**, not invoked at startup — its output is a static ~18 kB
  string and calling it each launch costs ~5 ms. Theme via
  `$env.NU_VIVID_THEME` in `90-local.nu`, then re-run setup.
- **`00-env.nu` does not filter PATH on `path exists`**, only `uniq`. Some
  tools prepend a directory they create lazily (carapace's bridge dir is the
  classic case), and filtering stripped it every session.

Composition holds because zoxide and atuin both *append* to hooks, keybindings
and completions rather than replacing them, and `vendor/autoload/` runs before
`autoload/`, so this config wins on any key it sets.

Non-generatable integration config (Homebrew `command_not_found`, fzf Ctrl+T /
Ctrl+G) lives in `autoload/70-integrations.nu`, each guarded with `which`.

**Carapace and direnv were removed** on 2026-08-05 to keep the config lean.
There is no external completer configured — `50-completions.nu` has the carapace
closure commented out with instructions. Do not reintroduce either without
being asked.

## Maintenance

After `brew upgrade nushell`:

```nu
nu-doctor              # paths still resolve?
nu-plugins-add-core    # re-register against the new plugin protocol
```

Plugins are protocol-versioned and silently break across upgrades.

## Reverting the migration

```nu
rm ~/Library/Application\ Support/nushell
mv ~/Library/Application\ Support/nushell.pre-migration-2026-07-31 ~/Library/Application\ Support/nushell
```
