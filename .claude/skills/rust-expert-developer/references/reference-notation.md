# Rust Reference — Grammar Notation

Distilled reference for the notation used in the **Lexer** and **Syntax** grammar snippets throughout *The Rust Reference*. Use it to decode any grammar production you encounter in the Reference.

Source: https://doc.rust-lang.org/reference/notation.html

## Notation table

Every form used in grammar snippets, with an example and its meaning:

| Notation | Example | Meaning |
|----------|---------|---------|
| `CAPITAL` | `KW_IF`, `INTEGER_LITERAL` | A token produced by the lexer (terminal, ALL_CAPS). |
| _ItalicCamelCase_ | _LetStatement_, _Item_ | A syntactical production (non-terminal). |
| `` `string` `` | `` `x` ``, `` `while` ``, `` `*` `` | The exact character(s), literally. |
| x? | `` `pub` ``? | An optional item (0 or 1). |
| x* | _OuterAttribute_* | 0 or more of x. |
| x+ | _MacroMatch_+ | 1 or more of x. |
| xa..b | `HEX_DIGIT`1..6 | a to b repetitions of x, **exclusive** of b. |
| xa..=b | `HEX_DIGIT`1..=5 | a to b repetitions of x, **inclusive** of b. |
| xn:a..=b | `` `#` ``n:1..=255 | a to b repetitions of x (inclusive of b), with the count **bound to the name n**. |
| xn | `` `#` ``n | x repeated the number of times bound to n by a previous labeled repetition. |
| Rule1 Rule2 | `` `fn` `` _Name_ _Parameters_ | Sequence of rules, in order. |
| `|` | `` `u8` `` \| `` `u16` ``, _Block_ \| _Item_ | Alternation — either one or the other. |
| `!` | !_COMMENT_ | Negative lookahead: matches if the expression does **not** follow, **without consuming** input. |
| `[ ]` | [`` `b` `` `` `B` ``] | Any **one** of the characters listed. |
| `[ - ]` | [`` `a` ``-`` `z` ``] | Any one character in the **range**. |
| `~[ ]` | ~[`` `b` `` `` `B` ``] | Any character **except** those listed. |
| `` ~`string` `` | `` ~`\n` ``, `` ~`*/` `` | Any characters **except** this sequence. |
| `( )` | (`` `,` `` _Parameter_)? | Groups items (for applying `?`, `*`, `+`, etc.). |
| `^` | `` `b'` `` ^ _ASCII_FOR_CHAR_ | **Hard cut operator**: once everything to its left matched, the rest of the sequence must match or parsing fails unconditionally. |
| U+xxxx..xxxxxx | U+0060 | A single Unicode character (by code point). |
| `<text>` | \<any ASCII char except CR\> | An English description of what should be matched. |
| _Rule_ suffix | _IDENTIFIER_OR_KEYWORD_ _except_ `` `crate` `` | A modification to the previous rule. |
| `// Comment.` | `// Single line comment.` | A comment extending to the end of the line. |

**Precedence:** Sequences bind tighter than `|` alternation. So `A B | C` means `(A B) | C`, not `A (B | C)`.

## The hard cut operator (`^`)

- The grammar uses **ordered alternation**: alternatives are tried left-to-right, taking the first that matches; on partial failure the parser normally **backtracks** to the next alternative.
- `^` **disables backtracking** for the rest of a sequence: once every expression left of `^` has matched, everything after it must match or lexing/parsing fails outright.
- Needed because some tokens start with a prefix that is itself a valid token.
  - Example: `c"…"` begins a C string literal, but `c` alone is a valid identifier. Without a cut after `c"`, a malformed `c"\0"` could backtrack and re-lex as identifier `c` + string `"\0"`. The cut prevents that once the opening delimiter is recognized.

## String table productions

- Unary operators, binary operators, and keywords are often given in simplified form: a plain listing of printable strings.
- These are a subset of the token rules, assumed to be produced by a **lexical-analysis phase** (a DFA over the disjunction of all such string-table entries) that feeds the parser.
- Any string in `monospace` font inside the grammar is an **implicit reference to a single member** of such a string-table production. (See the Reference's tokens documentation for detail.)

## Grammar visualizations

- Each grammar block has a toggle to show a **syntax (railroad) diagram**.
- In diagrams: a **square** element is a non-terminal rule; a **rounded rectangle** is a terminal.

## Quick checklist

- `CAPITAL` = lexer token (terminal); _ItalicCamelCase_ = production (non-terminal); `` `string` `` = literal chars.
- Repetition: `?` = 0/1, `*` = 0+, `+` = 1+.
- Bounded repetition: `a..b` exclusive of b; `a..=b` inclusive of b.
- Named repetition: `xn:a..=b` binds the count to n; later `xn` repeats exactly n times.
- Character classes: `[ ]` = any listed, `[a-z]` = range, `~[ ]` = any except listed, `` ~`string` `` = any except that sequence.
- `!expr` = negative lookahead (no input consumed); `<text>` = prose description; `except` suffix modifies the prior rule.
- `( )` groups; sequence binds tighter than `|` alternation.
- `^` = hard cut: past this point in the sequence there is no backtracking — matching must succeed or parsing fails.
- Ordered alternation: first matching alternative wins.
