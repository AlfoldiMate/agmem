# Generics & Traits In Depth

The type-level core of idiomatic Rust: generics end-to-end (monomorphization, bounds, `where`-clauses, const generics, turbofish, blanket impls) and traits in full (associated types vs type params, default methods, supertraits, coherence/orphan rule, trait objects & object safety, `dyn` vs `impl Trait`, marker/sealed traits, GATs, HRTBs, and the std traits worth implementing). Consult this before designing any trait or generic API, choosing a dispatch strategy, or reviewing type-level code. Distilled from the Microsoft *Rust Patterns* book (ch01 Generics, ch02 Traits) and the rust-skills `trait-*` rules. For the canonical naming/API rules see **api-guidelines.md** (C-*), **microsoft-guidelines.md** (M-*), and **design-patterns.md**; this file adds the deeper engineering patterns and a rule catalog.

---

## 1. Monomorphization and Zero-Cost Generics

Rust generics are **monomorphized** — the compiler emits one specialized copy of a generic item per concrete type it's instantiated with. This is the opposite of Java/C# type erasure. Bounds are checked at the *definition* site (unlike C++ templates, which only fail at instantiation), so errors are early and clear (Patterns Book ch01 §Monomorphization).

```rust
fn max_of<T: PartialOrd>(a: T, b: T) -> T {
    if a >= b { a } else { b }
}
// max_of(3_i32, 5) and max_of(2.0_f64, 7.0) generate two separate functions;
// no vtable, no runtime dispatch — identical to hand-written specialized code.
```

Bounds are mandatory at definition — the WRONG way relies on a capability the compiler can't see:

```rust,ignore
// WRONG: error at definition site — T doesn't implement Display
fn broken<T>(val: T) { println!("{val}"); }
```

```rust
// RIGHT: state the bound you use
fn fixed<T: std::fmt::Display>(val: T) { println!("{val}"); }
```

### Code bloat — the cost of monomorphization

Each unique instantiation duplicates the body. A generic `serialize<T: Serialize>` used with 50 types → 50 copies. Mitigate with the **outline pattern**: keep the generic shell tiny, delegate to a non-generic inner function that exists only once (Patterns Book ch01 §When Generics Hurt).

```rust,ignore
fn serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let v = serde_json::to_value(value)?; // generic part: minimal
    serialize_value(v)                    // non-generic core: one copy in the binary
}
fn serialize_value(v: serde_json::Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&v)
}
```

> Rule of thumb: generics on hot paths where inlining matters; `dyn Trait` on cold paths (logging, config, error handling) where a vtable call is negligible and binary size / compile time matter more.

## 2. Generics vs Enums vs Trait Objects — the primary decision

Three ways to say "different types, same interface" (Patterns Book ch01 §Decision Guide):

| Approach | Dispatch | Set | Extensible by others? | Overhead |
|---|---|---|---|---|
| **Generics** (`<T: Trait>` / `impl Trait`) | Static, monomorphized | Open | Yes | Zero — inlinable |
| **Enum** + `match` | Static branch | **Closed** | No | Zero — no vtable, inline storage |
| **Trait object** (`dyn Trait`) | Dynamic, vtable | Open | Yes | Fat pointer + indirect call |

Decision flow: Do you know **all** possible types at compile time? → small closed set: **enum**. Open set: **generics** (or `dyn` on cold paths). Types only known at runtime, or a heterogeneous collection needed: **`dyn Trait`** in `Vec<Box<dyn Trait>>`. See §12 (enum dispatch) for the closed-set fast path and rule `anti-type-erasure`.

## 3. Bounds, `where`-clauses, and bound placement

Add bounds **only where the code needs them**, and prefer `where`-clauses once they get long (rule `type-generic-bounds`). Bounds on a `struct` definition are almost always wrong — they infect every use, even storage.

```rust
use std::fmt::Debug;

// WRONG: bound on the type forces Clone even to just store a T
// struct Container<T: Clone> { items: Vec<T> }

// RIGHT: store anything; require capabilities only on the impls that use them
struct Container<T> { items: Vec<T> }

impl<T: Clone> Container<T> {
    fn duplicate(&self) -> Vec<T> { self.items.clone() }
}
impl<T: Debug> Container<T> {
    fn dump(&self) { println!("{:?}", self.items); }
}
```

`where`-clauses also express bounds inline expressions can't (`Vec<U>: Debug`, `I::Item: Clone`):

```rust
fn process<I>(iter: I)
where
    I: Iterator,
    I::Item: Clone + Debug,
{
    for x in iter { println!("{:?}", x.clone()); }
}
```

**Implied bounds**: a supertrait bound propagates. `fn f<T: Foo>` where `trait Foo: Clone + Debug {}` gives you `T: Clone` and `T: Debug` for free. This is *conditional impl* territory too — implement `Clone`/`Debug` for `Wrapper<T>` only when `T` is `Clone`/`Debug` (see §11, C-COMMON-TRAITS in **api-guidelines.md**).

## 4. Turbofish

Type arguments are usually inferred; when they aren't, disambiguate with `::<>` ("turbofish"). Needed most on `collect`, `parse`, and free functions whose type params don't appear in the arguments.

