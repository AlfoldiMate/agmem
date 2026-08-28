# Rust Design Patterns

Distilled reference from the *Rust Unofficial "Rust Design Patterns"* book. Covers **idioms** (community coding norms), **design patterns** (solutions to recurring problems), and **anti-patterns** (approaches that create more problems than they solve), plus functional-programming idioms. Each pattern involves trade-offs — understand *why* you pick one, not just *how*.

Source: https://rust-unofficial.github.io/patterns/

---

## Idioms

Community coding guidelines / social norms. Deviating is allowed but should be justified.

### Use borrowed types for arguments
- **Guideline:** Prefer borrowed slice/view types (`&str`, `&[T]`, `&T`) over references to owned types (`&String`, `&Vec<T>`, `&Box<T>`).
- **Why:** Deref coercion makes the borrowed form accept strictly more inputs (a `&str` param accepts `&String`, `&'static str`, and `.split()` results); it also avoids double indirection and needless allocation.

```rust
fn three_vowels(word: &str) -> bool { /* ... */ }   // accepts &String AND "Ferris"
// not: fn three_vowels(word: &String)              // rejects "Ferris" literal
```

### Concatenate strings with `format!`
- **Guideline:** Use `format!` to combine literal + non-literal strings for readability.
- **Why:** Most succinct/readable. Trade-off: a series of `push_str`/`push` on a pre-allocated `String` is more efficient in hot paths.

```rust
fn say_hello(name: &str) -> String { format!("Hello {name}!") }
```

### Constructors
- **Guideline:** Rust has no language-level constructors — use an associated `fn new() -> Self` by convention; add a `Default` impl for the zero-arg case.
- **Why:** `new` is the idiom users expect and allows custom init logic. It is common to provide **both** `new` and `Default`.

```rust
impl Second { pub fn new(value: u64) -> Self { Self { value } } }
```

### The `Default` trait
- **Guideline:** Implement or `#[derive(Default)]` for types with sensible zero-value defaults.
- **Why:** Unlocks generic use (`Option::unwrap_or_default`, `*_or_default`, `..Default::default()` struct update). Limit: only one impl per type, and `default()` takes no args (unlike named constructors).

```rust
#[derive(Default, Debug, PartialEq)]
struct Config { output: Option<PathBuf>, timeout: Duration, check: bool }
let c = Config { check: true, ..Default::default() };
```

### `mem::take` / `mem::replace` to keep owned values while mutating enums
- **Guideline:** To move a value out of a `&mut` enum/field while swapping in a new variant, use `mem::take(x)` (needs `Default`) or `mem::replace(x, new)`.
- **Why:** The borrow checker requires the slot always hold a value; this transfers ownership with **no allocation and no clone** — avoids the clone-to-satisfy-borrowck anti-pattern.

```rust
fn a_to_b(e: &mut MyEnum) {
    if let MyEnum::A { name, x: 0 } = e {
        *e = MyEnum::B { name: mem::take(name) };  // moves name out, no clone
    }
}
```

### On-stack dynamic dispatch
- **Guideline:** Dispatch dynamically over differently-typed trait impls via `&mut dyn Trait` bound conditionally — no `Box`, no heap.
- **Why:** Avoids heap allocation and downstream monomorphization/code bloat when perf isn't critical. Since Rust 1.79 temporary lifetime extension removes the old need for deferred `let` bindings.

```rust
let readable: &mut dyn io::Read =
    if arg == "-" { &mut io::stdin() } else { &mut fs::File::open(arg)? };
```

### Iterating over an `Option`
- **Guideline:** Treat `Option` as a 0-or-1 container; it implements `IntoIterator`, so use it with `extend`, `chain`, `flatten`, `filter_map`.
- **Why:** Integrates cleanly with generic iterator code. Prefer `if let Some(..)` over a literal `for` loop; prefer `std::iter::once` over `Some(x).into_iter()`.

```rust
logicians.extend(turing);                       // pushes the inner value if Some
logicians.iter().chain(turing.iter());          // append optional element
```

### Pass variables to a closure explicitly
- **Guideline:** Rebind variables in a block just before a `move` closure to control each capture (move / clone / borrow) individually.
- **Why:** Groups the transforms next to the closure, keeps names identical to surrounding code, and drops unneeded data at scope end.

```rust
let closure = {
    let num2 = num2.clone();     // cloned
    let num3 = num3.as_ref();    // borrowed
    move || *num1 + *num2 + *num3   // num1 moved
};
```

