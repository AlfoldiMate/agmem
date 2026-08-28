# Async / Await Essentials

Deep reference for Rust's async model — `Future`/`poll`, executors (tokio), `.await`, pinning intuition, tasks & structured concurrency, async channels, cancellation/cancel-safety, `spawn_blocking`, the never-hold-a-lock-across-await rule, backpressure, and async-fn-in-traits/bounds. Consult this before writing or reviewing any `async`/`.await` code, spawning tasks, wiring channels between tasks, or configuring a tokio runtime. Distilled from the Microsoft "Rust Patterns" book ch16 and the `rust-skills` `async-*` rule catalog. For canonical API/naming rules see **api-guidelines.md** (C-*), **microsoft-guidelines.md** (M-*), **design-patterns.md**, and **style-guide.md** — this file does not re-derive them.

---

## 1. The Async Model: `Future`, `poll`, runtimes

Rust's async is *fundamentally different* from Go goroutines or Python `asyncio`. Three facts get you started (Patterns Book ch16 §Futures, Runtimes, and `async fn`):

1. **A `Future` is a lazy state machine.** Calling an `async fn` executes *nothing*; it returns a `Future` that does work only when polled. Futures are "poll-driven," not "push-driven."
2. **You need a runtime to poll futures.** `std` defines the `Future` trait but ships no executor. Use `tokio` (production default), `async-std`, or `smol`.
3. **`async fn` is sugar.** The compiler rewrites it into a state machine implementing `Future`, where each `.await` is a suspension point.

```rust
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

// A Future is just a trait (Patterns Book ch16):
pub trait MyFuture {
    type Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output>;
}

// `async fn fetch(url: &str) -> Result<Vec<u8>, E>` desugars to roughly:
// fn fetch(url: &str) -> impl Future<Output = Result<Vec<u8>, E>>
async fn fetch_len(s: &str) -> usize {
    // `.await` yields control to the runtime until the inner future is ready
    ready(s.len()).await
}

// a trivial ready future for illustration
fn ready<T>(v: T) -> impl Future<Output = T> {
    std::future::ready(v)
}
```

`Poll` has two states: `Poll::Ready(output)` (done) and `Poll::Pending` (not yet — the future arranged to be woken later via the `Waker` in `cx`). The runtime keeps calling `poll` only after a `Waker` signals readiness, so idle futures cost nothing.

**Nothing runs until polled.** Calling an `async fn` without `.await`-ing or spawning it is a no-op — a frequent bug (the compiler warns `unused implementer of Future`).

```rust
async fn side_effect() { /* never runs */ }

async fn caller() {
    side_effect();        // WRONG: builds a Future, drops it, does nothing
    side_effect().await;  // RIGHT: actually drives it
}
```

### Pinning intuition

Async state machines can be *self-referential* (a local holds a reference to another local across an `.await`). Moving such a value in memory would dangle the internal pointer. `Pin<&mut F>` is the guarantee "this future will not be moved again," which is *why* `Future::poll` takes `self: Pin<&mut Self>`. You rarely construct `Pin` by hand — the runtime and `.await` handle it. Two escape hatches when you *do* need to hold a future across iterations:

- `std::pin::pin!(fut)` — pins to the stack (cheap, no allocation). See cancel-safety §7.
- `Box::pin(fut)` — pins to the heap; needed when the future must be `'static`/stored in a struct or `Vec<Pin<Box<dyn Future>>>`.

`Unpin` types (most concrete types; anything not self-referential) can be freely moved even when pinned. Adding `+ Unpin` bounds sidesteps pinning ceremony for generic reader/writer helpers.

---

## 2. Tokio quick-start: runtime, spawn, join

```toml
# Cargo.toml (style-guide.md for Cargo conventions)
[dependencies]
tokio = { version = "1", features = ["full"] }
```

```rust
use tokio::task;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    // Spawn concurrent tasks (lightweight, like green threads):
    let a = task::spawn(async {
        sleep(Duration::from_millis(100)).await;
        "task A done"
    });
    let b = task::spawn(async {
        sleep(Duration::from_millis(50)).await;
        "task B done"
    });

    // Await both — they run CONCURRENTLY, not sequentially:
    let (a, b) = tokio::join!(a, b);
    println!("{}, {}", a.unwrap(), b.unwrap()); // JoinHandle yields Result
}
```

`#[tokio::main]` wraps `main` in `Runtime::new().block_on(...)`. `task::spawn` returns a `JoinHandle<T>` whose output is `Result<T, JoinError>` (the `Err` means the task panicked or was aborted).

### Runtime flavors & tuning (`async-tokio-runtime`)

The default multi-threaded runtime is not always optimal; tune to the workload.

