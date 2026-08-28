# Concurrency, Channels & Shared State

Engineering patterns for multi-threaded Rust: threads vs concurrency vs parallelism, message passing with
channels (mpsc / crossbeam), and shared state via smart pointers, interior mutability, atomics, and locks.
Consult this before writing or reviewing any code that spawns threads, shares data between them, or picks a
synchronization primitive. **Async (`.await`, tokio, futures) is a separate file — see `async-await.md`.** For
API-shape decisions (naming a `Sender` newtype, `#[must_use]` on guards, sealed traits) cross-link
`api-guidelines.md` and `microsoft-guidelines.md`; for the actor/RAII idioms see `design-patterns.md`.

---

## 1. Concurrency ≠ Parallelism ≠ Threads

Three distinct concepts, routinely conflated (Patterns Book ch06 §Terminology):

| | Concurrency | Parallelism |
|---|---|---|
| **Definition** | Managing multiple tasks that can make progress | Executing multiple tasks *simultaneously* |
| **Hardware** | One core is enough | Requires multiple cores |
| **Analogy** | One cook switching between dishes | Multiple cooks, one dish each |
| **Rust tools** | `async/await`, channels, `select!` | `rayon`, `thread::spawn`, `par_iter()` |

A **thread** is the OS-level unit of execution that *enables* parallelism. Rust threads map **1:1 to OS
threads**, each with its own stack (typically 2–8 MB). Concurrency is about *structure* (decoupling
independent tasks); parallelism is about *execution* (running them at once). Async gives you concurrency
without threads; rayon gives you parallelism without manual thread management.

**Decision:** CPU-bound collection work → `rayon` (§7). I/O-bound concurrency → `async`/`tokio`
(`async-await.md`). Long-running background/I/O worker → `thread::spawn`. Short parallel tasks borrowing
local data → `thread::scope` (§6).

---

## 2. `std::thread` — OS Threads

`thread::spawn` takes a closure and returns a `JoinHandle<T>`; `.join()` blocks and yields the closure's
return value (or the panic payload as `Err`). The closure must be **`Send + 'static`** and is `FnOnce`.

```rust
use std::thread;
use std::time::Duration;

fn main() {
    let handle = thread::spawn(|| {
        for i in 0..5 {
            println!("spawned: {i}");
            thread::sleep(Duration::from_millis(100));
        }
        42 // return value
    });

    for i in 0..3 {
        println!("main: {i}");
    }

    let result = handle.join().unwrap(); // unwrap propagates a child panic
    println!("Thread returned: {result}");
}
```

**The `'static` bound and `move`** (Patterns Book ch06 §Thread::spawn type requirements):

```rust
use std::thread;
let data = vec![1, 2, 3];

// ❌ WRONG — closure borrows `data`, which is not 'static
// thread::spawn(|| println!("{data:?}"));

// ✅ RIGHT — move ownership into the thread
thread::spawn(move || println!("{data:?}"));
// `data` is no longer accessible on this thread
```

- `Send` — the closure and its captures can be transferred to another thread.
- `'static` — the closure may outlive the spawning frame, so it can't borrow non-`'static` locals.
- Escape hatch for the `'static` requirement without heap/`Arc`: **scoped threads** (§6).

---

## 3. Channels & Message Passing (Patterns Book ch05)

Message passing serializes access to data by *moving* it between threads — "share memory by communicating"
rather than communicating by sharing memory. Prefer it when state has complex invariants or operations are
long-running; prefer shared memory (§4–5) for short critical sections. See the decision rule in §9.

### 3.1 `std::sync::mpsc` — the standard channel

**M**ulti-**p**roducer, **s**ingle-**c**onsumer. `tx.clone()` for multiple producers; the receiver iterator
ends when **all** senders are dropped.

```rust
use std::sync::mpsc;
use std::thread;

fn main() {
    let (tx, rx) = mpsc::channel();

    let tx1 = tx.clone(); // clone for a second producer
    thread::spawn(move || {
        for i in 0..5 {
            tx1.send(format!("p1: {i}")).unwrap();
        }
    });
    thread::spawn(move || {
        for i in 0..5 {
            tx.send(format!("p2: {i}")).unwrap(); // original tx moved here
        }
    });

    // rx iterator terminates once ALL senders are dropped
    for msg in rx {
        println!("Received: {msg}");
    }
}
```

- **Unbounded by default** — can exhaust memory if the consumer is slow.
- `mpsc::sync_channel(N)` → **bounded** channel; `send()` blocks when full (backpressure).
- `rx.recv()` blocks; `rx.try_recv()` returns `Err(TryRecvError::Empty)` immediately if nothing's ready.
- `.send()` returns `Err(SendError)` if the receiver is gone — handle it, don't blanket-`unwrap` in
  production (`anti-unwrap-abuse`).