### Privacy for extensibility
- **Guideline:** Use `#[non_exhaustive]` (or a private `_field: ()`) so you can add public fields / enum variants later without a breaking change.
- **Why:** Forces clients to use `..` in patterns and blocks external struct-literal construction. Caveat: hurts ergonomics (clients must handle unknown variants) — use deliberately, not everywhere.

```rust
#[non_exhaustive]
pub struct S { pub foo: i32 }
// client must write: let S { foo, .. } = s;
```

### Easy doc initialization
- **Guideline:** In doc examples for types with heavy setup, hide (`#`) a helper fn that takes the constructed value as a param instead of repeating boilerplate per example.
- **Why:** Concise, DRY docs. Trade-off: the hidden helper is never called, so the example compiles but isn't actually *run*/tested.

### Temporary mutability
- **Guideline:** When data is mutated during prep then only read, downgrade it to immutable via a nested block or rebinding (`let data = data;`).
- **Why:** Compiler then prevents accidental later mutation; makes intent explicit at minimal cost.

```rust
let mut data = get_vec();
data.sort();
let data = data;            // now immutable
```

### Return the consumed argument on error
- **Guideline:** If a fallible fn moves an argument, hand it back inside the error variant so callers can retry without cloning.
- **Why:** Better perf (move, don't defensively clone). Std does this: `String::from_utf8` → `FromUtf8Error::into_bytes()`. Trade-off: slightly richer error type.

```rust
pub fn send(value: String) -> Result<(), SendError> { /* ... */ Err(SendError(value)) }
value = match send(value) { Ok(()) => break, Err(SendError(v)) => v };  // retry
```

### FFI idioms
**Error handling** — map rich Rust errors to C in one of three ways:
- Flat/variant-only enum → integer code: `impl From<MyError> for libc::c_int`.
- Structured error → code + a separate `*_description(...)` C fn returning the message.
- Custom type → parallel `#[repr(C)]` struct mirroring the layout.
- Trade-off: boilerplate; some types don't map cleanly to C.

**Accepting strings** — keep foreign strings *borrowed*, minimize `unsafe`. Convert `*const c_char` → `&str` via `CStr::from_ptr(...).to_str()` inside the smallest possible unsafe block; never copy into an owned string with manual pointer arithmetic.

```rust
let msg: &str = match CStr::from_ptr(msg).to_str() { Ok(s) => s, Err(_) => return };
```

**Passing strings** — extend the owned string's lifetime as long as possible; keep ownership in the caller unless the API demands transfer; use `Vec` (not `CString`) if C mutates the data. Common bug: `seterr(CString::new(x)?.as_ptr())` creates a **dangling pointer** because the `CString` drops immediately — bind it to a `let` first.

---

## Design Patterns

### Behavioural

#### Command
- **Guideline:** Encapsulate each action as an object so it can be stored, queued, executed later, or undone (execute/rollback).
- **Implementations:** (1) **Trait objects** `Vec<Box<dyn Cmd>>` — best for complex, stateful, multi-method commands (cost: dynamic dispatch). (2) **Function pointers** — best for simple functions, no dynamic dispatch. (3) **`Box<dyn Fn()>`** — closure-friendly middle ground. Prefer fn pointers/closures when commands are small.

#### Interpreter
- **Guideline:** When a problem recurs with long repetitive steps, express instances in a small DSL with a grammar, and write an interpreter to evaluate them.
- **Why/where:** Parsers, expression evaluators, DSLs. Rust's `macro_rules!` is itself an instance of this pattern.

#### Newtype
- **Guideline:** Wrap a type in a single-field tuple struct to create a *distinct* opaque type, not an alias.
- **Why:** Type safety (can't confuse `Miles(f64)` with `Kilometres(f64)`), zero runtime cost, encapsulation/privacy, backwards-compatible API evolution. Also enables impling foreign traits on foreign types (orphan-rule workaround). Cost: pass-through boilerplate for methods/traits.

```rust
struct Password(String);
impl Display for Password {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result { write!(f, "****") }
}
```

#### RAII with guards
- **Guideline:** Acquire a resource in a constructor, release it in `Drop`; mediate *all* access through a guard object whose lifetime the borrow checker ties to the borrow.
- **Why:** Compile-time guarantees that the resource is always finalized and never used after finalization (e.g. `MutexGuard`, references from it can't outlive it). Cost: guard overhead + `Deref` ergonomic complexity.

```rust
impl<T> Mutex<T> { fn lock(&self) -> MutexGuard<T> { MutexGuard { data: self } } }
impl<'a, T> Drop for MutexGuard<'a, T> { fn drop(&mut self) { /* unlock */ } }
```

#### Strategy (Policy)
- **Guideline:** Define the algorithm skeleton abstractly (a trait) and swap concrete implementations; achieves dependency inversion / separation of concerns.
- **Why:** Caller doesn't know the concrete impl; impls don't know the surrounding workflow. In Rust use a `trait` (or a plain closure for simple cases) rather than the classic OOP object graph. Cost: each strategy adds a module.

```rust
trait Formatter { fn format(&self, data: &Data, buf: &mut String); }
Report::generate(Json, &mut out);
```

#### Visitor
- **Guideline:** Encapsulate an algorithm that runs over a heterogeneous object structure (e.g. an AST) in a `Visitor` trait with `visit_*` methods, separate from the data.
- **Why:** Add new algorithms without touching the data types; a visitor *object* can carry state between nodes. For homogeneous data prefer iterators. Rust convention: provide free `walk_*` traversal helpers rather than data-side `accept` methods.

```rust
trait Visitor<T> {
    fn visit_expr(&mut self, e: &Expr) -> T;
    fn visit_stmt(&mut self, s: &Stmt) -> T;
}
```

### Creational

#### Builder
- **Guideline:** Build an object through chained calls on a separate builder helper, ending in `.build()`.
- **Why:** Rust lacks overloading / default+named args; the builder avoids constructor proliferation, supports fluent one-liners and stepwise config, and stays backwards-compatible as fields are added. Std example: `std::process::Command`. Cost: more code than a plain struct literal.

```rust
Foo::builder().name("X".into()).build();
```

#### Fold
- **Guideline:** Walk a data structure and produce a *new* structure, separating traversal from per-node transformation (like `map` but for complex/recursive structures where earlier nodes affect later ones).
- **Why:** Good for AST → AST transforms. Ownership choice is a trade-off: `Box` avoids cloning unchanged nodes but prevents structure reuse; borrows allow reuse but require cloning; `Rc` balances both.

```rust
trait Folder {
    fn fold_stmt(&mut self, s: Box<Stmt>) -> Box<Stmt> { /* recurse + rebuild */ }
}
```

### Structural

#### Compose structs together for better borrowing
- **Guideline:** Split a large struct into smaller sub-structs (each a concern), then recombine as fields, so fields can be borrowed independently.
- **Why:** The borrow checker borrows a whole struct at once; decomposition lets you mutably borrow one part while reading another — and usually yields a cleaner design.

```rust
struct Database { conn: ConnectionString, timeout: Timeout, pool: PoolSize }
```

#### Prefer small crates
- **Guideline:** Prefer small crates that "do one thing well."
- **Why:** Easier to understand, more modular, more reusable, and crates are the unit of compilation so more crates = more build parallelism. Costs: possible version conflicts (multiple semver-incompatible copies), uncurated crates.io quality/security risk, and lost cross-crate optimization (no LTO by default).

#### Contain unsafety in small modules
- **Guideline:** Isolate `unsafe` in the smallest module that upholds the needed invariants, and expose a safe API over it.
- **Why:** Shrinks the audit surface; outer safe code relies on the inner module's guarantees. Std example: `String` = `Vec<u8>` + a UTF-8 invariant. Cost: finding the right abstraction + possible safety-layer overhead.

#### Custom trait to manage complex type bounds
- **Guideline:** When bounds get unwieldy (especially `Fn` traits with output requirements), define a custom trait that encapsulates the contract and blanket-impl it.
- **Why:** Removes redundant type params, names the constraint meaningfully, and opens room for extra methods/impls.

```rust
trait Getter { type Output: Display; fn get_value(&mut self) -> Result<Self::Output, Error>; }
impl<F: FnMut() -> Result<T, Error>, T: Display> Getter for F { /* ... */ }
struct Value<G: Getter, S: Fn(&G::Output) -> Status> { /* clean bounds */ }
```

### FFI patterns

#### Object-based APIs
- **Guideline:** Design cross-language APIs around ownership roles: **encapsulated** types (owned by Rust, exposed as opaque pointers), **transactional** `#[repr(C)]` types (owned by the caller, transparent), and library functions that operate on the encapsulated types.
- **Why:** Foreign code can't uphold Rust lifetimes; naive translations (e.g. exposing an iterator with a borrowed lifetime as a raw pointer) risk use-after-free. Consolidate ownership hierarchies — bind an iterator's lifetime to its parent object rather than handing out independent pointers.

#### Type consolidation into wrappers
- **Guideline:** Fold *all* interactions with an exported object into one wrapper type that manages state internally, instead of exposing multiple related handles/iterators across the boundary.
- **Why:** Prevents aliasing-rule violations (a caller mutating a collection mid-iteration = UB) and dangling handles. Works best when the wrapped type supports efficient nth-access; complex iterators may need special internal logic.

---

## Anti-patterns

Approaches that seem to solve a problem but cause bigger ones. Each lists what to do **instead**.

### `Deref` polymorphism
- **What:** Impl `Deref<Target = Base>` on a wrapper to fake inheritance and call the base's methods on the wrapper via the dot operator.
- **Why bad:** Surprising to readers; abuses `Deref` (meant for smart pointers, not conversion); no true polymorphism (traits on `Base` don't transfer to the wrapper, breaking generic code); `self` semantics differ from inheritance; single-inheritance only, no privacy/interfaces.
- **Instead:** Impl the traits explicitly; write manual delegation methods (`fn m(&self) { self.f.m() }`); or use a delegation crate (`delegate`, `ambassador`).

### Clone to satisfy the borrow checker
- **What:** Sprinkling `.clone()` to make borrow errors disappear instead of fixing ownership.
- **Why bad:** Wastes memory/CPU and silently desynchronizes the copies (changes to one don't reach the other); masks a real design problem.
- **Instead:** Learn the ownership model; use `Rc`/`Arc` for genuine shared ownership; use `mem::take`/`mem::replace` to move owned values; run `cargo clippy` to catch needless clones. (Deliberate cloning is fine while learning or when perf doesn't matter.)

```rust
let y = &mut (x.clone());   // anti-pattern: y and x are now unrelated
```

### `#![deny(warnings)]`
- **What:** Crate-level attribute turning *all* warnings into hard errors.
- **Why bad:** Breaks Rust's stability grace period — a new/renamed lint or a newly-deprecated API can fail a previously-green build with no code change; makes external lint crates (clippy) unusable; ongoing maintenance burden.
- **Instead:** Enforce strictness *outside* the code — `RUSTFLAGS="-D warnings"` in CI — or deny an explicit, curated list of lints (and never deny `deprecated`).

---

## Functional programming idioms

### Programming paradigms (imperative → declarative)
- **Guideline:** Prefer declarative expression composition over imperative loops + mutable state; describe *what*, not *how*.
- **Why:** More concise and easier to reason about; the type system + iterator adapters handle execution.

```rust
(1..11).fold(0, |a, b| a + b)     // vs. a `for` loop mutating `sum`
```

### Generics as type classes
- **Guideline:** Use generic type parameters to split an API at compile time so state/protocol-specific methods only exist on the appropriate concrete type (`Request<Nfs>` vs `Request<Bootp>`).
- **Why:** Monomorphization makes each instantiation a genuinely distinct type; deduplicates shared fields, organizes `impl` blocks by state, and enforces correctness at compile time with zero runtime checks. (Compare Builder for construction sequences, Strategy when the API stays constant.)

### Functional optics (Iso / Poly Iso / Prism)
- **Guideline:** Understand APIs like Serde as composable optics — `Iso` (a bidirectional pair between two fixed types), `Poly Iso` (generic over the type, e.g. `FromStr`/`ToString`), `Prism` (also generic over the *format*).
- **Why:** Serde composes these — types implement `Serialize`/`Deserialize` (Poly Iso), visitors bridge type structure to data, deserializers handle format specifics (Prism) — so any type "just works" with any format. Rust needs proc-macros + type erasure to express optics indirectly.

---

## Quick checklist

- Take `&str`/`&[T]`/`&T`, not `&String`/`&Vec<T>`/`&Box<T>`, in fn signatures.
- Provide `new` and derive/impl `Default`; use the builder pattern instead of many constructors.
- Use `mem::take`/`mem::replace` (not `.clone()`) to move owned values out of `&mut` slots.
- `.clone()` to silence the borrow checker is an anti-pattern — reach for `Rc`/`Arc`/`mem::take` and study ownership.
- Never `#![deny(warnings)]` in code; enforce via `RUSTFLAGS="-D warnings"` in CI or deny an explicit lint list.
- Don't fake inheritance with `Deref` — delegate explicitly or use a delegation crate.
- Use the newtype pattern for type safety, encapsulation, and the orphan-rule workaround.
- Guard resources with RAII (`Drop` + a guard whose lifetime the borrow checker ties down).
- Use traits for Strategy/Visitor; closures for lightweight Command/Strategy.
- `#[non_exhaustive]` (or a private field) to keep structs/enums extensible without breaking clients.
- Prefer small single-purpose crates; contain `unsafe` in the smallest module behind a safe API.
- In FFI: keep foreign strings borrowed (`CStr::to_str`), extend `CString` lifetimes (avoid dangling `.as_ptr()`), and consolidate exported objects into wrapper types with clear ownership roles.
- Prefer declarative iterator/`fold` composition over imperative loops with mutable state.
