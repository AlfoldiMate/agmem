# Idiom Catalog: Ownership, Memory, Performance & Anti-Patterns

A scannable DO/DON'T engineering catalog for the parts of Rust where the *right* choice is
about cost, not correctness: who owns data, where it lives, how many times it is copied or
allocated, and how the compiler lays it out. Consult this before writing or reviewing any code
in a hot path, any type stored in large collections, any public function signature that takes
`&String`/`&Vec`, or any moment you reach for `.clone()` or `format!()`.

This file distills the `own-*`, `mem-*`, `perf-*`, `opt-*`, and `anti-*` rule families from
rust-skills. It does **not** re-derive the canonical guideline sets — for those see the sibling
references: `api-guidelines.md` (C-* codes), `microsoft-guidelines.md` (M-* codes),
`style-guide.md`, `design-patterns.md`, and `reference-notation.md`. Error-handling and async
anti-patterns are listed in the checklist here but covered in depth in
`error-handling-and-conversions.md`, `async-await.md`, and `concurrency-and-shared-state.md`.

Golden rule that governs the whole file: **measure before you optimize** (`perf-profile-first`,
`anti-premature-optimize`). Everything below is a *default*; a profiler overrides it.

---

## 1. Ownership & Borrowing

### 1.1 Borrow, don't clone (`own-borrow-over-clone`, `anti-clone-excessive`)

`.clone()` allocates and copies; `&T` is free. Clone only to (a) store owned data, (b) satisfy
`'static` for threads/async, or (c) cheaply bump an `Rc`/`Arc` refcount. Cloning to read, to
compare, or to iterate is always wrong.

```rust
// DON'T: clone to read, clone to iterate, clone to compare
fn count_words(text: &String) -> usize { text.clone().split_whitespace().count() }

// DO: borrow; accept &str so callers never allocate
fn count_words_ok(text: &str) -> usize { text.split_whitespace().count() }

fn process_all(items: &[String]) {
    for item in items { handle(item); } // &String, zero allocations
}
# fn handle(_: &str) {}
```

Clippy: `redundant_clone`, `clone_on_copy`, `clone_on_ref_ptr`. See `design-patterns.md`
"Clone to satisfy the borrow checker" anti-pattern.

### 1.2 `Copy` for small types, explicit `Clone` for the rest (`own-copy-small`, `own-clone-explicit`)

`Copy` (implicit, byte-for-byte, no `Drop`, no heap) makes small value types ergonomic. `Clone`
is explicit precisely because it may cost; keep that cost visible at call sites.

| Size | Recommendation |
|------|----------------|
| ≤ 16 bytes, no heap, no `Drop` | `#[derive(Clone, Copy)]` |
| 17–64 bytes | Consider `Copy`; benchmark if hot |
| > 64 bytes | Prefer references; `Clone` only |

```rust
#[derive(Clone, Copy)] struct Point { x: f64, y: f64 } // 16 bytes → Copy
#[derive(Clone)]       struct Person { name: String, age: u32 } // heap → Clone only
```

`&mut T` is **not** `Copy` (that would alias); it is reborrowed instead. See
`newtype-typestate-and-phantom.md` — newtype IDs are usually `Copy`.

### 1.3 `clone_from` to reuse an allocation (`mem-clone-from`)

`x = y.clone()` drops `x`'s buffer and allocates fresh. `x.clone_from(&y)` reuses `x`'s capacity
when it fits — decisive when cloning repeatedly into one variable.

```rust
# fn process(_: &str) {}
# let sources: Vec<&String> = vec![];
let mut buf = String::with_capacity(1024);
for source in sources {
    buf.clone_from(source); // reuses buffer if source fits; no alloc
    process(&buf);
}
```

When implementing `Clone` by hand, override `clone_from` too, delegating to each field's
`clone_from`.

### 1.4 `Cow` for conditional ownership (`own-cow-conditional`)

`Cow<'a, T>` is borrowed-or-owned: allocate only on the branch that mutates. Ideal for
"usually pass through unchanged, occasionally rewrite" and for returning a mix of `&'static`
literals and formatted strings.

```rust
use std::borrow::Cow;
fn normalize(path: &str) -> Cow<'_, str> {
    if path.contains("//") { Cow::Owned(path.replace("//", "/")) }
    else { Cow::Borrowed(path) } // zero-cost
}
fn err_msg(code: u32) -> Cow<'static, str> {
    match code {
        404 => Cow::Borrowed("Not Found"),          // no alloc
        _   => Cow::Owned(format!("Error {code}")), // alloc only for unknown
    }
}
```

### 1.5 `mem::take` / `mem::replace` to move out of `&mut` (`mem-take-replace`)

You cannot move a field out of `&mut self`. The lazy fix is `.clone()`; the zero-copy fix is
`mem::take` (swaps in `Default`) or `mem::replace` (swaps in a value you supply). Both are the
idiomatic tool for state-machine transitions, `Drop`, and `Future::poll`.

```rust
use std::mem;
struct Processor { items: Vec<String> }
impl Processor {
    fn flush(&mut self) -> Vec<String> { mem::take(&mut self.items) } // leaves empty Vec
}
// State machine: swap in the next state, inspect the old one.
enum State { Idle, Loading { url: String }, Done { body: String } }
impl Default for State { fn default() -> Self { State::Idle } }
fn complete(state: &mut State, body: String) {
    if let State::Loading { url } = mem::replace(state, State::Done { body }) {
        println!("finished {url}");
    }
}
```

`Option::take` is the same idea for `Option<T>` fields (yields `None`). See `design-patterns.md`
"mem::take / mem::replace".

### 1.6 Move large values via `Box`; borrow when you can (`own-move-large`)

