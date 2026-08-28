# Closures, Higher-Order Functions & Functional Style

Deep reference for the `Fn`/`FnMut`/`FnOnce` trait family, closure capture and dispatch, higher-order API design, and the functional-vs-imperative judgment calls that dominate idiomatic Rust (iterator/combinator chains, `Option`/`Result` combinators, `fold`/`scan`/`try_fold`, laziness, allocation avoidance). Consult this before writing or reviewing any callback-taking API, closure-returning function, or iterator pipeline. Distilled from the Microsoft *Rust Patterns* book (ch07 Closures & Higher-Order Functions, ch08 Functional vs. Imperative) and the leonardomso/rust-skills `closure-*` / `pat-*` rule catalog.

Cross-links: closure trait-bound choices reinforce **microsoft-guidelines.md M-*** (accept the weakest bound) and **api-guidelines.md** ergonomics; the `with`/bracketed-access and builder patterns connect to **design-patterns.md**; the `impl Trait`-vs-`Box<dyn>` decision echoes **api-guidelines.md C-NEWTYPE**-style zero-cost thinking.

---

## 1. The Three Closure Traits: `Fn`, `FnMut`, `FnOnce`

Every closure implements one or more of three traits, determined by *how it uses* its captures (not how you declare it). The traits form a subtrait hierarchy — each is a supertrait of the previous (Patterns Book ch07 §Fn, FnMut, FnOnce):

```text
Fn : FnMut : FnOnce
```

- **`FnOnce`** — may *consume* (move out) its captures; callable **at least once**. Every closure implements `FnOnce`.
- **`FnMut`** — *mutably borrows* captures; callable **repeatedly**, may mutate state. Implies `FnOnce`.
- **`Fn`** — *immutably borrows* captures (or captures nothing); callable **repeatedly and concurrently**, no mutation. Implies `FnMut` and `FnOnce`.

If a closure is `Fn`, it is automatically `FnMut` and `FnOnce`. A `Fn` closure satisfies an `FnOnce` bound — never the reverse (rust-skills `closure-fn-trait-bounds`).

```rust
// FnOnce — consumes a captured value, so it can be called only once.
let name = String::from("Alice");
let greet = move || {
    println!("Hello, {name}!"); // takes ownership of `name`
    drop(name);                  // `name` is consumed
};
greet();
// greet(); // ❌ error: value used after move — `name` was consumed

// FnMut — mutably borrows; callable many times.
let mut count = 0;
let mut increment = || count += 1; // note: binding must be `mut`
increment(); // count == 1
increment(); // count == 2

// Fn — immutably borrows; callable many times.
let prefix = "Result";
let display = |x: i32| println!("{prefix}: {x}");
display(1);
display(2);
```

**Which trait a closure gets is inferred from its body**, not from `move`. `move` changes *how* captures are taken (by value vs by reference); it does not decide `Fn`/`FnMut`/`FnOnce`. A `move` closure that only reads its captures is still `Fn` (rust-skills `closure-move-capture` Key Points).

---

## 2. Capture Modes & Disjoint Capture (edition 2021+)

A closure captures each variable by the *weakest* mode its body requires: by shared reference, then by mutable reference, then by move — the borrow checker picks the least restrictive that compiles.

### Disjoint (per-field) capture

Since edition 2021, closures capture the **minimal place** they use — `config.threshold`, not the whole `config`. Sibling fields stay independently accessible. Write closures that touch only what they need (rust-skills `closure-disjoint-capture`):

```rust
struct Config { threshold: i32, label: String }

fn demo() {
    let config = Config { threshold: 10, label: String::from("active") };

    // Captures ONLY `config.threshold` (a Copy field). `config.label` is untouched.
    let check = || config.threshold > 0;

    println!("label: {}", config.label); // ✅ fine — `label` was not captured
    assert!(check());
}
```

### `move` captures the whole *named place*

The footgun: `move || config.threshold > 0` moves **all of `config`**, not just the field — `config.label` becomes inaccessible afterward. Same for methods: `move || self.field` moves `*self`. **Bind the field to a local first**, then move the local (rust-skills `closure-disjoint-capture`):

```rust
struct Config { threshold: i32, label: String }

// Move only one field out; keep the rest of the struct usable.
fn make_checker(config: Config) -> (impl Fn() -> bool, String) {
    let threshold = config.threshold;       // copy the field out first
    let checker = move || threshold > 0;    // moves `threshold` (i32, Copy), NOT `config`
    (checker, config.label)                 // `config.label` still available
}
```

Copy types (integers, bools, `char`) are *copied* into a `move` closure — the original stays valid.

---

## 3. `move`: When a Closure Must Own Its Captures

A borrowing closure lives only as long as what it borrows. When a closure **escapes** its scope — returned, stored in a struct, sent to a thread or async task — it must own its captures and is usually required to be `'static`. `move` transfers ownership of every captured variable in (rust-skills `closure-move-capture`).