```rust
let v = (0..5).collect::<Vec<i32>>();       // collect can't infer the container
let n = "42".parse::<u32>().unwrap();        // parse's target is turbofished
let id = std::any::TypeId::of::<String>();   // no value argument to infer from
```

Argument-position `impl Trait` (APIT, `fn foo(x: impl Trait)`) **cannot** be turbofished by the caller — `foo::<X>()` is rejected. If callers must name the type, use an explicit `<T>` type parameter instead (Patterns Book ch02 §impl Trait positions).

## 5. Const generics & `const fn`

**Const generics** parameterize over *values* (integer, `bool`, `char`), not just types — one monomorphized copy per value, no runtime length field, no macros (Patterns Book ch01 §Const Generics; rule `const-generics`).

```rust
// One function for every array size; N is inferred from the argument.
fn sum<const N: usize>(arr: [i32; N]) -> i32 { arr.iter().sum() }
let _ = sum([1, 2, 3, 4]);      // N = 4
let _ = sum([0i32; 8]);         // N = 8

// Capacity is part of the type — mismatches are compile errors.
struct Buffer<const N: usize> { data: [u8; N], len: usize }
impl<const N: usize> Buffer<N> {
    const fn new() -> Self { Self { data: [0u8; N], len: 0 } }
    fn push(&mut self, b: u8) -> bool {
        if self.len < N { self.data[self.len] = b; self.len += 1; true } else { false }
    }
}
```

Const generics enable compile-time-checked dimensional correctness (e.g. `multiply(a: &Matrix<M,N>, b: &Matrix<N,P>) -> Matrix<M,P>` rejects mismatched inner dimensions). Rust 1.59+ supports defaults: `struct Buf<const N: usize = 64>`. Floating-point / custom-type const params are not yet stable.

**`const fn`** is Rust's `constexpr` — evaluable at compile time when used in `const`/`static` context (rule `const-fn`). Make constructors and simple utilities `const fn` whenever possible; it costs nothing and unlocks const contexts.

```rust
const fn celsius_to_fahrenheit(c: f64) -> f64 { c * 9.0 / 5.0 + 32.0 }
const BOILING_F: f64 = celsius_to_fahrenheit(100.0); // computed at compile time

// panic!() in const context becomes a compile error if actually reached:
const fn checked_div(a: u32, b: u32) -> u32 {
    if b == 0 { panic!("division by zero"); }
    a / b
}
const OK: u32 = checked_div(100, 4);   // 25
// const BAD: u32 = checked_div(100, 0); // compile error
```

`const fn` (as of 1.79+) allows arithmetic/bitops, `if`/`match`/`loop`/`while`, local `let mut`, calls to other `const fn`, references, and `panic!`. It **cannot** heap-allocate (`Box`/`Vec`/`String`), call trait methods (inherent only), or do I/O. `const fn` replaces `lazy_static!` for compile-time-computable tables.

---

## 6. Associated types vs generic type parameters

The single most-consequential trait design choice (Patterns Book ch02 §Associated Types; rule `trait-associated-type-vs-generic`).

- **Associated type** (`type Item;`) — **one** output binding per implementing type. Part of the type's identity; callers never turbofish it; you can only implement the trait *once* per type. Use when there's exactly one natural output: `Iterator::Item`, `Deref::Target`, `Add::Output`, `Future::Output`.
- **Generic parameter** (`trait T<Rhs>`) — **many** impls per type, one per parameter. Use when a type meaningfully implements the trait for many inputs: `From<T>`, `AsRef<T>`, `PartialEq<Rhs>`, `Add<Rhs>`.

Intuition: if it makes sense to ask "what is the `Item` of this?", use an associated type. If "can this convert to `f64`? to `String`?", use a generic parameter.

```rust
// WRONG: generic param on a trait with exactly one output — forces noise everywhere
// trait Parser<Output> { fn parse(&self, s: &str) -> Option<Output>; }
// fn run<P: Parser<String>>(...)  // callers must name Output constantly

// RIGHT: associated type — P::Output is unambiguous, no turbofish
trait Parser {
    type Output;
    fn parse(&self, input: &str) -> Option<Self::Output>;
}
struct NumberParser;
impl Parser for NumberParser {
    type Output = f64;
    fn parse(&self, s: &str) -> Option<f64> { s.trim().parse().ok() }
}
fn run<P: Parser>(p: &P, s: &str) -> Option<P::Output> { p.parse(s) }
```

```rust
// Generic param — the SAME type adds to many Rhs types (impossible with an assoc type)
#[derive(Clone, Copy)]
struct Vec2 { x: f64, y: f64 }
impl std::ops::Add<Vec2> for Vec2 {
    type Output = Vec2;
    fn add(self, r: Vec2) -> Vec2 { Vec2 { x: self.x + r.x, y: self.y + r.y } }
}
impl std::ops::Add<f64> for Vec2 {          // second impl — needs the generic param
    type Output = Vec2;
    fn add(self, r: f64) -> Vec2 { Vec2 { x: self.x + r, y: self.y + r } }
}
```

