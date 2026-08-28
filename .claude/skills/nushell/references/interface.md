# Themes, colours, and the line editor

Nushell 0.114.1.

## Colour values

Every colour setting accepts:

| Form | Example |
|---|---|
| Name | `red`, `light_cyan`, `default` |
| Name + attribute | `red_bold`, `green_underline`, `blue_reverse` |
| Abbreviation | `r` (red), `rb` (red bold), `lgu` (light green underline) |
| Hex | `"#ff0000"` — **quotes required**, or `#` starts a comment |
| Record | `{ fg: "#ff0000", bg: "#0000ff", attr: b }` |
| Closure | `{\|x\| if $x > 1mb { 'red' } else { 'green' } }` |

Attributes: `l` blink · `b` bold · `d` dimmed · `h` hidden · `i` italic ·
`r` reverse · `s` strikethrough · `u` underline · `n` nothing

`ansi --list` prints every name and abbreviation.

**Closures only run for table output.** They do not apply to `shape_*` keys, to
values printed directly, or to list elements.

## `color_config`

Two distinct families:

**`shape_*` — syntax highlighting** of what you type, keyed by the parser's
"shape" for each token:

`shape_string` `shape_string_interpolation` `shape_raw_string` `shape_record`
`shape_list` `shape_table` `shape_bool` `shape_int` `shape_float` `shape_range`
`shape_binary` `shape_datetime` `shape_custom` `shape_nothing` `shape_literal`
`shape_operator` `shape_filepath` `shape_directory` `shape_globpattern`
`shape_garbage` `shape_variable` `shape_vardecl` `shape_matching_brackets`
`shape_pipe` `shape_internalcall` `shape_external` `shape_external_resolved`
`shape_externalarg` `shape_match_pattern` `shape_block` `shape_signature`
`shape_keyword` `shape_closure` `shape_redirection` `shape_flag`

**Type colours — output values** in tables and records:

`bool` `int` `string` `float` `glob` `closure` `binary` `custom` `nothing`
`datetime` `filesize` `list` `record` `duration` `range` `semver`
`semver-range` `cell-path` `block` + the binary-viewer keys
(`binary_null_char`, `binary_printable`, …)

**UI elements:** `hints` `search_result` `header` `separator` `row_index`
`empty` `leading_trailing_space_bg` `banner_foreground` `banner_highlight1`
`banner_highlight2`

`foreground`, `background`, and `cursor` are *not* used by Nushell — they exist
so theme commands can set the terminal's own colours via OSC codes.

`highlight_resolved_externals = true` makes Nushell verify externals against
PATH and colour confirmed ones with `shape_external_resolved`. Costs a lookup
per token.

## Theme file styles

Two conventions exist; know which you have before trying to load one.

**Source style** — assigns `$env.config.color_config` directly. Applied with
`source`. The official Catppuccin port works this way.

```nu
source my-theme.nu
```

**Module style** — exports `main`, returns a record, applies nothing. This is
what `nu_scripts/themes/nu-themes/*` uses.

```nu
use nu-themes/dracula.nu
$env.config.color_config = (dracula)
```

Stdlib starters: `use std/config [dark-theme light-theme]`.

450+ themes: <https://github.com/nushell/nu_scripts/tree/main/themes/nu-themes>

## Tables

```nu
$env.config.table.mode = 'rounded'
table --list          # every border style
$data | table --theme psql --index false --expand
```

Styles: `rounded basic compact compact_double light thin with_love reinforced
heavy none psql markdown dots restructured ascii_rounded basic_compact single
double frameless default`

`mode: "none"` plus `error_style: "plain"` gives clean screen-reader output.

## `LS_COLORS`

Colours filenames in `ls`. Colon-separated `selector=attributes`:

```nu
$env.LS_COLORS = "di=1;34:*.nu=3;33;46"
$env.LS_COLORS = (vivid generate catppuccin-macchiato)   # easier
```

Nushell falls back to a built-in 8-bit default when unset.

## Reedline: menus

Menus are styled per-menu, **not** through `color_config`.