```rust
// Returned closure outlives the function frame → must own `name`.
fn make_greeter(name: String) -> impl Fn() {
    move || println!("hello, {name}")
}

// Thread closures need `'static` + `Send`; `move` supplies ownership.
// Clone BEFORE move when you need the value in both places.
fn spawn_and_keep(data: Vec<i32>) -> std::thread::JoinHandle<i32> {
    let data_for_thread = data.clone();     // the clone goes into the closure
    let handle = std::thread::spawn(move || data_for_thread.iter().sum());
    println!("original still owned: {data:?}"); // `data` still usable here
    handle
}
```

Rules:
- `std::thread::spawn` requires `FnOnce + Send + 'static`; `tokio::spawn(async move { … })` is the same idea — clone shared data before the `async move` block (see rules `async-clone-before-await`).
- Clone **selectively** — only what the closure needs, not the whole owning struct (§2, `closure-disjoint-capture`).
- Prefer borrowing; escalate to `move` only when the closure must outlive the scope.

---

## 4. Closures as Parameters: Static vs. Dynamic Dispatch

### Accept the *weakest* `Fn` trait the body needs

Bounding on the least-restrictive trait accepts the widest set of callers. Require `Fn` only if you call concurrently/re-entrantly; `FnMut` if you call repeatedly and may mutate; `FnOnce` if you call exactly once. The Patterns Book default guidance: **use `FnMut` as the default bound** for callbacks you call one-or-more times, escalate to `Fn` only for concurrency (ch07 §FnMut vs Fn; rules `closure-fn-trait-bounds`).

```rust
// FnOnce — called exactly once; accepts move-consuming closures.
fn run_once<F: FnOnce() -> String>(f: F) -> String { f() }

// FnMut — called repeatedly, may mutate. Parameter must be `mut`.
fn retry<F: FnMut() -> bool>(mut f: F, attempts: usize) -> bool {
    (0..attempts).any(|_| f())
}

// Fn — called repeatedly, read-only / shareable.
fn for_each<T, F: Fn(&T)>(items: &[T], f: F) {
    items.iter().for_each(|item| f(item));
}
```

| Trait | Captures | Calls | Accepts |
|-------|----------|-------|---------|
| `FnOnce` | may consume | exactly once | **all** closures |
| `FnMut` | may mutate | multiple | non-consuming |
| `Fn` | read-only | multiple / shared | pure closures |

Std examples: `Iterator::map` takes `FnMut`; `Option::map` takes `FnOnce`; `thread::spawn` takes `FnOnce + Send + 'static`.

### `impl Fn` (generic, static) vs `dyn Fn` (dynamic)

A generic `F: Fn(…)` / `impl Fn` **monomorphizes** at each call site: a specialized copy is emitted, enabling inlining and zero-cost dispatch — at the cost of code-size bloat when many closure types are substituted. `&dyn Fn` / `Box<dyn Fn>` share one compiled copy via a vtable: smaller binary, and the **only** option for storing heterogeneous closures. Choose by profiling, not habit (rules `closure-static-vs-dyn`).

```rust
// Hot path / single call site: generic — inlinable, zero allocation.
fn transform<F: Fn(i32) -> i32>(xs: &[i32], f: F) -> Vec<i32> {
    xs.iter().map(|&x| f(x)).collect()
}

// Borrow one closure for a single call without allocating: &dyn Fn.
fn call_once_dyn(f: &dyn Fn() -> i32) -> i32 { f() }

// Store heterogeneous closures: Box<dyn Fn>.
struct Registry { handlers: Vec<Box<dyn Fn(&str)>> }
impl Registry {
    fn new() -> Self { Self { handlers: Vec::new() } }
    fn register(&mut self, handler: impl Fn(&str) + 'static) {
        self.handlers.push(Box::new(handler));
    }
    fn dispatch(&self, event: &str) {
        self.handlers.iter().for_each(|h| h(event));
    }
}
```

| Situation | Use |
|-----------|-----|
| Hot inner loop, single call site | `impl Fn` / generic `F: Fn` |
| Callback stored in a struct field | `Box<dyn Fn>` |
| Collection of mixed closures | `Vec<Box<dyn Fn(…)>>` |
| Pass-through, one level deep, not stored | `&dyn Fn` (avoids allocation) |
| Called across an `await` point | `Box<dyn Fn + Send>` |

---

## 5. Returning Closures

A closure has an anonymous, unnameable type, so you cannot write it directly as a return type. Two options (Patterns Book ch07 §Return Values; rules `closure-impl-fn-return`):

### Default: `impl Fn` — zero allocation, static dispatch

```rust
fn adder(n: i32) -> impl Fn(i32) -> i32 { move |x| x + n }
fn multiplier(n: i32) -> impl Fn(i32) -> i32 { move |x| x * n }
```

