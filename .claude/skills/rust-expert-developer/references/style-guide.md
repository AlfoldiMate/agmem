# Rust Style Guide (rustfmt defaults)

Distilled reference for the formatting conventions the default Rust style enforces (and that `rustfmt` applies with default config). Use it to write code that matches the community default without running the formatter, and to understand *why* a rule exists.

Source: https://doc.rust-lang.org/nightly/style-guide/ (nightly Rust Style Guide, chapters: principles, items, statements, expressions, types, advice, cargo). All directives describe the **default** style; non-default styles / rustfmt options are not forbidden.

---

## Guiding principles (priority order)

Style decisions are made against these, roughly highest-priority first:

1. **Readability** — scan-ability; avoid misleading formatting; accessible (non-visual interfaces); readable in plain-text contexts (rustc errors, diffs, `grep`, no syntax highlighting).
2. **Aesthetics** — a sense of beauty; consistency with other languages/tools.
3. **Specifics** — version-control friendliness (clean diffs, merge-friendly); prevent rightward drift (excess indentation); minimise vertical space.
4. **Application** — easy to apply by hand; easy to implement in `rustfmt`/editors; internally consistent; simple rules.

If the guide and `rustfmt` disagree, treat it as a potential bug.

---

## Core whitespace & layout

- **Spaces, not tabs.** One indent level = **4 spaces**; all indentation (outside strings/comments) is a multiple of 4.
- **Max line width = 100 characters.**
- **Block indent, not visual indent** — smaller diffs, less rightward drift on rename.
  ```rust
  // Do (block indent)
  a_function_call(
      foo,
      bar,
  );
  // Don't (visual indent)
  a_function_call(foo,
                  bar);
  ```
- **Trailing commas** on every comma-separated item that is followed by a newline (last element of a multi-line list). Eases reordering/appending and shrinks diffs.
- **Blank lines:** separate items/statements by **zero or one** blank line (never two+).
- **No trailing whitespace** on any line (code, blank, comment) — mind string literals.

### Sorting ("version-sort")

Default ordering everywhere the guide says "sorted" is **version sort**: split each string into maximal non-digit and digit chunks; compare digit chunks by numeric value (ignoring leading zeroes); compare non-digit chunks lexically by Unicode except `_` sorts right after space and before other chars, and non-lowercase sorts before lowercase. Result: `u8, u16, u32, u128, usize`; `x86, x86_64, x87`.

---

## Comments

Recommendations (a mechanical formatter may leave comments alone):

- **Prefer line comments `//`** over block `/* */`.
- One space after `//`. For single-line block comments, one space inside each side: `/* x */`. For multi-line block comments, newline after `/*` and before `*/`.
- Prefer a comment on its own line; if trailing code, one space before `//`.
- Write complete sentences: capital first letter, ending period. (Inline block-comment notes may skip punctuation.)
- Full-line comments wrap at **80 chars** (incl. sigils, excl. indentation) or max width, whichever is smaller.

### Doc comments

- Prefer line doc comments `///` over `/** */`.
- Prefer **outer** doc comments (`///`); use **inner** (`//!`) only for module/crate-level docs.
- Put doc comments **before attributes**.

---

## Attributes

- Each attribute on its own line, indented to the item (inner `#![...]` indented to inside level).
- Prefer outer attributes.
- Format attributes with arg lists like function calls; split long ones across lines with trailing comma.
- Around `=`: one space each side — `#[foo = 42]`.
- Combine multiple derives into one, preserving order: `#[derive(Foo)] #[derive(Bar)]` → `#[derive(Foo, Bar)]`.
  ```rust
  #[repr(C)]
  #[long_multi_line_attribute(
      split,
      across,
      lines,
  )]
  struct CRepr { x: f32, y: f32 }
  ```

### "Small" items

Some constructs get a compact one-line form when "small" (heuristic on size/complexity, e.g. only simple names): `Foo { f1, f2 }` vs the multi-line block form.

---

## Items (module-level)

### Ordering at module top

- `extern crate` statements first, ordered alphabetically. (Note: `#[macro_use]` is not auto-moved — it can change semantics.)
- `use` statements and `mod` declarations come **before other items**; **imports before module declarations**.
- Version-sort within each group; `self`/`super` sort before other names.

