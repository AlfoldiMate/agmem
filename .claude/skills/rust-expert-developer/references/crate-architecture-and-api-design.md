# Crate Architecture & API Design

How to structure a crate or workspace and design its public surface: module tree and visibility, the facade/prelude re-export pattern, feature flags and additive features, semver-safe evolution (`#[non_exhaustive]`, sealed traits, `#[must_use]`), the newtype-at-boundaries idiom, and dependency hygiene. Consult this when laying out a new crate, curating a `lib.rs`, adding a feature flag, deciding whether a change is a breaking change, or reviewing a public API for ergonomics and forward-compatibility. Distilled from the Microsoft *Rust Patterns* book (ch00 intro, ch15 API Design, ch18 reference card, ch19 capstone) and the `api-*` rule catalog. The canonical rule codes live in the sibling references — cross-linked throughout: **api-guidelines.md** (C-* Rust API Guidelines), **microsoft-guidelines.md** (M-* Pragmatic Guidelines), **design-patterns.md** (idioms/anti-patterns), **style-guide.md** (Cargo.toml + rustfmt).

---

## Book framing: what "leveling up" means here

The *Rust Patterns* book is explicitly for developers who finished *The Rust Programming Language* but hit "how do I actually **design** this?" (Patterns Book ch00 §Who This Is For). API design is chapter 15's checkpoint — "Can apply the *parse, don't validate* pattern" — and the through-line of the whole book: **accept the most general type, return the most specific; make invalid states unrepresentable; parse at the boundary; never break downstream code silently** (ch15 §Key Takeaways). This file is the whole-topic source of truth; the capstone (§ below) shows it composed into one crate.

---

## Module tree & visibility

Standard crate layout (Patterns Book ch15 §Module Layout Conventions):

```text
my_crate/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Crate root — re-exports and public API (the facade)
│   ├── config.rs       # Feature module
│   ├── parser/         # Complex module with sub-modules
│   │   ├── mod.rs      # or parser.rs at parent level (Rust 2018+ path style)
│   │   ├── lexer.rs
│   │   └── ast.rs
│   ├── error.rs        # Error types
│   └── utils.rs        # Internal helpers (pub(crate))
├── tests/              # Integration tests — see the crate as an external user does
├── benches/            # cargo bench
└── examples/           # cargo run --example basic
```

Prefer `parser.rs` + `parser/` (2018 path style) over `parser/mod.rs`; both are valid, the former keeps the file discoverable next to its siblings.

**Visibility modifiers** (ch15; also ch18 §Module Visibility Quick Reference):

| Modifier | Visible to | Use for |
|----------|-----------|---------|
| `pub` | Everyone | The curated public surface only |
| `pub(crate)` | This crate | Cross-module internals (helpers, shared types) |
| `pub(super)` | Parent module | Tight coupling within a subsystem |
| `pub(in path)` | A specific ancestor | Rare; surgical exposure |
| *(none)* | Current module + children | Default — keep it private until proven public |

Rule: **modules are private; the crate root decides what is public.** `mod` declarations are private by default, so `mod parser;` keeps everything in `parser` internal until you re-export specific items. A field being `pub` inside a `pub(crate)` module is still invisible to the outside world — visibility is the *minimum* of every path segment.

Keep struct fields private and expose accessors (C-STRUCT-PRIVATE in api-guidelines.md) so you can change representation without breaking callers.

---

## The facade / re-export pattern

Curate `lib.rs` so users import from the crate root, not from your internal module tree (Patterns Book ch15). This decouples the public API from the file layout — you can move `Parser` between modules without a breaking change.

```rust
// lib.rs — the facade: internal modules are private, public types re-exported flat.
mod config;
mod error;
mod parser;
mod utils; // never re-exported — pub(crate) helpers stay internal

pub use config::Config;
pub use error::Error;
pub use parser::Parser;

// Users write:  use my_crate::Config;
// NOT:          use my_crate::config::Config;   // config is a private module
```

**Prelude module** — for crates with many traits that must be in scope to call methods (extension traits especially), offer a `prelude` so users glob-import the essentials:

```rust
// lib.rs
pub mod prelude {
    pub use crate::{Config, Parser};
    pub use crate::ext::ByteSliceExt; // trait must be in scope to call its methods
}
// Downstream:  use my_crate::prelude::*;
```

Re-export names must remain *stable* — a re-export is part of your public API. Renaming or removing a `pub use` is a breaking change just as if you'd renamed the type (C-STABLE).