Returning `impl FnMut` — the caller's binding must be `mut`:

```rust
fn counter(start: i32) -> impl FnMut() -> i32 {
    let mut n = start;
    move || { let cur = n; n += 1; cur }
}
// let mut next = counter(0); next() == 0; next() == 1;
```

### `Box<dyn Fn>` — only when you must

Reach for boxing when different `if`/`match` arms return **distinct** closure types (`impl Fn` cannot unify them), or when the closure is stored in a field/collection:

```rust
// Arms produce different closure types → impl Fn won't compile ("different closure").
fn make_transform(double: bool) -> Box<dyn Fn(i32) -> i32> {
    if double { Box::new(|x| x * 2) } else { Box::new(|x| x + 100) }
}
```

> **Anti-pattern (`anti-type-erasure`):** `Box<dyn Fn>` for a single concrete closure type pays a heap allocation + virtual call for nothing. Use `impl Fn` unless you genuinely need type erasure.

---

## 6. Function Pointers vs. Closures

A closure that captures **nothing** coerces to a function pointer `fn(…) -> …`; a capturing closure does not. `fn` pointers are the narrowest callable — every `fn` is also `Fn`, so prefer `Fn` bounds in generic APIs and let non-capturing closures / named functions coerce in.

```rust
fn double(x: i32) -> i32 { x * 2 }

// A non-capturing closure and a fn item both satisfy an `fn` pointer type.
let f: fn(i32) -> i32 = |x| x * 2;
let g: fn(i32) -> i32 = double;
assert_eq!(f(3) + g(3), 12);

// Passing a named function where a closure is expected — no `|x| ...` wrapper needed.
let doubled: Vec<i32> = [1, 2, 3].into_iter().map(double).collect();
assert_eq!(doubled, vec![2, 4, 6]);

// The same works for method-path fn items when signatures line up:
let names = ["alice", "bob"];
let upper: Vec<String> = names.into_iter().map(str::to_uppercase).collect();
assert_eq!(upper, vec!["ALICE".to_string(), "BOB".to_string()]);
```

Enum tuple-variant and tuple-struct constructors are themselves `fn` values — handy in `map`: `results.into_iter().map(Some)`, `ids.into_iter().map(Wrapper)`.

---

## 7. Designing Higher-Order APIs

Accept closures to let callers customize behavior. Pick bounds per §4. Example: a retry combinator parameterized on both the operation and the retry policy (Patterns Book ch07 §Implementing Your Own Higher-Order APIs):

```rust
fn retry<T, E, F, S>(mut operation: F, mut should_retry: S, max_attempts: usize) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    S: FnMut(&E, usize) -> bool, // (error, attempt) → try again?
{
    for attempt in 1..=max_attempts {
        match operation() {
            Ok(val) => return Ok(val),
            Err(e) if attempt < max_attempts && should_retry(&e, attempt) => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}
```

Both parameters are `FnMut` — the widest useful bound for something called in a loop.

### The `with` pattern — bracketed resource access

When setup and teardown are paired and forgetting either is a bug, **lend the resource through a closure** instead of exposing it: `set up → call closure(resource) → tear down`. The caller cannot forget setup/teardown, cannot misuse it, and — via a lifetime — cannot let the handle escape (Patterns Book ch07 §The `with` Pattern):

```rust
pub struct GpioPin<'a> { pin: u8, _ctl: &'a GpioController }
impl GpioPin<'_> {
    pub fn read(&self) -> bool { true }
    pub fn write(&self, _high: bool) {}
}

pub struct GpioController { /* hardware state */ }
impl GpioController {
    // Configure as input, run the closure, restore state — even on early return/panic.
    pub fn with_pin_input<R>(&self, pin: u8, mut f: impl FnMut(&GpioPin<'_>) -> R) -> R {
        // set_direction(pin, In);
        let handle = GpioPin { pin, _ctl: self };
        let result = f(&handle);
        // restore previous direction
        result
    }
}

// let level = gpio.with_pin_input(4, |pin| pin.read());
// The handle CANNOT escape: `gpio.with_pin_input(4, |pin| pin)` fails to compile.
```

**`with` vs RAII/`Drop`:** both guarantee cleanup. Use `Drop`/guard types when the caller holds the resource across many statements (e.g. `MutexGuard`). Use `with` when the operation is **bracketed** — one setup, one block, one teardown — and the caller must not be able to break the bracket. Std/ecosystem `with`-shaped APIs: `thread::scope`, `tempfile::tempdir`, `BufWriter` (flush on drop). See **design-patterns.md** for related RAII/guard idioms.

---

## 8. Functional vs. Imperative: The Core Principle

> Functional style shines when **transforming data through a pipeline**. Imperative style shines when **managing state transitions with side effects**. Most real code has both; the skill is knowing where the boundary falls (Patterns Book ch08 intro).