```nu
$env.config.menus ++= [{
  name: completion_menu
  marker: "| "
  input_mode: cursor_prefix     # diff | cursor_prefix | full_buffer
  output_mode: suggested_span   # suggested_span | full_buffer | extend_to_end
  type: {
    layout: columnar            # columnar | list | ide | description
    columns: 4
    col_width: 20
    col_padding: 2
  }
  style: {
    text: green
    selected_text: green_reverse
    description_text: yellow
  }
}]
```

`input_mode` supersedes the legacy `only_buffer_difference` (`true` ≡ `diff`,
`false` ≡ `cursor_prefix`).

Layouts: `columnar` (grid), `list` (vertical, supports
`description_position: before|after`), `ide` (floating box beside the cursor
with a description pane), `description` (help-style).

Built-in menus: `completion_menu`, `history_menu`, `help_menu`.

## Reedline: keybindings

```nu
$env.config.keybindings ++= [{
  name: completion_menu
  modifier: control          # none control alt shift shift_alt control_alt
                             # control_shift control_alt_shift ...
  keycode: char_t            # char_<x>, enter, tab, backspace, left, f1, ...
  mode: [emacs vi_insert vi_normal]
  event: { send: menu name: completion_menu }
}]
```

Three event kinds:

```nu
event: { send: Enter }                       # a Reedline event
event: { edit: InsertString, value: "!$" }   # an EditCommand
event: { until: [ {...} {...} ] }            # try in order, stop at first accepted
```

`until` is what makes one key both open a menu and page through it.

Run a command without polluting history or the validator — use
`executehostcommand`, not `InsertString` + `Enter`:

```nu
event: { send: executehostcommand, cmd: "cd (fd -t d | fzf | str trim)" }
```

Remove a default binding by rebinding the key to `{ send: none }`.

### Always-on completions (menu on every keystroke)

> Reference material only. This machine's config **does not use this** — it was
> built, then removed on 2026-08-05 in favour of a plain Tab-triggered menu.
> Do not add it back unless asked.

