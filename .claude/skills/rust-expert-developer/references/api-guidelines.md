# Rust API Guidelines

Distilled reference to the official Rust API Guidelines — the community's checklist for
designing idiomatic, unsurprising, future-proof public APIs. Every `C-xxx` code below is
verbatim from upstream. Use this when designing crate/module/type/fn signatures, naming,
trait impls, and public-surface decisions.

Source: https://rust-lang.github.io/api-guidelines/ (checklist at `/checklist.html`)

---

## Naming

### C-CASE — Casing conforms to RFC 430
- Types/traits/enum variants: `UpperCamelCase`. Modules/functions/methods/vars/macros: `snake_case`. Consts/statics: `SCREAMING_SNAKE_CASE`.
- Type params: concise `UpperCamelCase` (usually `T`). Lifetimes: short lowercase (`'a`).
- Treat acronyms as one word in CamelCase: `Uuid` not `UUID`; lowercase in snake_case (`is_xid_start`). Single letters count as a word only as the final segment (`btree_map`, `PI_2`).
- Crate names: avoid `-rs`/`-rust` suffix or prefix.

### C-CONV — Ad-hoc conversions follow `as_`, `to_`, `into_`
- `as_`: free, borrowed→borrowed view of the underlying representation (`str::as_bytes`).
- `to_`: expensive, borrowed→owned or same-abstraction transform (`Path::to_str`, `str::to_lowercase`).
- `into_`: variable cost, owned→owned deconstruction (`String::into_bytes`).
- Single-value wrappers expose `into_inner()`. Place `mut` in name where it sits in the type: `as_mut_slice` not `as_slice_mut`.

### C-GETTER — Getter names follow Rust convention
- Drop the `get_` prefix; use field-like names.
```rust
pub fn first(&self) -> &First { &self.first }
pub fn first_mut(&mut self) -> &mut First { &mut self.first }
```
- Keep `get` only when there is one obvious thing to retrieve (`Cell::get`). Bounds-checked getters may pair with `unsafe fn *_unchecked` variants.

### C-ITER — Collection iterator methods follow `iter`, `iter_mut`, `into_iter`
- For a container of `U`: `iter(&self) -> Iter` (`&U`), `iter_mut(&mut self) -> IterMut` (`&mut U`), `into_iter(self) -> IntoIter` (`U`).
- Applies to homogeneous collections and to methods (not free functions). Nuanced types opt out (`str::bytes`/`str::chars`).

### C-ITER-TY — Iterator type names match producing methods
- `iter()`→`Iter`, `iter_mut()`→`IterMut`, `into_iter()`→`IntoIter`, `keys()`→`Keys`. Module-prefix for clarity: `vec::IntoIter`.