### `Option`/`Result` are one-element collections (monadic combinators)

`Option<T>` is "a collection of zero or one"; every combinator mirrors a collection op. Prefer combinators over `if let`/`match` when both branches yield the *same type* and the bodies are short expressions (ch08 §8.1):

```rust
# fn maybe_config() -> Option<i32> { Some(1) }
# fn default_config() -> i32 { 0 }
# fn process(_: i32) {}
// if let … else { default() }  →  unwrap_or_else
process(maybe_config().unwrap_or_else(default_config));
```

**`Option` combinator map** (ch08 §8.1):

| Combinator | Replaces | Communicates |
|---|---|---|
| `opt.unwrap_or(d)` | `if let Some(x)=opt {x} else {d}` | value or fallback |
| `opt.unwrap_or_else(\|\| e())` | same, lazy default | value or lazy fallback |
| `opt.map(f)` | `match {Some(x)=>Some(f(x)),None=>None}` | transform inside, propagate absence |
| `opt.and_then(f)` | `match {Some(x)=>f(x),None=>None}` | chain fallible ops (flatmap) |
| `opt.filter(pred)` | `match {Some(x) if pred=>Some(x),_=>None}` | keep if it passes |
| `opt.zip(other)` | both-`Some`-or-`None` | both or neither |
| `opt.or(fb)` / `opt.or_else(\|\| …)` | first-available | try alternatives |
| `opt.map_or(d, f)` | `if let Some(x){f(x)} else {d}` | transform-or-default (one call) |
| `opt.map_or_else(d_fn, f)` | both sides closures | transform-or-lazy-default |
| `opt?` | `match {Some(x)=>x,None=>return None}` | propagate absence upward |

**`Result` combinator map** (ch08 §8.1): `res.map(f)`, `res.map_err(f)`, `res.and_then(f)`, `res.unwrap_or_else(\|e\| …)`, `res.ok()` (→ `Option`, discards error), `res?` (propagate, applying `From` via `.into()`).

### When `if let`/`match` beats a combinator

Combinators lose when (ch08 §8.1 "When `if let` IS better"):
- The `Some`/`Ok` branch needs **multiple statements**.
- **Control flow is the point** — the two branches are genuinely different code paths, not transform-or-default.
- **Side effects dominate** — both branches do I/O with different error handling; the combinator hides the important differences.

**Rule of thumb:** same result type + short expression bodies → combinator. Fundamentally different branch behavior → `if let`/`match`.

### Bool combinators: `then` / `then_some`

```rust
# let is_admin = true;
# fn compute_admin_permissions() -> Vec<&'static str> { vec![] }
let label = is_admin.then_some("ADMIN");                 // Option<&str>
let perms = is_admin.then(|| compute_admin_permissions()); // lazy value
```

Powerful for building lists from conditional elements (ch08 §8.2):

```rust
# struct User { is_admin: bool, is_verified: bool, score: u32 }
# let user = User { is_admin: true, is_verified: false, score: 150 };
let tags: Vec<&str> = [
    user.is_admin.then_some("admin"),
    user.is_verified.then_some("verified"),
    (user.score > 100).then_some("power-user"),
]
.into_iter()
.flatten() // drops the Nones
.collect();
```

---

## 9. Iterator Chains vs. Loops: Decision Framework

### When iterators win

**Data pipelines** — transform a collection through stages. Each stage is independently readable, no `mut`, reorderable, and LLVM inlines adapters to the same machine code as the loop (ch08 §8.3):

```rust
# #[derive(PartialEq)] enum Category { Server }
# struct Item { id: u32, category: Category }
# impl Item { fn last_temperature(&self) -> Option<f64> { Some(90.0) } }
# let inventory: Vec<Item> = vec![];
let results: Vec<_> = inventory.iter()
    .filter(|item| item.category == Category::Server)
    .filter_map(|item| item.last_temperature().map(|t| (item.id, t)))
    .filter(|(_, temp)| *temp > 80.0)
    .collect();
```

**Aggregation** — one value from a collection. Prefer the built-in (`sum`, `count`, `min`, `max`, `min_by_key`) over `fold` when one exists:

```rust
# struct Server; impl Server { fn power_draw(&self) -> f64 { 1.0 } }
# let fleet: Vec<Server> = vec![];
let total: f64 = fleet.iter().map(|s| s.power_draw()).sum();
// custom accumulation → fold
let (sum, count) = fleet.iter()
    .map(|s| s.power_draw())
    .fold((0.0, 0usize), |(s, n), p| (s + p, n + 1));
# let _ = (total, sum, count);
```

Replace under-functionalized loops with the named vocabulary (ch08 §8.10):

| Loop that sets a flag / finds / checks | Idiom |
|---|---|
| loop + `found = true; break` | `iter.any(pred)` |
| loop + `target = Some(x); break` | `iter.find(pred)` |
| loop + `all_ok = false; break` | `iter.all(pred)` |
| loop tracking first matching index | `iter.position(pred)` |