| Runtime | Use case | How |
|---|---|---|
| Multi-thread | IO-bound, many connections | `#[tokio::main]` (default) |
| Current-thread | CLI tools, tests, single connection, simpler debugging | `#[tokio::main(flavor = "current_thread")]` |
| Custom | fine-tuned perf, separate pools | `runtime::Builder::new_multi_thread()` |

```rust
use tokio::runtime::Builder;

fn main() {
    // available_parallelism() is stable (1.59+) and respects cgroup quotas —
    // prefer it over the unmaintained num_cpus crate (async-tokio-runtime).
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let rt = Builder::new_multi_thread()
        .worker_threads(cores * 2)   // IO-bound benefits from oversubscription
        .max_blocking_threads(32)    // pool for spawn_blocking (§8)
        .thread_name("io-worker")
        .enable_all()                // enable_io() + enable_time()
        .build()
        .unwrap();

    rt.block_on(async { /* ... */ });
}
```

Rules of thumb: **IO-bound** → `worker_threads` > cores (oversubscription helps); **CPU-bound** → `worker_threads == cores` (more gives no benefit). For mixed workloads, run *two* runtimes (an `io-worker` pool and a `cpu-worker` pool) so heavy compute never starves IO.

---

## 3. Concurrency vs sequential: `join!` / `try_join!`

Awaiting one future then another runs them *sequentially* (sum of durations). `join!` polls them concurrently on one task (max of durations). This is the single most common async performance win (`async-join-parallel`).

```rust
// WRONG: sequential — 300ms total
async fn fetch_slow() -> (User, Posts, Comments) {
    let user = fetch_user().await;         // 100ms
    let posts = fetch_posts().await;       // 100ms
    let comments = fetch_comments().await; // 100ms
    (user, posts, comments)
}

// RIGHT: concurrent — ~100ms total (max of the three)
async fn fetch_fast() -> (User, Posts, Comments) {
    tokio::join!(fetch_user(), fetch_posts(), fetch_comments())
}
```

`join!` runs futures on the **current task** (no `Send` requirement, no spawn overhead) — but it does *not* use multiple threads; it interleaves at `.await` points. To use multiple cores, `spawn` each future as its own task and join the handles.

### `try_join!` — fail-fast for fallible futures (`async-try-join`)

`try_join!` is `join!` that short-circuits: it returns `Err` the instant any future fails, dropping (cancelling) the rest at their next `.await`.

```rust
async fn fetch_all() -> anyhow::Result<(A, B, C)> {
    // concurrent AND fail-fast
    let (a, b, c) = tokio::try_join!(fetch_a(), fetch_b(), fetch_c())?;
    Ok((a, b, c))
}
```

Cancellation caveat: "dropped" ≠ "instantly stopped." A losing branch stops at its next suspension point; trailing cleanup code after an `.await?` may never run. For guaranteed cleanup use a `Drop` guard.

### Dynamic collections: `join_all` / `try_join_all` / bounded concurrency

`join!` needs a static list. For a runtime-sized collection use the `futures` crate:

```rust
use futures::future::{join_all, try_join_all};
use futures::stream::{self, StreamExt};

async fn fetch_users(ids: &[u64]) -> anyhow::Result<Vec<User>> {
    try_join_all(ids.iter().map(|&id| fetch_user(id))).await // fail-fast
}

// Bound concurrency (don't open 10_000 sockets at once):
async fn fetch_capped(ids: &[u64]) -> Vec<anyhow::Result<User>> {
    stream::iter(ids)
        .map(|&id| fetch_user(id))
        .buffer_unordered(10) // at most 10 in flight
        .collect()
        .await
}
```

`buffer_unordered(n)` or a `tokio::sync::Semaphore` (acquire a permit before each request) are the two idiomatic concurrency limiters. Prefer them over `join_all` on huge inputs — `join_all` pre-allocates and starts everything at once.

### `JoinSet` — structured task management (`async-joinset-structured`)