- Closing rule: **drop every `Sender`** (including the original) or `rx` iteration never terminates.

### 3.2 `crossbeam-channel` — the production workhorse

Faster than `std::sync::mpsc` and supports **MPMC** (multi-consumer), which `std::mpsc` cannot.

```rust,ignore
// Cargo.toml: crossbeam-channel = "0.5"
use crossbeam_channel::bounded;
use std::thread;

fn main() {
    let (tx, rx) = bounded::<String>(100); // bounded MPMC

    for id in 0..4 { // fan-out: multiple producers
        let tx = tx.clone();
        thread::spawn(move || {
            for i in 0..10 { tx.send(format!("w{id}: {i}")).unwrap(); }
        });
    }
    drop(tx); // drop original so the channel can close

    let rx2 = rx.clone();
    let c1 = thread::spawn(move || while let Ok(m) = rx.recv() { println!("[c1] {m}"); });
    let c2 = thread::spawn(move || while let Ok(m) = rx2.recv() { println!("[c2] {m}"); });
    c1.join().unwrap();
    c2.join().unwrap();
}
```

### 3.3 `select!` — listen on multiple channels

Like Go's `select`; crossbeam **randomizes** ready branches to avoid starvation. Combine work channels with
`tick` (periodic) and `after` (one-shot timeout).

```rust,ignore
use crossbeam_channel::{bounded, tick, after, select};
use std::time::Duration;

let (work_tx, work_rx) = bounded::<String>(10);
let ticker = tick(Duration::from_secs(1));
let deadline = after(Duration::from_secs(10));
drop(work_tx);

loop {
    select! {
        recv(work_rx) -> msg => match msg {
            Ok(job) => println!("Processing: {job}"),
            Err(_)  => break, // channel closed
        },
        recv(ticker)   -> _ => println!("heartbeat"),
        recv(deadline) -> _ => break, // timeout
    }
}
```

### 3.4 Bounded vs unbounded & backpressure

| Type | When full | Memory | Use case |
|------|-----------|--------|----------|
| **Unbounded** | never blocks (grows heap) | unbounded ⚠️ | rare — only if producer provably ≤ consumer |
| **Bounded(N)** | `send()` blocks until space | fixed | **production default** — prevents OOM |
| **Rendezvous** `bounded(0)` | `send()` blocks until a receiver takes it | none | precise synchronization / handoff |

**Rule:** always use **bounded** channels in production unless you can prove the producer never outpaces the
consumer. A bounded channel turns "slow consumer" from an OOM into natural backpressure.

### 3.5 Worker pool (fan-out / fan-in)

Dispatch `Job`s to N workers, collect `JobResult`s. With `std::mpsc`, share the single receiver behind
`Arc<Mutex<Receiver>>` (crossbeam avoids the mutex — `Receiver` is `Clone`). (Patterns Book ch05 Exercise)

```rust
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

fn worker_pool(jobs: Vec<String>, num_workers: usize) -> Vec<String> {
    let (job_tx, job_rx) = mpsc::channel::<String>();
    let (res_tx, res_rx) = mpsc::channel::<String>();
    let job_rx = Arc::new(Mutex::new(job_rx)); // shared work queue

    let mut handles = Vec::new();
    for wid in 0..num_workers {
        let job_rx = Arc::clone(&job_rx);
        let res_tx = res_tx.clone();
        handles.push(thread::spawn(move || loop {
            // Lock only to pull one job, then release before processing
            let job = { job_rx.lock().unwrap().recv() };
            match job {
                Ok(data) => res_tx.send(format!("{data} by w{wid}")).unwrap(),
                Err(_) => break, // all job senders dropped
            }
        }));
    }
    drop(res_tx); // so res_rx iteration can end

    let n = jobs.len();
    for j in jobs { job_tx.send(j).unwrap(); }
    drop(job_tx); // signal workers to finish

    let results: Vec<_> = res_rx.into_iter().collect();
    assert_eq!(results.len(), n);
    for h in handles { h.join().unwrap(); }
    results
}
```

### 3.6 Actor pattern — serialize state without a mutex

An actor owns its state on one thread and processes messages from a channel. Callers hold a cheap,
`Clone`-able **handle** (which is `Send + Sync`). Request/reply uses a one-shot reply channel embedded in the
message. (Patterns Book ch05 §Actor Pattern; see `design-patterns.md` for the newtype handle idiom.)

