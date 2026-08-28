---
name: rust-expert-developer
description: >-
  Expert guidance for writing, reviewing, and refactoring idiomatic Rust. Invoke
  whenever you author or change Rust code (`.rs` files, `Cargo.toml`), design a
  crate's public API (naming, signatures, trait impls, conversions, error types,
  constructors/builders, newtypes, feature flags), decide crate/module structure,
  choose or spot idioms/design-patterns/anti-patterns, or format code by hand to
  match rustfmt. Now also fires on deeper engineering work: generics & traits (`dyn`
  vs `impl Trait`, object safety, associated types vs params, blanket/sealed/marker
  traits, GATs, HRTBs, coherence/orphan rule); newtype, type-state and `PhantomData`
  designs; closures & functional/iterator style (`Fn`/`FnMut`/`FnOnce`, combinator
  pipelines); concurrency (threads, channels, `Arc`/`Mutex`/`RwLock`, atomics,
  `Send`/`Sync`, rayon); async/await (tokio, `select!`/`join!`, cancellation,
  `spawn_blocking`, no-lock-across-`.await`); error handling (`thiserror` vs `anyhow`,
  `?`, `From`/`TryFrom`/`FromStr`); serialization (serde, zero-copy, binary formats,
  numeric-overflow safety); unsafe & macros (`unsafe`, FFI, MaybeUninit, Miri,
  `macro_rules!`, `syn`/`quote` proc-macros); testing & benchmarking (unit/integration/
  doc tests, `proptest`, `insta`, `mockall`, `criterion`); and crate architecture &
  API evolution (module tree, feature flags, `#[non_exhaustive]`, semver). Distills
  SEVEN authoritative sources — the Rust API Guidelines, Microsoft's Pragmatic Rust
  Guidelines, the Rust Style Guide, the Rust Unofficial Design Patterns book, the Rust
  Reference notation, the Microsoft "Rust Patterns" book, and the leonardomso/rust-skills
  265-rule idiom/anti-pattern catalog — into a core-principles cheat-sheet plus deep,
  layered reference files. Triggers: "write/review/refactor Rust", "is this idiomatic?",
  "design this API/trait/type", "name this", "dyn vs impl", "object safety / GAT",
  "newtype / type-state", "closure / iterator chain", "Arc/Mutex", "async/await",
  "thiserror / anyhow", "serde / zero-copy", "unsafe", "macro_rules", "criterion / proptest",
  "public interface", "crate layout".
---

# Rust Expert Developer

Authoritative, distilled guidance for writing production-grade idiomatic Rust. The
reference files under `references/` are faithful digests of the canonical sources —
**read the relevant one before doing that kind of work**; the source codes (`C-*`,
`M-*`) are preserved verbatim so you can cite and cross-check.

The references are organised in **three layers**, and this skill practises
progressive disclosure — the Core principles below are the always-on cheat-sheet; the
detail lives in the layer files, opened on demand:

- **Layer 1 — Canonical guidelines.** The authoritative rulebooks: the Rust API
  Guidelines (`C-*`), Microsoft's Pragmatic Guidelines (`M-*`), the Style Guide, the
  Unofficial Design Patterns, and the Rust Reference grammar notation. These say *what
  the rules are*.
- **Layer 2 — Deep engineering patterns** (Microsoft "Rust Patterns" book). One file
  per domain — generics & traits, newtype/type-state/phantom, closures & functional
  style, concurrency, async, errors & conversions, serialization, unsafe & macros,
  testing & benchmarking, crate architecture. These are the *type-level and systems
  source of truth* — consult before designing anything non-trivial in that domain.