Note `Add` combines both: `Rhs` is a generic param (defaults to `Self`) *and* `Output` is an associated type — because addition has many operands but one result type. To constrain an associated type in a bound, write `P: Parser<Output = f64>` (cleaner than a free type param).

## 7. Default methods & supertraits

Define a trait as a **minimal orthogonal set of required methods** plus defaulted methods built on them (rule `trait-default-methods`). `Iterator` builds `map`/`filter`/`fold`/dozens on the single required `next`. Defaults double as documentation of the canonical relationships; implementors may override one for performance without changing observable semantics.

```rust
trait Summarise {
    fn sentences(&self) -> Vec<String>;            // required: the only thing to provide

    fn first_sentence(&self) -> Option<String> {   // defaulted — free
        self.sentences().into_iter().next()
    }
    fn is_empty(&self) -> bool { self.sentences().is_empty() }
}

struct Article { body: String }
impl Summarise for Article {
    fn sentences(&self) -> Vec<String> {
        self.body.split('.').map(str::trim).filter(|s| !s.is_empty())
            .map(str::to_owned).collect()
    }
    // first_sentence + is_empty come for free
}
```

Rules for defaults: they may only call other methods on `Self` (not external state); an override must preserve semantics; document which methods are required vs defaulted.

**Supertraits** require another trait as a prerequisite — `trait Entity: Identifiable + Timestamped` forces implementors to also implement both (Patterns Book ch02 §Supertraits). Std hierarchy: `Error: Display + Debug`, `Copy: Clone`, `Eq: PartialEq`, `Ord: Eq + PartialOrd`.

```rust
use std::fmt;
trait Error: fmt::Display + fmt::Debug {
    fn source(&self) -> Option<&(dyn Error + 'static)> { None }
}
```

## 8. Blanket impls & the orphan rule (coherence)

A **blanket impl** gives behavior to *every* type satisfying a bound (rule `trait-blanket-impl`). Std does this: `impl<T: Display> ToString for T`, reflexive `From<T> for T`, `impl<T: Error> From<T> for Box<dyn Error>`.

```rust
trait Loggable { fn log(&self); }
impl<T: std::fmt::Debug> Loggable for T {  // every Debug type is now Loggable
    fn log(&self) { eprintln!("[LOG] {self:?}"); }
}
```

Blanket impls are powerful but **irreversible**: because stable Rust has no specialization, you **cannot** also add a specific impl for a type the blanket already covers — that overlap is coherence error **E0119**. Design them once, carefully. Adding a public blanket impl is a potential semver-breaking (major) change when downstream types could provide overlapping impls (rule `trait-blanket-impl`) — not merely a minor bump. Blanket impls must live in the crate that owns the **trait**.

**Orphan rule / coherence** (rule `trait-coherence-newtype`): for any `impl Trait for Type`, *either the trait or the type must be local to your crate*. `impl ForeignTrait for ForeignType` is rejected (E0117) even with a type parameter — this is what stops two crates providing conflicting impls. The fix: wrap the foreign type in a local **newtype** (see api-guidelines.md C-NEWTYPE), then implement the foreign trait on the wrapper.

```rust
use std::fmt;
#[repr(transparent)]                 // same ABI as the inner type (needed for FFI/transmute)
struct CommaSeparated(Vec<i32>);
impl fmt::Display for CommaSeparated {   // Display foreign, CommaSeparated local → OK
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut it = self.0.iter().peekable();
        while let Some(n) = it.next() {
            write!(f, "{n}")?;
            if it.peek().is_some() { write!(f, ", ")?; }
        }
        Ok(())
    }
}
impl From<Vec<i32>> for CommaSeparated { fn from(v: Vec<i32>) -> Self { Self(v) } }
```

Provide `From`/`Into` and `inner()`/`into_inner()` accessors so callers can move in and out (api-guidelines.md C-CONV-TRAITS, C-GETTER).

## 9. Extension traits — adding methods to foreign types

The orphan rule blocks inherent methods on foreign types; the standard workaround is an **extension trait**: a new local trait whose methods have a blanket impl for any type meeting a bound. Callers `use` the trait and the methods appear (rules `api-extension-trait`, `trait-blanket-impl`; api-guidelines.md C-METHOD). Pervasive in the ecosystem: `itertools::Itertools`, `futures::StreamExt`, `tokio::io::AsyncReadExt`, `tower::ServiceExt`, `anyhow::Context`. Convention: **`Ext` suffix**.

```rust
pub trait IteratorExt: Iterator {
    fn mean(self) -> Option<f64>
    where Self: Sized, Self::Item: Into<f64>;
}
impl<I: Iterator> IteratorExt for I {         // blanket impl over ALL iterators
    fn mean(self) -> Option<f64>
    where Self: Sized, Self::Item: Into<f64> {
        let (mut sum, mut count) = (0.0f64, 0u64);
        for x in self { sum += x.into(); count += 1; }
        if count == 0 { None } else { Some(sum / count as f64) }
    }
}
// use crate::IteratorExt; then:  data.iter().copied().mean()
```