```rust
use std::sync::mpsc;
use std::thread;

enum CounterMsg {
    Increment,
    Get(mpsc::Sender<i64>), // reply channel travels in the message
}

struct CounterActor { count: i64, rx: mpsc::Receiver<CounterMsg> }
impl CounterActor {
    fn run(mut self) {
        while let Ok(msg) = self.rx.recv() {
            match msg {
                CounterMsg::Increment => self.count += 1,
                CounterMsg::Get(reply) => { let _ = reply.send(self.count); }
            }
        }
    }
}

#[derive(Clone)]
struct Counter { tx: mpsc::Sender<CounterMsg> } // handle: Send + Sync + cheap Clone
impl Counter {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || CounterActor { count: 0, rx }.run());
        Counter { tx }
    }
    fn increment(&self) { let _ = self.tx.send(CounterMsg::Increment); }
    fn get(&self) -> i64 {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx.send(CounterMsg::Get(reply_tx)).unwrap();
        reply_rx.recv().unwrap()
    }
}
```

**Actors vs mutexes:** actors win when state has complex invariants, operations are long, or you want to
sidestep lock-ordering entirely. Mutexes are simpler for short critical sections.

---

## 4. Smart Pointers — Ownership & Sharing (Patterns Book ch09)

| Pointer | Owners | Thread-safe | Mutability | Use when |
|---------|:------:|:-----------:|:----------:|----------|
| `Box<T>` | 1 | ✅ if `T: Send` | via `&mut` | heap alloc, trait objects, recursive types |
| `Rc<T>` | N | ❌ | none (wrap in `Cell`/`RefCell`) | shared ownership, **single thread**, graphs/trees |
| `Arc<T>` | N | ✅ | none (wrap in `Mutex`/`RwLock`) | shared ownership **across threads** |
| `Cell<T>` | — | ❌ | `.get()`/`.set()` | interior mutability for `Copy` types |
| `RefCell<T>` | — | ❌ | `.borrow()`/`.borrow_mut()` | interior mutability, any type, single thread |
| `Cow<'_,T>` | 0 or 1 | ✅ if `T: Send` | clone on write | avoid alloc when data usually unchanged |

```rust
use std::rc::Rc;
use std::sync::Arc;

// Box — single owner; required for recursive types (else infinite size)
enum List { Cons(i32, Box<List>), Nil }
let _boxed: Box<dyn std::fmt::Debug> = Box::new(42); // trait object

// Rc — reference-counted, single-threaded. Clone bumps the count, NOT a deep copy.
let a = Rc::new(vec![1, 2, 3]);
let _b = Rc::clone(&a);
let _c = Rc::clone(&a);
assert_eq!(Rc::strong_count(&a), 3);

// Arc — atomic refcount, safe to move clones across threads
let shared = Arc::new(String::from("data"));
let handles: Vec<_> = (0..3).map(|_| {
    let s = Arc::clone(&shared);
    std::thread::spawn(move || println!("{s}"))
}).collect();
for h in handles { h.join().unwrap(); }
```

**`Rc` vs `Arc`:** `Rc` uses non-atomic counts (cheaper, `!Send`/`!Sync`); `Arc` uses atomic counts (thread
-safe, slightly costlier). Use `Rc` by default in single-threaded code; the compiler forces `Arc` the moment
you cross a thread boundary. Never reach for `Arc` "just in case" — it's a measurable cost.

### 4.1 `Weak` — break reference cycles

`Rc`/`Arc` can't free cycles (A→B→A leaks). `Weak<T>` is a non-owning handle that does **not** raise the
strong count; `.upgrade()` yields `Option<Rc<T>>` (`None` if freed). **Rule:** ownership edges use
`Rc`/`Arc`; back-references and caches use `Weak`.

```rust
use std::rc::{Rc, Weak};
use std::cell::RefCell;

struct Node {
    value: i32,
    parent: RefCell<Weak<Node>>,       // Weak: does NOT keep parent alive
    children: RefCell<Vec<Rc<Node>>>,  // Rc: owns children
}

let parent = Rc::new(Node { value: 0, parent: RefCell::new(Weak::new()), children: RefCell::new(vec![]) });
let child  = Rc::new(Node { value: 1, parent: RefCell::new(Rc::downgrade(&parent)), children: RefCell::new(vec![]) });
parent.children.borrow_mut().push(Rc::clone(&child));

if let Some(p) = child.parent.borrow().upgrade() {
    assert_eq!(p.value, 0);
}
```

---

## 5. Interior Mutability & Shared State

