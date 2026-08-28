# Idiom Catalog: Naming, Docs, Project Layout & Lints

The cross-cutting engineering concerns that surround every Rust crate but sit *outside* type/API design: how to **name** things, how to **document** them, how to **lay out** modules and workspaces, how to **observe** running code (logging/tracing), how to **configure lints**, which **collection** to pick, and when to reach for `const` vs `static`. Consult this before writing or reviewing any crate skeleton, public surface, `Cargo.toml`, module tree, or logging code. Distilled from the leonardomso/rust-skills `name-*`, `doc-*`, `proj-*`, `obs-*`, `lint-*`, `coll-*`, and `const-*` rules.

This file complements the skill's canonical references — it does **not** re-derive them:
- **api-guidelines.md** — the 44 C-* Rust API Guidelines (naming C-CASE/C-CONV/C-ITER, docs C-EXAMPLE/C-QUESTION-MARK/C-LINK, etc.). Many rules here are the operational form of a C-* guideline; cross-linked inline.
- **microsoft-guidelines.md** — the 11 M-* Pragmatic Guidelines (e.g. M-CANONICAL-DOCS, M-MODULE-DOCS).
- **style-guide.md** — rustfmt formatting + `Cargo.toml` field ordering.
- **design-patterns.md** — idioms/patterns/anti-patterns.

---

## 1. Naming conventions

Case is compiler-enforced (`non_snake_case`, `non_camel_case_types`, `non_upper_case_globals` lints). Everything else is convention that readers and tools rely on. See api-guidelines.md **C-CASE**, **C-CONV**, **C-GETTER**, **C-ITER**.

### 1.1 Casing by item kind (`name-types-camel`, `name-variants-camel`, `name-funcs-snake`, `name-consts-screaming`)

| Item | Case | Example |
|------|------|---------|
| Types, traits, enums, type aliases | `UpperCamelCase` | `HttpClient`, `Serializable`, `type BoxFuture` |
| Enum variants | `UpperCamelCase` | `Status::InProgress` |
| Functions, methods, variables, modules | `snake_case` | `fn parse_json`, `mod http_client` |
| Constants & statics (incl. associated consts) | `SCREAMING_SNAKE_CASE` | `const MAX_CONNECTIONS`, `static COUNTER` |

```rust
struct HttpClient;                         // type: UpperCamelCase
enum Status { Pending, InProgress, Done }  // variants: UpperCamelCase
fn fetch_order() {}                        // fn: snake_case
const MAX_CONNECTIONS: u32 = 100;          // const: SCREAMING_SNAKE_CASE
static REQUEST_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
```

Variant naming: be specific (`NotFound`, not `Error`); avoid repeating the enum name (`ConnectionState::Connected`, not `ConnectionError` inside `ConnectionState`).

### 1.2 Acronyms are words (`name-acronym-word`)

Treat acronyms as words so boundaries stay clear: `HttpServer` not `HTTPServer`, `JsonParser`, `Uuid`, `Url`, `TcpStream`. In `snake_case` they lowercase fully: `parse_json`, `connect_tcp`, `generate_uuid`. Two-letter acronyms (`Io`, `Id`) may go either way; prefer word form (`IoHandler`). std is the reference: `TcpStream`, `UdpSocket`, `IpAddr`.

### 1.3 Conversion prefixes signal cost & ownership (`name-as-free`, `name-to-expensive`, `name-into-ownership`)

This is **C-CONV** made concrete. The prefix is a *promise about cost*:

| Prefix | Cost | Receiver → Result | Examples |
|--------|------|-------------------|----------|
| `as_` | Free, O(1) | `&self → &U` (borrow, reinterpret) | `str::as_bytes`, `Vec::as_slice`, `PathBuf::as_path` |
| `to_` | Expensive (allocates/computes) | `&self → U` (new owned value) | `str::to_lowercase`, `slice::to_vec`, `to_owned` |
| `into_` | Usually free (moves) | `self → U` (consumes) | `String::into_bytes`, `into_inner`, `into_iter` |

```rust
struct Email(String);
impl Email {
    fn as_str(&self) -> &str { &self.0 }               // free borrow → as_
    fn to_lowercase(&self) -> Email {                  // allocates → to_
        Email(self.0.to_lowercase())
    }
    fn into_string(self) -> String { self.0 }          // consumes self → into_
}
```

WRONG: `fn as_string(&self) -> String { format!("{}", self.0) }` — `as_` but allocates. RIGHT: name it `to_string`.

### 1.4 Getters, booleans, setters (`name-no-get-prefix`, `name-is-has-bool`)

**C-GETTER**: omit `get_` for simple field access — `user.name()`, not `user.get_name()`. Reserve `get`/`get_mut` for *fallible or computed* access that returns `Option` or does a lookup (`HashMap::get`, `Vec::get`). Setters keep `set_`: `set_timeout`. Builder methods take neither prefix and consume `self`.

Boolean methods carry a question prefix so call sites read as English:

| Prefix | Use | Example |
|--------|-----|---------|
| `is_` | state/property | `is_empty`, `is_valid` |
| `has_` | possession | `has_permission` |
| `can_` | capability | `can_edit` |
| `should_` / `needs_` / `will_` | policy / requirement / future | `should_retry`, `needs_update` |

Prefer the positive form and let callers negate (`!user.is_active()`) rather than defining `is_inactive`. Methods taking an argument use a verb phrase instead (`str::contains`, `starts_with`).

### 1.5 Iterators (`name-iter-convention`, `name-iter-method`, `name-iter-type-match`)

**C-ITER** / **C-ITER-TY**. Three canonical methods signal ownership; matching `IntoIterator` impls enable `for` loops:

| Method | Receiver | Yields | Iterator type |
|--------|----------|--------|---------------|
| `iter()` | `&self` | `&T` | `Iter<'a, T>` |
| `iter_mut()` | `&mut self` | `&mut T` | `IterMut<'a, T>` |
| `into_iter()` | `self` | `T` | `IntoIter<T>` |

Name the returned struct after its source method (`keys()`→`Keys`, `drain()`→`Drain`, custom `neighbors()`→`Neighbors`). Never name a custom iterator `Iterator` (collides with the trait) or `MyCollectionIterator` (verbose, non-matching).