### C-FEATURE — Feature names are free of placeholder words
- No `use-`/`with-` noise: `features = ["serde"]`, not `["use-serde"]` (matches Cargo's implicit optional-dep features).
- Features must be additive; avoid negative names like `no-abc`. Std pattern: `default = ["std"]`, `std = []`.

### C-WORD-ORDER — Names use a consistent word order
- Pick a word order and keep it. Std uses verb-object-error: `ParseBoolError`, `ParseIntError`, `StripPrefixError`, `RecvTimeoutError` (not `BoolParseError`). Consistency matters more than the specific order.

---

## Interoperability

### C-COMMON-TRAITS — Types eagerly implement common traits
- The orphan rule means only the trait's crate or the type's crate can impl — so implement common traits yourself; downstream users can't add them later.
- Eagerly derive/impl: `Copy`, `Clone`, `Eq`, `PartialEq`, `Ord`, `PartialOrd`, `Hash`, `Debug`, `Display`, `Default`.
- Providing both `Default` and a no-arg `new()` is conventional even if identical.

### C-CONV-TRAITS — Conversions use `From`, `AsRef`, `AsMut`
- Implement `From<T>`, `TryFrom<T>`, `AsRef<T>`, `AsMut<T>` where applicable.
- Never implement `Into`/`TryInto` directly — blanket impls derive them from `From`/`TryFrom`.

### C-COLLECT — Collections implement `FromIterator` and `Extend`
- Enables `collect`, `partition`, `unzip` (FromIterator = build new) and appending to existing (Extend). `Vec<T>` implements both.

### C-SERDE — Data structures implement `Serialize`, `Deserialize`
- Data-structure types (e.g. `IpAddr`) should implement Serde; compile-time markers (e.g. `LittleEndian`) need not.
- Gate behind an optional `serde` feature so you don't force the dep:
```rust
serde = { version = "1.0", optional = true, features = ["derive"] }

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct T { /* ... */ }
```

### C-SEND-SYNC — Types are `Send` and `Sync` where possible
- Auto-derived when appropriate; be careful with raw pointers. Assert with tests:
```rust
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
assert_send::<MyType>();
assert_sync::<MyType>();
```

### C-GOOD-ERR — Error types are meaningful and well-behaved
- Implement `std::error::Error`, be `Send + Sync`, and give a meaningful `Display`.
- `Send + Sync` is required for `thread::spawn` returns, sharing via `Arc`, and `io::Error::new`.
- Never use `()` as an error type (no `Display`, useless `Debug`, blocks `?`). Prefer a real struct/enum.
- `Display`: lowercase, no trailing punctuation, concise ("invalid IP address syntax"). `Error::description()` is deprecated — use `Display`.

### C-NUM-FMT — Binary number types provide `Hex`, `Octal`, `Binary` formatting
- For types supporting bitwise `|`/`&` (esp. bitflags), implement `UpperHex`/`LowerHex` (`{:X}`/`{:x}`), `Octal` (`{:o}`), `Binary` (`{:b}`). Plain quantity newtypes (`Nanoseconds(u64)`) don't need them.

### C-RW-VALUE — Generic reader/writer functions take `R: Read`/`W: Write` by value
- `Read`/`Write` are impl'd for `&mut R`/`&mut W`, so taking by value still lets callers pass `&mut`. Note this in docs so users know they can reuse the reader/writer. E.g. `serde_json::from_reader`, `to_writer`.

---

## Macros

### C-EVOCATIVE — Input syntax is evocative of the output
- Mirror real Rust syntax: use `struct`/`const` keywords and semicolons where the output has them, so input reads like the code it generates.

### C-MACRO-ATTR — Macros compose well with attributes
- Generated items must accept `#[cfg(...)]`, doc comments, and `#[derive(...)]` on individual items.

### C-ANYWHERE — Item macros work anywhere items are allowed
- Must work in module scope and function scope alike (path resolution is the common failure). Test both:
```rust
test_your_macro_in_a!(module);
#[test] fn anywhere() { test_your_macro_in_a!(function); }
```

### C-MACRO-VIS — Item macros support visibility specifiers
- Default private; `pub`-prefixed produces public items, matching normal Rust visibility rules.

### C-MACRO-TY — Type fragments are flexible
- `$t:ty` must accept primitives (`u8`, `&str`), relative paths (`m::Data`), absolute (`::base::Data`), upward (`super::Data`), and generics (`Vec<String>`). Naive expansions that break on paths are a common bug.

---

## Documentation

### C-CRATE-DOC — Crate-level docs are thorough and include examples
- The crate root doc should explain purpose and show real usage (see RFC 1687).

### C-EXAMPLE — All items have a rustdoc example
- Every public module/trait/struct/enum/fn/method/macro/type should have an example that shows *why*, not just *how*. Linking to a related item's example is acceptable when they overlap.

### C-QUESTION-MARK — Examples use `?`, not `try!`, not `unwrap`
- Examples are copied verbatim, so model good error handling. Hide boilerplate with `#`-prefixed lines:
```rust
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let v = example()?;
/// # Ok(()) }
/// ```
```

### C-FAILURE — Function docs include error, panic, and safety considerations
- `# Errors`: document when/why a `Result` returns `Err` and post-conditions on failure.
- `# Panics`: document panic conditions (`Vec::insert` panics if index out of bounds). When unsure, over-document.
- `# Safety`: every `unsafe fn` documents the invariants the caller must uphold.

### C-LINK — Prose contains hyperlinks to relevant things
- Link related items with `[`Type`]` shorthand and targets at the doc's end (`[`Type`]: trait.Type.html`). (RFC 1574.)

### C-METADATA — Cargo.toml includes all common metadata
- `[package]`: `authors`, `description`, `license`, `repository`, `keywords`, `categories`. Optional: `documentation` (only if not on docs.rs), `homepage` (only if a distinct site).

### C-RELNOTES — Release notes document all significant changes
- Maintain release notes; flag breaking changes (RFC 1105). Tag every crates.io release with an annotated git tag.

### C-HIDDEN — Rustdoc does not show unhelpful implementation details
- Hide internal impls with `#[doc(hidden)]`; restrict visibility with `pub(crate)` instead of exposing.

---

## Predictability

### C-SMART-PTR — Smart pointers do not add inherent methods
- Inherent methods on a smart pointer clash with method resolution on the pointee. Use associated-function form so call sites are unambiguous:
```rust
Box::into_raw(boxed)   // not boxed.into_raw()
```

### C-CONV-SPECIFIC — Conversions live on the most specific type involved
- Put conversions on the more specific type (`str` over `&[u8]`): `str::as_bytes`, `str::from_utf8`. Prefer `to_`/`as_`/`into_` over `from_` for chainability.

### C-METHOD — Functions with a clear receiver are methods
- If an op clearly belongs to a type, make it a method (no import, autoref, discoverable, clear ownership):
```rust
impl Foo { pub fn frob(&self, w: Widget) { } }   // not fn frob(foo: &Foo, ...)
```

### C-NO-OUT — Functions do not take out-parameters
- Return tuples/structs instead of `&mut` out-params: `fn foo() -> (Bar, Bar)`. Exception: reusing a caller-owned buffer (`Read::read(&mut self, buf: &mut [u8])`).

### C-OVERLOAD — Operator overloads are unsurprising
- Implement `std::ops` traits only where the operator's usual meaning/laws hold (e.g. `Mul` for multiplication-like, associative ops).

### C-DEREF — Only smart pointers implement `Deref` and `DerefMut`
- `Deref` drives implicit coercion and method resolution — restrict to genuine smart pointers (`Box`, `String`, `Rc`, `Arc`, `Cow`). Don't use it for inheritance-style reuse.

### C-CTOR — Constructors are static, inherent methods
- Primary: `Example::new()`. Domain names: `File::open`, `TcpStream::connect`. Secondary: `_with_foo`. Conversions: `from_*`.
- `from_*` constructors (vs the `From` trait) may be `unsafe` and take extra disambiguating args; `From` cannot.

---

## Flexibility

### C-INTERMEDIATE — Functions expose intermediate results to avoid duplicate work
- Return useful byproducts: `Vec::binary_search` returns the insertion index on miss; `HashMap::insert` returns the old value; `String::from_utf8` hands back the bytes and valid-up-to offset on error.

### C-CALLER-CONTROL — Caller decides where to copy and place data
- Need ownership → take by value (don't borrow-then-clone). Don't need it → borrow. Use `Copy` as a bound only when required, not to hint "cheap".

### C-GENERIC — Functions minimize assumptions via generics
- Fewer input assumptions = wider use. Prefer `fn foo<I: IntoIterator<Item = i64>>(iter: I)` over `&[i64]`/`&Vec<i64>`. `AsRef<Path>` (like `File::open`) accepts strings, `Path`, `OsString`. Trade-off: code bloat from monomorphization, homogeneous-only, verbose signatures.

### C-OBJECT — Traits are object-safe if useful as a trait object
- Decide early: object-safe traits can't use generics or `Self` (except as receiver). Exclude non-object-safe methods with `where Self: Sized`:
```rust
trait MyTrait {
    fn object_safe(&self, i: i32);
    fn not_object_safe<T>(&self, t: T) where Self: Sized;
}
```
- Objects give heterogeneous collections + smaller code; cost is dynamic dispatch and no generic methods (see `io::Read`, `Iterator`).

---

## Type safety

### C-NEWTYPE — Newtypes provide static distinctions
- Wrap same-underlying-type values to prevent mixups at compile time:
```rust
struct Miles(pub f64);
struct Kilometers(pub f64);
fn are_we_there_yet(d: Miles) -> bool { }   // Kilometers won't type-check
```

### C-CUSTOM-TYPE — Arguments convey meaning through types, not `bool`/`Option`
- `Widget::new(Small, Round)` beats `Widget::new(true, false)`. Enums self-document and extend cleanly (add `ExtraLarge`).

### C-BITFLAG — Flag sets are `bitflags`, not enums
- Enum = exactly one choice; a set of on/off flags wants the `bitflags` crate for typesafe `|`/`.contains()`:
```rust
bitflags! { struct Flags: u32 { const A = 0b001; const B = 0b010; } }
f(Flags::A | Flags::B);
```

### C-BUILDER — Builders enable construction of complex values
- For many/optional inputs, add a builder: required args in the constructor, chainable config methods, terminal build/spawn.
- **Non-consuming (preferred):** methods take `&mut self`, return `&mut Self`; terminal takes `&self`. Works for one-liners and conditional config.
- **Consuming:** methods take/return owned `self`; use when the terminal op must own the builder.

---

## Dependability

### C-VALIDATE — Functions validate their arguments
- Prefer, in order: (1) **static** — encode validity in types (`Ascii` wrapper over raw `u8`); (2) **dynamic** — check and fail via `panic!`/`Result`/`Option`; (3) `debug_assert!` for expensive checks stripped in release; (4) opt-out `*_unchecked`/`raw` variants for hot paths with known-valid input.

### C-DTOR-FAIL — Destructors never fail
- A failing `Drop` during a panic aborts. Do cleanup in `Drop` while ignoring/logging errors; expose a separate `close() -> Result` for fallible teardown.

### C-DTOR-BLOCK — Destructors that may block have alternatives
- Blocking in `Drop` is hard to debug — provide an explicit non-blocking, infallible teardown method as an alternative.

---

## Debuggability

### C-DEBUG — All public types implement `Debug`
- Implement `Debug` on every public type (rare exceptions), so values are inspectable.

### C-DEBUG-NONEMPTY — `Debug` representation is never empty
- Empty/edge values still print something: `""` → `"\"\""`, `Vec::<bool>::new()` → `"[]"`.

---

## Future proofing

### C-SEALED — Sealed traits protect against downstream implementations
- Prevent external impls (so you can add methods later) via a private supertrait outsiders can't name:
```rust
pub trait TheTrait: private::Sealed { fn method(); }
mod private { pub trait Sealed {} impl Sealed for usize {} }
impl TheTrait for usize { }
```
- Note: removing/changing public method signatures is still breaking. Document "sealed, not meant to be implemented outside this crate".

### C-STRUCT-PRIVATE — Structs have private fields
- Public fields pin the representation and forbid invariants/validation. Public fields only for passive C-style data; otherwise use getters/setters.

### C-NEWTYPE-HIDE — Newtypes encapsulate implementation details
- Hide leaky return types behind a newtype instead of exposing `Enumerate<Skip<I>>`:
```rust
pub struct MyResult<I>(Enumerate<Skip<I>>);
impl<I: Iterator> Iterator for MyResult<I> { /* delegate */ }
```
- `impl Trait` is an alternative (with trade-offs around `Debug`/`Clone`).

### C-STRUCT-BOUNDS — Data structures do not duplicate derived trait bounds
- Put bounds on impls, not the struct — struct bounds become breaking to change:
```rust
#[derive(Clone, Debug, PartialEq)] struct Good<T> { }        // good
#[derive(Clone, Debug, PartialEq)] struct Bad<T: Clone> { }  // avoid
```
- Never bound the struct on `Clone`/`PartialEq`/`PartialOrd`/`Debug`/`Display`/`Default`/`Error`/`Serialize`/`Deserialize`. Exceptions: references an associated type, `?Sized`, or a `Drop` impl needing the bound.

---

## Necessities

### C-STABLE — Public dependencies of a stable crate are stable
- A crate can't be ≥1.0 while any public dependency (types appearing in your public API) is pre-1.0. Beware leaks via `impl From<other_crate::Error>` even when the type is otherwise wrapped in a private struct.

### C-PERMISSIVE — Crate and its dependencies have a permissive license
- Prefer dual `license = "MIT OR Apache-2.0"` (matches Rust itself) with both LICENSE files. Single MIT/BSD is fine; avoid Apache-only for perfect Rust compatibility. Dependencies' licenses can restrict yours — keep them permissive too.

---

## Quick checklist
- **Naming**: RFC 430 casing; `as_`/`to_`/`into_` cost semantics; no `get_` prefix; `iter`/`iter_mut`/`into_iter` + matching `Iter`/`IterMut`/`IntoIter`; consistent word order.
- **Traits to impl eagerly**: `Debug`, `Clone`, `PartialEq`/`Eq`, `Hash`, `Display`, `Default`, `Send`/`Sync`; `From`/`TryFrom`/`AsRef` (never `Into`); `FromIterator`+`Extend` for collections; optional-feature Serde.
- **Errors**: real type implementing `Error` + `Send + Sync` + lowercase no-punctuation `Display`; never `()`.
- **Predictability**: methods over free fns; no out-params; `Deref` only for smart pointers; static inherent constructors (`new`/`from_*`).
- **Type safety**: newtypes for static distinctions; enums/custom types over `bool`/`Option` args; `bitflags` for flag sets; builders for complex construction.
- **Future-proofing**: private struct fields; seal traits you own; hide leaky types behind newtypes; bounds on impls not structs.
- **Docs**: crate-level docs + per-item examples using `?`; document Errors/Panics/Safety; link related items; full Cargo.toml metadata.
- **Dependability**: validate args (prefer static/type-level); destructors never fail or block.
- **Necessities**: stable public deps before 1.0; permissive (MIT OR Apache-2.0) licensing.