A move is a `memcpy`. Large structs (hundreds of bytes to KB) moved repeatedly cost real time;
`Box` shrinks the move to an 8-byte pointer copy. Better still: don't move — borrow `&`/`&mut`.

| Type size | Move frequency | Action |
|-----------|----------------|--------|
| < 128 B | any | don't box |
| 128–512 B | frequent | consider box |
| > 512 B | any | box or borrow |

```rust
struct GameState { board: Box<[[u8; 100]; 100]> } // 8-byte moves regardless of size
fn analyze(s: &GameState) { /* borrow: no move at all */ let _ = s; }
```

### 1.7 Lean on lifetime elision (`own-lifetime-elision`)

Write explicit lifetimes only when elision genuinely fails (multiple input refs feeding one
output; structs holding refs; `'static`). Use `'_` to keep an elided lifetime visible.

```rust
fn first_word(s: &str) -> &str { s.split_whitespace().next().unwrap_or("") } // elided
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str { if x.len() > y.len() { x } else { y } }
```

The three rules: each input ref gets its own lifetime; one input ref → output shares it;
`&self` → output shares `self`'s lifetime.

### 1.8 Accept slices, not owned collections (`own-slice-over-vec`, `anti-vec-for-slice`, `anti-string-for-str`)

`&[T]`/`&str`/`&Path` accept vecs, arrays, literals, sub-slices; `&Vec<T>`/`&String`/`&PathBuf`
accept exactly one type and force needless allocation. Deref coercion makes the flexible form a
free upgrade. This mirrors api-guidelines `C-CALLER-CONTROL` and Clippy `ptr_arg`.

```rust
fn sum(xs: &[i32]) -> i32 { xs.iter().sum() }        // Vec, array, slice all coerce
fn greet(name: &str) { println!("hi {name}"); }       // String, &str, literal all coerce
fn read_cfg(p: impl AsRef<std::path::Path>) { let _ = p.as_ref(); } // maximal flexibility
```

| Don't take | Take instead |
|------------|--------------|
| `&Vec<T>` | `&[T]` |
| `&String` | `&str` |
| `&Box<T>` | `&T` |
| `&PathBuf` | `&Path` |
| `&OsString` | `&OsStr` |

### 1.9 Shared ownership: `Rc` single-thread, `Arc` cross-thread (`own-rc-single-thread`, `own-arc-shared`)

`Rc` uses non-atomic refcounts (cheaper); `Arc` uses atomics (thread-safe). Never pay for atomics
you don't need; never try to send `Rc` across threads (it's `!Send`, the compiler stops you).
Prefer `Rc::clone(&x)`/`Arc::clone(&x)` over `x.clone()` to make the cheap refcount bump explicit.
Break reference cycles with `Weak` (back-references) — `Rc` leaks a cycle otherwise.

```text
Need shared ownership?
├── No  → owned value or &references
└── Yes → crosses threads?
    ├── No  → Rc<T>   (+ RefCell for mutation)
    └── Yes → Arc<T>  (+ Mutex/RwLock for mutation)
```

Don't `Arc::clone` inside a hot loop; clone once outside, pass `&Arc` in.

### 1.10 Interior mutability ladder (`own-refcell-interior`, `own-mutex-interior`, `own-rwlock-readers`)

Mutate through `&self` when the borrow checker is too strict. Pick the lightest tool:

| Type | Threading | Cost | Use when |
|------|-----------|------|----------|
| `Cell<T>` | single | none, never panics | `Copy` values, `get`/`set` |
| `RefCell<T>` | single | runtime borrow flags, can panic | non-`Copy`, `borrow`/`borrow_mut` |
| `Mutex<T>` | multi | OS lock | shared mutable state |
| `RwLock<T>` | multi | reader tracking | reads ≫ writes (>~80% reads) |

```rust
use std::cell::Cell;
struct Counter { n: Cell<u32> }
impl Counter { fn bump(&self) { self.n.set(self.n.get() + 1); } } // never panics
```

`RefCell::borrow` + `borrow_mut` alive together = runtime panic; use `try_borrow*` when unsure.
Prefer `parking_lot::{Mutex, RwLock}` (no poisoning, 1-byte, faster under contention). Std `Mutex`
poisons on panic — recover with `lock().unwrap_or_else(|e| e.into_inner())`. `RwLock` costs more
than `Mutex` when writes are frequent or the lock is held only briefly.

### 1.11 Drop order is observable — encode it (`mem-drop-order`)

Struct fields drop **top-to-bottom** (declaration order); locals and function args drop in
**reverse**; tuple/array elements in index order. RAII guards (locks, transactions, spans) do
real work in `Drop`, so wrong order = silent bug (lock released before commit, etc.).

```rust
struct Session {
    transaction: Transaction,          // declared first → drops first (commits)
    guard: std::sync::MutexGuard<'static, ()>, // drops second (lock released after commit)
}
# struct Transaction; impl Drop for Transaction { fn drop(&mut self) {} }
```

Reorder fields to encode the contract; drop a local early with `drop(x)` when scope order is
wrong; `ManuallyDrop<T>` opts out entirely (for `unsafe`/move-out-in-`Drop`); `mem::forget`
leaks (FFI hand-off only).

---

## 2. Memory Management & Allocation

### 2.1 Pre-size with `with_capacity` (`mem-with-capacity`)

A growing `Vec`/`String`/`HashMap` reallocates and copies at each capacity doubling. When the
final size is known or estimable, pre-allocate once. `collect()` already uses the iterator's
`size_hint`, so a direct `collect` is fine.

```rust
let mut v = Vec::with_capacity(1000);
for i in 0..1000 { v.push(i); } // zero reallocations
let m: std::collections::HashMap<_, _> = // collect uses size_hint
    (0..100).map(|k| (k, k * k)).collect();
```

`reserve`/`reserve_exact` add headroom; `shrink_to_fit` releases it.