Interior mutability = mutating through a shared (`&`) reference. Single-threaded: `Cell`/`RefCell`.
Multi-threaded: `Mutex`/`RwLock`/atomics.

### 5.1 `Cell` and `RefCell` (single-threaded)

- **`Cell<T>`** — `get`/`set`/`replace`/`take`. Never panics; only `Copy` types (or swap-in/out). Cheapest.
- **`RefCell<T>`** — runtime-checked borrows via `borrow()`/`borrow_mut()`. Works with any type but
  **panics at runtime** on a borrow-rule violation (e.g. `borrow_mut` while a `borrow` is live). Neither is
  `Sync`.

```rust
use std::cell::{Cell, RefCell};

struct Counter { count: Cell<u32> }        // Cell for a Copy counter
impl Counter {
    fn increment(&self) { self.count.set(self.count.get() + 1); } // &self, not &mut self!
}

struct Cache { data: RefCell<Vec<String>> } // RefCell for a non-Copy Vec
impl Cache {
    fn add(&self, item: String) { self.data.borrow_mut().push(item); }
    // ❌ holding two conflicting borrows PANICS at runtime:
    // let _r = self.data.borrow(); let _w = self.data.borrow_mut();
}
```

### 5.2 `Mutex`, `RwLock`, `Condvar` (multi-threaded)

Wrap shared mutable state as `Arc<Mutex<T>>` (exclusive) or `Arc<RwLock<T>>` (many readers **or** one
writer). `.lock()`/`.read()`/`.write()` return a `Result` because a lock can be **poisoned** (§8). The guard
releases the lock on `Drop` — scope it tightly. (Patterns Book ch06 §Shared State)

```rust
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

// Arc<Mutex<T>> — exclusive access, short critical sections
let counter = Arc::new(Mutex::new(0u64));
let mut handles = vec![];
for _ in 0..10 {
    let counter = Arc::clone(&counter);
    handles.push(thread::spawn(move || {
        for _ in 0..1000 {
            let mut g = counter.lock().unwrap(); // Err only if poisoned
            *g += 1;
        } // guard dropped → lock released each iteration
    }));
}
for h in handles { h.join().unwrap(); }
assert_eq!(*counter.lock().unwrap(), 10_000);

// Arc<RwLock<T>> — read-heavy, rare writes
let config = Arc::new(RwLock::new(String::from("initial")));
{ *config.write().unwrap() = "updated".into(); }   // exclusive
let _snapshot = config.read().unwrap().clone();    // shared
```

**`Condvar`** — wait for a condition without busy-looping; always paired with a `Mutex`. `wait()` atomically
unlocks and sleeps. **Always re-check the predicate in a `while` loop** — spurious wakeups are permitted.

```rust
use std::sync::{Arc, Mutex, Condvar};

let pair = Arc::new((Mutex::new(false), Condvar::new()));
let p2 = Arc::clone(&pair);
let h = std::thread::spawn(move || {
    let (lock, cvar) = &*p2;
    let mut ready = lock.lock().unwrap();
    while !*ready {                       // while-loop guards against spurious wakeups
        ready = cvar.wait(ready).unwrap();
    }
});
{
    let (lock, cvar) = &*pair;
    *lock.lock().unwrap() = true;
    cvar.notify_one(); // notify_all() to wake every waiter
}
h.join().unwrap();
```

**Primitive comparison:**

| Primitive | Use case | Cost | Contention |
|-----------|----------|------|------------|
| `Mutex<T>` | short critical sections | lock/unlock | threads queue |
| `RwLock<T>` | read-heavy, rare writes | reader-writer lock | readers concurrent, writer exclusive |
| atomics | counters, flags | hardware CAS | lock-free, no waiting |
| channels | producer/consumer handoff | queue ops | decoupled |

> `parking_lot::{Mutex, RwLock}` are faster, smaller, and **non-poisoning** (no `Result` on lock) — a common
> production swap. Their guards are `!Send`.

### 5.3 `Cow` — Clone on Write

Holds `Borrowed` or `Owned`; clones **only** when mutation is needed. Ideal for the common "usually no
transformation" path (normalization, padding, escaping). See `anti-clone-excessive`, `anti-string-for-str`.

```rust
use std::borrow::Cow;

fn normalize(input: &str) -> Cow<'_, str> {
    if input.contains('\t') {
        Cow::Owned(input.replace('\t', "    ")) // allocate only when needed
    } else {
        Cow::Borrowed(input)                    // zero allocation on the fast path
    }
}
fn pad(frame: &[u8], min: usize) -> Cow<'_, [u8]> {
    if frame.len() >= min { Cow::Borrowed(frame) }
    else { let mut v = frame.to_vec(); v.resize(min, 0); Cow::Owned(v) }
}
// .into_owned() clones only if currently Borrowed.
```