Use an extension trait to add convenience methods to foreign/generic types; **don't** when the method needs private fields (use a newtype), belongs on a type you own (add it inherently), or must work without an import (inherent methods only).

## 10. Trait objects: `dyn`, object safety, vtables

A `&dyn Trait` / `Box<dyn Trait>` is a **fat pointer** — two words: `(data_ptr, vtable_ptr)`. The vtable holds `drop_in_place`, `size`, `align`, and one function pointer per method. A call loads the vtable pointer, indexes the method, and calls it passing `data_ptr` as `self`. A plain `Circle` on the stack carries no vtable pointer — the pointer lives in the fat pointer, not the object (Patterns Book ch02 §Under the Hood).

```rust
use std::mem::size_of;
trait Drawable { fn draw(&self); fn area(&self) -> f64; }
struct Circle { r: f64 }
impl Drawable for Circle {
    fn draw(&self) { println!("circle r={}", self.r); }
    fn area(&self) -> f64 { std::f64::consts::PI * self.r * self.r }
}
fn main() {
    let shapes: Vec<Box<dyn Drawable>> = vec![Box::new(Circle { r: 5.0 })];
    for s in &shapes { s.draw(); }           // vtable dispatch
    assert_eq!(size_of::<&Circle>(), 8);     // thin pointer
    assert_eq!(size_of::<&dyn Drawable>(), 16); // fat: data + vtable
}
```

### Object safety (dyn-compatibility) rules

A trait is usable as `dyn Trait` only if every method is dispatchable through a vtable (rule `trait-object-safety`; Rust Reference "Object Safety"):