### 2.2 Clear and reuse in loops (`mem-reuse-collections`, `perf-drain-reuse`, `perf-collect-into`)

Allocating a fresh collection each loop iteration is allocator churn. Hoist the buffer out and
`clear()` it — capacity survives. `drain(..)` empties while keeping capacity *and* yields the
elements. On stable Rust use `buf.extend(iter)` to refill a cleared buffer (`collect_into` is
nightly-only).

```rust
# let batches: Vec<Vec<i32>> = vec![];
# fn process(_: &[i32]) {}
let mut buf = Vec::new();
for batch in &batches {
    buf.clear();                       // keep capacity
    buf.extend(batch.iter().filter(|&&x| x > 0).copied());
    process(&buf);
}
```

`clear` = empty, keep cap. `drain(..)` = empty + iterate, keep cap. `mem::take` = move out, cap
reset to 0.

### 2.3 Box the large enum variant (`mem-box-large-variant`)

An enum is as big as its largest variant — every instance pays for it. Box the outlier to keep
the enum small (better cache use, cheaper moves). Also mandatory for recursive types (`Expr`,
linked lists). Clippy `large_enum_variant` flags this.

```rust
enum Message {
    Quit,                       // small
    Move { x: i32, y: i32 },    // small
    Image(Box<[u8; 1024]>),     // 8 bytes here, 1 KB on heap
}
```

### 2.4 `Box<[T]>` / `Box<str>` for fixed-size heap data (`mem-boxed-slice`)

`Vec` carries ptr+len+**cap** (24 B); once a collection stops growing, `into_boxed_slice()` drops
the capacity word → 16 B (fat pointer) and signals "fixed size." Same for `String` → `Box<str>`.
Worthwhile across many instances.

```rust
struct Doc { paragraphs: Box<[String]> } // 16 B field vs 24 B
let d = Doc { paragraphs: vec![String::new()].into_boxed_slice() };
# let _ = d;
```

### 2.5 Inline small collections: `SmallVec` / `ArrayVec` / `TinyVec` / `ThinVec` (`mem-smallvec`, `mem-arrayvec`, `mem-thinvec`)

| Type | Stack | Heap | Empty size | Use when |
|------|-------|------|-----------|----------|
| `Vec<T>` | never | always | 24 B | unbounded, may grow |
| `SmallVec<[T; N]>` | up to N | beyond N | ~32 B | usually small, sometimes large |
| `ArrayVec<T, N>` | always | never | inline | hard cap, no heap allowed (embedded/RT) |
| `TinyVec<[T; N]>` | up to N | beyond N | — | like SmallVec, 100% safe code |
| `ThinVec<T>` | never | always | **8 B** | many instances, often empty; `Option<ThinVec>` is free |

```rust
use smallvec::SmallVec;
fn path_parts(p: &str) -> SmallVec<[&str; 8]> { p.split('/').collect() } // no heap for shallow paths
```

`SmallVec`/`ThinVec` add per-op branching or pointer indirection — profile; don't use in tight
iteration loops. `ArrayVec::try_push` returns `Err` when full (`push` panics).

### 2.6 Compact strings for many short strings (`mem-compact-string`)

`String` is 24 B and always heaps. `CompactString`/`SmartString` are 24 B but store ≤ 23 bytes
inline (no allocation); `EcoString` is 16 B (≤ 15 inline) with O(1) clone. Decisive for millions
of short usernames/keys/words. Keep `String`/`&str` at public API boundaries.

### 2.7 Right-size integers, pack structs, use niches (`mem-smaller-integers`)

`u64` for a value that fits `u8` wastes 7 bytes × N. Pick the smallest type that fits the domain;
order struct fields large-to-small to minimize padding; collapse booleans with `bitflags`;
exploit niche optimization with `NonZero*` so `Option<T>` costs nothing.

```rust
struct Pixel { r: u8, g: u8, b: u8, a: u8 } // 4 B, not 32 B
use std::num::NonZeroU64;
assert_eq!(size_of::<Option<u64>>(), 16);
assert_eq!(size_of::<Option<NonZeroU64>>(), 8); // 0 is the None niche
# use std::mem::size_of;
```

Field order matters: `{u8, u64, u8}` = 24 B (padding); `{u64, u8, u8}` = 16 B. See
`newtype-typestate-and-phantom.md` for `NonZero`/newtype IDs.

### 2.8 Guard type size with static assertions (`mem-assert-type-size`)

Adding a field can silently balloon a hot type stored 10M times. Lock the size at compile time so
growth is a deliberate, reviewed act.

```rust
struct Event { timestamp: u64, payload: [u8; 32] }
const _: () = assert!(std::mem::size_of::<Event>() == 40);
// or: static_assertions::assert_eq_size!(Event, [u8; 40]);
```

Do this for types in large collections, FFI/wire formats, and hot paths.

### 2.9 Arena / bump allocation for batch lifetimes (`mem-arena-allocator`)

When many small allocations share one lifetime (parse trees, per-request scratch), a bump
allocator (`bumpalo::Bump`) allocates by incrementing a pointer and frees everything in O(1) on
drop/reset. Copy data out before the arena drops; don't use for long-lived or escaping data.

```rust
use bumpalo::Bump;
fn parse<'a>(tokens: &[u32], arena: &'a Bump) -> Vec<&'a u32> {
    tokens.iter().map(|t| &*arena.alloc(*t)).collect()
} // whole arena freed at once
```

A thread-local reset-between-uses scratch arena is the common production shape. Measure — the
speedup depends on allocator and workload.

### 2.10 Zero-copy: return references, slice instead of copy (`mem-zero-copy`)