For a *dynamic set of spawned tasks*, `JoinSet` beats a `Vec<JoinHandle>` + `join_all`: add tasks dynamically, receive results **as they complete**, and — critically — **abort all tasks on drop** (structured concurrency: tasks don't outlive their owner).

```rust
use tokio::task::JoinSet;

async fn fetch_all(urls: Vec<String>) {
    let mut set = JoinSet::new();
    for url in urls {
        set.spawn(fetch(url)); // spawns onto the runtime
    }
    // results arrive out of order, as-completed
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(data)) => process(data),
            Ok(Err(e))   => eprintln!("task failed: {e}"),
            Err(join)    => eprintln!("task panicked/aborted: {join}"),
        }
    }
} // if we return early, remaining tasks are ABORTED here
```

| Feature | `JoinSet` | `join_all(Vec<JoinHandle>)` |
|---|---|---|
| Add tasks dynamically | Yes | No |
| Results as-completed | Yes | No (all at once, in order) |
| Abort-all on drop | Yes | No (handles just detach) |
| Runs on multiple threads | Yes (spawns) | Yes (spawns) |

To attach identity to out-of-order results, spawn `async move { (index, fetch(url).await) }` and match on the tuple.

---

## 4. Racing & timeouts: `select!` (`async-select-racing`)

`tokio::select!` polls several futures and returns when the **first** completes, dropping the others. Use for timeouts, cancellation, and competing alternatives.

```rust
use tokio::time::{sleep, Duration};

async fn with_timeout() -> Result<Data, Error> {
    tokio::select! {
        result = fetch_data() => result,
        _ = sleep(Duration::from_secs(5)) => Err(Error::Timeout),
    }
}
```

Prefer the purpose-built `tokio::time::timeout(dur, fut)` for the pure-timeout case (it returns `Result<T, Elapsed>`):

```rust
use tokio::time::{timeout, Duration};

async fn fetch_with_timeout() -> Result<String, Box<dyn std::error::Error>> {
    // `??`: first ? unwraps Elapsed, second ? unwraps the inner Result
    let data = timeout(Duration::from_secs(5), async {
        sleep(Duration::from_millis(100)).await;
        Ok::<_, Box<dyn std::error::Error>>("data".to_owned())
    })
    .await??;
    Ok(data)
}
```

**`select!` features:**
- **Patterns + guards:** `Some(cmd) = rx.recv(), if enabled => {…}` — a branch with a *false* guard is disabled.
- **`else` branch:** runs when all branches are disabled (e.g. every channel closed) — the idiomatic loop-exit condition.
- **`biased;`** as the first line: check branches top-to-bottom instead of the default random order. Use for strict priority (drain a shutdown signal before work).
- **Dynamic racing:** `select!` needs static branches; use `futures::future::select_all(vec_of_futures)` for a runtime-sized race.

```rust
// Canonical event loop: work until shutdown (async-select-racing)
async fn event_loop(mut cmds: tokio::sync::mpsc::Receiver<Command>,
                    shutdown: tokio_util::sync::CancellationToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            Some(cmd) = cmds.recv() => handle(cmd).await,
            else => break, // channel closed
        }
    }
}
```

> **`select!` silently drops the losing branches' futures and their in-progress state.** This is the cancel-safety footgun — see §7.

---

## 5. Async channels — pick the right one

Never use `std::sync::mpsc` in async code — its `recv()` **blocks the executor thread** (`async-mpsc-queue`). Use the `tokio::sync` family. Each channel encodes a different delivery contract:

| Channel | Producers | Consumers | Delivery | Best for |
|---|---|---|---|---|
| `mpsc` | many | **one** | every msg, backpressure | work queues, task→task |
| `oneshot` | one | one | exactly one value | request/response reply |
| `broadcast` | many | many | **every** msg to **every** sub | events, pub/sub, fan-out |
| `watch` | many | many | **latest value only** | config, status, state |

### `mpsc` — the workhorse (`async-mpsc-queue`, `async-bounded-channel`)

```rust
use tokio::sync::mpsc;

let (tx, mut rx) = mpsc::channel::<String>(100); // BOUNDED capacity 100

// multiple producers via cheap clone
for i in 0..10 {
    let tx = tx.clone();
    tokio::spawn(async move { tx.send(format!("msg {i}")).await.unwrap(); });
}
drop(tx); // drop the original so the channel closes when all clones drop

// single consumer; loop ends when ALL senders dropped
while let Some(msg) = rx.recv().await {
    println!("got {msg}");
}
```

**Always prefer bounded (`mpsc::channel(n)`) over `mpsc::unbounded_channel()`** (`async-bounded-channel`). An unbounded channel with a fast producer and slow consumer grows until OOM. A bounded channel applies **backpressure**: `tx.send().await` suspends the producer when full, throttling the whole pipeline. Sizing: start near the expected burst size, err small, then measure. Handling a full channel:

```rust
tx.send(msg).await?;                 // wait for capacity (default backpressure)
match tx.try_send(msg) { /* Full | Closed */ } // never wait; shed load
tokio::time::timeout(d, tx.send(msg)).await;    // bounded wait
let permit = tx.reserve().await?; permit.send(msg); // reserve slot, then send (infallible)
```

`reserve()` is ideal when the message is *expensive to build*: reserve capacity first, build only if a slot exists.

### `oneshot` — request/response (`async-oneshot-response`)

Single-use, no buffering — the reply half of the actor pattern. Combine with `mpsc` to build actors:

```rust
use tokio::sync::{mpsc, oneshot};

enum Cmd { Get { key: String, reply: oneshot::Sender<Option<String>> } }

async fn store(mut rx: mpsc::Receiver<Cmd>) {
    let mut map = std::collections::HashMap::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Get { key, reply } => {
                let _ = reply.send(map.get(&key).cloned()); // ignore if caller gone
            }
        }
    }
    let _ = &mut map; // (map also mutated by Set arm in a real impl)
}

async fn get(tx: &mpsc::Sender<Cmd>, key: &str) -> Option<String> {
    let (reply, rx) = oneshot::channel();
    tx.send(Cmd::Get { key: key.into(), reply }).await.ok()?;
    rx.await.ok().flatten()
}
```

`tx.send()` returns `Err(value)` if the receiver already dropped; `rx.await` returns `Err(RecvError)` if the sender dropped without sending. Producers can check `tx.is_closed()` / `tx.closed().await` to skip expensive work when nobody's waiting.

### `broadcast` — pub/sub, all subscribers get all messages (`async-broadcast-pubsub`)

```rust
use tokio::sync::broadcast;

let (tx, _) = broadcast::channel::<Event>(100); // capacity = per-receiver buffer
let mut rx1 = tx.subscribe();
let mut rx2 = tx.subscribe();
tx.send(event)?; // BOTH rx1 and rx2 receive it
```

Message type must be `Clone` (each receiver gets a clone; wrap big non-`Clone` payloads in `Arc`). A slow receiver that falls behind the buffer gets `Err(RecvError::Lagged(n))` — it missed `n` messages but can keep going; `Err(RecvError::Closed)` means all senders dropped. **Handle `Lagged` explicitly** or you'll treat a recoverable lag as a fatal error.

### `watch` — latest value only (`async-watch-latest`)

For state/config where only the *current* value matters and slow observers should **skip** intermediate values (no lag, no backpressure):

```rust
use tokio::sync::watch;

let (tx, mut rx) = watch::channel("initial");
tx.send("v1")?; tx.send("v2")?; tx.send("v3")?;
rx.changed().await?;                 // wakes once
assert_eq!(*rx.borrow(), "v3");      // saw only the latest; v1/v2 skipped
```

`send_if_modified` notifies only on real change. **`borrow()` returns a `Ref` guard — do not hold it across `.await`** (it can block senders); clone the value out first, or use `borrow_and_update()` to also mark the change consumed. `changed()` is cancel-safe.

---

## 6. `Send` bounds & the `!Send`-across-`.await` problem

`tokio::spawn` may resume a task on a *different* worker thread, so a spawned future must be `Send`. A future is `!Send` if it **holds a `!Send` value across an `.await`** — e.g. `Rc`, `RefCell`, or a `std::sync::MutexGuard`. The error is `future cannot be sent between threads safely` (Patterns Book ch16 §`Send` Bounds).

```rust
use std::rc::Rc;
use tokio::time::{sleep, Duration};

// WRONG: rc lives across the await → future is !Send → won't spawn
async fn not_send() {
    let rc = Rc::new(42);
    sleep(Duration::from_millis(10)).await;
    println!("{rc}");
}

// FIX 1: drop the !Send value before the await (copy the data out)
async fn fixed_scope() {
    let n = { let rc = Rc::new(42); *rc }; // rc dropped here
    sleep(Duration::from_millis(10)).await;
    println!("{n}"); // just an i32 — Send
}

// FIX 2: use a Send type (Arc instead of Rc)
async fn fixed_arc() {
    let arc = std::sync::Arc::new(42); // Arc: Send
    sleep(Duration::from_millis(10)).await;
    println!("{arc}");
}
```

**Clone/copy before the await** (`async-clone-before-await`): holding a *borrow* across `.await` also extends the future's lifetime and can defeat `Send`. Clone the needed field (or the cheap `Arc` handle) into an owned local *before* the suspension point. Clone *minimally* — clone the one small field you need, not the whole `Arc`'d struct.

```rust
async fn process(data: std::sync::Arc<Data>) {
    let needed = data.small_field.clone(); // owned; borrow of `data` ends here
    async_work().await;
    use_field(&needed); // no borrow held across await
}
```

The standard spawn-loop idiom: clone the `Arc` per iteration before `move`-ing it in.

```rust
let shared = std::sync::Arc::new(State::new());
for i in 0..10 {
    let shared = shared.clone(); // cheap handle clone
    tokio::spawn(async move { shared.do_it(i).await; });
}
```

---

## 7. Cancellation & cancel-safety (`async-cancel-safety`)

`select!` (and `try_join!`, timeouts, aborting a `JoinSet`) **drop the futures that don't win** — along with any state held *inside* those futures. A future halfway through reading bytes or accumulating into a `Vec` **silently loses that progress**. This compiles fine; the bug only appears under concurrent load.

A future is **cancel-safe** iff dropping it mid-poll loses no observable progress. Tokio documents which primitives are cancel-safe; treat everything else as unsafe-to-cancel.

| Operation | Cancel-safe? | Note |
|---|---|---|
| `mpsc/broadcast/watch/oneshot` recv/changed | **Yes** | position/message preserved |
| `tokio::time::sleep`, `interval.tick()` | **Yes** | timer resets cleanly |
| `Mutex::lock()` | **Yes** | lock simply not acquired if dropped |
| `AsyncRead::read()` | **Yes** | partial read surfaced to caller |
| `AsyncRead::read_exact()` | **No** | partially-filled buffer is lost |
| `AsyncRead::read_to_end()` | **No** | accumulation lives inside the future |
| accumulating into a local `Vec`/`String` inside the future | **No** | partial state inside the future |

```rust
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

// WRONG: read_exact owns a partial buffer; if the recv branch wins,
// the bytes already read are silently discarded.
async fn bad(stream: &mut TcpStream, rx: &mut mpsc::Receiver<u8>) {
    let mut buf = [0u8; 1024];
    tokio::select! {
        r = stream.read_exact(&mut buf) => { let _ = r; }
        m = rx.recv() => { let _ = m; }
    }
}

// RIGHT: keep accumulation state OUTSIDE the select, and use the
// cancel-safe `read` (partial reads are surfaced, not lost).
async fn good(stream: &mut TcpStream, rx: &mut mpsc::Receiver<u8>) -> std::io::Result<()> {
    let mut buf = [0u8; 1024];
    let mut filled = 0;
    loop {
        tokio::select! {
            n = stream.read(&mut buf[filled..]) => {
                filled += n?;
                if filled == buf.len() { filled = 0; /* process */ }
            }
            _m = rx.recv() => { /* handle message; buf survives */ }
        }
    }
}
```

**Fixes for non-cancel-safe operations:** (1) hoist accumulation state to the enclosing scope; (2) pin the future once and reuse it across iterations with `std::pin::pin!` so it's never dropped mid-way; (3) use `tokio-util` cancel-safe adapters; (4) **spawn the operation as a task** — the task keeps running even if `select!` drops the handle, and the `JoinHandle` itself is cancel-safe.

```rust
use std::pin::pin;
use tokio::io::AsyncReadExt;

async fn pinned<R: AsyncReadExt + Unpin>(mut r: R) {
    let mut buf = vec![0u8; 64];
    let read_fut = pin!(r.read_to_end(&mut buf)); // survives across select iterations
    let _ = read_fut.await;
}
```

### Explicit cancellation: `CancellationToken` (`async-cancellation-token`)

**Dropping a `JoinHandle` does *not* cancel the task — it just detaches it** (the task runs on in the background). For graceful shutdown use cooperative cancellation via `tokio_util::sync::CancellationToken` (an `AtomicBool` flag can't wake a task that's parked on an `.await`).

```rust
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let handle = tokio::spawn({
    let token = token.clone();
    async move {
        loop {
            tokio::select! {
                _ = token.cancelled() => { cleanup().await; break; }
                _ = do_work() => {}
            }
        }
    }
});

token.cancel();     // signal
handle.await.ok();  // now await clean completion
```

Key API: `.cancel()`, `.is_cancelled()` (sync check), `.cancelled().await` (async wait, cancel-safe), `.child_token()` (cancelled when parent is — hierarchical shutdown), `.drop_guard()` (auto-cancels the token when the guard drops — RAII shutdown). Wire `signal::ctrl_c()` to `token.cancel()` for graceful `SIGINT` handling, pass `child_token()` into each task in a `JoinSet`, then drain with a `timeout`.

---

## 8. Blocking & CPU work: `spawn_blocking` (`async-spawn-blocking`, `async-tokio-fs`)

The runtime has few worker threads. **A blocking call or CPU-heavy loop on a worker thread starves every other task on it** — no other future can make progress. Never call `std::thread::sleep`, sync file/DB I/O, or crunch numbers directly in an async fn.

```rust
// WRONG: blocks the whole executor thread for 5s
async fn bad() { std::thread::sleep(std::time::Duration::from_secs(5)); }

// RIGHT: move blocking/CPU work to the dedicated blocking pool
async fn good() {
    tokio::task::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::from_secs(5)); // fine here
    }).await.unwrap(); // JoinHandle → Result
}
```

Rough thresholds (`async-spawn-blocking`): `<10µs` OK inline; `10µs–1ms` consider `spawn_blocking`; `>1ms` definitely offload. What counts as blocking: crypto/hashing (bcrypt), image/video processing, compression, big-JSON parsing, `std::fs`, sync DB/HTTP drivers, `thread::sleep`.

```rust
async fn hash(pw: String) -> String {
    tokio::task::spawn_blocking(move || bcrypt::hash(pw, bcrypt::DEFAULT_COST).unwrap())
        .await.unwrap()
}

// data-parallel CPU: rayon INSIDE spawn_blocking (don't run rayon on worker threads)
async fn transform(items: Vec<Item>) -> Vec<Out> {
    tokio::task::spawn_blocking(move || {
        use rayon::prelude::*;
        items.par_iter().map(cpu_transform).collect()
    }).await.unwrap()
}
```

- **`spawn` vs `spawn_blocking`:** `spawn` takes a `Future` (async code, runs on worker threads); `spawn_blocking` takes a sync closure (runs on the blocking pool, sized by `max_blocking_threads`).
- **Prefer async-native APIs** where they exist: `tokio::fs` (`async-tokio-fs`), an async DB driver, `reqwest` — these avoid `spawn_blocking` overhead. `tokio::fs` is itself a `spawn_blocking` wrapper, so for many tiny files the overhead adds up: batch with `try_join_all`. `std::fs` is fine *before* the runtime starts (startup config load) or on a `current_thread` runtime.

---

## 9. Async fn in traits & higher-order async bounds

### Native `async fn` in traits — AFIT (`async-fn-in-trait`)

Since Rust **1.75**, write `async fn` directly in a trait — no `#[async_trait]` macro, no hidden per-call `Box<dyn Future>` allocation.

```rust
// GOOD: native async fn in trait (no macro, no boxing)
trait Repo {
    async fn get(&self, id: u64) -> anyhow::Result<String>;
}
struct PgRepo;
impl Repo for PgRepo {
    async fn get(&self, id: u64) -> anyhow::Result<String> { Ok(format!("row-{id}")) }
}
```

**Two caveats:**

1. **Not dyn-compatible.** You cannot make `Box<dyn Repo>` from a native async trait. For dynamic dispatch, keep `#[async_trait]` (it boxes the future → object-safe) or use `#[trait_variant::make(RepoSend: Send)]` to generate a boxed, dyn-compatible variant.
2. **Returned futures aren't `Send` by default.** On a multi-thread runtime, spawning needs `Send`. Either use `trait-variant`'s `Send` variant, or bound the return future explicitly:

```rust
use std::future::Future;
trait Repo {
    fn get(&self, id: u64) -> impl Future<Output = anyhow::Result<String>> + Send;
}
```

| Scenario | Approach |
|---|---|
| Static dispatch (generics / `impl Trait`) | Native `async fn` in trait |
| Need `dyn Trait` | `#[async_trait]` or `trait-variant` |
| Multi-thread tokio + spawned | `trait-variant` Send variant, or `+ Send` on the return future |
| Single-thread runtime / `LocalSet` | Native `async fn` (no `Send` needed) |

Cross-link: **design-patterns.md** (anti-pattern: needless type erasure — prefer `impl Trait`/generics over `Box<dyn>` when static dispatch suffices).

### Higher-order async: `AsyncFn` bounds (`async-async-fn-bounds`)

Since Rust **1.85**, use `AsyncFn` / `AsyncFnMut` / `AsyncFnOnce` for callbacks that return futures, instead of the two-generic `F: Fn() -> Fut, Fut: Future`. The old pattern is verbose *and* cannot accept an `async ||` closure that borrows a local across the call — `AsyncFn` links the future's lifetime to the call correctly.

```rust
// WRONG: two-generic pattern; rejects borrowing async closures
async fn retry_old<F, Fut, T, E>(times: usize, f: F) -> Result<T, E>
where F: Fn() -> Fut, Fut: Future<Output = Result<T, E>> { todo!() }

// RIGHT: single AsyncFn bound; concise, correct lifetimes
async fn retry<F, T, E>(times: usize, f: F) -> Result<T, E>
where F: AsyncFn() -> Result<T, E> {
    let mut i = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if i + 1 >= times => return Err(e),
            Err(_) => i += 1,
        }
    }
}
```

`AsyncFn` (`&self`, callable many times), `AsyncFnMut` (`&mut self`, mutates captures), `AsyncFnOnce` (`self`, consumes) mirror `Fn`/`FnMut`/`FnOnce` — prefer `AsyncFn` first. Add `+ Send` on the bound if you need `Send` futures for a multi-thread runtime.

---

## 10. Testing async (`test-tokio-async`)

`#[test]` can't drive an `async fn` (needs a runtime). Use `#[tokio::test]` — it builds a runtime per test.

```rust
#[tokio::test]
async fn talks_over_channel() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    tokio::spawn(async move { tx.send("hi").await.unwrap(); });
    assert_eq!(rx.recv().await, Some("hi"));
}

#[tokio::test(flavor = "current_thread")]      // deterministic, simpler
async fn single_thread() {}

#[tokio::test(start_paused = true)]            // virtual clock: no real waiting
async fn time_control() {
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
}
```

`start_paused = true` gives a paused virtual clock — `tokio::time::advance` fast-forwards timers so timeout/interval tests run instantly and deterministically. Note: `start_paused` and `time::advance` require Tokio's `test-util` feature, which is **not** pulled in by `features = ["full"]` — it must be added explicitly, e.g. `tokio = { version = "1", features = ["full", "test-util"] }` (typically as a `[dev-dependencies]` entry). Mock async traits with `mockall` + `#[async_trait]`.

---

## Rules & anti-patterns checklist

- **async-tokio-runtime** — DON'T reach for the default multi-thread runtime blindly; DO match flavor/`worker_threads` to workload (IO-bound → oversubscribe; CPU-bound → == cores; use `available_parallelism()`, not `num_cpus`).
- **async-join-parallel** — DON'T `.await` independent futures back-to-back (sequential = sum of latencies); DO `join!` them (concurrent = max). Not for *dependent* futures.
- **async-try-join** — DO `try_join!` for concurrent fallible futures needing fail-fast; it short-circuits on first `Err`. Trailing cleanup after `.await?` in a losing branch may not run.
- **async-joinset-structured** — DON'T hand-manage `Vec<JoinHandle>` for dynamic task sets; DO use `JoinSet` (as-completed results, abort-on-drop = structured concurrency).
- **async-select-racing** — DO `select!`/`timeout` to race futures & implement timeouts/cancellation; add `biased;` for priority; use `else` to exit loops on closed channels. Losing branches are dropped.
- **async-cancel-safety** — DON'T put non-cancel-safe ops (`read_exact`, `read_to_end`, in-future accumulation) as `select!` branches; DO hoist accumulation state outside, use `read`, pin-and-reuse, or spawn as a task. Compiles clean, corrupts under load.
- **async-cancellation-token** — DON'T `drop(handle)` expecting cancellation (it detaches); DON'T use `AtomicBool` (can't wake a parked task). DO use `CancellationToken` + `select!` for graceful, cooperative shutdown; `child_token()` for hierarchy; `drop_guard()` for RAII.
- **async-mpsc-queue** — DON'T use `std::sync::mpsc` in async (its `recv()` blocks the executor); DO use `tokio::sync::mpsc`. Drop the original `tx` so the channel closes when clones drop.
- **async-bounded-channel** — DON'T use `unbounded_channel()` (unbounded memory growth → OOM); DO use bounded `channel(n)` for backpressure. Size near burst, err small, then measure.
- **async-oneshot-response** — DO use `oneshot` for exactly-one request→response (not `mpsc(1)` or polled shared state); pair with `mpsc` for the actor pattern. `send`/`recv` `Err` = peer dropped.
- **async-broadcast-pubsub** — DO use `broadcast` when every subscriber must see every message; type must be `Clone` (Arc big payloads); handle `RecvError::Lagged(n)` — a slow sub missed `n`, it's recoverable, not fatal.
- **async-watch-latest** — DO use `watch` when only the latest value matters (config/state); slow observers skip intermediates. Don't hold `borrow()`'s `Ref` across `.await`; clone out or `borrow_and_update()`.
- **async-no-lock-await** — NEVER hold a `Mutex`/`RwLock` guard across `.await` (deadlock/starvation risk). Extract or fetch data, release the lock, *then* await. Prefer `std::sync::Mutex` for non-awaiting critical sections; reach for `tokio::sync::Mutex` only when the lock genuinely must span an await (usually a redesign smell).
- **async-clone-before-await** — DON'T hold a borrow/`Rc`/`!Send` value across `.await` (breaks `Send`, blocks `spawn`); DO clone the minimal owned data (or the cheap `Arc` handle) before the suspension point.
- **async-spawn-blocking** — DON'T run blocking I/O or CPU-heavy work (`>~1ms`) on worker threads (starves the runtime); DO `spawn_blocking` (or use async-native APIs / `rayon` inside `spawn_blocking`).
- **async-tokio-fs** — DON'T call `std::fs` in async (blocks the executor); DO use `tokio::fs` (or `spawn_blocking`). Batch many small reads with `try_join_all`; `std::fs` OK at startup / on `current_thread`.
- **async-fn-in-trait** — DO use native `async fn` in traits (1.75+) for static dispatch (no `#[async_trait]` boxing). Caveats: not dyn-compatible (use `trait-variant`/`#[async_trait]`), and returned futures aren't `Send` (add `+ Send` or a Send variant) for multi-thread spawning.
- **async-async-fn-bounds** — DO use `AsyncFn`/`AsyncFnMut`/`AsyncFnOnce` (1.85+) for async callbacks instead of `F: Fn() -> Fut, Fut: Future` — concise and accepts borrowing `async ||` closures.
- **test-tokio-async** — DO annotate async tests with `#[tokio::test]` (not `#[test]`, which can't drive futures); `current_thread` for determinism, `start_paused = true` + `time::advance` for instant timer tests.

---

## Gotchas / footguns

- **Calling `async fn` without `.await`/spawn does nothing.** Futures are lazy; the compiler warns `unused implementer of Future`. Don't ignore it.
- **`select!` silently discards in-flight state** of losing branches → the cancel-safety trap (§7). Compiles clean; only fails under concurrent load. The most dangerous async bug class.
- **Holding a lock across `.await`** compiles (with `tokio::sync::Mutex`) but can deadlock or serialize all tasks. With `std::sync::MutexGuard` it instead fails to compile when spawned (`!Send`), which is the *lucky* case — it caught the bug.
- **`drop(join_handle)` does not cancel the task.** It detaches; the task keeps running. Use `handle.abort()` or a `CancellationToken`.
- **`unbounded_channel()` has no backpressure** — a classic slow-OOM under production load. Default to bounded.
- **`broadcast` capacity is a per-receiver ring buffer**, not a global bound; a lagging receiver gets `Lagged`, not blocked. `watch` never lags but only keeps one value.
- **Sequential `.await` masquerading as concurrency.** `let a = f().await; let b = g().await;` runs one-then-the-other. Reach for `join!`.
- **`std::thread::sleep` in async blocks the whole worker thread.** Use `tokio::time::sleep`. Same for any sync I/O.
- **`Rc`/`RefCell`/`MutexGuard` across `.await` makes the future `!Send`** → can't `tokio::spawn`. Error text: `future cannot be sent between threads safely`. Fix by dropping before the await or switching to `Arc`.
- **`JoinHandle` output is `Result<T, JoinError>`** — a spawned task that panics surfaces as `Err`, not a process abort. Handle it; don't blindly `.unwrap()` in servers.
- **CPU work on a `current_thread` runtime blocks everything**, including the timer driver — timeouts won't even fire.
- **`watch`/`broadcast` `borrow()`/`Ref` held across `.await`** can block senders; clone the value out.
- **Overusing `tokio::sync::Mutex`** where a short `std::sync::Mutex` critical section (no await inside) would be simpler and faster.

---

## Cheat-sheet

| Need | Use | Notes |
|---|---|---|
| Run N independent futures concurrently | `tokio::join!` | one task; max-latency; no `Send` needed |
| …and fail fast on first error | `tokio::try_join!` | drops others on `Err` |
| Dynamic collection of futures | `join_all` / `try_join_all` | pre-allocates; start-all-at-once |
| …with a concurrency cap | `stream…buffer_unordered(n)` or `Semaphore` | bound in-flight work |
| Dynamic set of spawned tasks | `JoinSet` | as-completed, abort-on-drop |
| First-to-finish / timeout / cancel | `select!` / `time::timeout` | losing branches dropped — mind cancel-safety |
| Task→task work queue | `mpsc::channel(n)` (bounded) | backpressure; single consumer |
| One-shot reply | `oneshot::channel()` | actor pattern with mpsc |
| Fan-out events to all | `broadcast::channel(n)` | `Clone` msgs; handle `Lagged` |
| Share latest state/config | `watch::channel(v)` | skips intermediates |
| Graceful shutdown | `CancellationToken` + `select!` | `child_token`, `drop_guard` |
| Blocking / CPU-heavy work | `spawn_blocking` (+ `rayon` inside) | keeps runtime responsive |
| Async file I/O | `tokio::fs` | wraps `spawn_blocking`; batch small reads |
| Async trait method (static) | native `async fn` in trait (1.75+) | `+ Send` return / `trait-variant` for spawn/dyn |
| Async callback param | `AsyncFn`/`AsyncFnMut`/`AsyncFnOnce` (1.85+) | accepts borrowing `async ||` |
| Async test | `#[tokio::test]` | `start_paused` for virtual clock |
| Hold future across loop iterations | `pin!(fut)` (stack) / `Box::pin` (heap/`'static`) | avoids re-dropping non-cancel-safe futures |
| Lock (no await in scope) | `std::sync::Mutex` | simpler/faster; **never** hold across `.await` |
| Lock spanning `.await` (rare) | `tokio::sync::Mutex` | redesign smell — extract data first |

**Decision: which channel?** one consumer + backpressure → `mpsc` · single reply → `oneshot` · all-subscribers-all-messages → `broadcast` · latest-value-only → `watch`.

**Decision: concurrency primitive?** static & same-task → `join!`/`try_join!`/`select!` · dynamic & spawned & structured → `JoinSet` · dynamic same-task collection → `join_all`/`buffer_unordered`.

See also: **api-guidelines.md** (C-SEND-SYNC, C-GOOD-ERR for `JoinError`/channel errors), **microsoft-guidelines.md** (M-* pragmatic bounds), **design-patterns.md** (actor / message-passing, RAII guards, type-erasure anti-pattern), **style-guide.md** (Cargo/feature conventions).