| Feature | Allowed in `dyn Trait`? |
|---|---|
| `&self` / `&mut self` / `self` methods | Yes |
| Method returning `Self` by value | No (size unknown) — use `Box<Self>` or gate `where Self: Sized` |
| Generic method params (`fn f<T>`) | No (vtable can't hold infinite monomorphizations) — gate `where Self: Sized` |
| Associated function (no receiver, e.g. `fn create() -> Self`) | No |
| Associated constants | No |
| Associated types | Yes — but must be named in the `dyn` type (`dyn Iterator<Item = u32>`); GATs are not allowed |
| `Self: Sized` bound on the trait itself | Makes the whole trait non-object-safe |

The **workaround**: gate non-dispatchable methods with `where Self: Sized` — they're excluded from the vtable but callable on concrete types:

```rust
trait Transformer {
    fn transform_str(&self, s: &str) -> String;    // in vtable
    fn name(&self) -> &str;
    fn transform_debug<T: std::fmt::Debug>(&self, v: T) -> String
    where Self: Sized {                              // excluded from vtable
        self.transform_str(&format!("{v:?}"))
    }
}
struct Shout;
impl Transformer for Shout {
    fn transform_str(&self, s: &str) -> String { s.to_uppercase() }
    fn name(&self) -> &str { "shout" }
}
fn apply_all(ts: &[Box<dyn Transformer>], input: &str) {  // dyn works
    for t in ts { println!("[{}] {}", t.name(), t.transform_str(input)); }
}
```

When in doubt, write `let _: Box<dyn YourTrait>;` and let the compiler tell you.

### Static vs dynamic dispatch — cost model

| Aspect | Static (`impl Trait` / generics) | Dynamic (`dyn Trait`) |
|---|---|---|
| Call overhead | Zero — inlined | One indirection per call (~2ns) |
| Inlining | Yes | No (opaque fn pointer) |
| Binary size | Larger (one copy/type) | Smaller (shared code) |
| Pointer size | Thin (1 word) | Fat (2 words) |
| Heterogeneous collections | No | Yes (`Vec<Box<dyn Trait>>`) |
| Trait must be object-safe | No | Yes |

Default to generics for hot, simple code; reach for `dyn` when you need heterogeneous storage, runtime-chosen types, plugin/callback registration, or to cap binary size (rule `trait-dyn-vs-generic`). Tight loops calling a trait method millions of times can be 2–10× slower via vtable — but for cold paths the flexibility is worth it.

## 11. `impl Trait` — argument vs return position; RPITIT

`impl Trait` has **different semantics** by position (Patterns Book ch02 §impl Trait):

- **APIT** (argument position, `fn foo(x: impl T)`) — *caller* picks the type; pure sugar for `fn foo<X: T>(x: X)`. No turbofish at the call site.
- **RPIT** (return position, `fn foo() -> impl T`) — *callee* picks one concrete (existential) type; the caller only sees "some `T`". Avoids naming unnameable closure/iterator types and avoids `Box`.

```rust
fn evens(limit: i32) -> impl Iterator<Item = i32> {   // RPIT: callee picks the type
    (0..limit).filter(|x| x % 2 == 0)
}
fn print_all(items: impl Iterator<Item = i32>) {      // APIT == <I: Iterator<Item=i32>>
    for x in items { println!("{x}"); }
}
```

**RPITIT** (Rust 1.75+): `-> impl Trait` directly in trait definitions — each impl returns its own concrete type, no `Box<dyn>`, no associated type needed:

```rust
trait Container {
    fn items(&self) -> impl Iterator<Item = &str>;
}
struct CsvRow { fields: Vec<String> }
impl Container for CsvRow {
    fn items(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(String::as_str)
    }
}
```

Don't reach for `Box<dyn Trait>` when `impl Trait` works — that's needless heap + dynamic dispatch (rule `anti-type-erasure`). Use RPIT/`impl Trait` when callers don't need to name the type; use an **associated type** when callers must name or constrain it.

## 12. Enum dispatch — static polymorphism for closed sets

For a **closed set** of trait implementors, replace `dyn Trait` with an enum whose variants hold the concrete types. This removes the vtable indirection and the `Box` heap allocation, stores values inline (cache-friendly), and lets the compiler inline everything (Patterns Book ch02 §Enum Dispatch; rule `anti-type-erasure` "Pattern: Enum Instead of dyn").

```rust
trait Sensor { fn read(&self) -> f64; fn name(&self) -> &str; }
struct Gps { lat: f64 }
struct Thermometer { temp_c: f64 }
impl Sensor for Gps { fn read(&self) -> f64 { self.lat } fn name(&self) -> &str { "GPS" } }
impl Sensor for Thermometer { fn read(&self) -> f64 { self.temp_c } fn name(&self) -> &str { "Temp" } }

enum AnySensor { Gps(Gps), Thermometer(Thermometer) }
impl Sensor for AnySensor {                 // implement the trait on the enum for interop
    fn read(&self) -> f64 {
        match self { AnySensor::Gps(s) => s.read(), AnySensor::Thermometer(s) => s.read() }
    }
    fn name(&self) -> &str {
        match self { AnySensor::Gps(s) => s.name(), AnySensor::Thermometer(s) => s.name() }
    }
}
// Vec<AnySensor>: no Box, no vtable, contiguous storage.
```

The match-arm delegation is boilerplate; a `macro_rules!` helper, or the **`enum_dispatch`** crate, generates it. Decision: closed set → enum dispatch (`< ~20` variants manual, more → `enum_dispatch`); open set / runtime plugins → `dyn Trait`. Vtable ~2ns vs branch ~0.3ns; enum wins on perf, `dyn` wins on openness.

| Property | `dyn Trait` | Enum Dispatch |
|---|---|---|
| Dispatch cost | Vtable (~2ns) | Branch (~0.3ns) |
| Heap alloc | Usually (Box) | None (inline) |
| Cache-friendly | No (pointer chasing) | Yes (contiguous) |
| Open to new types | Yes | No (closed) |
| Trait must be object-safe | Yes | No |

## 13. Marker & sealed traits

**Marker traits** have no methods; they tag a type with a property. Std: `Send`, `Sync`, `Unpin`, `Sized`, `Copy`. Your own marker gates APIs at compile time (connects to the type-state pattern, api-guidelines.md / rule `api-typestate`):

```rust
trait Calibrated {}                       // marker
struct CalibratedSensor { reading: f64 }
impl Calibrated for CalibratedSensor {}
fn record<S: Calibrated>(_sensor: &S) { /* only calibrated sensors accepted */ }
```

**Sealed traits** can be *used* by downstream crates but not *implemented* by them — a private supertrait only your crate can satisfy (rule `api-sealed-trait`; api-guidelines.md C-SEALED). This lets you add methods later without a breaking change and guarantee all impls are correct.

```rust
mod private { pub trait Sealed {} }        // private module → external crates can't name it
pub trait Driver: private::Sealed {
    fn execute(&self, sql: &str) -> String;
    // added later — not breaking, since no external impls can exist:
    fn execute_pretty(&self, sql: &str) -> String { self.execute(sql) }
}
pub struct Postgres;
impl private::Sealed for Postgres {}       // only this crate can do this
impl Driver for Postgres { fn execute(&self, sql: &str) -> String { format!("pg: {sql}") } }
```

Seal when API stability / correctness / future method additions matter; **don't** seal when you want users to provide their own impls (extension points). Don't seal std-style open traits like `Iterator`.

## 14. GATs — Generic Associated Types

Since Rust 1.65, associated types can take their own generic (lifetime/type) parameters. The headline use is the **lending iterator** — one whose yielded item borrows from the iterator (`&self`) rather than the underlying collection, which plain `Iterator` cannot express (Patterns Book ch02 §GATs).

```rust
trait LendingIterator {
    type Item<'a> where Self: 'a;
    fn next(&mut self) -> Option<Self::Item<'_>>;
}
struct WindowIter<'d> { data: &'d [u8], pos: usize, win: usize }
impl<'d> LendingIterator for WindowIter<'d> {
    type Item<'a> = &'a [u8] where Self: 'a;
    fn next(&mut self) -> Option<&[u8]> {
        if self.pos + self.win <= self.data.len() {
            let w = &self.data[self.pos..self.pos + self.win];
            self.pos += 1;
            Some(w)
        } else { None }
    }
}
```

Reach for GATs for lending iterators, streaming parsers, or any trait whose associated type's lifetime depends on the `&self` borrow. For most code, plain associated types suffice — don't add a GAT until a lifetime dependency forces it.

## 15. HRTBs — Higher-Ranked Trait Bounds

`for<'a>` means "for **all** lifetimes `'a`", not one fixed lifetime the caller chose. It appears mostly on closure bounds like `Fn(&T) -> &U` (usually inferred) and in `serde`'s `for<'de> Deserialize<'de>` (Patterns Book ch02 §HRTBs).

```rust
// The closure must work for ANY input lifetime, not a single fixed one.
fn apply<F>(f: F, data: &str) -> &str
where
    F: for<'a> Fn(&'a str) -> &'a str,
{
    f(data)
}
// apply(|s| s.trim(), "  hi  ") == "hi"
```

You'll rarely write `for<'a>` by hand, but recognize it in errors ("expected a `for<'a> Fn(&'a ...)` bound") — it means the compiler needs a closure/impl that's polymorphic over the borrow. `DeserializeOwned` is defined as `for<'de> Deserialize<'de>` — "deserializable from data of any lifetime", i.e. the result doesn't borrow the input.

## 16. Type erasure with `Any` / `TypeId`

The escape hatch for storing values of *unknown* types and downcasting later (like `void*` / `object`) — `std::any::Any` (Patterns Book ch02 §Any). Use for plugin systems, type-indexed maps, and error downcasting (`anyhow::Error::downcast_ref`). Prefer generics/enums/trait objects when the type set is known — `Any` trades compile-time safety for flexibility.

```rust
use std::any::{Any, TypeId};
use std::collections::HashMap;
struct AnyMap(HashMap<TypeId, Box<dyn Any + Send>>);
impl AnyMap {
    fn new() -> Self { Self(HashMap::new()) }
    fn insert<T: Any + Send>(&mut self, v: T) { self.0.insert(TypeId::of::<T>(), Box::new(v)); }
    fn get<T: Any + Send>(&self) -> Option<&T> {
        self.0.get(&TypeId::of::<T>())?.downcast_ref()
    }
}
```

## 17. Typed-command pattern — GADT-style return-type safety

Traits with associated types give Haskell-GADT guarantees: the command type *determines* its response type, so mixing units is a compile error and byte-parsing lives in exactly one place per command (Patterns Book ch02 §Typed Commands). Combine with domain **newtypes** (`Celsius`, `Rpm`, `Volts`) — see api-guidelines.md C-NEWTYPE, rule `type-newtype-validated`.

```rust
use std::io;
#[derive(Debug, PartialEq)] struct Celsius(f64);
#[derive(Debug, PartialEq)] struct Rpm(u32);