**Re-export dependency types you expose** (C-RE-EXPORT idea; API Guidelines): if a function returns a `http::StatusCode` or takes a `serde_json::Value`, re-export it (`pub use http;`) or callers must add and version-match that dependency themselves.

---

## Public API design checklist

The book's polished-crate checklist (Patterns Book ch15 §Public API Design Checklist) — each maps to a canonical guideline:

1. **Accept references, return owned** — `fn process(input: &str) -> String`. Be permissive in inputs, specific in outputs (C-CALLER-CONTROL, C-PERMISSIVE).
2. **`impl Trait` for parameters** — `fn read(r: impl Read)` reads cleaner than `fn read<R: Read>(r: R)` when the type param is used once.
3. **Return `Result`, not `panic!`** — callers decide (see anti-panic-expected in the checklist below).
4. **Implement the standard traits eagerly** — `Debug`, `Clone`, `Default`, `PartialEq`, `From`/`Into` (C-COMMON-TRAITS; §Common traits below).
5. **Make invalid states unrepresentable** — newtypes + typestate (§ below).
6. **Builder for complex config** — typestate builder when fields are required (api-builder-pattern).
7. **Seal traits you don't want implemented** (§ Sealed traits; C-SEALED).
8. **`#[must_use]` on values that are bugs to discard** (§ below).

Add: **`#[non_exhaustive]` on public enums/structs that may grow** (§ below), and **name conversions/getters per convention** (C-CONV, C-GETTER in api-guidelines.md).

---

## Ergonomic parameters: `impl Into` / `AsRef` / `Cow` / `Borrow`

The Rust-specific "be liberal in what you accept" (Patterns Book ch15 §Ergonomic Parameter Patterns). Decision tree:

```text
Do you need to OWN the data inside the function?
├── YES → fn f(x: impl Into<T>)      "give me anything that becomes a T"; x.into() once
└── NO  → only need to READ it?
     ├── YES → fn f(x: impl AsRef<U>) or &U   "give me anything I can borrow as &U"
     └── MAYBE (modify sometimes) → Cow<'_, U>  borrow if possible, clone only when you must
```

```rust
use std::borrow::Cow;
use std::path::{Path, PathBuf};

fn connect(host: impl Into<String>) { let _host = host.into(); }   // owns
fn file_exists(path: impl AsRef<Path>) -> bool { path.as_ref().exists() } // reads
fn store(path: impl Into<PathBuf>) { let _p = path.into(); }

/// Only allocates if it must modify — otherwise borrows through.
fn normalize(msg: &str) -> Cow<'_, str> {
    if msg.contains('\t') {
        Cow::Owned(msg.replace('\t', "    "))
    } else {
        Cow::Borrowed(msg)
    }
}

fn main() {
    connect("localhost");            // &str — no .to_string()
    connect(String::from("db"));     // String — moved, no clone
    let _ = file_exists("/tmp/x");   // &str, String, &Path, PathBuf all work
    store("/etc/app");
    let _ = normalize("clean");      // Borrowed — free
}
```

| Pattern | Ownership | Allocation | Use when |
|---------|-----------|------------|----------|
| `&str` / `&[u8]` | Borrowed | Never | Simplest read-only param |
| `impl AsRef<str>` | Borrowed | Never | Accept `String`/`&str`/`Cow`, read only (api-impl-asref) |
| `impl Into<String>` | Owned | On conversion | Will store/own it (api-impl-into) |
| `Cow<'_, str>` | Either | Only if modified | Processing that usually doesn't modify |

**`Borrow<T>` vs `AsRef<T>`** (ch15 sidebar): both hand you `&T`, but `Borrow<T>` additionally promises `Eq`/`Ord`/`Hash` are *consistent* between owned and borrowed forms. That is why `HashMap<String, V>::get` takes `&Q where String: Borrow<Q>` — not `AsRef`. Use `Borrow` for lookup keys, `AsRef` for general "give me a reference." Cross-link api-guidelines.md C-GENERIC.

**Don't overuse `impl Into`** (api-impl-into §When NOT to Use): not on trait-object params (needs `Sized`), not in struct fields (`impl Trait` isn't allowed there), not on billion-call hot paths (take the concrete type), and not in return position when callers need to name the type.

---

## Newtype at boundaries

Wrap primitives in distinct types so the compiler catches swapped/mismatched arguments — zero runtime cost (`size_of::<Miles>() == size_of::<f64>()`). (api-newtype-safety; api-guidelines.md C-NEWTYPE; design-patterns.md Newtype idiom.)