### 5.4 Lazy globals: `OnceLock` / `LazyLock`

Replace `lazy_static!` / `once_cell` with std. (Patterns Book ch06 §Lazy Initialization)

| Type | Since | Init timing | Use when |
|------|-------|-------------|----------|
| `OnceLock<T>` | 1.70 | call-site (`get_or_init`) | init depends on runtime args |
| `LazyLock<T>` | 1.80 | definition-site (closure) | init is self-contained |
| `const fn` + `static` | always | compile-time | value is const-computable |

```rust
use std::sync::{OnceLock, LazyLock};
use std::collections::HashMap;

static CONFIG: OnceLock<HashMap<String, String>> = OnceLock::new();
fn config() -> &'static HashMap<String, String> {
    CONFIG.get_or_init(|| HashMap::from([("log".into(), "info".into())]))
}

static TABLE: LazyLock<Vec<u32>> = LazyLock::new(|| (0..100).map(|x| x * x).collect());
fn square(i: usize) -> u32 { TABLE[i] }
```

> **Migration:** `lazy_static! { static ref X: T = e; }` → `static X: LazyLock<T> = LazyLock::new(|| e);`
> Same semantics, no macro, no dependency.

---

## 6. Scoped Threads — borrow local data (`conc-scoped-threads`)

`std::thread::scope` (stable 1.63) guarantees **all spawned threads join before the scope returns** (even on
panic), so they may borrow non-`'static` stack data — no `Arc`, no cloning, no heap. A panicking child makes
`scope` itself panic after joining the rest.

```rust
use std::thread;

// ❌ Arc + clone just to share a slice — heap churn and boilerplate
// let data = Arc::new(data.to_vec()); let d1 = Arc::clone(&data); ...

// ✅ borrow directly; scope proves all threads finish first
fn parallel_sum(data: &[i64]) -> i64 {
    let (left, right) = data.split_at(data.len() / 2);
    thread::scope(|s| {
        let h1 = s.spawn(|| left.iter().sum::<i64>());
        let h2 = s.spawn(|| right.iter().sum::<i64>());
        h1.join().unwrap() + h2.join().unwrap()
    })
}

// Disjoint mutable borrows are fine — as long as they don't alias
fn parallel_fill(left: &mut [u8], right: &mut [u8]) {
    thread::scope(|s| {
        s.spawn(|| left.fill(0xAA));
        s.spawn(|| right.fill(0xBB));
    });
}
```

Use for a **fixed number of distinct short-lived sub-tasks** that share read-only or non-overlapping mutable
local refs, or when you need thread control rayon doesn't expose. For homogeneous collection processing,
prefer rayon (§7). When data genuinely outlives the parallel task, use `Arc` (§4).

---

## 7. Data Parallelism with rayon (`conc-rayon-par-iter`)

Rayon's work-stealing scheduler parallelizes CPU-bound iterators, often a one-word change `.iter()` →
`.par_iter()`. **CPU-bound only** — for I/O use async; rayon threads *block*.

```rust,ignore
use rayon::prelude::*; // enables .par_iter() on slices & most collections

fn sum_squares(data: &[f64]) -> f64 {
    data.par_iter().map(|x| x * x).sum()
}
fn normalize(data: &mut [f64]) {
    let max = data.par_iter().cloned().reduce(|| f64::NEG_INFINITY, f64::max);
    data.par_iter_mut().for_each(|x| *x /= max);
}
fn sort_large(data: &mut [f64]) {
    data.par_sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
}
```

| Concern | Guidance |
|---------|----------|
| I/O-bound work | use async, **not** rayon (its threads block) |
| Small collections | sequential is often faster (thread-spawn overhead) — profile first |
| Granularity | tune with `.with_min_len()` / `.with_max_len()` |
| Shared state | rayon does **not** prevent data races — use `Mutex`/atomics for shared mutation |
| Sweet spot | large collections, per-element work ≥ a few hundred ns |

**rayon vs threads vs scope:** `par_iter` for homogeneous collection work; `thread::spawn` for long-running
background/I/O workers; `thread::scope` for a fixed set of distinct short tasks borrowing local data.

---

## 8. Atomics & Memory Ordering (`conc-atomic-ordering`)

Atomics (`AtomicBool`, `AtomicUsize`, `AtomicU64`, …) are lock-free for simple values. Every operation takes
an `Ordering`. **Use the weakest correct ordering** — defaulting to `SeqCst` is correct-but-costly on
ARM/RISC-V (full barriers), while the *wrong* ordering is a data race the compiler can't catch.