### Functions

- Keep the signature on one line if it fits; avoid comments inside the signature. Layout: `[pub] [unsafe] [extern ["ABI"]] fn foo(a: i32, b: i32) -> i32 {`. Searchable by `fn name`.
- If it doesn't fit: break after `(` and before `)`, one arg per block-indented line, trailing comma:
  ```rust
  fn foo(
      arg1: i32,
      arg2: i32,
  ) -> i32 {
  ```

### Structs / unions

- Name on the `struct` line, `{` on same line, fields indented once with trailing commas, `}` on its own line unindented.
- Long field type → pull to its own line, indent again.
- Empty: prefer unit `struct Foo;` over `struct Foo {}` / `struct Foo();`.

### Tuple structs

- One line if possible: `pub struct Foo(String, u8);` — no trailing comma, no spaces around parens/semicolon. Multi-line form mirrors a multi-line call. For more than a few fields, prefer a named-field struct.

### Enums

- Each variant on its own block-indented line, formatted as struct / tuple-struct / identifier. Trailing comma on variants.
  ```rust
  enum FooBar {
      First(u32),
      Second,
      Error { err: Box<Error>, line: u32 },
  }
  ```
- Small struct-variant → single line with spaces inside braces, **no** trailing comma inside. If any struct variant is multi-line, make all struct variants multi-line.

### Traits

- Empty on one line: `trait Foo {}`. Otherwise break after `{` / before `}`.
- Bounds: space after `:` (not before), spaces around each `+`: `trait Foo: Debug + Bar {}`.
- Prefer a `where` clause over breaking a long bound list. If you must break: each bound (incl. first) on its own block-indented line, break **before** `+`, `{` on its own line.

### Impls

- Empty on one line: `impl Foo {}`. Otherwise break after `{`.
- If a non-inherent impl signature must break: break before `for`, block-indent the type, `{` on its own line.

### Generics

- Prefer one line: `fn foo<T: Display, U: Debug>(...)`. No space inside `< >`; space after `>` only before a word/`{` (not before `(`); space after each comma; no trailing comma on one line.
- Break the rest before breaking the generics clause; if the clause is large, prefer `where`.
- Multi-line: one param per block-indented line, break after `<` and before `>`, trailing comma. Prefer single-letter generic names.
- Associated-type bound: spaces around `=` → `<T: Example<Item = u32>>`.

### Where clauses

- If `where` follows a closing bracket, keep it on that line (`) where`); otherwise put `where` on a new line at the item's indent.
- One bound component per block-indented line, trailing comma (unless terminated by `;`); body/`=` starts on a new line after the clause. Very short → inline the bound on the type param instead.
  ```rust
  fn function<T, U>(args)
  where
      T: Bound,
      U: AnotherBound,
  {
      body
  }
  ```
- Multi-bound (`+`) in a where clause: break before each `+`, block-indent continuations, each bound on its own line.

### Type aliases

- One line: `pub type Foo = Bar<T>;`. Multi-line: break **before** `=`, block-indent the RHS.
- With a trailing where clause, break before `where`. With a *preceding* where clause, break after the last clause and do **not** indent before `=` (keep it visually distinct).

### Associated types

- Format like type aliases; bound gets space after `:`, none before: `pub type Foo: Bar;`.

### Extern items

- **Always specify the ABI**: `extern "C" fn foo`, `unsafe extern "C" { ... }` — not bare `extern fn` / `unsafe extern {}`.

### Modules / macro_rules

- `mod foo {` (spaces around keyword, space before `{`); `mod foo;` (no space before `;`). `macro_rules! foo { }`.

---

## Imports (`use`)

- One line where possible, no spaces inside braces:
  ```rust
  use a::b::c;
  use a::b::d::*;
  use a::b::{foo, bar, baz};
  ```