```rust
struct Collection<T> { items: Vec<T> }
impl<T> Collection<T> {
    fn iter(&self) -> impl Iterator<Item = &T> { self.items.iter() }
    fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> { self.items.iter_mut() }
}
impl<T> IntoIterator for Collection<T> {          // enables `for x in coll`
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter { self.items.into_iter() }
}
impl<'a, T> IntoIterator for &'a Collection<T> {  // enables `for x in &coll`
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.items.iter() }
}
```

### 1.6 Generic parameters & lifetimes (`name-type-param-single`, `name-lifetime-short`)

Type params are single uppercase letters with conventional meaning: `T` type, `E` error, `K`/`V` key/value, `I` input/item, `O` output, `S` state, `F` function, `A` allocator. Push complex bounds into a `where` clause, not the angle brackets. Lifetimes are short: `'a`, `'b`, and domain conventions `'de` (serde deserialize), `'src`, `'ctx`, `'input`. Prefer lifetime *elision* over naming when the compiler can infer it (`fn name(&self) -> &str`).

### 1.7 Crate names (`name-crate-no-rs`)

Don't suffix `-rs`/`-rust` (redundant on crates.io): `serde`, not `serde-rs`. Name for *purpose*, not implementation language (`python-ast`, `windows-sys`). Applies to repos too.

---

## 2. Documentation practices

Public docs are the API contract. See api-guidelines.md **C-EXAMPLE**, **C-QUESTION-MARK**, **C-FAILURE**, **C-LINK**, **C-METADATA**, **C-HTML-ROOT**, and microsoft-guidelines.md **M-CANONICAL-DOCS**, **M-MODULE-DOCS**.

### 2.1 Document everything public (`doc-all-public`)

`///` on every `pub` item: structs, fields, enums, each variant, functions, traits, methods, consts, type aliases. Enforce with the lint (§5.2):

```rust
#![warn(missing_docs)]

/// Configuration for connecting to the service.
pub struct Config {
    /// Maximum time to wait before timing out.
    pub timeout: std::time::Duration,
    /// Number of retry attempts for failed requests.
    pub retries: u32,
}
```

### 2.2 Module & crate docs with `//!` (`doc-module-inner`)

Inner doc comments document the *containing* module. Put them at the top of `lib.rs` (crate root landing page), `mod.rs`, or `module.rs`. Include: one-line summary, overview, an example, feature-flag table, links to key items.

```rust
//! Authentication utilities.
//!
//! - [`JwtAuth`] — JSON Web Token authentication
//! - [`SessionAuth`] — cookie-based sessions
//!
//! # Feature Flags
//! - `jwt` — enables JWT auth (default)
```

### 2.3 README as the single source of truth (`doc-crate-readme`)

Make the README render on GitHub, crates.io, *and* docs.rs from one file (**C-HTML-ROOT** territory):

```rust
// src/lib.rs
#![doc = include_str!("../README.md")]
```
```toml
# Cargo.toml
readme = "README.md"
```

