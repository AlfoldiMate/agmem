---
name: ast-grep-lite
description: The short ast-grep card preloaded into scout, verifier and architect — pattern, kind, has/inside with stopBy end, --debug-query. The full rule-writing workflow is the `ast-grep` skill; load that one in the main thread.
---

# ast-grep, the short card

`ast-grep` matches syntax, so it never hits strings or comments. Use it for
callers, definitions, implementors, and any "code shaped like X".

**Pattern** — one node, metavariables `$X` (one node) and `$$$` (many):

```bash
ast-grep run --pattern 'fn $NAME($$$) -> Result<$T, $E>' --lang rust src/
ast-grep run --pattern '$OBJ.unwrap()' --lang rust --json src/ | nu -c 'from json | get file | uniq'
```

**Rule** — when the match needs context. Relational rules take `stopBy: end`
or they stop at the first non-matching node:

```bash
ast-grep scan --inline-rules 'id: q
language: rust
rule:
  kind: function_item
  has:
    pattern: $X.await
    stopBy: end
  not:
    inside: { kind: impl_item, stopBy: end }' src/
```

Keys: `pattern`, `kind`, `regex`; `has`, `inside`, `precedes`, `follows`;
`all`, `any`, `not`. Combine with `all:` when one node needs several.

**When nothing matches**: the `kind` is probably wrong. Dump the tree and read
the node names:

```bash
ast-grep run --pattern 'impl $T for $U { $$$ }' --lang rust --debug-query=cst
```

**Shell escaping**: in double quotes write `\$X`; single quotes need nothing.

**No grammar for the language**: say so in one clause and fall back to `rg`.
The main thread's `/ast-grep-it <lang>` builds one.

Output stays a projection — `--json` piped to a count or a path list, never a
match dump in the reply.
