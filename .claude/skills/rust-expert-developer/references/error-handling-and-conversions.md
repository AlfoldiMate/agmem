# Error Handling & Conversions

Engineering patterns for Rust error handling and the conversion-trait family. Consult this before writing or
reviewing any code that returns `Result`/`Option`, defines an error type, chooses `thiserror` vs `anyhow`, decides
whether to panic, or implements `From`/`Into`/`TryFrom`/`FromStr`/`AsRef`. Distilled from the Microsoft *Rust Patterns*
book ch10 and the `rust-skills` `err-*`/`conv-*` rules. Cross-links the skill's canonical references:
`api-guidelines.md` (C-* codes), `microsoft-guidelines.md` (M-*), `design-patterns.md`, `style-guide.md`.

The one-line mental model: **`Result` for expected failures, `panic!` for bugs; `thiserror` for libraries,
`anyhow` for applications; `From` powers `?`; parse at the boundary so invalid states are unrepresentable.**

---

## 1. Error taxonomy: library vs application

The single most consequential decision. It determines your whole error strategy (see api-guidelines.md **C-GOOD-ERR**,
microsoft-guidelines.md on error types).

| | `thiserror` (libraries) | `anyhow` / `eyre` (applications) |
|---|---|---|
| **Use in** | Libraries, shared crates, public APIs | Binaries, CLI tools, top-level app code |
| **Error type** | Concrete `enum`/`struct` — callers can `match` | `anyhow::Error` — opaque, type-erased |
| **Effort** | Define your error enum + variants | Just return `Result<T>` (= `Result<T, anyhow::Error>`) |
| **Recovery** | Pattern-match variants | `error.downcast_ref::<MyError>()` if needed |
| **Guarantee** | Stable, documented failure modes | Ergonomic propagation + context, no type stability |

Rule of thumb (`err-thiserror-lib`, `err-anyhow-app`): if a caller might need to *react differently* to different
failures, give them a typed error (`thiserror`). If failures just need to be *reported with good context*, use
`anyhow`. A common hybrid: `thiserror` for the crate's public API, `anyhow` internally.

---

## 2. `thiserror` for typed library errors

`thiserror` derives `Display`, `std::error::Error` (including `source()`), and `From` impls from attributes —
no hand-written boilerplate. (Patterns Book ch10 §thiserror vs anyhow; `err-thiserror-lib`, `err-custom-type`.)

### WRONG — stringly-typed or hand-rolled

```rust
// String errors are not matchable — callers resort to substring checks.
fn validate_user(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name is empty".to_string()); // caller can only string-compare
    }
    Ok(())
}
```

Hand-writing `Display` + `Error` + `source()` for every variant is ~30 lines of tedium that `thiserror` generates.

### RIGHT — a typed enum

```rust
use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive] // forward-compatible: add variants without a breaking change (see §12)
pub enum ParseError {
    // Positional field interpolation.
    #[error("invalid value: {0}")]
    InvalidValue(String),

    // Named-field interpolation.
    #[error("invalid syntax at line {line}: {message}")]
    Syntax { line: usize, message: String },

    #[error("unexpected end of file")]
    UnexpectedEof,

    // #[from] auto-generates `From<Utf8Error>` AND sets this as the source.
    #[error("invalid utf-8 encoding")]
    Utf8(#[from] std::str::Utf8Error),

    // #[source] preserves the chain WITHOUT generating a From impl.
    #[error("i/o error reading input")]
    Io {
        #[source]
        source: std::io::Error,
        path: String,
    },

    // transparent: delegate Display AND source() to the inner error verbatim.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

Callers get precise, exhaustive matching:

```rust
# use thiserror::Error;
# #[derive(Error, Debug)] enum ParseError { #[error("eof")] UnexpectedEof, #[error("syn")] Syntax { line: usize, message: String } }
# fn parse(_: &str) -> Result<(), ParseError> { Ok(()) }
match parse("input") {
    Ok(()) => {}
    Err(ParseError::Syntax { line, message }) => eprintln!("line {line}: {message}"),
    Err(ParseError::UnexpectedEof) => eprintln!("file ended early"),
    Err(e) => eprintln!("{e}"),
}
```

Attribute cheat-sheet:

| Attribute | Effect |
|---|---|
| `#[error("...{0}...{field}...")]` | Generates `Display`; interpolates positional/named fields |
| `#[from]` | Generates `From<Inner>` **and** sets `source()` — enables `?` conversion |
| `#[source]` | Sets `source()` only (no `From`) — use when you also carry context fields |
| `#[error(transparent)]` | Forwards `Display` and `source()` to the single wrapped error |