- **Prefer multiple `use` lines over one wrapped list.** If a list must wrap: break after `{` and before `}`, block-indent, trailing comma.
- **Groups:** blank-line-separated blocks are groups; version-sort *within* a group but **never merge or reorder groups**. A `#[macro_use]` starts a new group.
- **List ordering:** version-sort names, with `self`/`super` first and glob/nested groups last. Applies recursively.
- **Normalisations tools apply:** `use a::self;`→`use a;`; `use a::{};`→ removed; `use a::{b};`→`use a::b;`. Tools do **not** otherwise merge/un-merge lists or touch globs by default.
- **Nested imports force multi-line** even if they'd fit one line; each nested import on its own line, non-nested names grouped onto as few lines as possible.

---

## Statements

### `let`

- Space after `:` and around `=`; no space before `;`: `let pattern: Type = expr;`.
- Prefer one line. If it doesn't fit, break after `=` (block-indent expr); if still too wide, break after `:` too.
- If the expression's first line fits after `=`, keep it there; otherwise move it to the next block-indented line. For a block expr whose type/pattern spans multiple lines, put `{` on a new unindented line; otherwise `{` follows `=`.

### `let ... else`

- One line only if the whole statement is short, the `else` block is a single expression with no statements and no comments: `let Some(1) = opt else { return };`.
- Otherwise: never break between `else` and `{`; always break before `}`. If the `let` part fits one line, put `else {` on the initializer's line; if not, break before `else`.
- With a multi-line initializer, put `else {` on the initializer's last line **only if** that line is just closing brackets/parens/braces at the `let`'s indent level; otherwise put `else` on its own line at the `let` indent.

### Macros / expressions in statement position

- `a_macro!(...);` — use `()` or `[]`, terminate with `;`, no spaces around name/`!`/delimiters/`;`.
- No space before the `;` of an expression statement.
- Terminate statement expressions with `;` **unless** they end in a block or are the block's value. Use `;` for void-typed calls even if propagatable.

---

## Expressions

### Blocks

- Newline after `{` and before `}` unless it qualifies for one line. Keywords before a block (`unsafe`, `async`) go on the `{` line with one space.
- **Single-line block allowed** only when: used in expression position (or an `unsafe` block in statement position) **and** it holds one single-line expression, no statements, no comments — then spaces inside braces: `let _ = { a_call() };`.
- Empty block is `{}`. Never put comments on the brace lines. Block attributes go on their own line before the block.

### Closures

- No extra space before the first `|` (unless `move`); space between the second `|` and the body: `|a, b| expr`, `move |a: i32| -> i32 { ... }`.
- Omit `{}` when possible; add braces when there's a return type, statements, comments, or a multi-line control-flow body.

### Struct / tuple / enum / array literals

- Struct literal small → one line, no trailing comma, spaces inside braces, space before `{`, space after `:` only: `Foo { field1, field2: 0 }`. Else one field per block-indented line + trailing comma.
- Functional update `..expr`: treat like a field but **never** a trailing comma, and no space after `..`.
- Tuple `(a, b, c)` — no inner-paren spaces, comma+space between. Multi-line → one element per block-indented line + trailing comma. One-tuple keeps its trailing comma.
- Tuple struct: no space before `(` — `Foo(a, b, c)`.
- Enum literal follows struct-literal rules; qualify with the enum name unless it's in the prelude (`Foo::Bar(a, b)`, but `Ok(x)`).
- Array small → one line, no bracket-edge spaces, comma+space. Repeat form `[42; 10]` — space after `;` only, break **after** `;` if wrapping. `vec![...]` and array macros use `[]` and the same rules.
- **Unit `()`** and **nullary calls `func()`** never break, even past 100 cols.

### Operators

- Spaces around binary operators, incl. `=`, `+=`, `*=`: `x + 1`, not `x+1`. No space for unary: `!x`, `*x` — but `&mut x` keeps its space.
- Prefer dereferencing to referencing in expressions: prefer `*t == u` over `t == &u`.
- Parenthesise for intent/precedence; don't rely on spacing to show precedence; tools won't add/remove parens.
- **Line-breaking binary ops:** block-indent continuations, one sub-expression per line. For assignment ops break **after** the operator; for other ops put the operator at the **start** of the next line. Prefer breaking at an assignment op over other ops.
  ```rust
  foo_bar
      + bar
      + baz
  ```
- `as` casts: spaces around `as`; break **before** `as`, block-indent. Chained casts that fit after one break stay on that line.

### Indexing / ranges