```rust
// WRONG: raw primitives — swap compiles, fails (or corrupts) at runtime.
fn add_to_group(user_id: u64, group_id: u64) { /* ... */ }

// RIGHT: distinct newtypes — swap is a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupId(u64);

fn add_to_group_safe(user_id: UserId, group_id: GroupId) { /* ... */ }

fn main() {
    let (u, g) = (UserId(1), GroupId(2));
    add_to_group(2, 1);          // silent bug — args swapped, still compiles
    add_to_group_safe(u, g);     // swapping u/g here would NOT compile
}
```

**Units that must not mix** — model conversions with explicit `From`:

```rust
#[derive(Clone, Copy)] struct Kilometers(f64);
#[derive(Clone, Copy)] struct Miles(f64);
impl From<Kilometers> for Miles {
    fn from(km: Kilometers) -> Self { Miles(km.0 * 0.621_371) }
}
fn drive(_d: Miles) {}
fn main() { let km = Kilometers(100.0); drive(km.into()); } // conversion is visible
```

Derive the right trait bundle for the newtype's role (api-common-traits): ID types → `Debug, Clone, Copy, PartialEq, Eq, Hash`; ordered → add `PartialOrd, Ord`; serialized → `#[derive(Serialize)] #[serde(transparent)]` to keep the wire format as the raw value. Don't newtype when there is genuinely no confusion possible (`struct X(i32)` for a one-off is overkill).

---

## Parse, don't validate

Don't check data and then pass the raw unchecked form around; **parse it into a type that can only exist if valid** (Patterns Book ch15 §Parse Don't Validate; api-parse-dont-validate; C-VALIDATE). `TryFrom`/`FromStr` are the standard tools.

```rust
use std::str::FromStr;

/// A validated TCP port (non-zero). Existence == validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port(u16);

#[derive(Debug)]
pub enum PortError { Zero, InvalidFormat }

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortError::Zero => write!(f, "port must be non-zero"),
            PortError::InvalidFormat => write!(f, "invalid port format"),
        }
    }
}
impl std::error::Error for PortError {}

impl TryFrom<u16> for Port {
    type Error = PortError;
    fn try_from(v: u16) -> Result<Self, Self::Error> {
        if v == 0 { Err(PortError::Zero) } else { Ok(Port(v)) }
    }
}
// FromStr for CLI/config text — reuses TryFrom, so validation lives in one place.
impl FromStr for Port {
    type Err = PortError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n: u16 = s.parse().map_err(|_| PortError::InvalidFormat)?;
        Port::try_from(n)
    }
}
impl Port { pub fn get(self) -> u16 { self.0 } }

// Downstream never re-checks — the type is the proof.
fn start_server(port: Port) { println!("listening on {}", port.get()); }

fn main() -> Result<(), PortError> {
    let p: Port = "8080".parse()?;   // validates once, at the boundary
    start_server(p);
    assert!(Port::try_from(0).is_err());
    Ok(())
}
```

| Approach | Data checked? | Compiler enforces validity? | Re-validation needed? |
|----------|:---:|:---:|:---:|
| Runtime `if`/`assert` | ✅ | ❌ | at every boundary |
| Validated newtype + `TryFrom` | ✅ | ✅ | never — type is proof |

**Rule of thumb** (ch15 Case Study): if you `match` on string values, replace the param with an **enum**; if a `bool` isn't self-evident at the call site, use a **two-variant enum** (`Strictness::Strict` / `::Lenient`). Chain validations with a `TryFrom<RawConfig> for ValidConfig` that parses each field once at the deserialization boundary. Use `conv-tryfrom-fallible`/`conv-fromstr-parsing` naming.

---

## `#[non_exhaustive]` — grow without breaking

Adding an enum variant or struct field is normally a breaking change (downstream `match` is exhaustive, struct literals name every field). `#[non_exhaustive]` forces external code to use a wildcard arm / `..` and blocks external struct-literal construction, so you can add variants and fields in a *minor* version (api-non-exhaustive; Patterns Book ch15; C-STABLE).

```rust
#[non_exhaustive]
pub enum DiagError { Timeout, HardwareFault } // adding a variant later is NOT a break

#[non_exhaustive]
pub struct Options { pub retries: u32 }        // adding a field later is NOT a break
impl Options {
    pub fn new(retries: u32) -> Self { Options { retries } } // give a ctor — no literal syntax outside
}
```