### When loops win

- **Multiple outputs from one pass** — building several collections + a stats struct at once. A `fold` with several `mut` accumulators in a tuple is *longer and harder to read* than the loop, and still mutates (ch08 §8.3 log-stream example; §8.9 three-way partition exercise).
- **State machines with I/O** — `loop { state = match state { … } }` *is* the algorithm; no combinator is cleaner.
- **Early exit with complex state** where `find`/`take_while` don't fit.
- **In-place mutation** — `for s in &mut fleet { … }` beats `.iter_mut().for_each(…)` (just a loop with extra syntax) and avoids the allocation `collect` would add (ch08 §8.5).

### Decision matrix

| What you're doing | Choose |
|---|---|
| Collection → collection | iterator chain |
| Single value from collection (sum/count/min/max) | built-in adapter |
| Single value, custom accumulation, no side effects | `.fold()` |
| Custom accumulation with mutation/side effects | `for` loop |
| Multiple outputs in one pass | `for` loop |
| State machine w/ I/O or side effects | `for`/`loop` + `match` |
| One `Option`/`Result` transform + default | combinator |

### Scoped mutability: imperative inside, functional outside

Rust blocks are expressions — confine `mut` to a construction phase, bind the result immutably (ch08 §8.3 sidebar):

```rust
# fn reading() -> f64 { 0.5 }
# fn stop_early() -> bool { false }
let samples: Vec<f64> = {
    let mut buf = Vec::with_capacity(10);
    while buf.len() < 10 {
        buf.push(reading());
        if stop_early() { break; }
    }
    buf
};
// `samples` is immutable; later `samples.push(...)` is a compile error.
```

Genuine wins for scoped mutability: sort-then-freeze (`sort` + `dedup` both return `()`), stateful termination (`take_while` drops the boundary element), multi-step field-by-field struct population. The lesson: **mutation scope can be smaller than variable lifetime**.

---

## 10. `fold`, `scan`, `try_fold`, `reduce`

- **`fold(init, f)`** — reduce to a single value; eager, consumes the whole iterator. Use only when no built-in (`sum`/`count`/`min`) fits and the accumulator has no side effects.
- **`try_fold(init, f)`** — short-circuiting fold: `f` returns `Result`/`Option`/`ControlFlow`; stops on the first `Err`/`None`. This is what `sum::<Result<_,_>>()` and friends build on.
- **`scan(state, f)`** — like `fold` but **yields each intermediate** (a stateful `map`); returns a lazy iterator. `f` gets `&mut state` and returns `Option<Item>` (return `None` to stop).
- **`reduce(f)`** — `fold` with no seed; uses the first element as the initial accumulator, returns `Option` (empty iterator → `None`).

```rust
// try_fold: stop at the first parse failure without collecting.
let sum: Result<i32, _> = ["1", "2", "x", "4"]
    .iter()
    .try_fold(0i32, |acc, s| s.parse::<i32>().map(|n| acc + n));
assert!(sum.is_err());

// scan: running total as a lazy stream.
let running: Vec<i32> = [1, 2, 3, 4]
    .iter()
    .scan(0, |acc, &x| { *acc += x; Some(*acc) })
    .collect();
assert_eq!(running, vec![1, 3, 6, 10]);

// reduce: max without a seed.
let max = [3, 7, 2, 9].into_iter().reduce(i32::max);
assert_eq!(max, Some(9));
```

---

## 11. `collect()`: The Power Tool

`collect` builds any type implementing `FromIterator`. Beyond `Vec` (ch08 §8.5):

```rust
use std::collections::HashMap;

# #[derive(Clone)] enum Error { BadInput(String) }
// Collect into Result<Vec<_>, _> — short-circuits on first Err (like a loop with `?`).
fn parse_all(input: &[String]) -> Result<Vec<i64>, Error> {
    input.iter()
        .map(|s| s.parse::<i64>().map_err(|_| Error::BadInput(s.clone())))
        .collect::<Result<_, _>>()
}

// Collect into a HashMap from (k, v) pairs.
# struct Server { id: String }
# let fleet: Vec<Server> = vec![];
let index: HashMap<_, _> = fleet.into_iter().map(|s| (s.id.clone(), s)).collect();
# let _ = index;
```

`Result<Vec<T>, E>: FromIterator` and `Option<Vec<T>>: FromIterator` are the key tricks — they turn "parse a list, fail on first error" into one `collect`. For simple string joins, prefer `slice.join(",")` over a manual push loop.

**When the loop wins:** `collect` allocates. In-place modification (`for s in &mut fleet { s.refresh()?; }`) has no better functional form.

---

## 12. Laziness & Avoiding Intermediate Allocations