- No spaces around `[]`; never break between target and `[`. If it must break, block-indent inside the brackets.
- Ranges have no spaces: `0..10`, `x..=y`, `..x.len()`, `foo..`. Break before the range operator if needed. Parenthesise compound bounds: `..(x + 1)`, `(x.f)..(x.f.len())`.

### Function / method calls

- No space between name and `(`, none inside parens, none before a comma, space after each comma, no trailing comma on one line: `foo(x, y, z)`, `x.foo().bar()`. No spaces around `.`. Prefer not to break the callee.
- Multi-line call (too wide / any arg or callee multi-line): one arg per block-indented line, break after `(` and before `)`, trailing comma.

### Macro uses & format-string macros

- If a macro parses like a known construct, format it that way (`foo!(a, b, c)` like a call). Guide only covers std/language macros, not third-party.
- Format-string macros (`println!`, `assert_eq!`): small args before the format string stay on one line, small args after stay on one line, the format string on its own line; otherwise one arg per line.
  ```rust
  assert_eq!(
      x, y,
      "x and y were not equal, see {}",
      reason,
  );
  ```

### Method / field chains

- A chain = sequence of field accesses, method calls, and `?`. Small → one line.
- Multi-line: each element on its own line, break **before** `.` and **after** `?`, block-indent each line. The first and second element may share a line if the first's last-line length ≤ the second line's indent (applied recursively).
- If any element is multi-line, put it and all later elements on their own lines. Prefer a uniform multi-line chain (each element one line) over mixing single/multi-line elements:
  ```rust
  self.pre_comment
      .as_ref()
      .map_or(false, |c| c.starts_with("//"))
  ```

### Control flow (`if`/`while`/`for`/`loop`/`match`)

- No extraneous parens on `if`/`while` conditions (but parens are fine to clarify arithmetic/logic).
- `} else {` / `} else if ... {` all on one line, one space each side of `else`.
- If a control line must break: block-indent the continuation and put `{` on its own line, unindented. Prefer breaking after `=` in an `if let`/`while let`, before `in` in a `for`. If the broken clause ends in closing brackets at the base indent, `{` may go on that line with a leading space.
- **Let-chains** one-line allowed only when: exactly two clauses, LHS is a literal/ident (optionally with unary prefixes), RHS is a single-line `let`: `if a && let Some(b) = foo() {`.
- **`if/else` as an expression** may be one line when in expression position, single `else`, small: `let y = if x { 0 } else { 1 };`.

### Match

- Break after `{` / before `}`, arms block-indented once. Don't line-break the discriminant.
- Trailing comma on an arm iff its body is **not** a block. Never start a pattern with `|`.
- Keep the LHS (before `=>`) on one line where possible; if the RHS stays on the same line, never wrap it in a block unless empty. Use a block RHS when the body has multiple statements, line comments, or won't fit on the LHS line. Never break right after `=>` without a block body.
- If a block RHS lets you avoid splitting the pattern, prefer that. If the pattern must split: one clause per line, break **before** `|`, no extra indent; an `if` guard that must break goes before `if`, block-indented, with a block body starting on a new line. Pack small clauses as many-per-line as fit.
  ```rust
  match foo {
      foo => bar,
      a_very_long_pattern
      | another_pattern if cond() => {
          do_thing()
      }
      baz => qux!(),
  }
  ```

### Combinable expressions

- When a call has a single multi-line argument (or similar paren-delimited multi-line construct — macro, tuple-struct, array, closure), format the outer call as if single-line if the result fits. Applied recursively:
  ```rust
  foo(bar(
      an_expr,
      another_expr,
  ))
  let x = foo(Bar { field: whatever });
  ```
- Multi-arg call whose **last** arg is a multi-line block closure (no other closures, first line fits) combines the same way: `foo(first, x, |p| { ... })`.

### Misc

- Hex literal letters may be upper or lower but not mixed in one literal; be consistent per project.
- Format patterns like their corresponding expressions.

---

## Types & bounds

Single-line spacing:

- Slice/array: `[T]`, `[T; expr]` (space after `;`, no bracket-edge spaces).
- Pointers: `*const T`, `*mut T` (no space after `*`). References: `&T`, `&'a mut T` (no space after `&`).
- Function type: `unsafe extern "C" fn(T, U) -> W` — single spaces around keywords/sigils, space after commas, no trailing comma, no bracket-edge spaces.
- Never type `!` treated like a name. Tuple type `(A, B, C)` — commas + space, no edge spaces, no trailing comma (except one-tuple).
- Paths / associated: `<Baz<T> as Trait>::Foo`, `Foo::Bar` — no spaces around `::` or angle brackets, single spaces around `as`.
- Generic: `Foo::Bar<T, U>` — comma+space, no trailing comma, no angle-bracket spaces.
- Bounds: `T + T + T`, `impl T + T` — single spaces around `+`.

Line-breaking:

- Avoid breaking types; break at the **outermost** scope first.
  ```rust
  // Prefer
  Foo<
      Bar,
      Baz<Type1, Type2>,
  >
  ```
- Break `[T; expr]` after the `;`. Function/generic types follow function/generics rules.
- Bound lists (`+`): break before `+`, block-indent; if you break one, break before **every** `+`.
- Precise-capturing `use<'a, T>` is formatted like a single path segment / trait bound named `use`.

---

## Non-formatting conventions

### Naming (casing)

| Item | Case |
|---|---|
| Types, enum variants, traits | `UpperCamelCase` |
| Struct fields, functions/methods, locals, macros, modules | `snake_case` |
| `const` / immutable `static` | `SCREAMING_SNAKE_CASE` |

- Reserved-word names: use a raw ident `r#crate` or trailing underscore `crate_` — **do not** misspell (`krate`).

### Style advice

- **Be expression-oriented:** `let x = if y { 1 } else { 0 };` over mutating `x` in each branch.
- Avoid `#[path]` annotations where possible.

---

## Cargo.toml conventions

- Same line width / indentation as Rust; **no indentation** of key names (start at column 0). One space each side of `=`.
- Blank line **between** sections (after the last pair, before the next header); **no** blank line between a header and its pairs, or between pairs.
- Version-sort keys within a section, **except** `[package]`: put it first, `name` then `version` at the top, other keys next, `description` last.
- Bare (unquoted) keys; quote only non-standard keys that require it — and avoid such keys.
- Use real multi-line strings (not `\n` escapes) for multi-line values like `description`.
- **Array values:** on the key's line if they fit; else block-indent one item per line with a trailing comma on every item and `]` alone on its own line.
- **Table values:** inline `{ ... }` if they fit; else break into a `[dependencies.name]` section.
  ```toml
  [dependencies]
  crate1 = { path = "crate1", version = "1.2.3" }

  [dependencies.extremely_long_crate_name_goes_here]
  path = "extremely_long_path_name_goes_right_here"
  version = "4.5.6"
  ```
- Metadata: authors as `Full Name <email@address>`; `license` a valid SPDX expression (`/` accepted for `OR`); `homepage` a full URL with scheme; `description` wraps at 80 cols, doesn't start with the crate name, first sentence on its own line.

---

## Quick checklist

- 4-space indent, **spaces not tabs**; max width **100**.
- **Block indent**, not visual indent; **trailing commas** on multi-line lists.
- No trailing whitespace; ≤ 1 blank line between items.
- Spaces around binary ops (`x + 1`), none for unary (`!x`) except `&mut x`.
- No space before `(` in calls; no spaces around `.`, `::`, `[]`; space after `:` in `let`/fields/bounds, none before.
- One `use` per line where reasonable; **version-sort within groups, never reorder/merge groups**; `self`/`super` first, globs last; nested imports force multi-line.
- Prefer `where` clauses over long inline bounds; break bound lists **before** every `+`.
- Method chains break **before** `.`, **after** `?`, block-indented; prefer uniform multi-line over mixed.
- Match: break after `{`; trailing comma only on non-block arms; never lead a pattern with `|`.
- `let x = if c { a } else { b };` — use expression-oriented style.
- Naming: `UpperCamelCase` types, `snake_case` values/fns/modules, `SCREAMING_SNAKE_CASE` consts.
- Always specify extern ABI (`extern "C"`); combine derives; doc comments before attributes.
- Cargo.toml: unindented keys, one space around `=`, blank line between sections, `[package]` first with `description` last.