trait IpmiCmd {
    type Response;                                   // GADT "index" — binds command → result
    fn payload(&self) -> Vec<u8>;
    fn parse_response(&self, raw: &[u8]) -> io::Result<Self::Response>;
}
struct ReadTemp { id: u8 }
impl IpmiCmd for ReadTemp {
    type Response = Celsius;                          // "this command yields a temperature"
    fn payload(&self) -> Vec<u8> { vec![self.id] }
    fn parse_response(&self, raw: &[u8]) -> io::Result<Celsius> { Ok(Celsius(raw[0] as i8 as f64)) }
}
struct ReadFan { id: u8 }
impl IpmiCmd for ReadFan {
    type Response = Rpm;
    fn payload(&self) -> Vec<u8> { vec![self.id] }
    fn parse_response(&self, raw: &[u8]) -> io::Result<Rpm> {
        Ok(Rpm(u16::from_le_bytes([raw[0], raw[1]]) as u32))
    }
}
struct Bmc;
impl Bmc {
    fn execute<C: IpmiCmd>(&self, cmd: &C) -> io::Result<C::Response> {  // zero dyn, monomorphized
        let raw = self.send(&cmd.payload())?;
        cmd.parse_response(&raw)
    }
    fn send(&self, _p: &[u8]) -> io::Result<Vec<u8>> { Ok(vec![0x19, 0x00]) }
}
// let t: Celsius = bmc.execute(&ReadTemp { id: 0x20 })?;
// let r: Rpm     = bmc.execute(&ReadFan  { id: 0x30 })?;
// `if t > r {}` → compile error (mismatched types). Parsing bugs live in one place.
```

For a heterogeneous runtime-loaded script, wrap commands and readings in enums (`AnyCmd`/`AnyReading`) and stay `dyn`-free via enum dispatch (§12).

## 18. Capability mixins — associated types as zero-cost composition

Compose behavior à la Ruby mixins, fully at compile time: **ingredient traits** (associated type + accessor), **mixin traits** (supertrait bounds on ingredients + default method bodies), and a **blanket impl** that auto-injects the methods (Patterns Book ch02 §Capability Mixins).

```rust
use std::io;
trait I2cBus { fn read(&self, addr: u8) -> io::Result<u16>; }
trait GpioPin { fn set_high(&self) -> io::Result<()>; }

trait HasI2c  { type I2c: I2cBus;   fn i2c(&self)  -> &Self::I2c; }   // ingredient
trait HasGpio { type Gpio: GpioPin; fn gpio(&self) -> &Self::Gpio; } // ingredient