**`#[from]` vs `#[source]` (`err-source-chain`):** `#[from]` implies `#[source]` and additionally generates the
`From` impl. Use `#[from]` for pure wrapping; use `#[source]` when the variant carries extra context fields (a `path`,
an `id`) so you construct it via `.map_err(...)` instead of `?`.

### Struct-based errors

For a single error shape with a wrapped cause (`err-custom-type`):

```rust
use thiserror::Error;

#[derive(Error, Debug)]
#[error("query failed for table '{table}' with filter '{filter}'")]
pub struct QueryError {
    pub table: String,
    pub filter: String,
    #[source]
    pub source: std::io::Error,
}
```

---

## 3. `anyhow` for applications

`anyhow::Error` is a type-erased, `Send + Sync + 'static` error that any `E: Error + Send + Sync + 'static` converts into. `Result<T>` aliases
`Result<T, anyhow::Error>`. (Patterns Book ch10 §anyhow; `err-anyhow-app`.)

### WRONG — `Box<dyn Error>` with no context

```rust
# struct Config;
fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string("app.toml")?; // WHICH file failed? no context
    todo!()
}
```

### RIGHT — `anyhow` with context

```rust,ignore
use anyhow::{anyhow, bail, ensure, Context, Result};

fn load_config() -> Result<Config> {
    let path = find_config()
        .context("failed to locate config file")?;         // static &str context

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config from {}", path.display()))?; // lazy

    let config: Config = toml::from_str(&content)
        .context("failed to parse config as TOML")?;

    ensure!(config.port > 0, "port must be positive, got {}", config.port); // assert-or-Err
    Ok(config)
}
```

The `anyhow` toolkit:

| Macro / method | Purpose |
|---|---|
| `anyhow!("msg {x}")` | Construct an ad-hoc error value |
| `bail!("msg")` | `return Err(anyhow!("msg"))` — early exit |
| `ensure!(cond, "msg")` | `if !cond { bail!("msg") }` — assertion that returns `Err` (not panic) |
| `.context("...")` | Wrap the error with a static message |
| `.with_context(\|\| ...)` | Wrap lazily — only runs on the error path (use for `format!`) |
| `err.downcast_ref::<T>()` | Recover the concrete underlying type when you must branch on it |
| `err.chain()` | Iterate the full cause chain |

`main` patterns:

```rust,ignore
use anyhow::Result;

// Idiomatic: `?` in main. Uses Debug formatting on error → prints the full chain.
fn main() -> Result<()> {
    let config = load_config()?;
    run_app(config)?;
    Ok(())
}

// Manual, when you want a custom exit code / clean single-line message:
fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e:#}"); // `{:#}` = one-line with causes
        std::process::exit(1);
    }
}
```

### Combining `thiserror` + `anyhow`

A library exposes typed errors; the application wraps them with `anyhow` context, and can still `downcast_ref` back
to the typed error when a specific one needs special handling (`err-anyhow-app`):

```rust,ignore
fn handle(err: anyhow::Error) {
    if let Some(api_err) = err.downcast_ref::<ApiError>() {
        match api_err {
            ApiError::RateLimited => wait_and_retry(),
            ApiError::NotFound(id) => log_missing(id),
        }
    }
}
```

---

## 4. Error context and cause chains

Raw errors lose the *what were we doing* information. Two ways to preserve it.

**With `anyhow`** — `.context()` / `.with_context()` build a chain (`err-context-chain`):

```
Error: failed to parse 'config.json'      // your context (top)
Caused by:
    0: expected ':' at line 5 column 12    // the underlying serde error
```

**With `thiserror`** — a `#[source]`/`#[from]` field makes `source()` return the cause, and any error reporter (or a
manual walk) reconstructs the chain (`err-source-chain`):

```rust
// Walk the chain manually for a std Error.
fn print_chain(error: &dyn std::error::Error) {
    eprintln!("error: {error}");
    let mut src = error.source();
    while let Some(e) = src {
        eprintln!("caused by: {e}");
        src = e.source();
    }
}
```

`context()` vs `with_context()`: `context(expr)` evaluates `expr` eagerly on every call — so `context(format!(...))`
allocates even on the success path — whereas `with_context(|| ...)` defers the closure to the error path. (A bare
`&'static str` costs nothing either way: anyhow stores it directly and only touches it on `Err`.) **Use `with_context`
whenever the message interpolates runtime data or is expensive** —
the happy path pays nothing.

Document error conditions with a `# Errors` doc section (`err-doc-errors`, see also `doc-errors-section.md`). Enable
`clippy::missing_errors_doc = "warn"` to enforce it on public fns:

```rust,ignore
/// Loads configuration from `path`.
///
/// # Errors
/// Returns [`ConfigError::NotFound`] if the file is missing,
/// [`ConfigError::Parse`] if the contents are not valid TOML.
pub fn load_config(path: &std::path::Path) -> Result<Config, ConfigError> { todo!() }
```

---

## 5. The `?` operator in depth

`?` is sugar for *match + `From` conversion + early return* (`err-question-mark`, Patterns Book ch10 §? Operator):

```rust
# fn operation() -> Result<i32, std::io::Error> { Ok(1) }
# fn f() -> Result<i32, std::io::Error> {
let value = operation()?;
// desugars to:
let value = match operation() {
    Ok(v) => v,
    Err(e) => return Err(From::from(e)), // <-- the From conversion is why #[from] matters
};
# Ok(value) }
```

That `From::from` is the entire reason `#[from]` / `impl From` exists: it lets `?` silently convert a foreign error
into your error type. Without it you must write `.map_err(...)` at every `?`.

`?` also works on `Option` inside a function returning `Option` (early-returns `None`):

```rust
fn first_word(text: &str) -> Option<&str> {
    let line = text.lines().next()?;          // None → return None
    let word = line.split_whitespace().next()?;
    Some(word)
}
```

Bridge `Option` → `Result` at a `?` with `.ok_or(...)` / `.ok_or_else(...)`:

```rust
# #[derive(Debug)] enum Error { Missing }
# fn f(map: &std::collections::HashMap<String, String>) -> Result<String, Error> {
let v = map.get("key").cloned().ok_or(Error::Missing)?;    // eager error value
let w = map.get("key").cloned().ok_or_else(|| Error::Missing)?; // lazy
# Ok(v) }
```

`?` in `main`: `fn main() -> Result<(), E>` works for any `E: Debug`; on `Err` the runtime prints the `Debug`
representation and exits with code `1`.

---

## 6. `From`-based error conversion

Implementing `From<SourceError> for YourError` is what enables clean `?`. Prefer deriving it with `#[from]`
(`err-from-impl`).

```rust
// Manual — only needed when NOT using thiserror.
#[derive(Debug)]
enum AppError {
    Io(std::io::Error),
    Parse(std::num::ParseIntError),
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self { AppError::Io(e) }
}
impl From<std::num::ParseIntError> for AppError {
    fn from(e: std::num::ParseIntError) -> Self { AppError::Parse(e) }
}

fn read_number(path: &str) -> Result<i32, AppError> {
    let s = std::fs::read_to_string(path)?; // io::Error → AppError via From
    let n = s.trim().parse::<i32>()?;        // ParseIntError → AppError via From
    Ok(n)
}
```

With `thiserror`, the same is one attribute per variant: `#[error("...")] Io(#[from] std::io::Error)`.

**Don't write a blanket `impl<E: Error> From<E> for AppError`** — it conflicts with the reflexive `From<T> for T` and
other impls, and erases which error you actually got. Add specific impls (or a typed variant) instead (`err-from-impl`).

When conversion needs extra context (a `path`), you cannot use `#[from]` (it takes only the inner error). Use
`#[source]` + `.map_err(...)`:

```rust,ignore
let content = std::fs::read_to_string(path)
    .map_err(|source| ConfigError::Read { path: path.into(), source })?;
```

---

## 7. When to panic vs return `Result`

The governing rule (`err-result-over-panic`, `anti-panic-expected`, Patterns Book ch10 §Panics): **`Result` for
expected/recoverable failures; `panic!` for bugs and violated invariants.** Libraries must almost never panic on
inputs they receive; applications may panic only at the top for truly unrecoverable states.

| Condition | Action |
|---|---|
| File not found, network error, timeout | `Result` |
| Invalid user input, parse failure, malformed data | `Result` |
| Index out of bounds from *user* data | `Result` (bounds-check first) |
| Index out of bounds from an *internal* bug | `panic!` (or leave the built-in panic) |
| Violated internal invariant | `panic!` / `assert!` |
| Unreachable / impossible state | `unreachable!()` |
| Not-yet-written code path | `unimplemented!()` / `todo!()` |
| Failed setup that makes the program pointless (top-level) | `panic!` / `expect` acceptable |
| Anything in tests / throwaway prototypes | `unwrap`/`expect` fine |

```rust
// panic! signals a BUG — don't "handle" it, fix the caller.
fn get(data: &[i32], i: usize) -> &i32 {
    &data[i] // out-of-bounds here means a programming error
}

// Result gives the caller a choice.
fn divide(a: i32, b: i32) -> Result<i32, &'static str> {
    if b == 0 { return Err("division by zero"); }
    Ok(a / b)
}
```