Iterators are **lazy**: adapters (`map`, `filter`, `take`, …) do nothing until a consumer (`collect`, `sum`, `for`, `count`, `any`) drives them. Chains **fuse** into a single pass. The one allocation cost is `collect` (ch08 §8.8; rules `anti-collect-intermediate`).

```rust
// ❌ Three allocations, three passes.
# fn bad(data: Vec<i32>) -> Vec<i32> {
let s1: Vec<_> = data.into_iter().filter(|x| *x > 0).collect();
let s2: Vec<_> = s1.into_iter().map(|x| x * 2).collect();
s2.into_iter().filter(|x| *x < 100).collect()
# }

// ✅ One allocation, one pass — collect only at the end.
fn good(data: Vec<i32>) -> Vec<i32> {
    data.into_iter()
        .filter(|x| *x > 0)
        .map(|x| x * 2)
        .filter(|x| *x < 100)
        .collect()
}
```

Never `collect` just to inspect (`anti-collect-intermediate`):

| Instead of collecting to… | Use |
|---|---|
| check has-any / non-empty | `.next().is_some()` |
| check any / all | `.any(p)` / `.all(p)` |
| count | `.count()` |
| sum | `.sum()` |
| find / get first / get last | `.find(p)` / `.next()` / `.last()` |

**Deferred collection** — return `impl Iterator`, let the caller decide whether to collect:

```rust
# struct Item; impl Item { fn is_valid(&self) -> bool { true } }
fn valid_items(items: &[Item]) -> impl Iterator<Item = &Item> {
    items.iter().filter(|i| i.is_valid())
}
// caller: valid_items(&items).count()  — no allocation
```

### Performance: iterators == loops

In release builds, `(0..1000).filter(|n| n % 2 == 0).map(|n| n * n).sum::<i64>()` and the equivalent hand loop produce **identical assembly** — zero-cost abstraction, measured not aspirational (ch08 §8.8). Prefer iterators over manual indexing: `for i in 0..len { data[i] }` adds a bounds check per access and blocks auto-vectorization; `data.iter()` has neither (rules `anti-index-over-iter`).

---

## 13. The `?` Operator: Functional Meets Imperative

`?` is `.and_then()` + early return + `From::from` on the error. Prefer `?` with named intermediates over long `.and_then()` chains (ch08 §8.4):

```rust
# struct Config; struct Error;
# fn read_file(_: &str) -> Result<String, Error> { Ok(String::new()) }
# fn parse_toml(_: &str) -> Result<Config, Error> { Ok(Config) }
// ✅ Named steps: debuggable, allow per-step `.context(...)?`, reuse of intermediates.
fn load_config() -> Result<Config, Error> {
    let contents = read_file("config.toml")?;
    let config = parse_toml(&contents)?;
    Ok(config)
}
```

**Anti-pattern:** a chain where every closure is `|x| next_step(x)` reinvents `?` without the readability. **When `.and_then()` beats `?`:** building an `Option`/`Result` value *without* an enclosing function to return from:

```rust
# use std::collections::HashMap;
# let config: HashMap<String, String> = HashMap::new();
let port: Option<u16> = config.get("port")
    .and_then(|v| v.parse::<u16>().ok())
    .filter(|&p| p > 0);
```

---

## 14. Pattern Matching as Function Dispatch

`match` is a functional construct: a total map from domain to range, checked for exhaustiveness (ch08 §8.6). Each arm is an expression of the same type — a function table indexed by variant:

```rust
# struct Db; enum Command { Get { key: String }, Set { key: String, value: String } }
# enum Response { Value(String), Ok } struct Error;
# impl Db { fn get(&self, _:&str)->Result<String,Error>{Ok(String::new())} fn set(&self,_:String,_:String)->Result<(),Error>{Ok(())} }
# let db = Db;
fn execute(db: &Db, cmd: Command) -> Result<Response, Error> {
    match cmd {
        Command::Get { key } => db.get(&key).map(Response::Value),
        Command::Set { key, value } => db.set(key, value).map(|_| Response::Ok),
    }
}
```

Prefer `match` over `if/else` chains: the compiler enforces exhaustiveness, so adding a variant becomes a build error rather than a silent fall-through. See §Rules for `pat-exhaustive-enum`, `pat-matches-macro`, `pat-let-else`, `pat-if-let-chains`, `pat-at-bindings`.

---

## 15. Chaining Methods on Custom Types (fluent APIs)

Any type whose methods take `self` and return `Self` (or a transformed type) is a combinator — builders and fluent config are functional programming in disguise (ch08 §8.7). See **design-patterns.md** (builder) and rules `api-builder-pattern` / `api-builder-must-use`.

```rust
# use std::time::Duration;
# #[derive(Default)] struct Config { timeout: Duration, retries: u32 }
# impl Config {
#   fn with_timeout(mut self, d: Duration) -> Self { self.timeout = d; self }
#   fn with_retries(mut self, n: u32) -> Self { self.retries = n; self }
# }
let config = Config::default()
    .with_timeout(Duration::from_secs(30))
    .with_retries(3);
```