Work through references into the original buffer instead of allocating copies. `str::lines()`
into `Vec<&str>`, slice a packet with `&buf[..16]`, parse into a struct of `&'a str` fields. Use
`bytes::Bytes` for refcounted zero-copy slicing, `memchr` for SIMD byte search. Copy only when
you must mutate, outlive the source, or send across threads without `Arc`.

```rust
struct Parsed<'a> { name: &'a str, value: &'a str }
fn parse(input: &str) -> Option<Parsed<'_>> {
    let (name, value) = input.split_once('=')?;
    Some(Parsed { name, value }) // no allocation
}
```

### 2.11 Avoid `format!`; `write!` into a buffer (`mem-avoid-format`, `mem-write-over-format`)

`format!` always allocates a fresh `String`, even for constant text. Return `&'static str` or
`Cow<'static, str>` for constants; `write!`/`writeln!` into an existing `String`/`Vec<u8>` (bring
`std::fmt::Write` or `std::io::Write` into scope) to reuse capacity in loops. See
`design-patterns.md` "Concatenate strings with `format!`" for the legitimate case.

```rust
use std::fmt::Write;
let mut buf = String::with_capacity(32);
for i in 0..1000 {
    buf.clear();
    write!(&mut buf, "item-{i}").unwrap(); // no alloc after first iteration
    process(&buf);
}
# fn process(_: &str) {}
```

`format!` is fine for one-offs, return values you must own, and cold/error paths.

---

## 3. Performance & Hot-Path Discipline

### 3.1 Profile first; never guess (`perf-profile-first`, `anti-premature-optimize`)

Intuition about bottlenecks is usually wrong; the hot 10% is where effort pays. Workflow:
write correct idiomatic code → benchmark hot paths → profile under realistic load → optimize
**one** thing → re-measure. Tools: `cargo flamegraph`, `perf` (Linux), `cargo instruments`
(macOS), `dhat` (heap), `criterion` (micro-bench). "The biggest wins come from algorithms and
data structures, not low-level tweaks." Common premature mistakes: `unsafe` for bounds-check
removal (iterators do it safely), `#[inline(always)]` everywhere, custom allocators, object
pools, hand-rolled SIMD.

### 3.2 Iterators over manual indexing (`perf-iter-over-index`, `anti-index-over-iter`)

`for i in 0..len { data[i] }` bounds-checks every access and blocks auto-vectorization; iterators
eliminate both and remove off-by-one risk. Use `zip` for parallel slices, `windows`/`chunks` for
neighborhoods, `enumerate` when you truly need the index.

```rust
fn dot(a: &[f64], b: &[f64]) -> f64 { a.iter().zip(b).map(|(&x, &y)| x * y).sum() }
```

| Index pattern | Iterator |
|---------------|----------|
| `for i in 0..v.len()` | `for x in &v` |
| `v[0]` / `v[len-1]` | `v.first()` / `v.last()` |
| `a[i] + b[i]` loop | `a.iter().zip(&b)` |

### 3.3 Keep iterators lazy; collect once (`perf-iter-lazy`, `perf-collect-once`, `anti-collect-intermediate`)

Each `.collect()` allocates and forces a pass. Chain adapters lazily and collect exactly once at
the end — or not at all when a consumer (`any`, `count`, `sum`, `find`) suffices. Iterators
short-circuit, so `find`/`any` beat "collect then check."

```rust
fn process(data: Vec<i32>) -> Vec<i32> {
    data.into_iter().filter(|x| *x > 0).map(|x| x * 2).take(10).collect() // 1 alloc, 1 pass
}
fn has_positive(d: &[i32]) -> bool { d.iter().any(|&x| x > 0) } // 0 alloc, short-circuits
```

Collect only when you must iterate twice, sort, or index. To hand ownership to the caller, return
`impl Iterator<Item = _>` and let *them* decide (see `closures-and-functional-style.md`).

### 3.4 One lookup with the entry API (`perf-entry-api`)

`contains_key` + `insert` hashes and probes the map twice. `entry()` does it once and returns a
handle. See api-guidelines `C-` map idioms.

```rust
use std::collections::HashMap;
fn count(words: &str) -> HashMap<&str, u32> {
    let mut m = HashMap::new();
    for w in words.split_whitespace() { *m.entry(w).or_insert(0) += 1; }
    m
}
```

`or_insert` / `or_insert_with` (lazy) / `or_default` / `.and_modify(f).or_insert(v)`.

### 3.5 Batch with `extend`; avoid `chain` in hot loops (`perf-extend-batch`, `perf-chain-avoid`)

`extend(iter)` pre-reserves from `size_hint` and inserts in one shot; a `push` loop may reallocate
repeatedly. `append(&mut other)` moves elements between vecs. `chain()` adds a branch per
`.next()` to decide which iterator is live — fine for one-off/short-circuit iteration, but in a
million-iteration inner loop split into two loops or pre-`extend_from_slice`.

```rust
# let chunks: Vec<&[i32]> = vec![];
# let total = 0;
let mut out = Vec::with_capacity(total);
for chunk in &chunks { out.extend_from_slice(chunk); } // no per-item branch, no realloc
```

### 3.6 Buffer I/O (`perf-io-buffering`)

Every unbuffered `read`/`write` is a syscall. Wrap files and sockets in `BufReader`/`BufWriter`
to batch them — often the single highest-impact, lowest-effort fix for I/O code. **Always
`writer.flush()?` explicitly**: `BufWriter`'s drop flush silently discards errors. Default buffer
is 8 KiB; use `with_capacity(64 * 1024)` for large sequential reads.

```rust
use std::io::{BufRead, BufReader, BufWriter, Write};
# use std::fs::File;
fn copy_lines(inp: &str, outp: &str) -> std::io::Result<()> {
    let r = BufReader::new(File::open(inp)?);
    let mut w = BufWriter::new(File::create(outp)?);
    for line in r.lines() { writeln!(w, "{}", line?)?; }
    w.flush() // must flush; drop swallows the error
}
```

