# Working with data

Nushell's reason to exist. Nushell 0.114.1.

## The shape of things

- **list** — `[1 2 3]`
- **record** — `{name: "a", size: 10}`
- **table** — a list of records, `[[name size]; [a 10] [b 20]]`

Because a table *is* a list of records, list commands work on tables and each
row is a record. There is no separate table API to learn.

## Cell-paths

The universal accessor. Works on records, tables, and nested combinations.

```nu
$data.users.0.name
$data | get users.0.name
$data | get users?.0?.name     # ? = null instead of error if missing
ls | get name                  # a column → list
ls | select name size          # columns → table
ls | first | get name          # a cell
```

`?` is the difference between a working script and a crash on missing data.
`$env.FOO?` is the idiom for optional environment variables.

Cell-paths are values: `let p = $.a.b; $data | get $p`.

## Core filters

Selection: `where` `filter` `select` `reject` `get` `first` `last` `skip` `take` `slice` `find`
Transform: `each` `par-each` `update` `insert` `upsert` `merge` `rename` `move` `flatten` `wrap` `transpose`
Aggregate: `reduce` `math sum` `math avg` `math max` `math min` `group-by` `uniq` `uniq-by` `length` `chunks` `window`
Order: `sort` `sort-by` `reverse` `shuffle` `roll`
Set: `union` `intersect` `difference` `zip` `join` `append` `prepend`
Test: `all` `any` `is-empty` `is-not-empty` `in`

```nu
ls | where size > 1mb | sort-by modified --reverse | first 5
ls | where name =~ '\.nu$'
ls | where type == dir
ps | where cpu > 10 | select pid name cpu

# update vs insert vs upsert
$t | update size {|r| $r.size * 2 }   # must exist
$t | insert kind "file"               # must NOT exist
$t | upsert kind "file"               # either way

# group and count
ls | group-by type | transpose kind rows | insert n {|r| $r.rows | length }

# reduce needs an accumulator
[1 2 3] | reduce --fold 0 {|it, acc| $acc + $it }
```

`where` supports a shorthand row condition (`where size > 1mb`) and a closure
form (`where {|r| $r.size > 1mb }`). The shorthand cannot call arbitrary code;
switch to the closure when it gets complex.

## Strings

```nu
"a,b,c" | split row ","          # list
"a b c" | split words
"hello" | split chars
$list | str join ", "

"Hello" | str downcase | str upcase | str title-case
"  x  " | str trim
"a-b" | str replace "-" "_"          # first
"a-b-c" | str replace --all "-" "_"
"abc" | str substring 0..2
"abc" | str contains "b"
"abc" | str length
```

Case converters: `str camel-case`, `str snake-case`, `str kebab-case`,
`str pascal-case`, `str screaming-snake-case`.

## `parse` — the anti-regex

Turns text into a table with named columns. Reach for it before regex.

```nu
"user:1000" | parse "{name}:{uid}"
# ╭─name─┬─uid──╮
# │ user │ 1000 │

^df -h | lines | skip 1 | parse -r '(?<fs>\S+)\s+(?<size>\S+)'
^git log --format='%h|%an|%s' | lines | split column '|' hash author subject
```

`parse -r` takes a real regex with named groups. Nushell uses the Rust `regex`
crate: **no backreferences, no lookaround.** Use raw strings `r#'...'#` so
backslashes survive.

Related: `detect columns` (whitespace-aligned output), `from ssv`
(space-separated values), `split column`.

## Format conversion

`from` / `to`: `csv tsv ssv json nuon yaml toml xml html md msgpack kdl ods xlsx url`

```nu
open data.json                  # parses by extension automatically
open --raw data.json            # raw text
open x.txt | from csv --separator '|'
$data | to json --indent 2
$data | to nuon                 # Nushell's own literal format — round-trips exactly
```

`open` infers the parser from the extension; `--raw` opts out. `save` writes,
`save --raw` skips serialisation.

`to nuon` is the right choice for debugging and for persisting Nu values
losslessly — unlike JSON it preserves types like `duration` and `filesize`.

## HTTP and SQLite

```nu
http get https://api.example.com/x | get items
http post --content-type application/json $url $payload

open db.sqlite | query db "select * from t where x > 1"
$table | into sqlite db.sqlite --table-name t
stor create --table-name tmp --columns {a: int}    # in-memory
```

## bash → Nushell

| bash | Nushell |
|---|---|
| `ls -la` | `ls -la` (returns a table) |
| `cat f` | `open --raw f` / `open f` to parse |
| `grep p f` | `open f \| lines \| where $it =~ p` |
| `wc -l < f` | `open f \| lines \| length` |
| `head -5` | `first 5` |
| `tail -5` | `last 5` |
| `sort \| uniq` | `sort \| uniq` |
| `sort -u \| wc -l` | `uniq \| length` |
| `cut -d, -f1` | `from csv \| get column0` |
| `awk '{print $2}'` | `split column ' ' \| get column2` |
| `sed 's/a/b/'` | `str replace 'a' 'b'` |
| `find . -name '*.nu'` | `glob **/*.nu` or `ls **/*.nu` |
| `du -sh *` | `ls \| insert s {\|r\| du $r.name}` |
| `export X=1` | `$env.X = 1` |
| `X=1 cmd` | `X=1 cmd` or `with-env {X: 1} { cmd }` |
| `$?` | `$env.LAST_EXIT_CODE` |
| `cmd1 && cmd2` | `cmd1; cmd2` (Nu stops on error) |
| `$(cmd)` | `(cmd)` |
| `for f in *; do` | `ls \| each {\|f\| ... }` |
| `if [ -f x ]` | `if ('x' \| path exists)` |
| `command -v x` | `which x` |
| `x \| jq '.a'` | `x \| from json \| get a` |
| heredoc | a multi-line string literal |
| `eval "$(tool init)"` | not possible — tools must emit `.nu` |

The last row matters: **Nushell cannot `eval` shell script.** Tool integrations
must ship a `.nu` file. See `config.md` § *Integrations*.

## Metadata

Values carry provenance, which is what lets errors point at real spans.

```nu
ls | metadata
$x | metadata | get span
open f.json | metadata          # includes content type
```