trait FanDiag: HasI2c + HasGpio {                                    // mixin: needs both
    fn read_fan_rpm(&self, id: u8) -> io::Result<u32> {              // default body
        Ok(self.i2c().read(0x48 + id)? as u32 * 60)
    }
}
impl<T: HasI2c + HasGpio> FanDiag for T {}   // blanket: provide ingredients, get the method
```

Add `where Self::I2c: SomeCapability` to individual defaults so a **conditional method** only *exists* when the ingredient supports it — a compile error, not a runtime `respond_to?`. Partial test rigs that implement only some ingredients get only the corresponding mixins, enforced by the compiler. Multiple mixins can share one ingredient with no diamond problem.

## 19. Common std traits worth implementing

Public types should implement the standard traits so they interoperate (rule `api-common-traits`; api-guidelines.md C-COMMON-TRAITS, C-DEBUG, C-SEND-SYNC). Most are `derive`-able.

| Trait | Derive when | Requires |
|---|---|---|
| `Debug` | **Always** for public types | all fields `Debug` |
| `Clone` | duplicable | all fields `Clone` |
| `Copy` | small, plain value | all fields `Copy`, no `Drop` |
| `PartialEq` | equality makes sense | all fields `PartialEq` |
| `Eq` | total equality | `PartialEq`, no floats |
| `Hash` | used as map/set key | `Eq`, consistent with `PartialEq` |
| `Default` | a sensible default exists | all fields `Default` |
| `PartialOrd` / `Ord` | ordering / total order | `PartialEq` / `Eq`, no floats for `Ord` |

Minimum bundle: `#[derive(Debug, Clone, PartialEq)]`; add `Eq, Hash` for keys, `Ord, PartialOrd` for `BTreeMap`/sorting, `Default`, `Copy`, `Serialize`/`Deserialize` as needed. Hand-write when derive is wrong (case-insensitive `PartialEq`+`Hash` must stay consistent; custom `Debug` to redact secrets — see type-display-vs-debug and rule `obs-no-sensitive-data`). Implement `From` not `Into` (blanket impl gives `Into` free — api-guidelines.md C-CONV-TRAITS, rule `api-from-not-into`).

---

## Rules & anti-patterns checklist

Distilled from the rust-skills `trait-*` rules and adjacent type-level rules cited above.

- **trait-associated-type-vs-generic** — DO use `type Output;` when each impl has exactly one output; use `<Rhs>` when a type implements the trait for many inputs. Wrong choice either blocks needed impls or forces turbofish noise everywhere.
- **trait-default-methods** — DO reduce a trait to a minimal required set + defaults built on them. Defaults cut boilerplate and document the canonical relationships; override only for performance, never changing semantics.
- **trait-blanket-impl** — DO use `impl<T: Bound> Trait for T` to extend a whole class at once; DON'T expect to also add a specific impl for a covered type (E0119 — no specialization). Adding a public blanket impl is a potential semver-breaking (major) change when downstream types could supply overlapping impls.
- **trait-coherence-newtype** — DON'T `impl ForeignTrait for ForeignType` (E0117). DO wrap the foreign type in a local `#[repr(transparent)]` newtype and implement the trait on that; add `From`/`inner()`/`into_inner()`.
- **trait-object-safety** — DO keep a trait dyn-compatible when you need `dyn Trait`: no generic methods, no `Self`-by-value return, no associated consts, no `Self: Sized` on the trait. Gate non-dispatchable methods with `where Self: Sized` to exclude them from the vtable.
- **trait-dyn-vs-generic** — DO default to generics/`impl Trait` (inlinable, zero cost) for hot/known-type code; use `dyn Trait` for heterogeneous storage, runtime-chosen types, plugins, or to shrink binary size. DON'T `Box<dyn>` a single known type "to be flexible".
- **type-generic-bounds** — DO add bounds only where used (on impls/fns, rarely on the struct) and prefer `where`-clauses once long. Bounds on struct definitions infect every use; redundant/over-broad bounds cut flexibility.
- **const-generics** — DO parameterize over sizes with `<const N: usize>` instead of copy-pasting per size or carrying a runtime length; one monomorphized copy per value, capacity checked at compile time.
- **const-fn** — DO mark constructors and simple utilities `const fn`; it's free and enables const contexts, replacing `lazy_static!` for compile-time-computable values.
- **api-sealed-trait** — DO seal a trait (private supertrait) when you need to add methods later or guarantee correctness; DON'T seal when users legitimately need their own impls.
- **api-extension-trait** — DO add methods to foreign/generic types via an `Ext`-suffixed trait + blanket impl; DON'T when the method needs private fields (newtype) or belongs inherently on a type you own.
- **api-common-traits** — DO derive `Debug, Clone, PartialEq` at minimum for public types, plus `Eq/Hash/Ord/Default/Copy/Serialize` per use. Missing traits break testing, collections, and debugging.
- **anti-type-erasure** — DON'T return/store `Box<dyn Trait>` when `impl Trait`, generics, or an enum work — that's needless heap + dynamic dispatch. `Box<dyn>` is right only for genuinely heterogeneous or runtime-unknown types, recursive types, and registered callbacks.