Downstream must write `_ =>` on the match and `Options::new(..)` instead of `Options { .. }`. **Inside the defining crate** exhaustive matches and literals still work. Apply to error enums and option structs that will evolve; **don't** apply to complete, stable types (`enum Ordering { Less, Equal, Greater }`) — the wildcard just adds noise. Can also mark a single variant: `#[non_exhaustive] Error { code: u32 }` forces `..` when destructuring that variant.

---

## Sealed traits — usable but not implementable

A public trait can be implemented by anyone, which blocks you from adding required methods later (a break) and lets others supply incorrect impls. Seal it: make it require a private supertrait only your crate can name (api-sealed-trait; Patterns Book ch15; api-guidelines.md C-SEALED).

```rust
mod private { pub trait Sealed {} }        // private module → external crates can't name Sealed

pub trait DatabaseDriver: private::Sealed { // sealed: usable, not implementable outside
    fn connect(&self, url: &str) -> Result<(), String>;
}

pub struct Postgres;
impl private::Sealed for Postgres {}       // only THIS crate can do this
impl DatabaseDriver for Postgres {
    fn connect(&self, _url: &str) -> Result<(), String> { Ok(()) }
}

// External code can still USE it:
fn run(d: &impl DatabaseDriver) -> Result<(), String> { d.connect("postgres://localhost") }
fn main() { run(&Postgres).unwrap(); }
```

Because no external impls can exist, you can later add a **defaulted** method (`fn format_pretty(&self) -> String { self.format() }`) with no break. Seal when API stability is critical, correctness is subtle, or you'll extend the trait; **don't** seal designed extension points (`Iterator`-like traits users must implement). Related: `#[non_exhaustive]` seals data shapes; sealed traits seal behavior contracts.

---

## `#[must_use]` — catch silently-dropped values

Some return values are almost always bugs to ignore: `Result`, RAII guards, iterator adapters, pure computed values, and builder methods that return `Self` (api-must-use, api-builder-must-use; Patterns Book ch15; C-MUST-USE idea).

```rust
#[must_use = "this `Result` may be an `Err` that should be handled"]
pub fn validate(_s: &str) -> Result<(), String> { Ok(()) }

#[must_use = "dropping the guard immediately releases the lock"]
pub struct LockGuard;

// Builder methods that return Self MUST be must_use, or the builder is silently dropped:
#[must_use = "builders do nothing unless you call build()"]
pub struct ConfigBuilder { verbose: bool }
impl ConfigBuilder {
    #[must_use] pub fn verbose(mut self, v: bool) -> Self { self.verbose = v; self }
    pub fn build(self) -> bool { self.verbose }
}

fn main() {
    let _ = validate("x");                    // ignoring without `let _` warns
    ConfigBuilder { verbose: false }.verbose(true); // WARNS: return value unused (bug!)
}
```

The failure `#[must_use]` prevents (api-builder-must-use §Bad): `req.timeout(d); req.header(...); req.send();` — each builder call is dropped, the request ships with *no* config, and it compiles clean without the attribute. Enable `clippy::return_self_not_must_use` and `clippy::must_use_candidate` to find gaps. Don't slap it on side-effecting calls whose return is genuinely optional (`log(...) -> Result`).

---

## Common traits, conversions, operators, collections (fold-in)

- **Common traits** (api-common-traits; C-COMMON-TRAITS): derive `Debug` on *every* public type (M-PUBLIC-DEBUG in microsoft-guidelines.md), plus `Clone`/`PartialEq` by default; add `Eq, Hash` for map keys, `Ord` for sorting, `Default` for `unwrap_or_default`, `Copy` for small POD. Manual `Debug` to redact secrets (`Password([REDACTED])`). Public types that render for users get `Display` (M-PUBLIC-DISPLAY).
- **`From`, not `Into`** (api-from-not-into; C-CONV-TRAITS): implement `From<T> for U`; the blanket impl gives you `Into<U> for T` free, and `impl Into` params then accept your type. Implementing `Into` directly is non-idiomatic (`clippy::from_over_into`). Use `TryFrom` for fallible conversions.
- **`Default`** (api-default-impl): derive when every field's default is right; hand-write for non-zero defaults (per-field default values are nightly-only). Don't implement `Default` when a field has no sensible default — offer a constructor or builder instead.
- **Operator overloading** (api-operator-overload; C-OVERLOAD): only when meaning is obvious (numeric/vector newtypes, set `|`/`&`). Implement the `&`-reference form too so callers needn't clone, and pair each binary op with its `*Assign`. Never give `+`/`*` side effects.
- **Collections** (api-impl-fromiterator; C-COLLECT, C-ITER): a collection type should implement `FromIterator` (enables `.collect::<Bag<_>>()`), `Extend` (batch insert; `collect` uses it internally), and `IntoIterator` for owned/`&`/`&mut` (for-loops in all three forms). Delegate to the inner container.
- **Extension traits** (api-extension-trait): to add methods to a foreign type, define `TypeExt` and `impl TypeExt for Type` — orphan rules forbid inherent impls or foreign-trait-on-foreign-type. Users must import the trait (hence the prelude). Ecosystem: `Itertools`, `StreamExt`, `anyhow::Context`.