```rust
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};

// Relaxed — atomic, but no ordering vs other memory. Independent counters/stats.
static COUNTER: AtomicU64 = AtomicU64::new(0);
fn bump() { COUNTER.fetch_add(1, Ordering::Relaxed); }

// Acquire/Release — paired handoff. Producer writes payload THEN publishes (Release);
// consumer loads the flag (Acquire) and is guaranteed to see the payload.
static READY: AtomicBool = AtomicBool::new(false);
static VALUE: AtomicU64 = AtomicU64::new(0);
fn producer(v: u64) {
    VALUE.store(v, Ordering::Relaxed);    // 1. write payload
    READY.store(true, Ordering::Release); // 2. publish
}
fn consumer() -> Option<u64> {
    if READY.load(Ordering::Acquire) {        // synchronizes-with the Release
        Some(VALUE.load(Ordering::Relaxed))   // payload now visible
    } else { None }
}
```

| Ordering | Use when |
|----------|----------|
| `Relaxed` | atomic but no ordering needed (counters, stats) |
| `Acquire` | load that must see all stores before a matching `Release` |
| `Release` | store that must be visible before a matching `Acquire` load |
| `AcqRel` | read-modify-write (`fetch_add`, `compare_exchange`) — both Acquire & Release |
| `SeqCst` | need one global order across **multiple** atomics (e.g. Dekker-style mutual exclusion) |

**CAS spin loop** — `compare_exchange_weak` + `std::hint::spin_loop()`:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
fn lock(locked: &AtomicBool) {
    while locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::hint::spin_loop();
    }
}
```

> **Verification:** use the `loom` crate to exhaustively explore all interleavings the C11 model permits for
> small concurrent units. **Don't hand-roll lock-free structures** — reach for `crossbeam`, `arc-swap`,
> `dashmap` (Patterns Book ch06 §Lock-Free Patterns).

---

## 9. Send & Sync; Message-Passing vs Shared-Memory

**Marker traits** (auto-derived; the compiler infers them):

- **`Send`** — a value can be *moved* to another thread. Almost everything is `Send`; exceptions: `Rc`,
  `RefCell` guards, raw pointers, `MutexGuard`/`RwLockReadGuard` (never `Send`).
- **`Sync`** — `&T` can be *shared* across threads (i.e. `T: Sync ⇔ &T: Send`). `Mutex<T>`/`RwLock<T>` are
  `Sync` when `T: Send`; `Rc`, `Cell`, `RefCell` are **not** `Sync`.
- `Rc` is `!Send + !Sync` → the compiler rejects it across threads; use `Arc`. `RefCell` is `Send` (if `T:
  Send`) but `!Sync` → use `Mutex`/`RwLock` for shared mutation.
- You almost never implement these manually. `unsafe impl Send/Sync` only for hand-audited primitives
  (e.g. the `SeqLock` in Patterns Book ch06) — get it wrong and you've silently introduced UB.

**Choosing the model:**

| Prefer message passing (channels/actors) | Prefer shared memory (`Arc<Mutex/RwLock>`, atomics) |
|------------------------------------------|-----------------------------------------------------|
| Complex invariants / long operations | Short, simple critical sections |
| Want to serialize access without lock-ordering reasoning | Read-heavy shared config (`RwLock`) |
| Producer/consumer, pipelines, fan-out/fan-in | A single counter or flag (atomics) |
| Ownership naturally *moves* with the data | Many threads read the same large structure |

Patterns Book decision flow: *no shared mutable state* → channels; *read-heavy* → `RwLock`; *short critical
section* → `Mutex`; *simple counter/flag* → atomics; *complex state* → actor + channels.

---

## 10. Thread-Local State (`conc-thread-local`)

Prefer `thread_local!` with `Cell`/`RefCell` over `static mut`. `static mut` needs `unsafe` on every access
and is UB under concurrent access — **in Rust 2024 taking a reference to a `static mut` is a hard error**
(`static_mut_refs`). Thread-locals give each thread an independent copy via safe APIs, no synchronization.

```rust
use std::cell::RefCell;
thread_local! {
    static BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(4096));
}
fn append(data: &[u8]) { BUFFER.with_borrow_mut(|b| b.extend_from_slice(data)); }
fn flush() -> Vec<u8>  { BUFFER.with_borrow_mut(|b| std::mem::take(b)) }

