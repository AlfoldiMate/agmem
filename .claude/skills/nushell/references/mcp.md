# The Nushell MCP server (`nu --mcp`)

Nushell ships an MCP server in the binary. No package, no wrapper — the same
`nu` that runs your config also speaks MCP over stdio.

```json
{ "mcpServers": { "nu": { "type": "stdio", "command": "nu", "args": ["--mcp"] } } }
```

`serverInfo` reports `nushell-mcp-server`, version-locked to the binary.
Verified against **0.115.1**.

The point of using it over a plain shell tool is not that it runs commands —
everything runs commands. It is that the result stays **structured** and stays
**addressable**: a pipeline returns a table rather than bytes, and every result
is retained server-side so it can be re-sliced without re-running the command.

## The three tools

| Tool | Takes | Returns |
|---|---|---|
| `evaluate` | `input: string` — Nushell source | a NUON record (below) |
| `list_commands` | `find?: string` — substring over names, descriptions, search terms | the native command list |
| `command_help` | `name: string` | full signature, flags, input/output types, examples |

`list_commands` and `command_help` cover **native commands only**. They are the
cheap way to answer "does this command exist here, and what does it take" —
cheaper than `evaluate`-ing `help`, and they are how you check a signature
against *this* build rather than against memory. That matters more in Nushell
than elsewhere, because signatures move at minor versions
(`gotchas.md` § *Version drift*).

## The response record

`evaluate` returns NUON, not text:

| Field | Meaning |
|---|---|
| `cwd` | working directory *after* the command — `cd` persists |
| `history_index` | 0-based handle into `$history` for this result |
| `timestamp` | when it ran |
| `output` | the result, when it fits under the size limit |
| `note` | present **instead of** `output` when truncated |

A failed evaluation comes back as an MCP error whose content is a NUON error
record — code, message, labelled spans — so `nu::parser::*` versus
`nu::shell::*` is readable straight off the wire. That distinction is the first
thing to check when a snippet "should work"; see `gotchas.md` § *Parse time*.

## `$history` — why capping output is the wrong reflex

Every evaluation's **full** result is pushed to `$history: list<any>`. The
inline response may be truncated; the stored value never is.

So the size limiter belongs *after* the run, not inside it:

```nu
cargo build | complete          # response: { history_index: 7, note: ... }
$history.7.stderr | lines | where $it =~ '^error'
$history.7.stderr | lines | where $it =~ '^error' | skip 30 | first 30
```

Capping the live pipeline (`| first 20`, `o+e>| tail -50`, `head -c 500`)
throws away the rows you did not know you needed and buys nothing, because the
truncation you were trying to avoid is applied to the *response*, not to the
stored result. Paging a saved `$history.N` is fine; capping the first run of a
pipeline is not.

**Index by the number from the response, never `$history | last`.** Each call
pushes its own entry, so on the following call `last` refers to itself.

Cap generation only when generating costs something real — a metered API, a
paid model call. Local commands: just run them.

`complete` already splits `stdout`, `stderr` and `exit_code` into columns.
Reach for `o+e>| complete` only when the interleaved ordering is the thing you
need (a build log where warnings must stay adjacent to the lines they precede).

### Note on the tool description

`evaluate`'s own description still says "avoid commands that produce a large
amount of output" and offers `| first 5` as the `head -5` equivalent. The
server-level instructions contradict it and win. Treat the bash-translation
table in that description as a language crib, not as output policy.

## Session state persists

The server holds one persistent stack across calls — REPL semantics:

- `let x = ...` in one call is readable in the next.
- `$env.FOO = ...` persists, and so does `cd` (watch `cwd` in the response).
- External processes do **not** see `let` bindings, only environment variables.
- `use`, `source`, `plugin use`, `overlay use` persist too — including
  `plugin use polars` (`plugins.md`).

This is a real difference from a stateless `Bash` tool: setup done once is not
re-paid per call.

## Tunables

Set them like any other env var, on the persistent stack:

| Env var | Default | Effect |
|---|---|---|
| `NU_MCP_OUTPUT_LIMIT` | `10kb` | inline truncation threshold; `0b` disables |
| `NU_MCP_HISTORY_LIMIT` | `100` | entries retained; ring buffer, indices never shift |
| `NU_MCP_PROMOTE_AFTER` | `120sec` | how long a call may block before promotion |

```nu
$env.NU_MCP_OUTPUT_LIMIT = 50kb
$env.NU_MCP_PROMOTE_AFTER = 10min      # before a known-slow build
```

Eviction is ring-buffer: the oldest entry is dropped and surviving indices keep
their numbers, so an old `history_index` becomes unavailable rather than
pointing at the wrong result.

## Long-running work

A call exceeding `NU_MCP_PROMOTE_AFTER` — or cancelled by the client — is
auto-promoted to a background job. The call *errors* with a job id; the full,
untruncated output is delivered to the main thread's mailbox.

```nu
job list                       # still running?
job recv                       # blocks until the result lands
job recv --timeout 60sec
job kill 1
```

**Promoted jobs bypass `$history`** — collect them with `job recv`, not by
index. Either raise `NU_MCP_PROMOTE_AFTER` beforehand, or spawn deliberately:

```nu
job spawn { uvicorn main:app }                     # fire and forget
job spawn { ls | job send 0 }; job recv            # main thread is id 0
job spawn { "done" | job send 0 --tag 1 }; job recv --tag 1
```

Gotchas: the command is `job list`, not `job ls`. `job recv` reads only the
current job's mailbox and takes no id. `job send` always needs a target id.

## Using it well

1. **Ask the binary, not your memory.** `list_commands <term>` then
   `command_help <name>` before writing a pipeline you are unsure of.
2. **Keep data structured.** Native commands already return NUON — never
   `| to json` them. Externals return strings; parse once with `from json` /
   `from csv` / `lines` / `detect columns`, then filter natively.
3. **`http get` auto-parses** on `Content-Type` — no `from json`. Opt out with
   `--raw`.
4. **`glob`, not `find` or `ls -r`.** `ls **/*` descends hidden directories and
   buries the answer.
5. **Run it once, slice it afterwards.** The `history_index` is the whole
   mechanism; read it out of every response.
6. **`par-each` when order does not matter**, plain `each` when it does or when
   side effects must be serial.
7. **No `2>&1`** — Nushell has its own redirection (`o>`, `o+e>|`, `| ignore`).
   `&&` does not exist either; `;` sequences. See `data.md` for the full
   translation table.

Where this server does not help: it is one stateful shell, so it is not a
sandbox and not a parallelism primitive. Concurrency comes from `par-each` and
`job spawn` inside it, not from issuing overlapping calls.