```rust
// Extension trait: add a method to a foreign type via the orphan-rule workaround.
pub trait ByteSliceExt { fn as_hex(&self) -> String; }
impl ByteSliceExt for [u8] {
    fn as_hex(&self) -> String { self.iter().map(|b| format!("{b:02x}")).collect() }
}
fn main() { assert_eq!((b"hi").as_hex(), "6869"); } // trait must be in scope
```

---

## Feature flags & additive features

Features are **additive**: enabling one must never *remove* items or change signatures, because Cargo unifies the union of all features requested anywhere in the dependency graph. If crate A enables `foo` and crate B doesn't, both get `foo`. So a feature may *add* impls/items but must not gate away or alter existing ones (C-FEATURE in api-guidelines.md).

```toml
# Cargo.toml
[features]
default = ["json"]          # keep default minimal — users opt in to the rest
json = ["dep:serde_json"]   # dep: syntax (Rust 1.60+) — no implicit feature named serde_json
xml  = ["dep:quick-xml"]
full = ["json", "xml"]      # meta-feature

[dependencies]
serde      = { version = "1", features = ["derive"] }
serde_json = { version = "1", optional = true }
quick-xml  = { version = "0.31", optional = true }
```

```rust
#[cfg(feature = "json")]
pub fn to_json<T: serde::Serialize>(v: &T) -> String { serde_json::to_string(v).unwrap() }

// Fail loudly if a required combination is missing:
#[cfg(not(any(feature = "json", feature = "xml")))]
compile_error!("enable at least one of: json, xml");
```

**Best practices** (ch15): minimal `default`; always use `dep:` to avoid an implicit feature named after each optional dep; document features in crate docs (`//! # Features`). Use `#[cfg_attr(feature = "x", derive(Foo))]` to conditionally derive without gating the whole item.

**Optional serde** (api-serde-optional): most libraries should make serde a feature, not a hard dep, so non-serializing users don't pay compile time / binary size. Make the dep optional and define the feature so the example below is self-consistent:

```toml
[dependencies]
serde = { version = "1", features = ["derive"], optional = true }

[features]
serde = ["dep:serde"]   # `serde` feature turns on the optional `serde` dep
```

```rust
// crate root (lib.rs) — required for the doc(cfg(...)) attribute to render on docs.rs
#![cfg_attr(docsrs, feature(doc_cfg))]

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))] // shows the feature gate on docs.rs
pub struct DiagResult { pub fc: u32, pub passed: bool }
```

Make serde *required* only when the crate is fundamentally about serialization (config parser, data-format lib). Test the matrix: `cargo test`, `cargo test --features serde`, `cargo test --all-features`.

`cfg_attr` cheat: `#[cfg(feature="x")]` includes/excludes a whole item; `#[cfg_attr(feature="x", derive(Foo))]` adds an attribute only when on; `#[cfg_attr(test, derive(PartialEq))]` for test-only derives.

---

## Splitting crates & workspaces

Split a large crate into a workspace of focused crates (M-SMALLER-CRATES in microsoft-guidelines.md — smaller crates compile in parallel and give clean dependency boundaries; Patterns Book ch15 §Workspace Organization).

```toml
# Root Cargo.toml
[workspace]
members = ["core", "parser", "server", "client", "cli"]

[workspace.dependencies]        # one source of truth for versions
serde  = { version = "1", features = ["derive"] }
tokio  = { version = "1", features = ["full"] }

# member/Cargo.toml
# [dependencies]
# serde = { workspace = true }
```

Benefits: single `Cargo.lock` (all members share versions), `cargo test --workspace`, shared build cache, enforced layering. Typical split: a dependency-light `core` (shared types/traits) that binaries and client libs depend on, so leaf crates don't pull each other's heavy deps. When to split: a subsystem has a distinct dependency set, a stable sub-API you want to version independently, or compile times dominated by one big module.