use std::cell::Cell;
thread_local! { static CALLS: Cell<u32> = Cell::new(0); } // Cell for Copy types
fn record() { CALLS.with(|c| c.set(c.get() + 1)); }
```

- `with_borrow` / `with_borrow_mut` (stable 1.73) beat the older `with(|v| v.borrow_mut())`.
- Thread-local destructors run at thread exit. Value is strictly per-thread — never expect it to be visible
  to other threads. Use for scratch buffers, per-thread caches, reused allocations.

---

## 11. Drop Ordering & Lifecycle (Patterns Book ch09)

Deterministic drop order matters when fields hold interdependent resources (a `Sender` and a `JoinHandle`).

| What | Drop order | Why |
|------|-----------|-----|
| Local variables | **reverse** declaration order | later locals may reference earlier |
| Struct fields | declaration order (top→bottom, RFC 1857) | matches construction |
| Tuple elements | left→right | — |

**Practical impact:** if a struct holds a `Sender` and a `JoinHandle` where the thread reads from the
channel, put `Sender` **above** `JoinHandle` so the channel closes (thread exits) before you join — otherwise
`drop` deadlocks.

- **`ManuallyDrop<T>`** suppresses automatic drop (union fields, two-phase commit, controlled leaks); you
  must `unsafe { ManuallyDrop::drop(&mut x) }` yourself. In safe app code you almost never need it.
- **`Box::leak`** is the idiomatic way to get a `&'static` from a runtime value (controlled leak for
  long-lived singletons).

**`Pin`** prevents a value from moving — needed for self-referential types and manually-implemented
`Future`s. Normal `async/await` handles pinning transparently; wrapping another `Future`/`Stream` → use the
`pin-project` crate rather than `unsafe` projections. (Detailed `Pin` coverage lives in `async-await.md`.)

---

## Rules & anti-patterns checklist

- **`conc-scoped-threads`** — DO use `thread::scope` to borrow local stack data across short-lived threads;
  DON'T `Arc::new(data.to_vec())` just to share a slice. Scope guarantees join-before-return, so borrows are
  sound without heap allocation.
- **`conc-rayon-par-iter`** — DO use `par_iter()` for CPU-bound data parallelism; DON'T use rayon for I/O
  (its threads block — use async). Profile small collections; thread-spawn overhead can lose to sequential.
- **`conc-atomic-ordering`** — DO use the weakest correct `Ordering`; DON'T slap `SeqCst` on everything
  (costly on ARM/RISC-V) nor pick a too-weak one (silent data race). Pair `Release` stores with `Acquire`
  loads; reserve `SeqCst` for a total order across multiple atomics.
- **`conc-thread-local`** — DO use `thread_local!` + `Cell`/`RefCell` (via `with_borrow_mut`); DON'T use
  `static mut` (UB + hard error in 2024 edition). Each thread gets its own safe, unsynchronized copy.
- **`anti-unwrap-abuse`** — DON'T blanket-`unwrap()` `send`/`recv`/`lock` in production. `send` fails when
  the receiver is gone; `lock` fails on poisoning — handle these paths (see §8 on poisoning recovery).
- **`anti-clone-excessive`** — DON'T `.clone()` large data to hand to a thread when a scoped borrow or `Arc`
  clone (cheap refcount bump) suffices. `Cow` (§5.3) avoids allocation on the unchanged path.
- **`anti-lock-across-await`** / **`async-no-lock-await`** — (async, see `async-await.md`) DON'T hold a
  `std::sync::MutexGuard` across `.await`; it isn't `Send` and risks deadlock. Cross-linked here because the
  sync-vs-async lock choice is easy to conflate.
- **`anti-premature-optimize`** — DON'T hand-roll lock-free structures or spinlocks before profiling proves
  lock contention. Use `Mutex`/`RwLock` first; reach for `crossbeam`/`arc-swap`/`dashmap` when measured.

---

## Gotchas / footguns

- **Channel never closes.** `rx` iteration / `recv()` only ends when **every** `Sender` (including the
  original and every clone) is dropped. Forgetting `drop(tx)` after cloning into workers hangs the consumer
  forever. In the worker-pool, drop both `job_tx` and the extra `res_tx`.
- **`RefCell` double-borrow panics at runtime**, not compile time. A `borrow_mut()` while any `borrow()` is
  live aborts the thread. Keep borrow scopes tiny; never hold a `Ref`/`RefMut` across a call that might
  re-enter.
- **Mutex poisoning.** If a thread panics while holding a `Mutex`/`RwLock`, the lock is *poisoned* and later
  `.lock()` returns `Err(PoisonError)`. Recover with `.lock().unwrap_or_else(|e| e.into_inner())` when the
  data is still consistent, or propagate. `parking_lot` locks don't poison.