### 3.7 Faster hasher when DoS resistance is unneeded (`perf-ahash`)

Std `HashMap` uses SipHash (DoS-resistant but ~2–4× slower). For maps keyed by trusted internal
data (integer IDs, handles) switch hashers. **Security note:** never use a non-DoS-resistant
hasher for keys from untrusted external input.

| Hasher | Crate | DoS-resistant | Use |
|--------|-------|---------------|-----|
| SipHash | std | yes | untrusted external keys (default) |
| `ahash` | `ahash` | yes (randomized) | safe general upgrade, ~2× faster |
| `FxHash` | `rustc-hash` | **no** | trusted integer/pointer keys only |

### 3.8 No `format!` in hot paths (`anti-format-hot-path`)

Restatement in the perf frame of §2.11: a `format!` per loop iteration is allocation churn.
Reuse a buffer with `write!`, or `push_str`/`push` for the fastest concatenation, or implement
`Display` and let the caller control allocation. See `mem-write-over-format`.

### 3.9 `black_box` in benchmarks (`perf-black-box-bench`)

The optimizer deletes computations whose results are unused and constant-folds fixed inputs — a
benchmark then measures nothing. Wrap both the input and the result in `std::hint::black_box`
(re-exported by criterion).

```rust
# use criterion::{Criterion, black_box};
fn bench(c: &mut Criterion) {
    c.bench_function("f", |b| b.iter(|| black_box(expensive(black_box(42)))));
}
# fn expensive(x: i32) -> i32 { x }
```

---

## 4. Compiler & Codegen Optimization

Everything here is a build-time knob or annotation applied *after* profiling proves the hot spot.

### 4.1 Tune the release profile (`perf-release-profile`, `opt-lto-release`, `opt-codegen-units`, `opt-pgo-profile`, `opt-target-cpu`)

The default release profile favors compile speed. For production binaries (not library crates —
let downstream choose):

```toml
[profile.release]
opt-level = 3
lto = "fat"          # cross-crate inlining/DCE/devirt; +10–20%, slow compile
codegen-units = 1    # whole-crate optimization; +5–20%
panic = "abort"      # no unwind tables, smaller
strip = true
```

- **LTO**: `false` → `"thin"` (+5–15%, medium) → `"fat"` (+10–20%, slow). Enables cross-crate
  inlining.
- **codegen-units = 1**: whole-crate visibility for LLVM; slower build.
- **PGO** (`opt-pgo-profile`): instrument → run representative workloads → rebuild with
  `-Cprofile-use`. +10–30% on top; add BOLT for another 5–15%.
- **target-cpu** (`opt-target-cpu`): default is a generic baseline (SSE2). `.cargo/config.toml`
  `rustflags = ["-C", "target-cpu=native"]` unlocks AVX2/AVX-512 etc. — but the binary then only
  runs on that class of CPU; use runtime `is_x86_feature_detected!` for portable binaries.
- Optimize dependencies even in dev: `[profile.dev.package."*"] opt-level = 3`.

### 4.2 Inlining attributes (`opt-inline-small`, `opt-inline-always-rare`, `opt-inline-never-cold`)

The compiler usually inlines better than you. Guidance:

| Attribute | When |
|-----------|------|
| none | default; let the compiler decide (same-crate) |
| `#[inline]` | small hot fns, **especially public/generic across crate boundaries** (body otherwise unavailable without LTO) |
| `#[inline(always)]` | tiny, verified-hot fns only, proven by benchmark; overuse bloats i-cache |
| `#[inline(never)]` | large or cold code; keep it out of the hot path |

### 4.3 `#[cold]` and branch hints (`opt-cold-unlikely`, `opt-likely-hint`, `opt-inline-never-cold`)

Extract error/panic/fallback construction into `#[cold] #[inline(never)]` functions so the hot
path stays dense and the compiler lays cold code elsewhere and biases branch prediction.

```rust
fn parse(input: &str) -> Result<i32, ()> {
    match input.parse() { Ok(n) => Ok(n), Err(_) => cold_err() }
}
#[cold] #[inline(never)]
fn cold_err() -> Result<i32, ()> { Err(()) }
```

On stable, structure code so the hot path falls through and unlikely cases early-return (the
compiler treats early returns as unlikely). `std::hint::cold_path()` is stable (1.95) for marking
a rare branch; `std::hint::{likely, unlikely}` remain nightly. Order match arms most-common-first.

### 4.4 Bounds-check elimination (`opt-bounds-check`)

Prefer iterator patterns (`zip`, `windows`, `chunks_exact`, slice patterns, `split_at`) that give
the optimizer the information to drop bounds checks and vectorize. `get_unchecked` is a last
resort behind proven `assert!`s (with a `// SAFETY:` comment). Verify elimination in generated
asm (`cargo-show-asm` / `cargo asm`) — it is never guaranteed.

```rust
fn header(data: &[u8]) -> Option<(u8, &[u8])> {
    let [magic, rest @ ..] = data else { return None }; // one check, no per-field checks
    Some((*magic, rest))
}
```

### 4.5 Cache-friendly layout (`opt-cache-friendly`)

An L3 miss (~100+ cycles) dwarfs an L1 hit (~4). When you sweep one field over many records,
prefer Struct-of-Arrays over Array-of-Structs; split hot/cold fields into separate structs; use
contiguous `Vec` + integer indices instead of pointer-chasing linked structures;
`#[repr(C, align(64))]` + padding to prevent false sharing between atomics.

### 4.6 SIMD (`opt-simd-portable`)