`catch_unwind` is for *boundaries only* — FFI edges (unwinding across `extern "C"` is UB) and thread/worker pools
where one task's panic must not kill the pool. It is **not** a general try/catch (Patterns Book ch10 §catch_unwind):

```rust
let result = std::panic::catch_unwind(|| risky());
match result {
    Ok(v) => println!("ok: {v}"),
    Err(_) => eprintln!("task panicked — continuing"),
}
# fn risky() -> i32 { 0 }
```

`process::abort()` is the sledgehammer: unrecoverable state, security violation, corrupt data — terminates with no
unwinding.

---

## 8. `unwrap` / `expect` discipline

`unwrap()` panics with no context; `expect("msg")` panics with your message. Both are still panics.
(`err-no-unwrap-prod`, `err-expect-bugs-only`, `anti-unwrap-abuse`, `anti-expect-lazy`.)

- **Never `unwrap()`/`expect()` on recoverable failures** — I/O, network, DB, user input, parsing. Propagate with `?`,
  supply a default, or handle explicitly.
- **`expect()` is only for invariants that indicate a bug.** Its message must explain *why the invariant holds*, so a
  future reader can find the broken assumption. Prefix with `BUG:` by convention.

```rust
// WRONG — user/environment failures dressed up as invariants:
// let cfg: Config = serde_json::from_str(input).expect("Invalid JSON"); // user error!
// let port: u16 = s.parse().expect("Invalid port");                     // user error!

// RIGHT — genuine invariants:
# use std::collections::HashMap;
# fn f(mut cache: HashMap<String, i32>) -> i32 {
cache.insert("k".into(), 1);
*cache.get("k").expect("BUG: key must exist immediately after insert")
# }
```

Good `expect` messages: `"BUG: HashMap entry exists after insert"`,
`"BUG: static regex compilation failed — regex syntax error in source"`. Bad: `"failed"`, `"should not be None"`.

Legitimate `unwrap`/`expect` sites (`anti-unwrap-abuse`): tests; `const`/`static` init of compile-time-known-valid
data (a literal regex); immediately after a check that guarantees success (prefer the `entry` API or `if let`);
mutex poisoning (`.lock().expect("poisoned")` — poisoning is itself a bug signal).

Alternatives table (`err-no-unwrap-prod`):

| Situation | Use instead of `unwrap()` |
|---|---|
| Can propagate | `?` |
| Sensible constant default | `.unwrap_or(x)` |
| `Default` value | `.unwrap_or_default()` |
| Default is expensive to compute | `.unwrap_or_else(\|\| ...)` |
| `Option` → `Result` at a `?` | `.ok_or(e)?` / `.ok_or_else(\|\| e)?` |
| Genuine invariant | `.expect("BUG: why")` |
| Branch on both arms | `match` / `if let` |

Enforce with `clippy::unwrap_used` and (stricter) `clippy::expect_used` set to `warn`, allowing per-item where justified.

---

## 9. Don't silently swallow errors

`let _ = result;`, `if let Err(_) = ... {}`, and `.ok()` discard errors — failures vanish and debugging becomes
impossible (`anti-empty-catch`). Every error deserves at least a log.

```rust,ignore
// WRONG
let _ = write_to_file(data);          // failure disappears
let value = risky().ok();             // error info lost

// RIGHT
if let Err(e) = write_to_file(data) { error!("failed to write file: {e}"); }
send_notification()?;                 // or propagate
```

Ignoring is acceptable only when *documented and truly non-actionable* — a best-effort metric, a TCP `shutdown` whose
error you cannot act on:

```rust,ignore
// INTENTIONAL: cleanup failure is not critical to the operation.
let _ = std::fs::remove_file(&tmp_path);
```

For batch work, collect failures instead of dropping them:

```rust,ignore
let (ok, failed): (Vec<_>, Vec<_>) = items.into_iter().map(process).partition(Result::is_ok);
if !failed.is_empty() { warn!("{} items failed", failed.len()); }
```

Lints: `clippy::let_underscore_drop`, `clippy::ignored_unit_patterns`.

---

## 10. Error message style

Follow the std-library convention (`err-lowercase-msg`): **lowercase first letter, no trailing punctuation.** Messages
compose into chains, so `"failed to read config"` reads cleanly as `"config load error: failed to read config: no
such file"`, whereas `"Failed to read config."` produces ugly `"...: Failed to read config.: ..."`.

```rust
# use thiserror::Error;
#[derive(Error, Debug)]
enum ConfigError {
    #[error("failed to read config file")]        // lowercase, no period
    Read(#[from] std::io::Error),
    #[error("key not found: {0}")]                // data goes at the end
    KeyNotFound(String),
    #[error("invalid JSON format")]               // acronyms keep their case
    Parse(String),
}
```