`.cargo/config.toml` (workspace root) customizes Cargo without touching `Cargo.toml`: `[build] target`, `[target.X] runner`/`linker` (e.g. QEMU for cross builds), `[alias]` (`ci = "clippy --workspace -- -D warnings"`), `[env]` build-time vars. Embed build metadata with `env!("CARGO_PKG_VERSION")` (compile error if missing) or `option_env!("GIT_SHA")` (returns `Option`), set from `build.rs` via `println!("cargo::rustc-env=GIT_SHA={sha}")`.

---

## Dependency hygiene & supply chain

Gate CI on `cargo audit` (known CVEs) and `cargo deny check` (licenses, bans, advisories, sources) — Patterns Book ch15 §cargo deny/audit.

```toml
# deny.toml — each key must be on its own line under its header
[advisories]
vulnerability = "deny"
[licenses]
allow = ["MIT", "Apache-2.0", "BSD-3-Clause"]
[bans]
multiple-versions = "warn"
deny = [{ name = "openssl" }]   # force rustls, e.g.
[sources]
allow-git = []                  # no git deps in production
```

| Tool | Purpose | When |
|------|---------|------|
| `cargo audit` | known CVEs in the lockfile | CI, pre-release |
| `cargo deny check` | licenses + bans + advisories + sources | CI |
| `cargo deny check bans` | forbid specific crates / dup versions | enforce arch decisions |

Also: fill in `Cargo.toml` metadata (`description`, `license`, `repository`, `keywords`, `categories`) — C-METADATA, doc-cargo-metadata — and a crate-level `//!` doc with a runnable quick-start (C-CRATE-DOC, doc-crate-readme).

---

## Evolving APIs without breakage (semver)

What is and isn't a breaking change, and the tools that keep additions non-breaking (C-STABLE; ch15). Verify with `cargo semver-checks`.

| Change | Breaking? | Make it safe with |
|--------|:---:|---|
| Add enum variant | ✅ yes | `#[non_exhaustive]` on the enum from day one |
| Add public struct field | ✅ yes | `#[non_exhaustive]` + constructor, or keep fields private |
| Add required trait method | ✅ yes | seal the trait, or give the method a default body |
| Add **defaulted** method to a **sealed** trait | ❌ no | sealed trait (no external impls to break) |
| Add a new pub type / fn / inherent method | ❌ no | — |
| Add an impl of *your* trait for *your* type | ❌ no | — |
| Rename/remove pub item or `pub use` | ✅ yes | deprecate first (`#[deprecated]`), remove at major |
| Loosen a bound / widen a param to `impl Into` | ❌ no (usually) | — |
| Tighten a bound / narrow a return type | ✅ yes | major version |
| Add a blanket impl / new `From` | ⚠️ maybe | can cause inference/coherence breaks downstream |
| Change a fn to take `impl AsRef<T>` from `&T` | ❌ no | callers passing `&T` still compile |

Deprecate before removing: `#[deprecated(since = "1.2.0", note = "use `Config::builder` instead")]`. Sealed traits + `#[non_exhaustive]` + private fields are the three levers that convert most "would-be breaking" additions into minor-version-safe ones.

---

## Worked whole-crate example (capstone shape)

The capstone (Patterns Book ch19) composes every lever above into one crate — the target architecture for a real library:

- **Newtype IDs**: `struct TaskId(u64)` derives `Debug, Clone, Copy, PartialEq, Eq, Hash` (api-newtype-safety).
- **Typestate lifecycle**: `Task<Pending>` → `Task<Running>` → `Task<Completed>`/`Failed`, each transition *consumes* `self` and returns the next state type; invalid transitions don't compile (api-typestate).
- **Validated inputs at the boundary**: `Priority` (1–10), `Port`-style newtypes constructed via `TryFrom` so downstream never re-checks (api-parse-dont-validate).
- **Error type with `thiserror`**: `#[derive(Error)] #[non_exhaustive] enum SchedulerError` — variants can grow without breaking users; `#[from]` wires source errors.
- **`#[must_use]`** on the scheduler handle / results so callers can't drop them silently.
- **Clean module structure**: private `mod worker`, `mod task`; `lib.rs` re-exports `Scheduler`, `TaskId`, `SchedulerError` at the root (facade).

```rust
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

struct Pending; struct Running; struct Completed;

pub struct Task<State> { id: TaskId, name: String, _s: PhantomData<State> }

impl Task<Pending> {
    pub fn new(id: TaskId, name: impl Into<String>) -> Self {
        Task { id, name: name.into(), _s: PhantomData }
    }
    pub fn start(self) -> Task<Running> {           // consumes self → next state
        Task { id: self.id, name: self.name, _s: PhantomData }
    }
}
impl Task<Running> {
    pub fn complete(self) -> Task<Completed> {
        Task { id: self.id, name: self.name, _s: PhantomData }
    }
}

fn main() {
    let t = Task::<Pending>::new(TaskId(1), "compute");
    let t = t.start();
    let _done = t.complete();
    // t.complete() a second time would not compile — t was moved.
}
```