The chain **fails** when it mixes pure transforms with I/O and side effects — the reader can't tell which calls fail or mutate. Separate the pure pipeline from the I/O bookends:

```rust
# struct Data; struct Processed; struct Error; struct RuleSet;
# fn load_data(_: &str) -> Result<Data, Error> { Ok(Data) }
# impl Data { fn validate(self) -> Self { self } fn transform(self, _: RuleSet) -> Processed { Processed } }
# fn save_to_disk(_: &str, _: &Processed) -> Result<(), Error> { Ok(()) }
# fn run(path: &str, output: &str, rules: RuleSet) -> Result<(), Error> {
let data = load_data(path)?;                       // I/O
let processed = data.validate().transform(rules);  // pure pipeline
save_to_disk(output, &processed)?;                 // I/O
# Ok(()) }
```

---

## Rules & anti-patterns checklist

Closure rules (rust-skills `closure-*`):

- **`closure-fn-trait-bounds`** — DO bound on the weakest `Fn` trait the body needs (`FnOnce` ⊇ `FnMut` ⊇ `Fn`). Requiring `Fn` when you call once needlessly rejects move-consuming closures. `FnMut` param must be `mut f`.
- **`closure-impl-fn-return`** — DO return closures as `impl Fn`/`FnMut`/`FnOnce`; DON'T `Box<dyn Fn>` a single concrete type (needless alloc + virtual call). Box only for divergent-arm return types or field/collection storage.
- **`closure-static-vs-dyn`** — DO use `impl Fn`/generic for hot single-call-site callbacks (inlinable); use `&dyn Fn`/`Box<dyn Fn>` to cut code-size bloat or to store heterogeneous closures. `&dyn Fn` avoids the allocation when you only borrow for one call.
- **`closure-move-capture`** — DO add `move` when the closure outlives its scope (returned, stored, `thread::spawn`, `async move`); DO clone selectively *before* `move` when you need the value in both places. `move` controls *how* captures are taken, not `Fn` vs `FnMut`.
- **`closure-disjoint-capture`** — DO capture only the fields you use (edition-2021 disjoint capture keeps siblings accessible); DON'T `move || self.field` — that moves all of `*self`. Fix: bind the field to a local first, then move the local.

Pattern rules (rust-skills `pat-*`):

- **`pat-exhaustive-enum`** — DO match owned enums exhaustively (or list variants with `|`); DON'T use `_ =>` on enums you own — it silently swallows new variants at runtime instead of failing the build. Reserve `_`/`..` for foreign `#[non_exhaustive]` enums, and document why.
- **`pat-matches-macro`** — DO use `matches!(v, Pattern)` (with optional `if` guard) for boolean pattern tests instead of a `match … => true, _ => false`. Pairs with `is_`/`has_`/`can_` predicate methods. Tests: `assert_matches!` (Rust 1.96).
- **`pat-let-else`** — DO use `let Pattern = expr else { diverge };` for early-return extraction to kill rightward drift. The `else` block **must** diverge (`return`/`continue`/`break`/`panic!`/`bail!`). Bound var is in scope *after* the statement. Prefer `?` when `else` would just propagate an error.
- **`pat-if-let-chains`** — DO combine multiple `let` bindings and boolean guards with `&&` in one `if` header (Rust 1.88 / **edition 2024**) instead of nested `if let`. Short-circuits left-to-right. Use `let ... else` instead when the goal is early-return on failure.
- **`pat-at-bindings`** — DO use `name @ pattern` to bind a value while testing it against a pattern (range, sub-struct, or whole variant) in one arm, avoiding re-access or a guard that repeats the condition. Works in `match`, `if let`, `while let`, `let ... else`, fn params.

On-topic functional anti-patterns:

- **`anti-collect-intermediate`** — DON'T `.collect()` mid-chain or just to check length/sum/emptiness; keep the chain lazy and collect once at the end (or not at all, using `any`/`all`/`count`/`sum`/`find`).
- **`anti-index-over-iter`** — DON'T `for i in 0..len { data[i] }`; use `.iter()`/`.iter_mut()`/`.zip()`/`.enumerate()`. Manual indexing adds a bounds check per access and blocks auto-vectorization. Keep indices only for `step_by`, multi-array non-aligned access, or 2D loops.
- **`anti-type-erasure`** — prefer `impl Trait` over `Box<dyn Trait>` when a single concrete type suffices (mirrors `closure-impl-fn-return`).
- **Over-functionalizing** (ch08 §8.10) — DON'T write 5+-deep adapter chains nobody can read; break at ~4 adapters with named intermediates or a helper.
- **Under-functionalizing** (ch08 §8.10) — DON'T hand-roll a flag-and-break loop that `any`/`all`/`find`/`position` already expresses.
- **`?`-reinvention** (ch08 §8.4) — DON'T write `.and_then(|x| next(x)).and_then(|y| next2(y))` when `?` with named steps is clearer.