Reedline has **no native setting** for this — it is an open feature request,
[reedline#356](https://github.com/nushell/reedline/issues/356). There is no
`completions.auto` key; `config nu --doc` has nothing of the kind.

It can be built from keybindings: bind each printable character to *insert
itself, then open the menu*. Once open, the menu filters live as you keep
typing, so a single trigger per key is enough.

```nu
let chars = [ ...(seq char 'a' 'z') ...(seq char '0' '9') '-' '.' '/' ]
$env.config.keybindings ++= ($chars | each {|c|
  {
    name: $"auto_menu_char_($c)"
    modifier: none
    keycode: $"char_($c)"
    mode: [emacs vi_insert]        # NEVER vi_normal — see below
    event: [
      { edit: InsertChar, value: $c }
      { send: menu, name: ide_completion_menu }
    ]
  }
})
```

Verified working on 0.114.1: typing `a` → `al` → `alp` shows commands, then
narrows, then switches to file completions, with no Tab pressed.

**Enter must be rebound, or nothing runs on the first press.** Reedline's
`Enter` event means "accept the open menu selection, otherwise submit". With a
menu permanently open it always takes the first branch, so Enter completes the
word (adding a trailing space) and a second press is needed to run the command.
From reedline 0.49.0 `src/engine.rs`:

```rust
ReedlineEvent::Enter | ReedlineEvent::Submit | ReedlineEvent::SubmitOrNewline
    if self.menus.iter().any(|menu| menu.is_active()) =>
{ menu.replace_in_buffer(...); menu.menu_event(Deactivate); return Handled }
```

All three submit events hit that arm, so switching to `Submit` or
`SubmitOrNewline` changes nothing. Deactivate the menu first instead —
`ReedlineEvent::Esc` calls `deactivate_menus()` and does *not* leave vi insert
mode (that is `ViChangeMode`). A list of events becomes `Multiple`, run in order:

```nu
{ name: submit, modifier: none, keycode: enter
  mode: [emacs vi_insert vi_normal]
  event: [ { send: Esc } { send: Enter } ] }
```

Enter then reaches the ordinary arm, which consults the validator, so multi-line
input with unclosed brackets still works.

That arm is also the **only** caller of `replace_in_buffer` — the only way to
accept a completion — so Enter can no longer do it and another key must. `Menu`
returns `Inapplicable` when a menu is already active, which makes `until` do
exactly the right thing:

```nu
{ name: accept, modifier: none, keycode: tab, mode: [emacs vi_insert]
  event: { until: [ { send: menu, name: ide_completion_menu } { send: Submit } ] } }
# closed -> Menu activates (Handled, stops).  open -> Menu Inapplicable, Submit accepts.
```

Four more things to get right — the last three were all learned by getting them
wrong:

- **Never bind `vi_normal`.** Letters there are motions — `h/j/k/l`, `w`, `b`,
  `dd`. Binding them destroys normal-mode navigation. Use `[emacs vi_insert]`.

- **You cannot bind uppercase letters this way.** Nushell lowercases the keycode
  string before extracting the character
  (`nu-cli/src/reedline_config.rs`: `let keycode_lower = keycode.to_ascii_lowercase();`
  then `keycode_lower.strip_prefix("char_")`). So `char_A` resolves to
  `Char('a')` — the *same key* as `char_a`. Bind both and the later one wins
  silently, so **every letter you type inserts as a capital**. Shift+letter has
  no binding and falls through to the default insert, which is fine. For the
  same reason, only bind symbols that are unshifted on your layout: a
  `(none, char__)` binding is dead, because `_` always arrives with Shift.

- **Do not bind Backspace.** It is tempting (`{ edit: Backspace }` then the
  menu, to re-widen the list while deleting), but with the menu perpetually open
  it stops Backspace deleting at all. Reedline's default already refreshes an
  open menu as the buffer shrinks.

- **Cost is per keystroke.** Internal completions are free; an external
  completer is not. Carapace measures ~45–62 ms per call, so in *argument*
  position (`git sta`) every character pays it. Command position stays fast.

Binding Space is usually a mistake too: the prefix is then empty and the
completer returns its entire candidate set.

Invalid keycodes are **silently ignored** — no error at startup — so a typo in
`keycode` just means the key does nothing. Verify what actually registered:

```nu
$env.config.keybindings | where name =~ auto_menu | length
$env.config.keybindings | where keycode? == char_a | get event.0.0.value
```

Discovery:

```nu
keybindings list             # all modifiers, keycodes, events, edits
keybindings listen           # press a key, see its name
keybindings default | where mode == vi_insert
```

Some keys are indistinguishable to the terminal (Tab/Ctrl+I, Enter/Ctrl+M)
unless `$env.config.use_kitty_protocol = true` and the terminal supports it.

## Abbreviations

Expand inline as you type, unlike aliases which resolve at parse time. The
expansion is visible and editable before Enter.

```nu
$env.config.abbreviations = { gs: "git status", ll: "ls -l" }
```

## Prompts

```nu
$env.PROMPT_COMMAND = {|| ... }         # string, closure, or null
$env.PROMPT_COMMAND_RIGHT = ""
$env.PROMPT_INDICATOR = "> "
$env.PROMPT_INDICATOR_VI_INSERT = ": "
$env.PROMPT_INDICATOR_VI_NORMAL = "〉"
$env.PROMPT_MULTILINE_INDICATOR = "::: "
$env.config.render_right_prompt_on_last_line = false
```

Every one has a `TRANSIENT_PROMPT_*` counterpart, used to redraw *past* prompts
more simply after a command runs. Nushell blanks
`TRANSIENT_PROMPT_COMMAND_RIGHT` and `TRANSIENT_PROMPT_MULTILINE_INDICATOR` by
default so copy/paste from scrollback is clean; set them to `null` to restore.

Starship:

```nu
$env.STARSHIP_SHELL = "nu"
$env.PROMPT_COMMAND = {|| starship prompt --cmd-duration $env.CMD_DURATION_MS $'--status=($env.LAST_EXIT_CODE)' }
$env.PROMPT_COMMAND_RIGHT = ""
```