Keep proper nouns/acronyms cased (`JSON`, `HTTP`, `OAuth`). `Display` is the user-facing message (clean); `Debug` may
carry more detail for developers.

---

## 11. Conversion traits: `From` / `Into` / `TryFrom` / `TryInto`

The infallible/fallible conversion families (api-guidelines.md **C-CONV-TRAITS**). **Always implement the `From`-side,
never the `Into`-side** — the blanket `impl<T, U: From<T>> Into<U> for T` gives you `Into`/`TryInto` for free
(`api-from-not-into`, `conv-tryfrom-fallible`).

```rust
struct UserId(u64);

// Implement From — get Into automatically.
impl From<u64> for UserId {
    fn from(id: u64) -> Self { UserId(id) }
}
// Now BOTH work:
let a = UserId::from(42);
let b: UserId = 42u64.into();
```

Fallible conversions use `TryFrom` (never a bespoke `fn foo_from_bar`), giving callers `.try_into()?`
(`conv-tryfrom-fallible`):

```rust
#[derive(Debug)]
struct Port(u16);
#[derive(Debug)]
struct PortError(u32);
impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "port {} is out of range (0-65535)", self.0)
    }
}
impl std::error::Error for PortError {}

impl TryFrom<u32> for Port {
    type Error = PortError;
    fn try_from(v: u32) -> Result<Self, Self::Error> {
        u16::try_from(v).map(Port).map_err(|_| PortError(v))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let p: Port = 8080_u32.try_into()?; // standard idiom, integrates with ?
    println!("{}", p.0);
    Ok(())
}
```

Guidance: use a **concrete** error type on `TryFrom`, not `String`/`Box<dyn Error>`, so callers can match. If the
conversion is genuinely infallible, use `From` instead of `TryFrom`.

### Accepting conversions in APIs

`impl Into<T>` vs `impl AsRef<T>` — the ownership question (`api-impl-into`, `api-impl-asref`):

```rust
// Into<T>: you will OWN/STORE the value (may allocate).
fn set_name(name: impl Into<String>) { let name: String = name.into(); let _ = name; }
set_name("Alice");                 // &str
set_name(String::from("Alice"));   // String

// AsRef<T>: you only READ it (cheap borrow, no allocation).
fn count_bytes(data: impl AsRef<[u8]>) -> usize { data.as_ref().len() }
count_bytes("hello");
count_bytes(vec![1u8, 2, 3]);

// The canonical AsRef<Path> pattern — accepts &str, String, &Path, PathBuf, OsStr…
fn read_file(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<u8>> {
    std::fs::read(path.as_ref())
}
```

Decision matrix (`api-impl-asref`):

| Want | Trait |
|---|---|
| Single concrete type, simplest API | `&T` |
| Read-only, many input types | `AsRef<T>` |
| Mutable write target, many input types | `AsMut<T>` |
| Take ownership / store the value | `Into<T>` |
| HashMap/HashSet key lookup by borrowed form | `Borrow<T>` (Eq/Hash-consistent) |
| Smart-pointer transparent deref | `Deref<Target = T>` |

`AsMut<T>` is the mutable mirror (`conv-asmut-mutable`): `impl AsMut<[u8]>` accepts `&mut Vec<u8>`, `&mut [u8; N]`,
`&mut [u8]` with zero conversion cost. Reserve it for genuinely generic write targets — don't blanket every `&mut T`.

```rust
fn fill_zeros(mut buf: impl AsMut<[u8]>) {
    for b in buf.as_mut().iter_mut() { *b = 0; }
}
```

**`Borrow<T>` vs `AsRef<T>`:** both give a `&T` view, but `Borrow` additionally *promises* that the borrowed value
hashes/compares identically to the owner — which is why `HashMap<String, V>::get` takes `&Q where String: Borrow<Q>`
so you can look up with `&str`. Use `AsRef` for plain flexible borrowing; use `Borrow` only when Eq/Hash equivalence
is required.

### `FromStr` — the parsing hook

Implement `FromStr` to unlock `str::parse::<T>()`, clap `value_parser` auto-detection, and generic `T: FromStr` code.
Never expose a private `fn parse_foo(&str)` instead (`conv-fromstr-parsing`).