---

## Gotchas / footguns

- **`move` ≠ `FnOnce`.** `move` only decides *how* captures are taken. A `move` closure reading a `String` is still `Fn`. The trait is inferred from the body.
- **`move` grabs the whole named place.** `move || cfg.field` moves all of `cfg`; `move || self.x` moves `*self`. Bind the field to a local first (`closure-disjoint-capture`).
- **`FnMut` bindings must be `mut`.** `let mut f = counter(0); f();` — forgetting `mut` on the call-site binding is a common compile error when consuming a returned `impl FnMut`.
- **Divergent closure types don't unify.** Two closures with different bodies have *different types*; `if c { |x| x*2 } else { |x| x+1 }` won't type-check as `impl Fn` — box them or use a match returning `Box<dyn Fn>`.
- **`take_while` drops the boundary element.** It excludes the first element that fails the predicate, so a stateful "stop after condition" that should keep the last item needs `scan`/`chain` or a loop (ch08 §8.3).
- **`_ =>` on your own enum is a silent bug factory.** Adding a variant compiles clean and does the wrong thing at runtime (`pat-exhaustive-enum`).
- **Intermediate `collect` is a hidden allocation cliff.** `.map().collect().iter().map().collect()` allocates twice and breaks fusion — the loop it replaced allocated once. Chain adapters directly (`anti-collect-intermediate`).
- **`fold` with `mut` tuple accumulators is a loop in disguise** — longer and harder to read than the loop, and still mutates. Use a real loop for multi-output single-pass work (ch08 §8.3, §8.9).
- **`.and_then()` when there's no function to return from** is the *correct* choice — you can't `?` your way through building a standalone `Option` (ch08 §8.4).
- **`if let` chains need edition 2024.** `if let … && let … && cond` won't compile on 2021 — set `edition = "2024"` (`pat-if-let-chains`).
- **`iter()` vs `into_iter()` vs `iter_mut()`** decide whether closures receive `&T`, `T`, or `&mut T` — a mismatch here is the usual cause of `&&x` / `*x` noise in predicates.

---

## Cheat-sheet

| Need | Reach for |
|---|---|
| Callback called once (may consume) | `F: FnOnce(…)` |
| Callback called many times, may mutate (default) | `F: FnMut(…)` (`mut f`) |
| Callback called many times, read-only/concurrent | `F: Fn(…)` |
| Return a closure, one concrete type | `-> impl Fn(…)` |
| Return a closure whose type depends on runtime | `-> Box<dyn Fn(…)>` |
| Store closures in a field/collection | `Box<dyn Fn(…)>` / `Vec<Box<dyn Fn(…)>>` |
| Borrow a closure for one call, no alloc | `&dyn Fn(…)` |
| Closure outlives scope (thread/async/return) | `move` (clone before `move` to keep original) |
| Capture one field, keep struct usable | `let f = cfg.field; move \|\| …` |
| Setup/teardown that must not be forgotten | `with_*(…, closure)` bracketed pattern |
| `Option` value-or-fallback | `unwrap_or` / `unwrap_or_else` |
| `Option`/`Result` transform inside | `map` / `map_err` |
| Chain fallible steps | `and_then` (or `?`) |
| Keep if predicate holds | `filter` |
| Both-or-neither | `Option::zip` |
| `if cond { Some(x) } else { None }` | `cond.then_some(x)` / `cond.then(\|\| …)` |
| Collection → collection | `iter().filter().map().collect()` |
| Single value (sum/count/min/max) | `.sum()` / `.count()` / `.min()` / `.min_by_key()` |
| Custom reduce, no side effects | `.fold(init, f)` / `.reduce(f)` |
| Short-circuiting reduce | `.try_fold(init, f)` |
| Running/intermediate accumulation, lazy | `.scan(state, f)` |
| Parse list, fail on first error | `.map(…).collect::<Result<Vec<_>, _>>()?` |
| Exists / for-all / first-match | `.any(p)` / `.all(p)` / `.find(p)` / `.position(p)` |
| Boolean pattern test | `matches!(v, Pat)` |
| Early-return extraction | `let Pat = e else { return … };` |
| Multi-binding condition (edition 2024) | `if let … && let … && cond { … }` |
| Bind value while pattern-matching it | `name @ pattern` |
| Multi-output single pass / state machine / in-place | plain `for` / `loop` |

**Related references:** **microsoft-guidelines.md** (M-* pragmatic bounds & API ergonomics), **api-guidelines.md** (C-* — `FromIterator`, `impl Trait` returns, common traits), **design-patterns.md** (builder, RAII/guard, newtype), **style-guide.md** (rustfmt on chained method calls).