Evaluation criteria the book grades on (ch19): invalid state transitions don't compile; public API uses validated types; clean module structure; key types documented with their invariants. That is the shape to aim for in any production crate.

---

## Rules & anti-patterns checklist

Distilled from the `api-*` rule files. `id` = DO / DON'T — reason (fix).

- **api-newtype-safety**: DO wrap confusable primitives (IDs, units, timestamps) in newtypes — the compiler catches swapped args at zero cost; DON'T pass bare `(u64, u64)`.
- **api-parse-dont-validate**: DO parse into a type constructible only from valid data at the boundary; DON'T scatter `if is_valid(...)` checks — you'll forget one. (Fix: private field + `parse`/`TryFrom` constructor.)
- **api-typestate**: DO encode state-machine states as distinct types with move-consuming transitions; DON'T guard transitions with runtime `if self.state != X`.
- **api-non-exhaustive**: DO put `#[non_exhaustive]` on public enums/structs that may grow; DON'T on complete/stable types. Reason: lets you add variants/fields in minor releases.
- **api-sealed-trait**: DO seal traits users shouldn't implement (private supertrait); DON'T seal designed extension points. Reason: lets you add defaulted methods without breaking downstream.
- **api-must-use**: DO `#[must_use]` on `Result`, guards, pure results, `-> Self` builder methods; DON'T on side-effecting calls with optional returns. Reason: silent drops are invisible bugs.
- **api-builder-must-use**: DO `#[must_use]` every builder method (and the builder type); reason: an ignored `self`-returning call drops all config and still compiles.
- **api-builder-pattern**: DO use a builder for many-optional-field construction (typestate builder for required fields); DON'T write `new(a, b, true, None, false)` positional soup.
- **api-common-traits**: DO derive `Debug` always, plus `Clone`/`PartialEq`, adding `Eq`/`Hash`/`Ord`/`Default`/`Copy` per role; DON'T ship bare structs — they can't be printed, compared, or keyed.
- **api-default-impl**: DO implement `Default` when a sensible default exists (derive, or hand-write for non-zero); DON'T implement it when a field has no meaningful default (use a ctor/builder).
- **api-from-not-into**: DO implement `From<T> for U` (gives `Into` free); DON'T implement `Into` directly (`clippy::from_over_into`). Use `TryFrom` for fallible.
- **api-impl-into**: DO take `impl Into<T>` when the fn will own the value; DON'T on trait-object params, struct fields, or hot paths.
- **api-impl-asref**: DO take `impl AsRef<T>` for read-only access to `String`/`&str`/`Path`/…; DON'T force one concrete borrowed type on callers.
- **api-impl-fromiterator**: DO implement `FromIterator` + `Extend` + `IntoIterator`×3 on collection types; DON'T force callers into manual `for … push` loops.
- **api-extension-trait**: DO add methods to foreign types via a `TypeExt` trait + impl; DON'T attempt inherent impls or foreign-trait-on-foreign-type (orphan rules).
- **api-operator-overload**: DO overload only when the meaning is obvious, implement the `&` form and the matching `*Assign`; DON'T give operators side effects.
- **api-serde-optional**: DO gate serde behind a feature for general-purpose libs (`optional = true` + `serde = ["dep:serde"]`); DON'T make every user pay for serde unless the crate is about serialization.