```rust
use std::str::FromStr;

#[derive(Debug, PartialEq)]
enum Color { Red, Green, Blue }

#[derive(Debug)]
struct ParseColorError(String);
impl std::fmt::Display for ParseColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown color: {}", self.0)
    }
}
impl std::error::Error for ParseColorError {}

impl FromStr for Color {
    type Err = ParseColorError; // concrete Err, NOT String — so callers can match
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "red" => Ok(Color::Red),
            "green" => Ok(Color::Green),
            "blue" => Ok(Color::Blue),
            other => Err(ParseColorError(other.to_owned())),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let c: Color = "green".parse()?; // the idiom
    assert_eq!(c, Color::Green);
    Ok(())
}
```

Notes: pair `FromStr` with `Display` (round-trip); for *infallible* string wrapping prefer `From<&str>`/`From<String>`.

---

## 12. Parse, don't validate (type-state safety)

The overarching design principle that makes error handling *shrink*: instead of validating a value everywhere it's
used and hoping you never forget, **parse it once at the boundary into a type that can only hold valid data.** Invalid
states become unrepresentable, so downstream code needs no checks (`api-parse-dont-validate`, `anti-stringly-typed`;
see api-guidelines.md **C-NEWTYPE**, `design-patterns.md` newtype idiom, `api-typestate.md`).

### WRONG — scattered validation on raw strings

```rust,ignore
fn send_email(email: &str) -> Result<(), Error> {
    if !is_valid_email(email) { return Err(Error::InvalidEmail); } // did the caller already check?
    smtp_send(email)
}
fn add_to_list(email: &str) -> Result<(), Error> {
    if !is_valid_email(email) { return Err(Error::InvalidEmail); } // duplicated — or forgotten
    Ok(())
}
```

### RIGHT — a validated newtype (constructor is the only door)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String); // field private → cannot construct an invalid Email

#[derive(Debug)]
pub enum EmailError { Invalid }