Tag non-Rust code fences in the README (` ```bash `, ` ```text `, ` ```rust,no_run `) so rustdoc doesn't try to compile them as doctests.

### 2.4 The standard doc sections

Rustdoc renders `#`-prefixed headings specially. Use these exact names:

| Section | When | Rule |
|---------|------|------|
| `# Examples` | always, at least one | `doc-examples-section` |
| `# Errors` | any `-> Result` fn | `doc-errors-section` (**C-FAILURE**) |
| `# Panics` | any fn that can panic | `doc-panics-section` |
| `# Safety` | any `unsafe fn`/`unsafe trait` | `doc-safety-section` |

```rust
/// Reads a file as UTF-8.
///
/// # Errors
///
/// Returns an error if the file does not exist ([`Error::NotFound`]),
/// the process lacks permission ([`Error::PermissionDenied`]), or the
/// contents are not valid UTF-8 ([`Error::InvalidUtf8`]).
///
/// # Panics
///
/// Panics if `path` is empty.
pub fn read_file(path: &std::path::Path) -> Result<String, Error> {
    # let _ = path; unimplemented!()
}
# enum Error { NotFound, PermissionDenied, InvalidUtf8 }
```

For `# Safety`, spell out every caller precondition (pointer validity, alignment, initialization, ownership transfer) and the consequence of violating it — this is *mandatory* for sound `unsafe`. Also add `// SAFETY:` comments to each `unsafe { }` block explaining why the invariants hold (enforced by `undocumented_unsafe_blocks`, §5.2). When you panic instead of returning `Result`, document *why* (programming error vs runtime condition) and point at a non-panicking alternative (`checked_*`, `get`).

### 2.5 Examples that model best practice (`doc-examples-section`, `doc-question-mark`, `doc-hidden-setup`)

**C-QUESTION-MARK**: use `?`, not `.unwrap()`, so examples teach propagation *and* fail the doctest on error. Hide boilerplate with `# ` line prefixes; use ` ```no_run ` for examples with side effects (starting a server) and ` ```ignore ` for pseudocode.

```rust
/// Loads configuration.
///
/// # Examples
///
/// ```
/// # use my_crate::Config;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let config = Config::load("config.toml")?;   // ? not unwrap
/// println!("port: {}", config.port);
/// # Ok(())
/// # }
/// ```
pub struct Config { pub port: u16 }
```

`.unwrap()` is acceptable only for genuinely-infallible literals (`"42".parse::<i32>().unwrap()`, a static regex). Run doctests with `cargo test --doc`.

### 2.6 Intra-doc links (`doc-intra-links`, `doc-link-types`)

**C-LINK**: link types/methods with `[`Name`]` syntax — clickable, verified at doc-build, auto-updating on rename. Never write bare-text type names.

| Syntax | Links to |
|--------|----------|
| `[Vec]`, `[Option]` | item in scope |
| `[Self::new]` | method on current type |
| `[String::new]` | method on other type |
| `[text](Self::len)` | custom link text |
| `[`x`]: crate::Error` | reference-style (for long paths / reuse) |

Disambiguate colliding names with `fn@`, `mod@`, `struct@`, `enum@`, `trait@`, `type@`, `const@`, `macro@` prefixes. Fail CI on broken links: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, or `[lints.rustdoc] broken_intra_doc_links = "deny"`.

### 2.7 Cargo.toml metadata (`doc-cargo-metadata`)

**C-METADATA**. Required to publish: `name`, `version`, `license` (or `license-file`), `description`. Strongly recommended: `repository`, `documentation`, `readme`, `keywords` (≤5, specific — not `["rust", "fast"]`), `categories` (from crates.io slugs), `rust-version` (MSRV, §3.9).

```toml
[package]
name = "my-crate"
version = "0.1.0"
edition = "2024"
rust-version = "1.80"
description = "A fast, ergonomic HTTP client"
license = "MIT OR Apache-2.0"          # dual-license is the Rust-ecosystem norm
repository = "https://github.com/user/my-crate"
documentation = "https://docs.rs/my-crate"
readme = "README.md"
keywords = ["http", "client", "async"]
categories = ["network-programming"]
```

Verify before publishing: `cargo package --list`, `cargo publish --dry-run`. Enforce metadata with `clippy::cargo` (§5.3).

---

## 3. Project & module layout

Start minimal; add structure only when complexity demands. See design-patterns.md for the export/visibility idioms.

### 3.1 Keep small projects flat (`proj-flat-small`)

Under ~10 files, keep them directly in `src/`. Add structure by thresholds:

| Files | Structure |
|-------|-----------|
| < 10 | flat in `src/` |
| 10–20 | group by feature |
| 20+ | feature folders with submodules |

Over-structure smells: folders with 1–2 files, `mod.rs` that only re-exports, deep nesting for simple concepts. Under-structure smells: files > 300–500 lines, `_`-prefixed grouping (`user_model.rs`, `user_service.rs`) — that's a folder trying to be born.

### 3.2 Thin `main.rs`, logic in `lib.rs` (`proj-lib-main-split`)

Integration tests (in `tests/`) can only reach the *library* crate, never `main.rs`. Put all logic in `lib.rs` and make `main.rs` a shell:

```rust
// src/main.rs
fn main() -> anyhow::Result<()> {
    let config = my_app::Config::from_env()?;
    my_app::run(config)
}
```
```rust
// src/lib.rs
pub mod config;
pub use config::Config;
pub fn run(config: Config) -> anyhow::Result<()> {
    # let _ = config; Ok(())
}
```

### 3.3 Multiple binaries in `src/bin/` (`proj-bin-dir`)

Each file in `src/bin/` becomes a binary automatically — no `[[bin]]` needed. `src/bin/server.rs` → binary `server`; `src/bin/server/main.rs` for a multi-file binary. Shared code lives in `lib.rs` and both binaries `use my_project::…`. Run with `cargo run --bin server`.

### 3.4 Organize by feature, not by type (`proj-mod-by-feature`)

Group everything about one feature together (`user/{model,repository,service,handler}.rs`), *not* by layer (`models/`, `services/`, `handlers/`). Adding a feature then touches one folder; deleting it removes one folder. Cross-cutting concerns go in a `shared/` module.

### 3.5 Pick one multi-file module style (`proj-mod-rs-dir`)

Two equivalent styles — choose one and enforce it:
- `mod.rs` style (`user/mod.rs` + `user/model.rs`) — clearer for large/deeply-nested modules; used by tokio/serde.
- Adjacent-file style (`user.rs` + `user/model.rs`) — interface visible without entering the folder; the 2018+ default.

Enforce consistency with `[lints.clippy] mod_module_files = "warn"` (forces `mod.rs`) *or* `self_named_module_files = "warn"` (forces adjacent) — pick exactly one.

### 3.6 Visibility ladder (`proj-pub-crate-internal`, `proj-pub-super-parent`)

Expose the *minimum*. Public API is a contract; internals should be free to change.

| Visibility | Scope | Use for |
|------------|-------|---------|
| `pub` | everywhere | public API |
| `pub(crate)` | this crate | shared internal utilities |
| `pub(super)` | parent module | helpers shared between sibling submodules |
| `pub(in path)` | a specific module | precise control |
| (none) | current module | implementation details |

```rust
pub struct Widget { state: Internal }  // field stays private
pub(crate) struct Internal { buffer: Vec<u8> }
```

### 3.7 Flat public API via `pub use` + prelude (`proj-pub-use-reexport`, `proj-prelude-module`)

Keep internals nested but re-export a flat surface so users write `use my_crate::HttpClient` instead of `use my_crate::transport::http::client::HttpClient`. Rename on re-export for versioning (`pub use v1::Client as LegacyClient`). Re-export dependency types users need (`pub use bytes::Bytes;`). Offer a conservative `prelude` module of the always-needed items:

```rust
// src/lib.rs
mod client; mod config; mod error;
pub use client::HttpClient;
pub use config::Config;
pub use error::Error;

pub mod prelude {
    pub use crate::{Config, Error, HttpClient};
    #[cfg(feature = "async")]
    pub use crate::AsyncClient;
}
```

Be conservative in a prelude (removing an item is a breaking change); avoid clashy names; glob-re-export internal modules sparingly and never `pub use external_crate::*`.

### 3.8 Workspaces (`proj-workspace-large`, `proj-workspace-deps`)

For multi-crate projects use a workspace: one `Cargo.lock`, shared build cache, synchronized versions. Declare shared deps *once* in `[workspace.dependencies]` and inherit with `dep.workspace = true` to kill version drift.

```toml
# root Cargo.toml
[workspace]
members = ["crates/*"]
resolver = "3"                 # default for edition 2024; use "2" for 2021

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.32", features = ["full"] }

[workspace.lints.clippy]
correctness = { level = "deny", priority = -1 }
```
```toml
# crates/core/Cargo.toml
[package]
name = "my-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
tokio = { workspace = true, features = ["net"] }  # workspace + extra features

[lints]
workspace = true               # inherit workspace lints
```

Gotcha: `optional = true` cannot live in `[workspace.dependencies]` — set it in the *member* (`serde = { workspace = true, optional = true }`).

### 3.9 MSRV, features & build scripts (`proj-msrv-declare`, `proj-feature-additive`, `proj-build-rs-minimal`)

- **MSRV** (`proj-msrv-declare`): set `rust-version` so Cargo emits a clear error on old toolchains instead of a cryptic type error. The edition-2024 resolver (`resolver = "3"`) is MSRV-aware. Pick the oldest toolchain you truly support; bumping MSRV is a semver-minor change for libraries. Gate it in CI. Find the real floor with `cargo msrv`.
- **Features must be additive** (`proj-feature-additive`): Cargo unifies features across the graph, so a feature must only *add* capability, never remove it. Make `std` a feature in `default` (not a `no_std` opt-out). Use `dep:` syntax (`serde = ["dep:serde"]`) to keep optional-dep names out of the feature namespace. Mutually exclusive features can't be enforced by Cargo — `compile_error!` if both are set.

```toml
[features]
default = ["std"]
std = []
serde = ["dep:serde"]          # additive; dep: keeps namespace clean
```

- **build.rs** (`proj-build-rs-minimal`): keep it minimal, deterministic, idempotent. Always emit `cargo::rerun-if-changed=build.rs` and list every input file. Never make network calls. Probe compiler capabilities with the `autocfg` crate rather than parsing `rustc --version`. Only write into `OUT_DIR`.

---

## 4. Observability: logging & tracing

Use `tracing`, structure your data, and let the *binary* — never a library — own configuration. See obs-* rules.

### 4.1 `tracing` over `println!`/`log` (`obs-tracing-over-log`)

`println!` has no levels, targets, or structure. The `log` facade adds levels but only flat strings. `tracing` adds structured fields *and* spans that follow execution across `.await` and threads.

| Approach | Levels | Structured | Async spans | `log` compat |
|----------|--------|-----------|-------------|--------------|
| `println!` | ✗ | ✗ | ✗ | ✗ |
| `log` | ✓ | ✗ | ✗ | ✓ |
| `tracing` | ✓ | ✓ | ✓ | ✓ (feature) |

```rust
use tracing::info;
fn handle_login(id: u64) {
    info!(user.id = %id, "user logged in");   // queryable field + stable message
}
```

### 4.2 Structured fields, not string interpolation (`obs-structured-fields`)

Values baked into the message (`"processed {items} items"`) are opaque to aggregators. Emit discrete fields; keep the message short and stable.

```rust
use tracing::info;
fn process_batch(user_id: u64, items: usize, elapsed_ms: u64) {
    info!(user.id = user_id, items, elapsed_ms, "batch processed");
}
```

Field sigils: `field = value` (typed primitive), `field = %expr` (`Display`), `field = ?expr` (`Debug`), bare `field` (shorthand for `field = field`). Prefer `%` for clean-`Display` values; JSON backends quote `?` output inconsistently. Use OpenTelemetry-style dotted names (`user.id`, `http.status`).

### 4.3 Spans for context (`obs-instrument-spans`)

`#[tracing::instrument]` wraps a function in a span built from its args — the preferred way to instrument async fns. **Footgun:** never hold a span *entry guard* (`let _g = span.enter()`) across `.await` — it attaches to the wrong task on resume. Use `.instrument(span).await` for manual spans.

```rust
use tracing::{info, instrument, Instrument, info_span};

#[instrument(skip(db), fields(user.id = user_id))]     // skip large/sensitive args
async fn fetch_user(user_id: u64, db: &DbPool) -> Result<String, DbError> {
    info!("fetching user");
    Ok(db.query_user(user_id).await?)
}

async fn process_job(job_id: &str) {
    let span = info_span!("process_job", job.id = job_id);
    async move { /* work */ }.instrument(span).await;   // NOT span.enter()
}
# struct DbPool; #[derive(Debug)] struct DbError;
# impl DbPool { async fn query_user(&self, _: u64) -> Result<String, DbError> { Ok("a".into()) } }
```

### 4.4 Levels & filtering (`obs-levels-filter`)

| Level | For |
|-------|-----|
| `error!` | failures needing attention (DB connection lost) |
| `warn!` | recoverable anomalies (retrying after timeout) |
| `info!` | lifecycle events (server started, request complete) |
| `debug!` | development diagnostics (query params) |
| `trace!` | per-iteration detail (loop counters, raw bytes) |

Configure `EnvFilter` from `RUST_LOG` in the binary; prefer `try_from_default_env` with a fallback so the binary still starts when `RUST_LOG` is unset:

```rust
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,myapp=debug,hyper=warn".into()),
        )
        .init();
}
```

Compile out verbose levels in release via cargo features: `tracing = { version = "0.1", features = ["release_max_level_info"] }`.

### 4.5 Libraries emit, binaries subscribe (`obs-library-facade`)

Installing a subscriber is a once-per-process operation. A library that calls `tracing_subscriber::fmt::init()` or `env_logger::init()` conflicts with the application (panic or silent no-op) and steals control. **Library `Cargo.toml`: depend on `tracing` only** (put `tracing-subscriber` in `[dev-dependencies]` for tests, using the non-panicking `try_init()`). The binary owns setup.

### 4.6 Error chains, logged once (`obs-error-chain`)

Two failures to avoid: (1) logging only `Display` drops the source chain; (2) logging at every propagation layer records the same error repeatedly. **Propagate with `?`/`.context()`; log once, at the boundary that handles the error.** Capture the chain with `error = ?err` (Debug) or `{err:#}` (anyhow's alternate Display walks the chain).

```rust
use anyhow::{Context, Result};
use tracing::error;

async fn fetch(id: u64) -> Result<Vec<u8>> {
    read_db(id).await.with_context(|| format!("reading record {id}"))  // add context, don't log
}
async fn handle(id: u64) -> Result<(), String> {
    match fetch(id).await {
        Ok(_) => Ok(()),
        Err(err) => { error!(error = %format!("{err:#}"), "request failed"); Err("internal".into()) }
    }
}
# async fn read_db(_: u64) -> Result<Vec<u8>> { Ok(vec![]) }
```

If a non-handling layer *must* log (e.g. a background task discarding the error), use `warn!` not `error!`.

### 4.7 Never log secrets or PII (`obs-no-sensitive-data`)

Logs ship to aggregators with weaker access controls than your secrets manager. `#[instrument]` auto-captures *all* args — `skip(arg)` / `skip_all` the sensitive ones, then re-add safe fields with `fields(...)`. Wrap secret values in a redacting newtype (override `Debug`+`Display` to emit `[redacted]`) or use the `secrecy` crate's `Secret<T>` / `.expose_secret()`. Never log full request bodies in production.

```rust
struct Secret(String);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("[redacted]") }
}
#[tracing::instrument(skip(creds), fields(user = %creds.username))]
fn authenticate(creds: &Credentials) -> bool { true }
# struct Credentials { username: String, password: Secret }
```

---

## 5. Lint policy

Configure lints centrally (workspace level, Rust 1.74+) with the severity ladder deny → warn → allow. See lint-* rules and api-guidelines.md **C-STABLE**.

### 5.1 Severity by clippy group (`lint-deny-correctness`, `lint-warn-suspicious`, `lint-warn-style`, `lint-warn-complexity`, `lint-warn-perf`)

| Group | Level | Catches |
|-------|-------|---------|
| `clippy::correctness` | **deny** | outright wrong code: `== NaN`, `for x in iter::repeat(1)`, impossible conditions |
| `clippy::suspicious` | **deny** (or warn) | probable bugs: `1 << 4 + 1` precedence, side effects in `map`, swapped comparison operands |
| `clippy::style` | warn | non-idiomatic: `len() == 0`→`is_empty()`, `matches!`, needless `return`, `if let` over single-arm `match` |
| `clippy::complexity` | warn | needless complexity: manual `Option::map`, `and_then(|x| Some(..))`→`map`, `clone_on_copy` |
| `clippy::perf` | warn | inefficiency: single-char `&str` patterns→`char`, `iter.nth(0)`, unnecessary `to_owned`, `box_collection` |

```rust
#![deny(clippy::correctness)]
#![warn(clippy::suspicious, clippy::style, clippy::complexity, clippy::perf)]
```

### 5.2 Docs & unsafe lints (`lint-missing-docs`, `lint-unsafe-doc`)

```rust
#![warn(missing_docs)]                            // every pub item documented (rust lint)
#![warn(clippy::undocumented_unsafe_blocks)]      // every unsafe block has // SAFETY:
```

`missing_docs` only fires on `pub` items; adopt gradually (`warn` → fix → `deny`). Pair with rustdoc lints: `#![warn(rustdoc::broken_intra_doc_links)]`.

### 5.3 Published-crate & cfg lints (`lint-cargo-metadata`, `lint-cfg-check`)

`clippy::cargo` checks `Cargo.toml`: missing `description`/`license`/`repository` (`cargo_common_metadata`), wildcard deps (`serde = "*"`), `multiple_crate_versions`, `negative_feature_names` (`no-std` should be `std`), `redundant_feature_names`. Set `cargo = "allow"` for unpublished crates.

`unexpected_cfgs` (Rust 1.80) catches cfg typos — `#[cfg(feature = "serde_")]` silently produces dead code. Cargo auto-registers feature names; declare *custom* cfgs:

```toml
[lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(tokio_unstable)'] }
```

### 5.4 Selective pedantic & nursery (`lint-pedantic-selective`, `lint-clippy-nursery-selected`)

Never enable the whole `pedantic` or `nursery` group — both are noisy. Enable the group then `allow` the noisy lints, *or* cherry-pick.

Pedantic keepers: `doc_markdown`, `semicolon_if_nothing_returned`, `unused_self`, `wildcard_imports`, `match_wildcard_for_single_variants`. Common `allow`s: `missing_errors_doc`, `missing_panics_doc` (handled by doc policy), `module_name_repetitions`, `must_use_candidate`, `too_many_lines`.

Nursery keepers: `significant_drop_tightening` (guards held too long — overlaps with the lock-across-await bug), `redundant_clone`, `use_self`, `redundant_else`, `or_fun_call`.

### 5.5 Restriction lints & workspace config (`lint-workspace-lints`)

`restriction` lints are opt-in-one-at-a-time (never the group): `unwrap_used = "deny"`, `expect_used = "warn"`, `panic = "deny"`, `dbg_macro = "warn"`, `print_stdout = "warn"`, `todo = "warn"`. Configure everything once at the workspace root; use `priority = -1` so group-level entries lose to specific overrides:

```toml
[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "warn"

[workspace.lints.clippy]
correctness = { level = "deny", priority = -1 }
suspicious  = { level = "deny", priority = -1 }
style       = { level = "warn", priority = -1 }
perf        = { level = "warn", priority = -1 }
unwrap_used = "deny"                  # specific override beats the group

[workspace.lints.rustdoc]
broken_intra_doc_links = "deny"
```

Per-crate overrides are legitimate: a binary's entry point may `unwrap_used = "allow"`; test-utils may `print_stdout = "allow"`.

### 5.6 Formatting in CI (`lint-rustfmt-check`)

Run `cargo fmt --all --check` in CI (fails without modifying). Project settings go in `rustfmt.toml` (see style-guide.md for the field details). `#[rustfmt::skip]` preserves hand-aligned data (matrices). CI clippy gate: `cargo clippy --workspace --all-targets -- -D warnings`.

---

## 6. Collections choice

Match the collection to the access pattern — the wrong one silently turns O(n) into O(n²). See coll-* rules.

### 6.1 Sequences (`coll-seq-choice`)

| Need | Type |
|------|------|
| general growable list (default) | `Vec<T>` |
| FIFO queue / deque / sliding window | `VecDeque<T>` |
| linked list (almost never) | `LinkedList<T>` — only if profiling proves it |

**Footgun:** `Vec::remove(0)` is O(n) (shifts every element) — a drain loop is O(n²). Use `VecDeque::pop_front` (O(1)):

```rust
use std::collections::VecDeque;
let mut queue: VecDeque<String> = items.into_iter().collect();
while let Some(item) = queue.pop_front() { /* O(1) */ }
# let items: Vec<String> = vec![];
```

### 6.2 Maps (`coll-map-choice`)

| Need | Type |
|------|------|
| fast lookup, order irrelevant (default) | `HashMap` |
| sorted iteration / range queries | `BTreeMap` (`.range(a..=b)`) |
| insertion-order iteration + O(1) lookup | `IndexMap` (`indexmap` crate) |

`HashMap`'s default SipHash is DoS-resistant but not fastest; swap a faster hasher for non-adversarial hot paths (`perf-ahash`). Reach for `IndexMap` when output must be deterministic (config files, reproducible reports).

### 6.3 Sets (`coll-set-membership`)

`Vec::contains` is O(n); membership-in-a-loop is O(n×m). Use `HashSet` (O(1)) — or `BTreeSet` when you also need sorted output. Same rule for dedup: collect into a set.

```rust
use std::collections::HashSet;
fn find_common(all: &[String], active: &[String]) -> Vec<String> {
    let set: HashSet<&String> = active.iter().collect();      // build once
    all.iter().filter(|u| set.contains(u)).cloned().collect() // O(1) per check
}
```

Order-preserving dedup: `items.into_iter().filter(|s| seen.insert(s.clone()))`. A `Vec` with `.contains` is fine only for ≤ ~8 items, or when duplicates/order matter.

### 6.4 Priority queue (`coll-binaryheap`)

`BinaryHeap<T>` is a max-heap: O(log n) `push`/`pop`, O(1) `peek`. Beats re-sorting a `Vec`. For a **min-heap**, wrap in `std::cmp::Reverse<T>`.

```rust
use std::cmp::Reverse;
use std::collections::BinaryHeap;
fn top_k(values: &[i32], k: usize) -> Vec<i32> {
    let mut heap: BinaryHeap<Reverse<i32>> = BinaryHeap::with_capacity(k + 1);
    for &v in values {
        heap.push(Reverse(v));
        if heap.len() > k { heap.pop(); }        // keep k largest
    }
    heap.into_iter().map(|Reverse(v)| v).collect()
}
```

`.into_sorted_vec()` drains sorted. `BinaryHeap` has no efficient arbitrary removal or priority update — use a keyed-priority-queue crate for that.

---

## 7. `const` & `static`

### 7.1 `const` vs `static` (`const-vs-static`)

`const` is *substituted inline* at each use (no address, no storage). `static` is *one instance at a fixed address* exposing `&'static T`.

| Situation | Use |
|-----------|-----|
| small constant (number, bool, tiny array, mask) | `const` |
| string literal | `const` (or `static`) |
| large lookup table | `static` (avoid duplicating at each use site) |
| need `&'static T` | `static` |
| mutable counter/flag | `static AtomicXxx` |
| lazily initialized value | `static LazyLock<T>` (stable 1.80) |
| single-writer init | `static OnceLock<T>` |

**Footgun:** `static mut` is unsafe to read, and taking a reference to one (`&`/`&mut`) is a hard error in edition 2024 (the `static_mut_refs` lint). For mutable global state use atomics, `OnceLock`, or `LazyLock`.

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

const MAX_RETRIES: u32 = 3;                       // inlined
static LOOKUP: [u8; 256] = [0u8; 256];            // one copy, shareable
static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
static CONFIG_PATH: LazyLock<String> =
    LazyLock::new(|| std::env::var("CONFIG_PATH").unwrap_or_else(|_| "/etc/app.toml".into()));

fn record() { REQUEST_COUNT.fetch_add(1, Ordering::Relaxed); }
```

### 7.2 `const fn` (`const-fn`)

Mark pure, allocation-free functions `const fn` so they work in const contexts (array lengths, initializers, const-generic args) *and* at runtime — zero cost in const contexts. Adding `const` is a non-breaking change; the main restrictions are heap allocation and most trait-method calls.

```rust
const fn align_up(n: usize, align: usize) -> usize { (n + align - 1) & !(align - 1) }
const ALIGNED: usize = align_up(13, 8);           // 16, at compile time
```

### 7.3 `const { }` blocks (`const-block`)

`const { expr }` (Rust 1.79) forces compile-time evaluation *inside* a regular function — for compile-time assertions (build fails, not runtime panic), inlined one-off values, and per-element array initializers.

```rust
const SIZE: usize = 64;
fn process(buf: &[u8]) {
    const { assert!(SIZE.is_power_of_two(), "SIZE must be a power of two") };  // compile-time
    assert!(buf.len() <= SIZE);                                                // runtime (dynamic)
}
```

### 7.4 Const generics (`const-generics`)

Parameterize over constant *values* with `<const N: usize>` — one monomorphized copy per value, no runtime length field, compile-time size checks. Supports integer/bool/char (not yet float/custom). `N` is usually inferred from an array argument; defaults allowed since 1.65 (`Buf<const N: usize = 64>`).

```rust
fn sum<const N: usize>(arr: [i32; N]) -> i32 { arr.iter().sum() }
let _ = sum([1, 2, 3, 4]);                        // N = 4, inferred

struct Buffer<const N: usize> { data: [u8; N], len: usize }
impl<const N: usize> Buffer<N> {
    const fn new() -> Self { Self { data: [0u8; N], len: 0 } }
}
```

---

## Rules & anti-patterns checklist

Scannable DO/DON'T, grouped by category. Rule ids are preserved for cross-reference.

**Naming**
- `name-types-camel` — DO `UpperCamelCase` for types/traits/enums. Compiler-warned otherwise.
- `name-variants-camel` — DO `UpperCamelCase` for variants; be specific, don't repeat the enum name.
- `name-funcs-snake` — DO `snake_case` for fns/methods/vars/modules.
- `name-consts-screaming` — DO `SCREAMING_SNAKE_CASE` for consts/statics/associated consts.
- `name-acronym-word` — DO treat acronyms as words (`HttpServer`); DON'T `HTTPServer`. Clear word boundaries.
- `name-as-free` — DO use `as_` only for free O(1) reference conversions. DON'T allocate behind `as_`.
- `name-to-expensive` — DO use `to_` for allocating/computing conversions. Signals cost to callers.
- `name-into-ownership` — DO use `into_` for `self`-consuming conversions; usually cheap, moves out.
- `name-no-get-prefix` — DON'T prefix simple getters with `get_`; `user.name()`. Reserve `get`/`get_mut` for fallible/computed access.
- `name-is-has-bool` — DO prefix boolean methods (`is_`/`has_`/`can_`); prefer positive form.
- `name-iter-convention` / `name-iter-method` — DO name iterator methods `iter`/`iter_mut`/`into_iter`; impl `IntoIterator` for `&T`/`&mut T`/`T`.
- `name-iter-type-match` — DO name the iterator struct after its method (`keys`→`Keys`); DON'T call it `Iterator`.
- `name-type-param-single` — DO single-letter type params (`T`,`E`,`K`,`V`); push bounds to `where`.
- `name-lifetime-short` — DO short lifetimes (`'a`,`'de`,`'src`); prefer elision.
- `name-crate-no-rs` — DON'T suffix crate names `-rs`/`-rust`; name for purpose.

**Documentation**
- `doc-all-public` — DO `///` every public item incl. fields/variants. Enforce with `missing_docs`.
- `doc-module-inner` — DO `//!` at the top of `lib.rs`/`mod.rs`/module files for module docs.
- `doc-crate-readme` — DO `#![doc = include_str!("../README.md")]` + `readme = "README.md"` for one source of truth.
- `doc-examples-section` — DO a runnable `# Examples`; doctests keep them correct.
- `doc-question-mark` — DO use `?` in examples, not `.unwrap()`; models propagation, fails on error.
- `doc-hidden-setup` — DO hide boilerplate with `# `; use `no_run`/`ignore` where appropriate.
- `doc-errors-section` — DO a `# Errors` section for every `-> Result` fn.
- `doc-panics-section` — DO a `# Panics` section; document *why* panic over `Result`; point at safe alternatives.
- `doc-safety-section` — DO a `# Safety` section on every `unsafe fn`/trait listing caller preconditions.
- `doc-intra-links` / `doc-link-types` — DO `[`Type`]` intra-doc links (verified, auto-updating); fail CI on broken links.
- `doc-cargo-metadata` — DO fill `description`/`license`/`repository`/`keywords`/`categories` for publishing.

**Project layout**
- `proj-flat-small` — DO keep < 10-file projects flat; DON'T over-nest.
- `proj-lib-main-split` — DO put logic in `lib.rs`, keep `main.rs` thin; enables integration tests.
- `proj-bin-dir` — DO put extra binaries in `src/bin/`; no `[[bin]]` needed.
- `proj-mod-by-feature` — DO group by feature (`user/…`), not by layer (`models/…`).
- `proj-mod-rs-dir` — DO pick one multi-file module style and enforce it with a lint.
- `proj-pub-crate-internal` — DO `pub(crate)` internals; shrink the public surface.
- `proj-pub-super-parent` — DO `pub(super)` for helpers shared between sibling submodules.
- `proj-pub-use-reexport` — DO `pub use` a flat API over nested internals; rename for versioning.
- `proj-prelude-module` — DO offer a conservative `prelude`; removing items is breaking.
- `proj-workspace-large` — DO use a workspace for multi-crate projects (shared lock/cache).
- `proj-workspace-deps` — DO declare deps once in `[workspace.dependencies]`; `optional` goes in the member.
- `proj-msrv-declare` — DO set `rust-version`; test it in CI; bump = semver-minor.
- `proj-feature-additive` — DO make features strictly additive; `std` opt-in, not `no_std` opt-out; use `dep:`.
- `proj-build-rs-minimal` — DO keep `build.rs` minimal/deterministic; emit `rerun-if-changed`; no network; use `autocfg`.

**Observability**
- `obs-tracing-over-log` — DO use `tracing`; DON'T `println!` for diagnostics.
- `obs-structured-fields` — DO emit key-value fields; DON'T bake values into the message string.
- `obs-instrument-spans` — DO `#[instrument]`; DON'T hold a span guard across `.await` (use `.instrument`).
- `obs-levels-filter` — DO use levels meaningfully; filter with `EnvFilter`/`RUST_LOG`; prefer `try_from_default_env`.
- `obs-library-facade` — DON'T install a subscriber in a library; the binary owns setup.
- `obs-error-chain` — DO log the full chain (`{err:#}` / `?err`) once at the handling boundary.
- `obs-no-sensitive-data` — DON'T log secrets/PII; `skip(...)` them and use redacting newtypes/`secrecy`.

**Lints**
- `lint-deny-correctness` — DO `deny(clippy::correctness)`; these are real bugs.
- `lint-warn-suspicious` — DO warn (or deny) `clippy::suspicious`; likely bugs.
- `lint-warn-style` / `lint-warn-complexity` / `lint-warn-perf` — DO warn these groups for idiomatic, simple, efficient code.
- `lint-missing-docs` — DO `warn(missing_docs)`; adopt gradually then `deny`.
- `lint-unsafe-doc` — DO `warn(clippy::undocumented_unsafe_blocks)`; every `unsafe` block gets `// SAFETY:`.
- `lint-cargo-metadata` — DO `clippy::cargo` for published crates; catches metadata/wildcard-dep issues.
- `lint-cfg-check` — DO enable `unexpected_cfgs` and declare custom cfgs; catches feature-gate typos.
- `lint-pedantic-selective` — DON'T enable all of `pedantic`; cherry-pick, `allow` the noisy ones.
- `lint-clippy-nursery-selected` — DON'T enable the whole `nursery`; pick `significant_drop_tightening`, `redundant_clone`, `use_self`.
- `lint-workspace-lints` — DO configure lints once at workspace level; `priority = -1` on group entries.
- `lint-rustfmt-check` — DO run `cargo fmt --all --check` in CI.

**Collections**
- `coll-seq-choice` — DO default to `Vec`; `VecDeque` for queues; avoid `LinkedList`. DON'T `Vec::remove(0)` in a loop.
- `coll-map-choice` — DO `HashMap` default, `BTreeMap` for range/sorted, `IndexMap` for insertion order.
- `coll-set-membership` — DO `HashSet`/`BTreeSet` for membership/dedup; DON'T loop `Vec::contains`.
- `coll-binaryheap` — DO `BinaryHeap` for priority queues; `Reverse<T>` for a min-heap.

**Const/static**
- `const-vs-static` — DO `const` for inlined small values, `static` for one addressed instance; DON'T `static mut`.
- `const-fn` — DO mark pure allocation-free fns `const fn`; non-breaking, zero-cost in const contexts.
- `const-block` — DO use `const { assert!(...) }` for compile-time checks.
- `const-generics` — DO parameterize over values with `<const N: usize>` for array-generic code.

---

## Gotchas / footguns

- **`as_` that allocates** — the prefix promises O(1); if you `format!`/`clone`, rename to `to_`. Silently misleads on cost.
- **Span guard across `.await`** (`obs-instrument-spans`) — `let _g = span.enter()` held over `.await` attaches the span to whatever task resumes on that thread. Compiles clean, corrupts traces. Use `#[instrument]` or `.instrument(span).await`. Same shape as holding a `MutexGuard` across `.await` (`significant_drop_tightening` / `anti-lock-across-await`).
- **Log-and-return** (`obs-error-chain`) — logging at every layer multiplies one failure across your aggregator. Log once at the boundary; propagate elsewhere.
- **`#[instrument]` leaks secrets** (`obs-no-sensitive-data`) — it auto-captures *all* args as fields, including passwords. Always `skip` sensitive args. A `?arg`/`%arg` elsewhere re-leaks unless the type redacts in `Debug`/`Display`.
- **Library installs a subscriber** (`obs-library-facade`) — `tracing_subscriber::fmt::init()` in a lib panics or no-ops the second call and steals the application's control. Keep `tracing-subscriber` in `[dev-dependencies]` only.
- **`Vec::remove(0)` / `Vec::contains` in loops** (`coll-*`) — each is O(n); in a loop the whole thing is O(n²). Reach for `VecDeque` / `HashSet`.
- **`static mut`** (`const-vs-static`) — hard error to reference in edition 2024. Use atomics / `OnceLock` / `LazyLock`.
- **Large table as `const`** — `const` is substituted at each use site, potentially duplicating a big array. Use `static` for lookup tables.
- **cfg typos compile silently** (`lint-cfg-check`) — `#[cfg(feature = "serde_")]` is just dead code with no warning until `unexpected_cfgs` is on.
- **`optional` in `[workspace.dependencies]`** (`proj-workspace-deps`) — not allowed; set `optional = true` in the member crate.
- **Non-additive features** (`proj-feature-additive`) — a `no_std`-style opt-out feature breaks consumers when a *third* crate enables it, because Cargo unifies features. Always additive.
- **`enter()` vs entire `nursery`/`pedantic` groups** (`lint-*-selective`) — enabling the whole group floods CI with false positives and churns as lints graduate. Cherry-pick.
- **`missing_docs` misses private items** — it only fires on `pub`. Internal APIs still deserve docs, just not enforced by this lint.
- **README code fences as doctests** (`doc-crate-readme`) — with `include_str!`, untagged fences in the README get compiled by rustdoc. Tag non-Rust ones (` ```bash `, ` ```text `).
- **`priority` in workspace lints** (`lint-workspace-lints`) — without `priority = -1` on a group entry, a specific override may lose to the group. Group entries need lower priority than the overrides.

---

## Cheat-sheet

**Naming decision table**

| Situation | Convention | Rule |
|-----------|-----------|------|
| type/trait/enum | `UpperCamelCase` | `name-types-camel` |
| fn/var/module | `snake_case` | `name-funcs-snake` |
| const/static | `SCREAMING_SNAKE_CASE` | `name-consts-screaming` |
| acronym in name | word form (`Http`) | `name-acronym-word` |
| free borrow conversion | `as_` | `name-as-free` |
| allocating conversion | `to_` | `name-to-expensive` |
| consuming conversion | `into_` | `name-into-ownership` |
| simple getter | bare name | `name-no-get-prefix` |
| boolean query | `is_`/`has_`/`can_` | `name-is-has-bool` |
| iterator method | `iter`/`iter_mut`/`into_iter` | `name-iter-method` |

**Doc sections** — `# Examples` (always), `# Errors` (Result), `# Panics` (can panic), `# Safety` (unsafe). Use `?` + hidden `# ` setup; `[`Type`]` links.

**Lint policy starter (workspace root)**

| Group/lint | Level |
|------------|-------|
| `clippy::correctness`, `clippy::suspicious` | deny |
| `clippy::style`, `complexity`, `perf` | warn |
| `missing_docs`, `undocumented_unsafe_blocks` | warn |
| `unsafe_code` | deny (or forbid) |
| `unwrap_used`, `panic` | deny (allow in binaries) |
| `pedantic`, `nursery` | selective only |
| `rustdoc::broken_intra_doc_links` | deny |

**Collection picker**

| Need | Type |
|------|------|
| growable list | `Vec` |
| queue/deque | `VecDeque` |
| lookup, unordered | `HashMap` |
| sorted / range | `BTreeMap` / `BTreeSet` |
| insertion order + lookup | `IndexMap` |
| membership / dedup | `HashSet` / `BTreeSet` |
| priority queue | `BinaryHeap` (`Reverse` = min) |

**const/static picker**

| Value | Use |
|-------|-----|
| small constant | `const` |
| large table / `&'static` | `static` |
| mutable global | `static AtomicXxx` |
| lazy init | `static LazyLock<T>` |
| once init | `static OnceLock<T>` |
| compile-time assert | `const { assert!(..) }` |
| value-generic code | `<const N: usize>` |

**Project structure by scale** — < 10 files: flat `src/`. Library + logic: `lib.rs` + thin `main.rs`. Extra binaries: `src/bin/`. Multi-crate: workspace with `[workspace.dependencies]` + `[workspace.lints]`. Group by feature, expose flat via `pub use` + `prelude`.

**Observability stack** — `tracing` everywhere (libs: `tracing` only); structured fields; `#[instrument(skip(secrets))]`; `EnvFilter`/`RUST_LOG` in the binary; log the chain once at the boundary.

Cross-references: naming ↔ **api-guidelines.md** C-CASE/C-CONV/C-GETTER/C-ITER; docs ↔ C-EXAMPLE/C-QUESTION-MARK/C-FAILURE/C-LINK/C-METADATA and **microsoft-guidelines.md** M-CANONICAL-DOCS/M-MODULE-DOCS; formatting ↔ **style-guide.md**; idioms/anti-patterns ↔ **design-patterns.md**.