- **Deadlock via lock ordering.** Two threads acquiring locks A→B and B→A deadlock. Always acquire multiple
  locks in a single global order, or restructure to hold one lock at a time. Actors (§3.6) sidestep this.
- **Holding a guard too long serializes everything.** `let g = m.lock().unwrap();` then doing slow work
  under the guard turns a "parallel" program sequential. Lock → copy/mutate → drop guard → do slow work.
- **`RwLock` writer starvation.** A stream of readers can starve a writer (platform-dependent). For
  write-heavy or contended locks, a `Mutex` is often simpler and fairer.
- **`bounded(0)` is a rendezvous, not "no buffering-but-async".** `send` blocks until a receiver actively
  takes the item — great for handoff sync, a deadlock trap if you expected fire-and-forget.
- **`SeqLock` / `UnsafeCell` non-atomic writes** are technically a data race under Rust's abstract machine
  even when the protocol makes readers retry; sound only for machine-word `Copy` types on real hardware.
  Prefer `AtomicU64` or a `Mutex` (Patterns Book ch06 caveat).
- **`Arc<Mutex<T>>` ≠ free lunch.** It's shared *mutable* state with a lock. If contention is high, an actor
  (message passing) or sharding the data often scales better.
- **`Weak` back-references must be upgraded before use** and may be `None` — never assume the target is
  alive. Cycles built purely from `Rc`/`Arc` leak silently (no crash, just unfreed memory).
- **Spurious wakeups.** Always re-check a `Condvar` predicate in a `while` loop, never a single `if`.
- **Struct field order = drop order.** A `JoinHandle` dropped before its `Sender` closes can deadlock. Put
  the `Sender` first (§11).

---

## Cheat-sheet

| Need | Reach for | Notes |
|------|-----------|-------|
| Spawn a long-running/background thread | `thread::spawn(move \|\| …)` | closure must be `Send + 'static` |
| Parallel task borrowing local data | `thread::scope(\|s\| s.spawn(…))` | auto-joins, no `Arc` (`conc-scoped-threads`) |
| Parallel map/filter/reduce over a collection | `rayon` `par_iter()` | CPU-bound only (`conc-rayon-par-iter`) |
| Move values between threads | channel (`mpsc` / `crossbeam`) | bounded in production (§3.4) |
| Multi-consumer channel / `select!` | `crossbeam-channel` | `std::mpsc` is single-consumer |
| Serialize access to complex state | actor + channel (§3.6) | no lock-ordering to reason about |
| Shared exclusive mutable state | `Arc<Mutex<T>>` | short critical sections; `parking_lot` = faster, no poison |
| Read-heavy shared state | `Arc<RwLock<T>>` | many readers OR one writer |
| Lock-free counter/flag | `AtomicU64` / `AtomicBool` | weakest correct `Ordering` (`conc-atomic-ordering`) |
| Wait for a condition | `Condvar` + `Mutex` | re-check predicate in `while` |
| Shared ownership, one thread | `Rc<T>` (+ `Cell`/`RefCell`) | non-atomic refcount, `!Send` |
| Shared ownership, many threads | `Arc<T>` (+ `Mutex`/`RwLock`) | atomic refcount |
| Break a reference cycle | `Weak<T>` | `.upgrade() -> Option<Rc<T>>` |
| Interior mutability, `Copy`, 1 thread | `Cell<T>` | never panics |
| Interior mutability, any type, 1 thread | `RefCell<T>` | runtime-checked, panics on violation |
| Per-thread scratch state | `thread_local!` + `Cell`/`RefCell` | not `static mut` (`conc-thread-local`) |
| Avoid alloc when data usually unchanged | `Cow<'_, T>` | clones only on write |
| Lazy global, runtime-arg init | `OnceLock<T>` + `get_or_init` | stable 1.70 |
| Lazy global, self-contained init | `LazyLock<T>` | stable 1.80; replaces `lazy_static!` |
| Prevent a value from moving | `Pin<P>` | self-ref types, manual `Future`s → see `async-await.md` |

**Ordering pairing rule:** `Release` (store, publish) ↔ `Acquire` (load, observe); `AcqRel` for RMW;
`SeqCst` only for a global total order across multiple atomics; `Relaxed` for independent counters.

Cross-references: `async-await.md` (tokio channels, `Pin`, lock-across-await), `design-patterns.md` (RAII
guards, newtype handles, actor idiom), `api-guidelines.md` (`C-SEND-SYNC` audit, `#[must_use]` guards),
`microsoft-guidelines.md` (pragmatic concurrency defaults).