impl Email {
    /// The single validation point.
    pub fn parse(s: impl Into<String>) -> Result<Self, EmailError> {
        let s = s.into();
        if s.contains('@') && s.len() > 3 { Ok(Email(s)) } else { Err(EmailError::Invalid) }
    }
    pub fn as_str(&self) -> &str { &self.0 }
}
impl AsRef<str> for Email { fn as_ref(&self) -> &str { &self.0 } }
impl std::fmt::Display for Email {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

// Downstream fns take `Email` and NEVER re-validate — the type is the proof.
fn send_email(email: &Email) { let _ = email.as_str(); }
```

Related tactics:

- **Enums over stringly-typed states** (`anti-stringly-typed`): `OrderStatus::Completed`, not `"completed"`. Exhaustive
  `match` catches missing cases at compile time; swapped/typo'd string arguments become compile errors. Pair with
  `FromStr`/`Display` (and serde `#[serde(rename_all = "snake_case")]`) to parse at the boundary.
- **Bounded newtypes**: `Port(u16)` where `new` rejects `0`; `Percentage(u8)` where `new` rejects `> 100`.
- **Validate-once, `expect`-after** (`err-expect-bugs-only`): once a value is a `ValidatedEmail`, methods on it may
  `expect("BUG: ValidatedEmail always contains @")` because the invariant is now type-guaranteed.
- **`#[non_exhaustive]` on public error enums** (`api-non-exhaustive.md`): lets you add variants later without a
  breaking change, but forces downstream `match` to include a `_` arm.

---

## Rules & anti-patterns checklist

Distilled from the assigned `err-*`, `conv-*`, and related anti-pattern rules. Each: id — DO/DON'T — reason.

- **err-thiserror-lib** — DO use `thiserror` for library error types. Callers get typed, matchable, documented failures
  with no `Display`/`Error` boilerplate.
- **err-anyhow-app** — DO use `anyhow`/`eyre` in applications & CLIs. Ergonomic propagation + context; skip typed enums
  you don't need. Still `downcast_ref` when you must branch.
- **err-custom-type** — DO define domain error enums/structs instead of `String`/`Box<dyn Error>`. Makes failure modes
  part of the API contract and enables pattern matching.
- **err-question-mark** — DO propagate with `?`, not `match`/`unwrap`. `?` = match + `From::from` + early return.
- **err-from-impl** — DO implement `From<Source>` (or `#[from]`) so `?` auto-converts. DON'T write a blanket
  `impl<E: Error> From<E>` — it conflicts and erases type info.
- **err-source-chain** — DO preserve causes with `#[source]`/`#[from]` (or a manual `source()`). Never collapse a cause
  to `e.to_string()` — you lose the chain. `#[from]` = `From` + source; `#[source]` = source only.
- **err-context-chain** — DO add context with `.context()`/`.with_context()`. Use `with_context` for
  runtime/interpolated messages so the happy path pays nothing.
- **err-result-over-panic** — DO return `Result` for recoverable errors; libraries must not panic on inputs. Panic is
  only for bugs.
- **err-no-unwrap-prod** — DON'T `unwrap()` in production. Use `?`, `unwrap_or*`, `ok_or*`, or `expect` for invariants.
- **err-expect-bugs-only** — DO reserve `expect()` for invariants that indicate a bug; message explains *why* the
  invariant holds (prefix `BUG:`). Not for user/IO/network errors.
- **err-lowercase-msg** — DO start error messages lowercase with no trailing punctuation, so chained messages compose
  cleanly. Keep acronyms/proper nouns cased.
- **err-doc-errors** — DO document failure conditions under a `# Errors` doc section; enable
  `clippy::missing_errors_doc`. Callers need to know what returns `Err`.
- **conv-tryfrom-fallible** — DO implement `TryFrom` for fallible conversions (gives `TryInto` free); concrete `Error`
  type, not `String`. Never an ad-hoc `fn x_from_y`.
- **conv-fromstr-parsing** — DO implement `FromStr` for string→type parsing to enable `.parse()` and clap integration;
  concrete `Err` type. Pair with `Display`.
- **conv-asmut-mutable** — DO accept `impl AsMut<T>` for generic mutable write targets (Vec/array/slice) with zero cost;
  don't over-apply to every `&mut T`.
- **api-from-not-into** — DO implement `From`, never `Into` (blanket impl gives `Into` free). Enforce with
  `clippy::from_over_into`.
- **api-impl-into** — DO accept `impl Into<T>` when you'll own/store the value; implement `From` for the conversion.
  Avoid on hot paths and where the type must be named.
- **api-impl-asref** — DO accept `impl AsRef<T>` when you only borrow (read) the value; canonical for `AsRef<Path>` and
  `AsRef<str>`. Use `Borrow` when Eq/Hash equivalence matters.
- **api-parse-dont-validate** — DO parse inputs into validated newtypes at the boundary so invalid states are
  unrepresentable; downstream code skips checks.
- **anti-unwrap-abuse** — DON'T `.unwrap()` in production; OK in tests, const/static init, or after a proven check.
- **anti-panic-expected** — DON'T `panic!`/`expect` on expected failures (network/file/input). Return `Err`. Panic
  only for bugs/invariants/unrecoverable init.
- **anti-expect-lazy** — DON'T reach for `.expect()` on recoverable errors just because it's shorter than `?`. It's
  still a panic.
- **anti-empty-catch** — DON'T silently drop errors (`let _ =`, empty `Err(_) => {}`, `.ok()`). At minimum log; ignore
  only when documented and non-actionable.
- **anti-stringly-typed** — DON'T model fixed value sets or semantic values as `String`. Use enums/newtypes; the
  compiler then catches typos and swapped arguments.

---

## Gotchas / footguns

- **`?` needs the conversion to exist.** `error[E0277]: ? couldn't convert the error to X` means there's no
  `From<Source> for X`. Add a `#[from]` variant, an `impl From`, or a `.map_err(...)`. In `anyhow` this always works
  because `anyhow::Error: From<E>` for any `E: Error + Send + Sync + 'static`.
- **`#[from]` demands a unique source type per variant.** Two `#[from]` variants with the same inner type won't
  compile (ambiguous `From` impl). Disambiguate by wrapping one in a newtype or using `#[source]` + `map_err`.
- **`anyhow::Error` is not `std::error::Error`.** It's a distinct type. A library function returning it forces
  `anyhow` on all callers and denies them matching — that's why libraries use `thiserror`.
- **`{}` vs `{:#}` vs `{:?}` on `anyhow::Error`.** `{}` prints only the top message; `{:#}` prints the one-line cause
  chain; `{:?}` prints the chain *plus* a backtrace when `RUST_BACKTRACE=1`. `fn main() -> Result` uses `{:?}`.
- **`.context()` on `Option`.** `anyhow`'s `Context` is implemented for `Option` too — `opt.context("missing X")?`
  turns `None` into an error. Don't reach for `.ok_or_else(|| anyhow!(...))`.
- **`with_context` closure runs only on `Err`.** Never put side effects in it; it won't run on success.
- **`std::io::Error` loses its `ErrorKind` through `.to_string()`.** If you flatten a cause to a string you can no
  longer test `if e.kind() == ErrorKind::NotFound`. Keep the typed source.
- **Implementing `Into` directly** silently disables `T::from(...)` call syntax and trips `clippy::from_over_into`.
  Always implement `From`.
- **`TryFrom<i32> for u8` etc. already exist.** Don't reimplement standard numeric conversions; reach for the built-in
  `u8::try_from(n)?`. Reserve your `TryFrom` for domain types.
- **`FromStr` can't borrow from the input.** `type Err` and the output are owned; `fn from_str(&str)` has no lifetime
  tying the result to the argument. To hold a borrowed slice, write a dedicated `fn parse(&'a str) -> ...` instead.
- **`AsRef` is not transitive and can be ambiguous.** A type with `AsRef<str>` and `AsRef<[u8]>` needs a turbofish or
  type annotation when both fit. Also, `impl AsRef<Path>` accepts `&str` but *not* raw bytes — mind the target type.
- **`catch_unwind` requires `UnwindSafe`.** Captured `&mut`/`RefCell` state may not be unwind-safe; you'll need
  `AssertUnwindSafe`. And it only catches unwinding panics — with `panic = "abort"` there is nothing to catch.
- **Panics across an FFI boundary are UB.** Any `extern "C"` callback that can panic must wrap its body in
  `catch_unwind` and convert to an error code.
- **`#[non_exhaustive]` forces a `_` arm downstream** — good for the library's evolution, but means external code can
  never exhaustively match; keep the common variants stable and documented.

---

## Cheat-sheet

**Choosing the error strategy**

| You are writing… | Error approach |
|---|---|
| A library / public API | `thiserror` enum, `#[non_exhaustive]`, `## Errors` docs |
| An application / CLI / binary | `anyhow::Result`, `.context(...)`, `?` in `main` |
| A library internal helper | Either; `anyhow` internally + convert at the public edge is fine |
| Code that must branch on failure | Typed enum (or `downcast_ref` from `anyhow`) |

**`thiserror` attributes**

| Attribute | Generates | Sets `source()` |
|---|---|---|
| `#[error("...")]` | `Display` | — |
| `#[from]` | `From<Inner>` | ✅ |
| `#[source]` | — | ✅ |
| `#[error(transparent)]` | delegates `Display` | delegates |

**Panic vs Result quick call**

| Kind of failure | Use |
|---|---|
| Expected / external (IO, net, input, parse) | `Result` + `?` |
| Bug / violated invariant | `panic!` / `assert!` / `expect("BUG: …")` |
| Impossible branch | `unreachable!()` |
| Unwritten code | `todo!()` / `unimplemented!()` |
| Corrupt/unsafe state | `process::abort()` |
| FFI / worker-pool boundary | `catch_unwind` |

**`Option`/`Result` combinators instead of `unwrap`**

| Goal | Call |
|---|---|
| Propagate | `?` |
| Constant default | `.unwrap_or(x)` |
| `Default` default | `.unwrap_or_default()` |
| Lazy default | `.unwrap_or_else(\|\| ...)` |
| `Option`→`Result` | `.ok_or(e)?` / `.ok_or_else(\|\| e)?` |
| `Result`→`Option` (drop error) | `.ok()` — only if intentional & logged |
| Add context (anyhow) | `.context(...)` / `.with_context(\|\| ...)` |
| Invariant | `.expect("BUG: …")` |

**Conversion traits**

| Trait | Direction | Fallible? | Call site | Implement |
|---|---|---|---|---|
| `From<T>` | `T → Self` | no | `Self::from(x)` / `x.into()` | ✅ implement this |
| `Into<T>` | `Self → T` | no | `x.into()` | ❌ derive via `From` |
| `TryFrom<T>` | `T → Self` | yes | `Self::try_from(x)?` / `x.try_into()?` | ✅ implement this |
| `TryInto<T>` | `Self → T` | yes | `x.try_into()?` | ❌ derive via `TryFrom` |
| `FromStr` | `&str → Self` | yes | `s.parse::<Self>()?` | ✅ for parseable types |
| `AsRef<T>` | `&Self → &T` | no (borrow) | `x.as_ref()` | ✅ for borrow views |
| `AsMut<T>` | `&mut Self → &mut T` | no (borrow) | `x.as_mut()` | ✅ for mutable views |

**API parameter choice**

| Need | Parameter |
|---|---|
| Own/store the value | `impl Into<T>` |
| Read-only, flexible | `impl AsRef<T>` |
| Mutate, flexible | `impl AsMut<T>` |
| Map key lookup | `&Q where K: Borrow<Q>` |
| One concrete type | `&T` / `T` |

Cross-references: api-guidelines.md (**C-GOOD-ERR**, **C-CONV-TRAITS**, **C-NEWTYPE**, **C-QUESTION-MARK**),
microsoft-guidelines.md (M-* error/type guidance), design-patterns.md (newtype idiom, anti-patterns),
`api-non-exhaustive.md`, `api-typestate.md`, `doc-errors-section.md`.