Prefer letting LLVM auto-vectorize (help it: iterators, no early exits, `chunks_exact`). Then the
`wide` crate (stable, portable), then `std::simd` (nightly, `portable_simd`), then
architecture intrinsics behind `#[target_feature]` + runtime detection as the last resort.

---

## 5. General Anti-Patterns (type & API design)

### 5.1 Stringly-typed data (`anti-stringly-typed`)

A `&str` where a fixed value set exists accepts any garbage, validates nothing, and lets swapped
arguments compile. Model the set with an `enum` (exhaustive `match` is compiler-checked) and
semantic values with validated newtypes. Parse at the boundary (`FromStr`/`TryFrom`), use the
type internally. See `api-parse-dont-validate`, api-guidelines `C-NEWTYPE`/`C-VALIDATE`, and
`newtype-typestate-and-phantom.md`.

```rust
#[derive(Clone, Copy, PartialEq)] enum Status { Pending, Completed, Cancelled }
struct Email(String); // constructed only via validating Email::new
fn process(status: Status, email: &Email) { /* can't swap args; can't pass invalid email */ }
```

### 5.2 Over-abstraction (`anti-over-abstraction`)

Generics and traits cost compile time, binary size, and cognition. Start concrete; generalize on
the **rule of three** (three real implementations sharing behavior), for public library APIs, or
when static dispatch is measured-necessary. Smells: a generic trait with one impl, `T,U,V,W`
soup, marker traits, `where T: A + B + C + D`, phantom generics. YAGNI beats "might need it later."

### 5.3 Needless type erasure (`anti-type-erasure`)

`Box<dyn Trait>` adds heap allocation + dynamic dispatch. When one concrete type flows through,
return `impl Trait` (RPIT, and RPITIT in traits since 1.75) or take `impl Trait`/`<T>` — zero
overhead via monomorphization. For a closed set of variants, an `enum` + `match` beats
`Box<dyn>`. Reserve `Box<dyn>` for genuinely heterogeneous runtime collections, config-selected
types, and recursive structures. See `trait-dyn-vs-generic`, `closure-static-vs-dyn`,
`generics-and-traits.md`, and `design-patterns.md` "On-stack dynamic dispatch".

```rust
fn evens() -> impl Iterator<Item = i32> { (0..10).filter(|x| x % 2 == 0) } // no Box
```

### 5.4 Excessive clone / premature optimization

Covered as §1.1 and §3.1 — the two most common review findings. Both reduce to: borrow by
default, and let the profiler, not intuition, authorize complexity.

---

## Rules & anti-patterns checklist

Scannable index — rule id → DO/DON'T → reason. Grouped by family.

### Ownership (`own-*`)
- **own-borrow-over-clone** — DO pass `&T`; DON'T `.clone()` to read. Cloning allocates; borrowing is free.
- **own-copy-small** — DO derive `Copy` for ≤16 B heap-free types; DON'T for anything with heap/`Drop`. Ergonomics without cost.
- **own-clone-explicit** — DO implement `Clone` (not `Copy`) for heap types; keep cost visible; override `clone_from`.
- **own-cow-conditional** — DO return `Cow` when you usually borrow, sometimes own. Allocates only on the owning branch.
- **own-lifetime-elision** — DO rely on elision; DON'T annotate lifetimes the compiler infers. Less clutter.
- **own-move-large** — DO `Box` large frequently-moved structs (or borrow). A move is a memcpy; box makes it 8 bytes.
- **own-slice-over-vec** — DO take `&[T]`/`&str`/`&Path`; DON'T take `&Vec`/`&String`/`&PathBuf`. Flexibility + no forced alloc.
- **own-rc-single-thread** — DO use `Rc` in single-threaded sharing; DON'T pay `Arc` atomics you don't need. Break cycles with `Weak`.
- **own-arc-shared** — DO use `Arc` (+`Mutex`/`RwLock`) to share across threads; clone once outside hot loops.
- **own-refcell-interior** — DO use `RefCell`/`Cell` for single-thread interior mutability; DON'T across threads (`!Sync`).
- **own-mutex-interior** — DO use `Mutex` for cross-thread mutable state; prefer `parking_lot`; handle poisoning.
- **own-rwlock-readers** — DO use `RwLock` when reads ≫ writes; DON'T when writes are frequent or locks are brief (use `Mutex`).