Anti-patterns that intersect this topic (from `anti-*`): **anti-stringly-typed** (replace string params with enums/newtypes — the ch15 Case Study), **anti-string-for-str** / **anti-vec-for-slice** (take `&str`/`&[T]`, not `String`/`Vec` by value, in params), **anti-over-abstraction** (don't seal/newtype/typestate where a plain type suffices), **anti-panic-expected** (return `Result` from library APIs — never `panic!` on expected input errors).

---

## Gotchas / footguns

- **Features must be additive.** A feature that *removes* an item or changes a signature breaks anyone in a graph where another crate turns it on. Cargo unifies the union; you can't assume "my feature is off."
- **Implicit features from optional deps.** Without `dep:`, `serde_json = { optional = true }` silently creates a public feature named `serde_json`. Always use `dep:serde_json` inside your own feature to keep the surface intentional.
- **`#[non_exhaustive]` doesn't help retroactively.** Add it on day one. Slapping it on an existing enum is *itself* a breaking change (existing downstream matches lose exhaustiveness).
- **`#[non_exhaustive]` is a no-op inside the defining crate.** You still get exhaustive matches and struct literals locally — the restriction only applies to *other* crates. Easy to forget when your integration tests live in the same crate vs `tests/`.
- **Sealed trait leak.** If the `Sealed` supertrait (or a type that names it) is reachable via a `pub use`, the seal is broken. Keep `mod private` genuinely private.
- **`impl Into` in a struct field or return position doesn't compile** the way you'd hope — `impl Trait` isn't allowed in field types, and returning `impl Into<String>` is opaque and near-useless to callers.
- **Re-exports are API.** Removing/renaming a `pub use` breaks users even though the underlying type still exists. Deprecate re-exports too.
- **Not re-exporting a dependency type you expose** forces callers to add and version-match that crate themselves; a version mismatch then yields confusing "expected X, found X" errors. Re-export it.
- **`Borrow` vs `AsRef` for map keys.** Using `AsRef` where `Borrow` is required (lookup keys) compiles for the wrong reasons or fails to; `HashMap::get` needs `Borrow` for Eq/Hash consistency.
- **`Copy` newtype hides moves.** A `Copy` `struct Id(u64)` silently duplicates; fine for IDs, wrong for anything with move semantics (owned handles, capabilities).
- **Blanket impls and new `From`s can break downstream inference** even though they look purely additive — a new impl can make a previously-unambiguous method call ambiguous. Treat broad blanket impls as semver-sensitive.
- **`compile_error!` guards fire at build, not lint** — good for "no format feature enabled," but keep the message actionable.

---

## Cheat-sheet

| Goal | Mechanism | Rule / code |
|------|-----------|-------------|
| Flat public imports | facade: private `mod`, `pub use` at root | ch15; C-STABLE |
| Ergonomic string param, will own | `fn f(x: impl Into<String>)` | api-impl-into |
| Ergonomic param, read-only | `fn f(x: impl AsRef<str>)` / `&str` | api-impl-asref |
| Borrow-or-clone processing | `fn f(&str) -> Cow<'_, str>` | ch15 |
| Map lookup key generality | `Borrow<Q>` bound | ch15; C-GENERIC |
| Prevent swapped args / unit mixing | newtype `struct Id(u64)` | api-newtype-safety; C-NEWTYPE |
| Validity guaranteed by type | `TryFrom`/`FromStr` + private field | api-parse-dont-validate; C-VALIDATE |
| Compile-time state machine | typestate `T<State>`, move transitions | api-typestate |
| Add enum variant later, safely | `#[non_exhaustive]` | api-non-exhaustive; C-STABLE |
| Add struct field later, safely | `#[non_exhaustive]` + ctor, or private fields | api-non-exhaustive; C-STRUCT-PRIVATE |
| Trait usable but not implementable | sealed trait (private supertrait) | api-sealed-trait; C-SEALED |
| Catch dropped Result/guard/builder | `#[must_use]` | api-must-use, api-builder-must-use |
| Complex construction | builder (typestate for required fields) | api-builder-pattern; C-BUILDER |
| Conversion between types | `impl From<T>` (gives `Into` free) | api-from-not-into; C-CONV-TRAITS |
| Method on a foreign type | `TypeExt` extension trait | api-extension-trait |
| Custom collection | `FromIterator`+`Extend`+`IntoIterator`×3 | api-impl-fromiterator; C-COLLECT |
| Optional serialization | `optional = true` + `#[cfg_attr(feature=…)]` | api-serde-optional; C-FEATURE |
| Additive optional dep | `dep:` syntax in feature list | ch15 |
| Split for compile time / layering | Cargo workspace + `workspace.dependencies` | M-SMALLER-CRATES |
| Supply-chain gate | `cargo audit`, `cargo deny check` | ch15 |
| Detect a breaking change | `cargo semver-checks`, deprecate-then-remove | C-STABLE |
| Embed build metadata | `env!` / `option_env!` + `build.rs` | ch15 |

**Cross-references:** api-guidelines.md (C-*), microsoft-guidelines.md (M-SMALLER-CRATES, M-PUBLIC-DEBUG/DISPLAY), design-patterns.md (Newtype, builder, sealed idioms), style-guide.md (Cargo.toml conventions). For error-type design in public APIs see the error-handling reference; for testing the public surface see the testing reference.