## Gotchas / footguns

- **Struct-level bounds leak.** `struct S<T: Clone>` requires `Clone` even to *store* a `T`; move the bound to the impls that need it. (Derives are the exception — `#[derive(Clone)]` on `S<T>` correctly generates a conditional `impl<T: Clone> Clone`.)
- **Blanket impl locks you out.** Once `impl<T: Bound> Trait for T` exists you can never add `impl Trait for SpecificType` — compile-clean today, E0119 the moment you try. Plan the design before publishing.
- **Object-safety errors fire at the `dyn` use site**, often far from the trait. A generic method, `Self`-by-value return, or associated const silently makes the trait non-object-safe; you find out only when you write `Box<dyn Trait>`.
- **`Self` in return position** (`fn clone_self(&self) -> Self`) kills object safety — the size is unknown at runtime. That's why `Clone` isn't object-safe; use `fn clone_box(&self) -> Box<dyn Trait>` for clonable trait objects.
- **APIT can't be turbofished.** `fn foo(x: impl T)` gives callers no way to write `foo::<X>()`; if callers must name the type, use an explicit `<X: T>` parameter instead.
- **RPIT is a single concrete type.** `fn f(cond: bool) -> impl Iterator` cannot return two *different* iterator types from two branches — that's a type mismatch. Unify with `.chain`/`Either`, or return `Box<dyn Iterator>`.
- **`max_of<T>` returning `&str` needs a lifetime; returning `i32` doesn't.** Monomorphization inserts the elided `<'a>` only for reference-returning instantiations — a subtlety when reasoning about which copies compile.
- **`==` on floats blocks `Eq`/`Hash`.** `f64` is only `PartialEq`/`PartialOrd` (NaN breaks totality), so a struct with float fields can't derive `Eq`, `Hash`, or `Ord` — and thus can't be a `HashMap` key. Use an integer/newtype key or ordered-float.
- **Manual `Hash` must match manual `PartialEq`.** If two values are `==` they must hash equal, or `HashMap` breaks. The case-insensitive-string pattern must lowercase in *both*.
- **`dyn` in a tight loop** can be 2–10× slower (no inlining). Profile before assuming; for a closed set, enum dispatch recovers the cost.
- **const-generic params can't do arbitrary arithmetic yet.** `[T; N]` is fine but `[T; N + 1]` / `[T; N * 2]` in most positions needs `generic_const_exprs` (unstable). Restructure to avoid computed const-generic lengths.
- **GATs need explicit `where Self: 'a`.** Omitting the bound on `type Item<'a>` usually fails to compile; add `where Self: 'a` to tie the item to the receiver borrow.

## Cheat-sheet

| Need | Use | Notes |
|---|---|---|
| One output type per impl | associated type `type X;` | `Iterator::Item`, `Deref::Target`; no turbofish |
| Many impls per type | generic param `Trait<T>` | `From<T>`, `Add<Rhs>`, `PartialEq<Rhs>` |
| Zero-cost, type known | generics / `impl Trait` (static) | monomorphized, inlinable |
| Heterogeneous collection | `Vec<Box<dyn Trait>>` | trait must be object-safe |
| Closed set, fast | enum + `match` (enum dispatch) | inline storage, no vtable; `enum_dispatch` crate |
| Foreign trait on foreign type | newtype + `impl` (orphan rule) | `#[repr(transparent)]`, add `From`/`into_inner` |
| Methods on foreign type | `Ext` trait + blanket impl | `use` to bring into scope |
| Behavior for all `T: Bound` | blanket impl `impl<T: B> Tr for T` | irreversible; breaking if downstream impls could overlap |
| Prevent external impls | sealed trait (private supertrait) | add methods later without breakage |
| Return unnameable type | RPIT `-> impl Trait` | callee picks type; RPITIT in traits (1.75+) |
| Item borrows from `&self` | GAT `type Item<'a> where Self: 'a` | lending iterators |
| Closure over any lifetime | HRTB `for<'a> Fn(&'a T) -> &'a U` | usually inferred |
| Array size as a type param | const generic `<const N: usize>` | inferred from arg; defaults since 1.59 |
| Compile-time value | `const fn` | no heap/trait-calls; replaces `lazy_static!` |
| Command → response type link | trait + `type Response` | GADT-style; one `parse` per command |
| Store unknown types | `Box<dyn Any>` + `downcast_ref` / `TypeId` | last resort; loses static safety |
| Object-safe generic method | `fn m<T>(&self, ..) where Self: Sized` | excluded from vtable |
| Interop public type | `#[derive(Debug, Clone, PartialEq)]` + as needed | see §19 table |

**Cross-references:** api-guidelines.md (C-NEWTYPE, C-COMMON-TRAITS, C-CONV-TRAITS, C-SEALED, C-METHOD, C-DEBUG, C-SEND-SYNC), microsoft-guidelines.md (M-*), design-patterns.md (newtype, RAII, extension-trait idioms), style-guide.md (formatting of `where`-clauses and bounds), reference-notation.md (grammar for the object-safety and const-generics rules).