- **Layer 3 — Idiom & anti-pattern rule catalogs** (leonardomso/rust-skills, 265
  scannable DO/DON'T rules). Two fast-scan files — naming/docs/project concerns, and
  ownership/memory/performance. Use these to *review* code and answer "is this
  idiomatic?" quickly.

Move down the layers as depth demands: Layer 1 for the canonical rule and its code,
Layer 2 when you must *design* a trait/API/concurrency/async/unsafe surface, Layer 3
to lint an existing surface. Layers 2 and 3 defer to Layer 1 for the canonical rules
rather than re-deriving them, and cross-link by code (e.g. `api-guidelines.md`
`C-NEWTYPE`, `microsoft-guidelines.md` `M-*`, `design-patterns.md`).

## How to use this skill

1. Apply the **Core principles** below to every Rust change — they are the
   highest-value cross-cutting rules, drawn from all three layers.
2. For deeper work, open the matching reference file from the **Reference router**.
   Pick the layer by intent:
   - Need the canonical rule / its `C-*`/`M-*` code, a formatting question, or the
     right named idiom → **Layer 1**.
   - Designing or reviewing a real trait/generic/type/concurrency/async/unsafe/macro/
     serde/test/crate surface → **Layer 2** (the domain file). Start here for anything
     that needs a design decision.
   - Fast idiom/anti-pattern check on existing code, or "is this named/laid-out/
     allocated idiomatically?" → **Layer 3**.
3. When a question spans layers, read Layer 2 for the design and cite Layer 1 for the
   rule; use Layer 3 as the review checklist.

## Reference router

### Layer 1 — Canonical guidelines (the authoritative rulebooks)

| Reference file | Covers | Consult when… |
|---|---|---|
| `references/api-guidelines.md` | All 44 `C-*` Rust API Guidelines, grouped by chapter (naming, interoperability, macros, docs, predictability, flexibility, type-safety, dependability, debuggability, future-proofing). | Designing or reviewing any **public API**: naming, signatures, trait impls, conversions, errors, constructors, newtypes, generics, sealing, feature flags, versioning. |
| `references/microsoft-guidelines.md` | The 11 `M-*` "Universal" items from Microsoft's Pragmatic Rust Guidelines. | Deciding lint/tooling policy, `Debug`/`Display` on public types, crate-vs-feature splitting, weasel-word-free naming, documenting magic values, structured logging. |
| `references/style-guide.md` | Default rustfmt formatting + non-formatting conventions + `Cargo.toml` rules. | Formatting by hand, settling a layout question, or explaining *why* rustfmt reformatted something. |
| `references/design-patterns.md` | 15 idioms, the Behavioural/Creational/Structural/FFI design patterns, and the 3 anti-patterns, plus functional idioms. | Choosing how to build something, or reviewing code for idiom violations / anti-patterns. |
| `references/reference-notation.md` | The grammar-notation table used throughout the Rust Reference (`?* +`, ranges, `[]`, `~`, `^` hard-cut, ordered alternation). | Reading or citing a grammar production from the Rust Reference. |

### Layer 2 — Deep engineering patterns (Microsoft "Rust Patterns" book)

| Reference file | Covers | Consult when… |
|---|---|---|
| `references/generics-and-traits.md` | Generics (monomorphization, bounds, const generics, turbofish) and traits in full (associated types vs params, object safety, `dyn` vs `impl Trait`, blanket/sealed/marker traits, GATs, HRTBs, coherence) — the type-level source of truth. | Consult before designing any trait or generic API, choosing between static and dynamic dispatch (generics/`impl Trait` vs `dyn`/enum), deciding associated type vs generic parameter, working around the orphan rule, making a trait object-safe or sealed, using const generics/GATs/HRTBs, or picking which std traits to derive — and when reviewing any type-level Rust. |
| `references/newtype-typestate-and-phantom.md` | Newtype (IDs, validated invariants, orphan-rule, `repr(transparent)`, `NonZero`), type-state state machines & typestate builders, config-trait & dual-axis patterns, and `PhantomData` (variance, drop-check, units, lifetime branding) for compile-time-unrepresentable illegal states. | When designing a domain type or ID/unit wrapper, a protocol/state-machine API, a builder that must enforce required fields at compile time, a zero-cost unit or typed-handle abstraction, a wrapper over a raw pointer (choosing variance/drop-check), an FFI newtype, or escaping the orphan rule; and when reviewing Rust for stringly-typed APIs, boolean-flag state soup, or misuse of `Deref`. |
| `references/closures-and-functional-style.md` | Closures and the `Fn`/`FnMut`/`FnOnce` family (capture, `move`, `impl Fn` vs `Box<dyn Fn>`, fn pointers), higher-order API design, and functional-vs-imperative Rust — iterator/combinator chains, `Option`/`Result` monadic combinators, `fold`/`scan`/`try_fold`, laziness, and when loops beat pipelines. | Consult before writing or reviewing any callback-taking API, closure-returning function, or iterator/combinator pipeline: choosing `Fn` trait bounds, deciding `impl Fn` vs `Box<dyn Fn>` vs `&dyn Fn`, using `move`/clone-before-move, designing bracketed-access (`with`) APIs, or deciding between a functional pipeline and an imperative loop (including `Option`/`Result` combinator vs `if let`/`match`, collect tricks, and allocation/laziness concerns). |
| `references/concurrency-and-shared-state.md` | Threads, channels (`mpsc`/crossbeam/`select!`/worker pools/actors), and shared state — `Box`/`Rc`/`Arc`/`Weak`, `Cell`/`RefCell`/`Mutex`/`RwLock`/`Condvar`/`Cow`, atomics & orderings, `Send`/`Sync`, scoped threads, rayon, thread-locals, deadlock/poisoning; message-passing vs shared-memory (async is separate). | Consult before writing or reviewing any Rust that spawns threads, shares data between threads, or chooses a synchronization primitive: picking channels vs `Mutex`/`RwLock` vs atomics, sizing bounded channels, building worker pools or actors, using `Arc`/`Rc`/`Weak` or interior mutability, selecting a memory `Ordering`, using scoped threads or rayon, or diagnosing deadlocks/poisoning/leaks. Not for async/`.await`/tokio (see `async-await.md`). |
| `references/async-await.md` | Async/await model — `Future`/`poll`/pinning, tokio runtimes & tasks, structured concurrency (`join!`/`try_join!`/`JoinSet`/`select!`), async channels, cancel-safety, `spawn_blocking`, no-lock-across-`.await`, async-fn-in-traits/`AsyncFn` bounds. | Consult before writing or reviewing any async/`.await` code: spawning tasks, choosing between `join!`/`try_join!`/`JoinSet`/`select!`, wiring `mpsc`/`oneshot`/`broadcast`/`watch` channels, handling cancellation/graceful shutdown, offloading blocking/CPU work, fixing `!Send`-across-`.await` or lock-across-`.await` issues, configuring a tokio runtime, or defining async fns in traits. |
| `references/error-handling-and-conversions.md` | Error handling (`Result`/`?`, `thiserror` vs `anyhow`, context & source chains, panic vs `Result`, unwrap discipline) and conversion traits (`From`/`Into`, `TryFrom`, `FromStr`, `AsRef`/`AsMut`, parse-don't-validate). | Consult before writing or reviewing any Rust that returns `Result`/`Option`, defines an error type, chooses `thiserror` vs `anyhow`, decides whether to panic or unwrap, adds error context, or implements `From`/`Into`/`TryFrom`/`FromStr`/`AsRef`/`AsMut` conversions. |
| `references/serialization-and-binary-data.md` | serde model, zero-copy/borrowing deserialization, binary formats & layout (`repr(C)`/zerocopy/bytemuck/bytes), endianness, and numeric conversion/overflow safety. | When writing or reviewing any (de)serialization boundary, wire-format DTO or config struct, custom serde impl, binary-protocol/packet parser, zero-copy buffer handling, or arithmetic and casts on untrusted numbers. |
| `references/unsafe-and-macros.md` | Unsafe Rust (5 superpowers, `MaybeUninit`, `SAFETY` docs, `unsafe extern`/FFI, raw pointers, manual `Send`/`Sync`, UB, Miri) and macros (`macro_rules!` fragments/hygiene/recursion, proc macros with `syn`/`quote`). | Consult before writing or reviewing any `unsafe` block, any `extern`/FFI boundary, `#[unsafe(no_mangle)]`, manual `Send`/`Sync`, `MaybeUninit`, or any macro — declarative `macro_rules!` or a `syn`/`quote` procedural macro (derive/attribute/function-like), and when deciding macro vs function/generic. |
| `references/testing-and-benchmarking.md` | Testing (unit/integration/doc tests, `cfg(test)`, AAA, panics, proptest, insta snapshots, RAII fixtures, trait/mockall mocking, tokio async, loom) and benchmarking (criterion, `black_box`, baselines). | Consult before writing or reviewing any Rust test module, `tests/` integration file, doctest, `benches/` criterion harness, or when choosing between `should_panic` vs `Result`, property vs example tests, snapshot vs `assert_eq!`, hand-written doubles vs mockall, or setting up coverage/regression tracking. |
| `references/crate-architecture-and-api-design.md` | Structuring a crate/workspace and designing its public API — module tree & visibility, facade/prelude, feature flags, newtypes, parse-don't-validate, `#[non_exhaustive]`, sealed traits, `#[must_use]`, and semver-safe evolution. | Consult when laying out a new crate or workspace, curating `lib.rs` / re-exports, adding or reviewing feature flags, choosing parameter types (`impl Into`/`AsRef`/`Cow`), designing newtypes or validated types, deciding whether an API change is a breaking change, or reviewing a public surface for ergonomics and forward-compatibility. |

### Layer 3 — Idiom & anti-pattern rule catalogs (leonardomso/rust-skills, 265 scannable rules)

| Reference file | Covers | Consult when… |
|---|---|---|
| `references/idioms-naming-docs-and-project.md` | Cross-cutting crate concerns — naming/casing/conversion-prefix conventions, doc practices (sections, links, README, metadata), project & workspace layout, tracing/logging observability, clippy lint policy, collection choice, and const/static usage — as a scannable DO/DON'T rule catalog. | Consult before creating or reviewing any crate skeleton, public surface naming, `Cargo.toml`, module/workspace tree, logging/tracing code, or lint configuration; when deciding which collection or const/static form to use; or when answering "is this named/documented/laid-out idiomatically?" |
| `references/idioms-ownership-memory-and-performance.md` | Engineering DO/DON'T catalog for ownership & borrowing, memory/allocation (`Box`/`Rc`/`Arc`/`Cow`, SmallVec/arena, capacity hints, avoiding clones/allocs), hot-path performance (iterators, entry API, buffering, hashers), codegen tuning (LTO/inline/cold/SIMD), and general anti-patterns (excess clone, over-abstraction, premature opt, stringly-typed, `String`-for-`&str`, `Vec`-for-slice). | Before writing or reviewing any Rust that lives in a hot path, stores a type in large collections, takes `&String`/`&Vec`/`&PathBuf` in a signature, or reaches for `.clone()`/`format!`/`Box<dyn>`; when choosing between `Rc`/`Arc`/`Cow`/`Box`, a collection type (`Vec`/`SmallVec`/`ArrayVec`/`ThinVec`/boxed slice), or an interior-mutability primitive; when deciding capacity hints, inline/cold attributes, or release-profile/LTO/PGO/target-cpu settings; and whenever profiling reveals allocation churn, needless copies, or bounds-check/vectorization misses. |

## Core principles (always apply)

**Correctness & safety first**
- Make illegal states unrepresentable: encode invariants in the type system rather
  than checking at runtime; prefer newtypes and enums over bare primitives/booleans
  (`C-NEWTYPE`, `C-CUSTOM-TYPE`, `C-BITFLAG`). For real state machines and validated
  wrappers reach for **type-state** and validated newtypes, not stringly-typed or
  boolean-flag soup (`newtype-typestate-and-phantom.md`) — and type-state transitions
  must **consume `self`** (take `self`, not `&self`) or the guarantee evaporates.
- Handle errors with `Result` + `?`; give errors meaningful types that impl
  `std::error::Error` + `Display` + `Debug` + `Send + Sync + 'static`
  (`C-GOOD-ERR`, `C-CTOR`). Reserve panics for genuine programmer bugs. Libraries use
  `thiserror` (concrete enums); applications may use `anyhow` — but never expose
  `anyhow::Error` from a library API (`error-handling-and-conversions.md`).
- Keep `unsafe` minimal, isolated, and justified with a `// SAFETY:` comment on
  **every** unsafe block; `unsafe` unlocks only the 5 superpowers — borrow/aliasing/
  lifetime rules still apply and `miri` enforces them (`M-STATIC-VERIFICATION`,
  `unsafe_op_in_unsafe_fn`, `unsafe-and-macros.md`).

**Concurrency & async discipline**
- Choose your sharing model deliberately: prefer message-passing (channels) for
  ownership transfer, `Arc<Mutex<_>>`/`RwLock` for shared mutable state, atomics for
  simple counters/flags — don't default to one (`concurrency-and-shared-state.md`).
- **Never hold a lock guard across `.await`** (deadlock/starvation; a `std::sync`
  guard is also `!Send`). `async fn` is lazy — calling it without `.await`/spawn is a
  silent no-op; and dropping a `JoinHandle` detaches, it does **not** cancel — use
  `abort()`/`CancellationToken` (`async-await.md`).

**Type-level & dispatch design**
- Static vs dynamic dispatch is a deliberate trade-off: generics/`impl Trait` for
  speed and monomorphization, `dyn Trait`/enum for heterogeneous collections and
  smaller code. `Self` in return position or generic methods kill object safety, so
  `dyn`-compatibility must be designed in, not assumed (`generics-and-traits.md`).
- Put trait bounds on the impls that need them, not on the struct — struct-level
  bounds leak to every use (derives are the conditional-impl exception).

**Naming (RFC 430)** — `UpperCamelCase` types/traits/variants, `snake_case`
fns/modules/vars, `SCREAMING_SNAKE_CASE` consts. Acronyms are one word (`Uuid`, not
`UUID`). Conversions follow cost: `as_` (free borrow), `to_` (expensive), `into_`
(owns) (`C-CONV`). No `get_` prefix on getters (`C-GETTER`). Strip weasel words —
`Service`/`Manager`/`Factory` (`M-WEASEL-WORDS`); keep names short since the module
path already gives context (`M-SHORT-NAMES`).

**API design**
- Accept borrowed/generic inputs, return concrete owned types: take `&str`/`&[T]`/
  `impl AsRef<..>`/`impl IntoIterator`, not `&String`/`&Vec<T>` (`C-GENERIC`,
  borrowed-types idiom).
- Derive the common traits eagerly where they fit: `Debug` (on *every* public type —
  `M-PUBLIC-DEBUG`), `Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default`, plus
  `Serde` behind a feature (`C-COMMON-TRAITS`, `C-SERDE`). Types should be `Send`+
  `Sync` unless there's a reason not to (`C-SEND-SYNC`).
- Constructors are associated `fn new() -> Self` + a `Default` impl; use the builder
  pattern for many/optional args (`C-CTOR`, `C-BUILDER`). Reserve associated fns for
  construction — put other computation in free functions (`M-REGULAR-FN`).
- Smart-pointer-style: prefer explicit conversions over `Deref` polymorphism
  (anti-pattern) — `DerefMut` on a validated newtype even bypasses its constructor
  invariants. Keep the public surface small and use `#[non_exhaustive]` / sealed
  traits / private fields to preserve room to evolve (`C-STRUCT-PRIVATE`, `C-SEALED`,
  `C-STABLE`). Cargo features must be **additive** — a feature may never remove or
  alter existing items (`crate-architecture-and-api-design.md`).

**Idioms over cleverness** — `format!` to build strings; `mem::take`/`mem::replace`
to move out of `&mut`; iterators/combinators over index loops; `Option`/`Result`
adapters over manual matching where clearer; pass owned data into closures with an
explicit `let` binding when needed (`move` grabs the whole named place — bind the
field to a local first for disjoint capture). Don't `.clone()` just to silence the
borrow checker, and don't `#![deny(warnings)]` in committed code (anti-patterns).

**Docs & observability** — every public item has a `///` doc with an example that
runs as a doctest (`C-EXAMPLE`, `C-QUESTION-MARK`); document errors, panics, and
safety (`C-FAILURE`). Document every magic value (prefer a named `const`) and its
rationale (`M-DOCUMENTED-MAGIC`). Use structured logging with message templates and
redaction, not `format!`'d log strings (`M-LOG-STRUCTURED`). With `tracing`: never
hold a `span.enter()` guard across `.await` (use `#[instrument]` or
`.instrument(span).await`), `skip()` sensitive args, and keep `tracing-subscriber` in
a library's `[dev-dependencies]` only (`idioms-naming-docs-and-project.md`).

**Formatting** — 4-space block indent, 100-col width, trailing commas on multiline
lists, version-sorted + grouped `use`s. Just run `rustfmt`; the style guide explains
the rules when you must format by hand.

**Tooling gate** — before considering a change done, run `cargo fmt`, `cargo clippy`
(all groups), and `cargo test`. Suppress a lint with `#[expect(..., reason = "…")]`
(warns when stale), never a bare `#[allow]` (`M-LINT-OVERRIDE-EXPECT`).

### High-value gotchas (from Layers 2 & 3)

Fast recall for the traps that compile clean and fail later — see the linked file for
the full treatment:

- **generics-and-traits** — A blanket impl permanently blocks any specific impl for a
  covered type (E0119; no stable specialization). Object-safety errors surface at the
  `dyn` use site, far from the trait; generic methods / `Self`-by-value return /
  associated consts silently break `dyn`-compatibility (why `Clone` isn't
  object-safe — use `fn clone_box(&self) -> Box<dyn Trait>`).
- **newtype-typestate-and-phantom** — `PhantomData<T>` inherits `T`'s `Send`/`Sync`
  and drop-check; use `*const T` / `fn() -> T` to decouple. `&mut T` is invariant, so
  wrapping a mutated `*mut T` with `PhantomData<&'a T>` is unsound — use
  `PhantomData<&'a mut T>`.
- **closures-and-functional-style** — `move` controls *how* captures are taken, not
  `Fn`/`FnMut`/`FnOnce` (the trait is inferred from the body). A returned `impl FnMut`
  needs a `mut` binding at the call site; divergent closure bodies have different
  types and won't unify as `impl Fn` — box them.
- **concurrency-and-shared-state** — A channel never closes until **every** `Sender`
  (original + clones) is dropped — forgetting `drop(tx)` hangs the consumer forever.
  `RefCell` double-borrow panics at runtime, not compile time. `Mutex`/`RwLock`
  poisoning makes later `.lock()` return `Err` after a panic-while-held — recover with
  `into_inner()` or use `parking_lot`. Deadlock comes from inconsistent lock ordering
  (A→B vs B→A) — acquire in one global order or use actors.
- **async-await** — `select!` silently drops losing branches' in-flight state (the
  cancel-safety trap: compiles clean, fails under load).
- **error-handling-and-conversions** — `?` requires a `From<Source>` for your error
  type (E0277) unless using `anyhow`'s blanket conversion; two `#[from]` variants
  sharing an inner type fail to compile (ambiguous `From`). `anyhow` `Display` differs:
  `{}` top message, `{:#}` one-line chain, `{:?}` chain + backtrace.
- **serialization-and-binary-data** — Zero-copy borrowed structs cannot outlive the
  input buffer; use `DeserializeOwned`/`String`/`Cow` for storage. `Cow` only borrows
  with `#[serde(borrow)]` (escaped strings still force `Owned`). `deny_unknown_fields`
  is incompatible with `flatten`; internally-tagged enums reject primitive/tuple
  variants (use adjacently tagged).
- **unsafe-and-macros** — `assume_init` on partially-initialized memory is UB even if
  you plan to fill the rest. Mutating through `&self` via a raw pointer is UB unless
  the field is inside `UnsafeCell`. Arenas (bumpalo/FixedArena) don't run destructors —
  only allocate `Drop`-free types.
- **testing-and-benchmarking** — `harness = false` is mandatory for criterion or the
  bench never runs; missing `black_box` lets the optimizer delete the whole benchmark.
  Bare `#[should_panic]` is a false green — always pass `expected = "..."`;
  `should_panic` on integer overflow passes in debug but fails in `--release` (wraps
  silently) — gate with `#[cfg(debug_assertions)]`.
- **crate-architecture-and-api-design** — Use `dep:` syntax for optional deps or Cargo
  creates an implicit public feature named after the dependency. `#[non_exhaustive]`
  must be added on day one (adding it later is itself a breaking change, and it's a
  no-op inside the defining crate). A sealed trait leaks if the private supertrait is
  reachable via any `pub use`.
- **idioms (ownership/memory)** — `BufWriter` drop swallows flush errors — always
  `flush()?` explicitly. Drop order is silent and observable (struct fields top-down,
  locals bottom-up) — reordering breaks RAII guards.
- **idioms (naming/docs/project)** — Log-and-return multiplies one error across the
  aggregator; log once at the handling boundary, propagate elsewhere with
  `?`/`.context()`. `#[instrument]` auto-captures **all** args as fields including
  secrets — `skip()` them and use redacting newtypes.

## Suggested workflow for a Rust change

1. **Design** the types/API first — reach for `api-guidelines.md` and the
   type-safety principles; make invalid states unrepresentable.
2. **Implement** idiomatically — check `design-patterns.md` for the right idiom and
   to avoid anti-patterns.
3. **Name & document** per the naming + docs principles above.
4. **Format & lint** — `cargo fmt` + `cargo clippy`; consult `style-guide.md` only
   for disputes.
5. **Verify** — `cargo test` (and doctests). Behavior bugs pass compilation, so
   exercise the real path when the change has runtime surface.

> In this repo (Helios), "verify" also means the dual-target compile gate from
> `CLAUDE.md` — both the native `server` build and the `wasm32` `web` build must stay
> green. See the project instructions for the exact commands.