### Memory (`mem-*`)
- **mem-with-capacity** — DO pre-allocate when size is known. Avoids reallocation copies.
- **mem-reuse-collections** — DO hoist buffers out of loops and `clear()`. Keeps capacity, kills churn.
- **mem-clone-from** — DO `x.clone_from(&y)` when cloning repeatedly into `x`. Reuses the buffer.
- **mem-take-replace** — DO `mem::take`/`mem::replace` to move out of `&mut`; DON'T `.clone()`. Zero-copy.
- **mem-box-large-variant** — DO box the oversized enum variant. Enum size = largest variant.
- **mem-boxed-slice** — DO use `Box<[T]>`/`Box<str>` for fixed-size heap data. Drops the capacity word.
- **mem-smallvec** — DO `SmallVec<[T;N]>` for usually-small collections. Inline until N; profile the branch cost.
- **mem-arrayvec** — DO `ArrayVec<T,N>` for a hard cap with no heap (embedded/RT). `try_push` for overflow.
- **mem-thinvec** — DO `ThinVec` for many often-empty vecs. 8 B empty; free `Option`.
- **mem-compact-string** — DO `CompactString`/`EcoString` for millions of short strings. Inline small; keep `String` at APIs.
- **mem-smaller-integers** — DO pick the smallest fitting integer, order fields large→small, use `bitflags`/`NonZero`.
- **mem-assert-type-size** — DO `const _: () = assert!(size_of::<T>() == N);` on hot/wire types. Catches silent bloat.
- **mem-arena-allocator** — DO bump-allocate batch/request-scoped data (`bumpalo`); DON'T for escaping/long-lived data.
- **mem-zero-copy** — DO return `&str`/slices/`Bytes` into the source; DON'T copy to read. Slice, don't allocate.
- **mem-avoid-format** — DON'T `format!` constant text; return `&'static str`/`Cow`.
- **mem-write-over-format** — DO `write!` into a reused buffer in loops; DON'T `format!` per iteration.
- **mem-drop-order** — DO order struct fields to encode drop sequence (fields drop top-down, locals reverse). RAII guards are order-sensitive.

### Performance (`perf-*`)
- **perf-profile-first** — DO profile before optimizing. Intuition targets the wrong code.
- **perf-iter-over-index** — DO iterate; DON'T `for i in 0..len`. Eliminates bounds checks, enables SIMD.
- **perf-iter-lazy** — DO keep chains lazy, collect once; short-circuit with `any`/`find`.
- **perf-collect-once** — DON'T `.collect()` intermediate steps. Each one allocates and re-passes.
- **perf-collect-into** — DO reuse a buffer via `extend` (stable) / `collect_into` (nightly). Reuses allocation.
- **perf-entry-api** — DO `entry().or_insert(..)`; DON'T `contains_key`+`insert`. One lookup, not two.
- **perf-extend-batch** — DO `extend`/`extend_from_slice`/`append` for bulk inserts; DON'T `push` in a loop.
- **perf-drain-reuse** — DO `drain(..)` to empty-and-iterate while keeping capacity.
- **perf-chain-avoid** — DON'T `chain()` in million-iteration inner loops (per-item branch); split loops. Fine for one-off/short-circuit.
- **perf-io-buffering** — DO wrap I/O in `BufReader`/`BufWriter`; **always `flush()?`**. Batches syscalls; drop-flush hides errors.
- **perf-ahash** — DO switch to `ahash`/`FxHash` for trusted keys; DON'T for untrusted external input (security).
- **perf-black-box-bench** — DO `black_box` inputs and results in benchmarks. Stops the optimizer from measuring nothing.
- **perf-release-profile** — DO tune `[profile.release]` (lto/codegen-units/panic/strip) for production binaries.

### Codegen / build (`opt-*`)
- **opt-lto-release** — DO `lto = "fat"` for release binaries; DON'T for library crates (users choose).
- **opt-codegen-units** — DO `codegen-units = 1` for max optimization; costs build time.
- **opt-pgo-profile** — DO PGO for stable-workload production apps. +10–30%.
- **opt-target-cpu** — DO `target-cpu=native` for known deployment hardware; DON'T ship it as a portable binary.
- **opt-inline-small** — DO `#[inline]` small hot public/generic fns (cross-crate). Body must be visible to inline.
- **opt-inline-always-rare** — DON'T `#[inline(always)]` by default; only benchmark-proven tiny hot fns. Overuse bloats i-cache.
- **opt-inline-never-cold** — DO `#[inline(never)] #[cold]` on error/panic construction. Keeps hot path dense.
- **opt-cold-unlikely** — DO `#[cold]` rarely-called fns; improves layout and branch prediction.
- **opt-likely-hint** — DO structure hot path as fall-through, unlikely as early-return; `std::hint::cold_path()` (stable).
- **opt-bounds-check** — DO use iterator/slice patterns for BCE; `get_unchecked` only behind proven asserts + `// SAFETY:`.
- **opt-cache-friendly** — DO SoA / hot-cold split / index-based graphs for sequential access; pad atomics against false sharing.
- **opt-simd-portable** — DO prefer autovectorization → `wide` → `std::simd` → intrinsics (last resort).

### Anti-patterns (`anti-*`)
- **anti-clone-excessive** — DON'T clone to read/iterate/compare. Borrow; `Arc` for shared ownership.
- **anti-collect-intermediate** — DON'T collect mid-chain. Stay lazy; collect once.
- **anti-index-over-iter** — DON'T index when iterators work. Bounds checks + no SIMD + off-by-one risk.
- **anti-format-hot-path** — DON'T `format!` in loops/hot fns. Reuse a buffer with `write!`.
- **anti-premature-optimize** — DON'T optimize before profiling. Simple idiomatic code first; the compiler is smart.
- **anti-string-for-str** — DON'T take `&String`. `&str` is strictly more flexible (Clippy `ptr_arg`).
- **anti-vec-for-slice** — DON'T take `&Vec<T>`. `&[T]` accepts vecs, arrays, slices.
- **anti-stringly-typed** — DON'T use strings for fixed sets/semantic values. Enums + validated newtypes catch errors at compile time.
- **anti-over-abstraction** — DON'T generalize speculatively. Rule of three; concrete in private code.
- **anti-type-erasure** — DON'T `Box<dyn Trait>` for one concrete type. `impl Trait`/generics/`enum` are zero-cost.
- **anti-unwrap-abuse** — DON'T `.unwrap()` in production; `?`/`ok_or`/`unwrap_or`. (Depth: `error-handling-and-conversions.md`.)
- **anti-expect-lazy** — DON'T `.expect()` for recoverable errors; reserve for invariant/bug proofs. (See error-handling ref.)
- **anti-panic-expected** — DON'T `panic!` on expected failures (I/O, input); return `Result`. (See error-handling ref.)
- **anti-empty-catch** — DON'T silently drop errors (`let _ =`, `.ok()`); log, propagate, or document why. (See error-handling ref.)
- **anti-lock-across-await** — DON'T hold a lock guard across `.await` (deadlock/blocking); extract value, drop guard first. (Depth: `async-await.md`, `concurrency-and-shared-state.md`; Clippy `await_holding_lock`.)

---

## Gotchas / footguns

- **`BufWriter` drop swallows flush errors.** A dropped `BufWriter` tries to flush but *discards*
  any error. Data loss looks like success. Always `writer.flush()?` explicitly (`perf-io-buffering`).
- **Drop order is silent.** Reordering struct fields or locals changes when locks/transactions
  release — no warning, wrong behavior. Fields drop top-to-bottom, locals bottom-to-top (`mem-drop-order`).
- **`RefCell` panics at runtime.** A live `borrow()` plus `borrow_mut()` aborts the program;
  compiles clean. Use `try_borrow*` when overlap is possible (`own-refcell-interior`).
- **`std::sync::Mutex` across `.await` can deadlock the runtime**, not just block. Even
  `tokio::sync::Mutex` held across await serializes tasks (`anti-lock-across-await`).
- **Fast hasher on untrusted keys is a security bug**, not just a perf choice — hash flooding.
  `FxHash` is predictable; keep it to internal integer/pointer keys (`perf-ahash`).
- **`Option<Vec<T>>` gets no niche optimization** — still 24 B, `None` is a distinct state, not a
  null pointer. `ThinVec` (empty = null) or `Option<Box<[T]>>` do better (`mem-thinvec`).
- **`SmallVec`/`ThinVec` can be *slower*** than `Vec` in tight loops (per-op branch / pointer
  indirection). They save allocations, not cycles — profile (`mem-smallvec`, `mem-thinvec`).
- **Benchmarks lie without `black_box`.** The optimizer deletes unused results and folds constant
  inputs; you measure an empty loop (`perf-black-box-bench`).
- **`target-cpu=native` binaries SIGILL on older CPUs.** Great for a pinned deployment target,
  fatal for a distributed binary. Use runtime feature detection for portability (`opt-target-cpu`).
- **Cross-crate inlining needs `#[inline]` or LTO.** A non-`#[inline]` function's body isn't
  available to other crates, so it won't inline across the boundary without LTO (`opt-inline-small`).
- **Struct field order changes size.** `{u8, u64, u8}` is 24 B (padding); `{u64, u8, u8}` is 16 B.
  Order large→small (`mem-smaller-integers`).
- **`mem::take` needs `T: Default`.** For non-`Default` types use `mem::replace` with an explicit
  value, or make the field `Option<T>` and use `Option::take` (`mem-take-replace`).
- **`collect_into` is nightly-only.** On stable use `buf.extend(iter)` for the same buffer reuse
  (`perf-collect-into`).
- **Bounds-check elimination is never guaranteed.** Iterator patterns *enable* it; verify hot code
  in generated assembly rather than assuming (`opt-bounds-check`).

---

## Cheat-sheet

### Ownership / sharing decision

| Need | Reach for |
|------|-----------|
| Read only | `&T` / `&[T]` / `&str` |
| Mutate in place, no ownership | `&mut T` |
| Own it (store/thread/async) | move, or `.clone()` if source still needed |
| Borrow-or-own return | `Cow<'a, T>` |
| Shared, single-thread | `Rc<T>` (+ `RefCell`/`Cell` for mutation) |
| Shared, multi-thread | `Arc<T>` (+ `Mutex`/`RwLock`) |
| Move out of `&mut` | `mem::take` / `mem::replace` / `Option::take` |

### Collection choice

| Situation | Type |
|-----------|------|
| Unbounded, grows | `Vec<T>` (pre-size with `with_capacity`) |
| Fixed after build, many instances | `Box<[T]>` |
| Usually small, sometimes large | `SmallVec<[T; N]>` |
| Hard cap, no heap allowed | `ArrayVec<T, N>` |
| Often empty, many instances | `ThinVec<T>` |
| Many short strings | `CompactString` / `EcoString` |
| Insert-or-update a map | `entry().or_insert*()` |
| Trusted integer keys, hot map | `FxHashMap` / `AHashMap` |

### Avoid-allocation toolkit

| Instead of | Use |
|------------|-----|
| `.clone()` to read | `&T` |
| `x = y.clone()` in a loop | `x.clone_from(&y)` |
| `format!` constant | `&'static str` / `Cow::Borrowed` |
| `format!` in loop | `write!` into reused `String` |
| new `Vec` per iteration | hoist + `clear()` + `extend` |
| `.collect()` then `.len()`/`.is_empty()` | `.count()` / `.any(..)` |
| `push` loop | `extend` / `extend_from_slice` |
| `.to_string()`/`.to_vec()` to slice | slice `&s[..]` / `&v[..]` |

### Release-profile knobs (production binary)

| Knob | Value | Effect |
|------|-------|--------|
| `opt-level` | `3` (or `"s"`/`"z"` for size) | optimization vs size |
| `lto` | `"fat"` | cross-crate inline/DCE, +10–20% |
| `codegen-units` | `1` | whole-crate opt, +5–20% |
| `panic` | `"abort"` | no unwind tables, smaller |
| `strip` | `true` | drop symbols |
| PGO | `-Cprofile-generate`→`-Cprofile-use` | +10–30% |
| `target-cpu` | `native` / specific | SIMD, non-portable |

### Inline / cold annotations

| Attribute | Apply to |
|-----------|----------|
| (none) | most functions — trust the compiler |
| `#[inline]` | small hot public/generic fns (cross-crate) |
| `#[inline(always)]` | benchmark-proven tiny hot fns only |
| `#[inline(never)]` + `#[cold]` | error/panic/fallback construction |

### Profiling tools

| Tool | Use |
|------|-----|
| `cargo flamegraph` | where time goes (start here) |
| `perf` (Linux) / `cargo instruments` (macOS) | sampling profiler |
| `dhat` | heap allocation profile |
| `criterion` (+ `black_box`) | micro-benchmarks |
| `cargo asm` / `cargo-show-asm` | verify inlining / BCE |
| `valgrind --tool=cachegrind` | cache behavior |
