# Testing & Benchmarking

Engineering patterns for testing Rust (unit/integration/doc tests, `#[cfg(test)]`, assertions, table &
property-based tests, snapshots, fixtures, mocking, panic & error testing, coverage) and benchmarking with
`criterion`. Consult this file before writing or reviewing any test module, `tests/` file, doctest, or `benches/`
harness. Distilled from the Microsoft *Rust Patterns* book ch14 and the rust-skills `test-*` rules. For naming,
API shape, and error-type decisions that these tests exercise, see `api-guidelines.md` (C-*), `microsoft-guidelines.md`
(M-*), and `design-patterns.md`.

---

## The three test tiers

Rust has three test tiers built in, all run by `cargo test` (Patterns Book ch14 §Unit, Integration, Doc Tests):

| Tier | Location | Sees | Compiled as | Runs with |
|------|----------|------|-------------|-----------|
| **Unit** | `src/**` in `#[cfg(test)] mod tests` | private + public items | part of the crate | `cargo test --lib` |
| **Integration** | `tests/*.rs` | **public API only** | one separate crate per file | `cargo test --test '*'` |
| **Doc** | `///` doc comments | public API | one binary per example | `cargo test --doc` |

Decision rule: test internal helpers and invariants as **unit tests** (they need private access); test the crate's
public surface as **integration tests** (they prove the real external contract); make every public example a **doctest**
so docs cannot rot.

### Unit tests: `#[cfg(test)] mod tests` (rule `test-cfg-test-module`)

Put unit tests in a `tests` submodule gated by `#[cfg(test)]` so they compile only under `cargo test`, never into the
release binary, yet stay next to the code and can reach private items. Import the parent with `use super::*`
(rule `test-use-super`).

```rust
pub fn factorial(n: u64) -> u64 {
    (1..=n).product()
}

#[cfg(test)]
mod tests {
    use super::*; // access to private items too

    #[test]
    fn factorial_of_zero_is_one() {
        // (1..=0).product() == 1, the multiplicative identity for the empty range
        assert_eq!(factorial(0), 1);
    }

    #[test]
    fn factorial_of_five_is_120() {
        assert_eq!(factorial(5), 120);
    }

    // Tests may return Result — `?` works inside, and an Err fails the test.
    #[test]
    fn parses_a_number() -> Result<(), Box<dyn std::error::Error>> {
        let value: u64 = "42".parse()?;
        assert_eq!(value, 42);
        Ok(())
    }
}
```

WRONG — no `#[cfg(test)]`, so the module compiles into every release build, and a bare `mod tests` file placed under
`tests/` cannot reach private items at all:

```rust
mod tests { // ⛔ ships in release; also loses the cfg-gating that strips test deps
    #[test]
    fn t() { /* ... */ }
}
```

`use super::*` is preferred over spelling out `use crate::my_module::foo;` because the test module is a *child* of the
module under test — `super::*` pulls in **private** items too (rule `test-use-super`). For nested modules,
`use super::super::*` reaches the grandparent. Large suites can nest `mod parsing { use super::*; … }` inside `tests`
to namespace the output (`tests::parsing::accepts_valid_json`).

### Integration tests: `tests/` (rule `test-integration-dir`)

Each file in `tests/` compiles as its own crate and can only touch the **public** API — this is the real external
contract users see. This also enforces good API design: if a workflow is awkward to drive from `tests/`, it is awkward
for users.

```rust
// tests/integration_test.rs
use my_crate::{Client, Config, Error};

#[test]
fn full_workflow_succeeds() {
    let client = Client::new(Config::default());
    let result = client.process("input");
    assert!(result.is_ok());
}

#[test]
fn strict_config_rejects_invalid_input() {
    let client = Client::new(Config::strict());
    let result = client.process("invalid");
    assert!(matches!(result, Err(Error::InvalidInput { .. })));
}
```

Shared helpers go in a **module directory**, not a top-level file: `tests/common/mod.rs`. A `tests/common.rs` would be
compiled and reported as its own (empty) test binary; `tests/common/mod.rs` is treated purely as a shared module you
`mod common;` into each test file.

```
my_project/
├── src/{lib.rs, internal.rs}
└── tests/
    ├── integration_test.rs   # each file = one test binary
    ├── api_tests.rs
    └── common/mod.rs         # shared utilities, NOT a test binary
```

```rust
// tests/api_tests.rs
mod common;
use my_crate::Client;

#[test]
fn test_with_shared_config() {
    common::setup_test_environment();
    let client = Client::new(common::test_config());
    // ...
}
```

### Doc tests (rule `test-doctest-examples`)

Examples in `///` fenced blocks are compiled and executed by `cargo test`. They demonstrate usage *and* guard against
API drift — a renamed function breaks its doctest (Patterns Book ch14 §Doc tests). See `api-guidelines.md` C-EXAMPLE
(all items have examples) and C-QUESTION-MARK (examples use `?`, not `unwrap`).

```rust
/// Computes the factorial of `n`.
///
/// # Examples
///
/// ```
/// use my_crate::factorial;
/// assert_eq!(factorial(5), 120);
/// ```
///
/// # Panics
///
/// Panics if the result overflows `u64` (in debug builds).
///
/// ```should_panic
/// my_crate::factorial(100);
/// ```
pub fn factorial(n: u64) -> u64 {
    (1..=n).product()
}
```

Doctest fence attributes — pick deliberately:

| Fence | Behavior | Use for |
|-------|----------|---------|
| ` ``` ` | compile **and** run | normal examples |
| ` ```no_run ` | compile, do **not** run | code that blocks/needs network/servers |
| ` ```ignore ` | neither compile nor run | pseudo-code, platform-specific snippets (avoid) |
| ` ```should_panic ` | run, expect a panic | documenting panic conditions |
| ` ```compile_fail ` | expect a **compile error** | proving a misuse is rejected (e.g. non-`Clone`) |
| ` ```text ` | not Rust | plain output/diagrams |

Hide setup lines with a leading `# ` — they compile and run but do not render in the docs. End a `?`-using example with
a hidden `Ok`-returning line so the example block itself type-checks as a `Result`:

```rust
/// ```
/// # use std::io::Write;
/// # let mut file = tempfile::NamedTempFile::new().unwrap();
/// # writeln!(file, "test data").unwrap();
/// use my_crate::process_file;
/// let result = process_file(file.path())?;
/// assert!(!result.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
# fn process_file(_: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> { Ok("x".into()) }
```

---

## Test structure & naming

### Arrange–Act–Assert (rule `test-arrange-act-assert`)

Each test verifies **one** behavior, split into three visible phases: set up inputs (Arrange), invoke the code under
test (Act), verify results (Assert). One assertion of intent per test — a test that checks five unrelated things is hard
to name and hard to debug when it fails.

WRONG — many concerns in one test:

```rust
#[test]
fn test_user() {
    assert_eq!(User::new("alice", "alice@example.com").unwrap().name(), "alice");
    assert!(User::new("", "email@example.com").is_err());
    let u = User::new("bob", "bob@example.com").unwrap();
    assert!(u.validate());
}
```

RIGHT — one behavior per test, phases marked:

```rust
#[test]
fn user_creation_fails_with_empty_name() {
    // Arrange
    let name = "";
    let email = "email@example.com";

    // Act
    let result = User::new(name, email);

    // Assert
    assert!(matches!(result, Err(UserError::EmptyName)));
}
```

Push repeated Arrange into helper functions inside the test module (`fn create_test_user() -> User`) and Assert logic
into assert helpers (`fn assert_order_total(order: &Order, expected: f64)`) — keeps each test readable while the shared
setup lives in one place.

### Descriptive names (rule `test-descriptive-names`)

Test names are output and documentation. `test_parse` tells you nothing when it fails; `parse_returns_error_for_empty_input`
tells you exactly what broke. Prefer one of these schemas:

- `function_condition_expected` — `parse_invalid_json_returns_syntax_error`
- `scenario_expectation` — `empty_cart_has_zero_total`
- `when_given_then` (BDD) — `when_user_not_found_then_returns_404`

Drop the redundant `test_` prefix; the `#[test]` attribute already marks it. Name edge/error cases for the case:
`handles_unicode_emoji`, `rejects_negative_quantity`, `timeout_returns_timeout_error`.

---

## Assertions & table tests

- `assert!(cond)` — boolean. Add a message: `assert!(x > 0, "x was {x}")`.
- `assert_eq!(a, b)` / `assert_ne!(a, b)` — print both sides on failure (needs `PartialEq + Debug`; see
  `api-guidelines.md` C-COMMON-TRAITS / C-DEBUG).
- `assert!(matches!(v, Pattern))` — the idiomatic way to assert a value matches an enum variant/shape without requiring
  `PartialEq`, e.g. `assert!(matches!(result, Err(Error::NotFound)))`.
- Float comparisons: never `assert_eq!` two floats; assert `(a - b).abs() < EPSILON` for a tolerance you choose.
- `debug_assert!` — compiled out in release; for internal invariants, not test assertions.

**Table / parameterized tests** — loop over `(input, expected)` rows. Include the row in the failure message (or use a
labeled struct) so you know which case failed:

```rust
#[test]
fn slugify_handles_cases() {
    let cases = [
        ("Hello World", "hello-world"),
        ("  trim  ", "trim"),
        ("Café", "cafe"),
    ];
    for (input, expected) in cases {
        assert_eq!(slugify(input), expected, "input = {input:?}");
    }
}
# fn slugify(s: &str) -> String { s.to_string() }
```

Trade-off: a single `#[test]` loop stops at the first failing row and reports one test. For independent
pass/fail per case, generate a `#[test]` per row with a macro, or reach for the `rstest` crate's `#[case(...)]`.

---

## Testing panics & errors

### `#[should_panic]` (rule `test-should-panic`)

Verify defensive panics fire — optionally matching the message (a **substring** match, not equality). Prefer this over
manual `std::panic::catch_unwind`.

```rust
struct NonEmpty<T>(Vec<T>);
impl<T> NonEmpty<T> {
    fn new(items: Vec<T>) -> Self {
        assert!(!items.is_empty(), "NonEmpty cannot be empty");
        NonEmpty(items)
    }
}

#[test]
#[should_panic(expected = "NonEmpty cannot be empty")]
fn non_empty_rejects_empty_vec() {
    NonEmpty::new(Vec::<i32>::new());
}
```

Key rule: use `#[should_panic]` **only** for genuine panics (invariant violations, programmer errors). For
**recoverable** errors, return `Result` and assert on the `Err` — do not make the function panic just to test it
(rule `test-should-panic`; see `microsoft-guidelines.md` on error handling and `design-patterns.md` anti-pattern
`anti-panic-expected`).

```rust
// WRONG: a bad config is recoverable — it should not panic
#[test]
#[should_panic]
fn invalid_input_panics() { parse_config("invalid"); }

// RIGHT: return Result, assert the error
#[test]
fn invalid_input_returns_error() {
    assert!(parse_config("invalid").is_err());
}
# fn parse_config(_: &str) -> Result<(), ()> { Err(()) }
```

Always prefer `expected = "..."` over a bare `#[should_panic]`: a bare one passes on *any* panic, including an unrelated
`unwrap` in your setup, giving a false green.

---

## Property-based testing (rule `test-proptest-properties`)

Instead of hand-picked examples, assert **properties** that hold for all inputs; `proptest` generates hundreds of random
values and, on failure, **shrinks** to the minimal reproducing case (Patterns Book ch14 §Property-Based Testing). Reach
for it when the input space is large and edge cases are hard to enumerate.

```rust
// Cargo.toml: [dev-dependencies]  proptest = "1"
use proptest::prelude::*;

fn reverse(v: &[i32]) -> Vec<i32> { v.iter().rev().cloned().collect() }

proptest! {
    #[test]
    fn reverse_twice_is_identity(v in prop::collection::vec(any::<i32>(), 0..100)) {
        prop_assert_eq!(reverse(&reverse(&v)), v);
    }

    #[test]
    fn sort_is_idempotent(mut v in prop::collection::vec(any::<i32>(), 0..100)) {
        v.sort();
        let once = v.clone();
        v.sort();
        prop_assert_eq!(v, once);
    }
}
```

Inside `proptest!`, use `prop_assert!`/`prop_assert_eq!` (not `assert!`) — they report the failing input and cooperate
with shrinking. The `x in strategy` binding drives generation.

**Strategies cheat-sheet:**

| Strategy | Generates |
|----------|-----------|
| `any::<T>()` | any `T: Arbitrary` |
| `0..100i32` | ints in range |
| `"[a-z]+@[a-z]+\\.[a-z]{2,3}"` | strings matching a regex |
| `prop::collection::vec(elem, 0..10)` | vecs of length 0–9 |
| `prop::option::of(any::<i32>())` | `Option<i32>` |
| `(strat_a, strat_b).prop_map(|(a,b)| …)` | composed custom type |
| `strat.prop_filter("finite", |x| x.is_finite())` | filtered (use sparingly — rejections slow generation) |

Custom strategies compose tuples then `prop_map`; or `#[derive(proptest_derive::Arbitrary)]` on a struct.

**Canonical properties to check:** roundtrip (`decode(encode(x)) == x`), idempotence (`f(f(x)) == f(x)`), commutativity,
associativity, identity element, and invariants (`len(push(v, x)) == len(v) + 1`). Serialization roundtrips
(`parse(x.to_string()) == x`) are the highest-value first property for any type.

Configure via an inner attribute: `#![proptest_config(ProptestConfig { cases: 1000, ..Default::default() })]`. Proptest
persists past failures to `proptest-regressions/` — commit that file so a shrunk counterexample is re-tested forever.

`quickcheck` is the older alternative (`#[quickcheck] fn prop(x: Vec<i32>) -> bool`); proptest's regex strategies and
richer shrinking make it the default choice today.

---

## Snapshot testing with insta (rule `test-snapshot-testing`)

For large structured output — pretty-printed structs, rendered errors, JSON/YAML, generated code, CLI output —
hand-written `assert_eq!` on a giant literal is brittle and painful to update. `insta` records an approved snapshot on
first run, diffs against it thereafter, and gives you a one-keystroke review flow when output legitimately changes.

```toml
[dev-dependencies]
insta = { version = "1", features = ["json", "yaml"] }
```

```rust
use insta::{assert_debug_snapshot, assert_json_snapshot};

#[test]
fn renders_not_found_error() {
    let err = AppError::NotFound { id: 42 };
    assert_debug_snapshot!(err); // creates snapshots/…renders_not_found_error.snap on first run
}

#[test]
fn serializes_default_config() {
    assert_json_snapshot!(Config::default());
}
```

Workflow: `cargo test` writes `.snap.new` for new/changed output → `cargo insta review` shows the diff, press `a` to
accept → commit the `.snap` files so changes are reviewed in the PR. In CI, prevent silent acceptance with
`INSTA_UPDATE=no cargo test` (fails if any snapshot is new or changed).

Choose snapshots vs `assert_eq!`:

| Output | Use |
|--------|-----|
| short scalar (`true`, `42`, `"ok"`) | `assert_eq!` |
| multi-line / structured | `assert_debug_snapshot!` |
| JSON / YAML | `assert_json_snapshot!` / `assert_yaml_snapshot!` |
| rendered message / compiler-style text | `assert_snapshot!` |

---

## Fixtures & RAII cleanup (rule `test-fixture-raii`)

Setup/teardown (temp files, env vars, servers, DB transactions) must clean up **even when the test panics**. A cleanup
line at the end of the test body does **not** run after a failed assertion. Use RAII: put the teardown in a `Drop` impl
so it runs on scope exit unconditionally (Patterns Book ch14 §Test Fixtures; `design-patterns.md` RAII guard).

WRONG — cleanup skipped on panic, leaking state into other tests:

```rust
#[test]
fn test_with_temp_file() {
    let path = "/tmp/test_file.txt";
    std::fs::write(path, "data").unwrap();
    let result = process_file(path);
    std::fs::remove_file(path).unwrap(); // ⛔ never runs if the assert below panics
    assert!(result.is_ok());
}
```

RIGHT — `tempfile` (or a custom guard) cleans up on drop:

```rust
use tempfile::NamedTempFile;

#[test]
fn process_file_reads_contents() {
    let file = NamedTempFile::new().unwrap();       // deleted when `file` drops
    std::fs::write(file.path(), "data").unwrap();
    let result = process_file(file.path());
    assert!(result.is_ok());                        // file still cleaned up if this panics
}
```

Custom guard for environment variables — note `std::env::set_var` is `unsafe` since the 2024 edition (env writes are not
thread-safe), so env-touching tests must run single-threaded:

```rust
struct EnvGuard { key: String, original: Option<String> }

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        // SAFETY: env writes are not thread-safe; run these tests single-threaded.
        unsafe { std::env::set_var(key, value) };
        EnvGuard { key: key.to_string(), original }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => unsafe { std::env::set_var(&self.key, v) },
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}

#[test]
fn reads_config_from_env() {
    let _guard = EnvGuard::set("MY_VAR", "test_value");
    assert!(read_config().is_ok());
} // MY_VAR restored here, panic or not
# fn read_config() -> Result<(), ()> { Ok(()) }
```

Other RAII fixtures: `tempfile::TempDir` (deletes dir + contents), a `TestServer` whose `Drop` sends a shutdown signal,
a `TestTransaction` whose `Drop` runs `ROLLBACK`. For quick one-off cleanup without a named type, the `scopeguard`
crate's `defer! { … }` macro also runs on scope exit including panic (like a `Drop` guard), because it registers the
guard immediately — unlike a plain trailing cleanup statement, which a panic skips.

---

## Mocking & test doubles

Rust's trait system *is* the dependency-injection mechanism — most cases need no mocking framework. Extract each external
dependency behind a trait, make the service generic (or `dyn`) over it, and inject a real impl in production and a test
double in tests (rule `test-mock-traits`; Patterns Book ch14 §Mocking).

WRONG — concrete `PostgresConnection` field forces a real database to test anything:

```rust
struct UserService { db: PostgresConnection }
```

RIGHT — depend on a trait; production and test both implement it:

```rust
trait UserRepository {
    fn find_by_id(&self, id: u64) -> Option<User>;
}

struct UserService<R: UserRepository> { repo: R }

impl<R: UserRepository> UserService<R> {
    fn get_user(&self, id: u64) -> Result<User, Error> {
        self.repo.find_by_id(id).ok_or(Error::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockUserRepo { users: HashMap<u64, User> }
    impl UserRepository for MockUserRepo {
        fn find_by_id(&self, id: u64) -> Option<User> { self.users.get(&id).cloned() }
    }

    #[test]
    fn get_user_found_returns_user() {
        let mut mock = MockUserRepo { users: HashMap::new() };
        mock.users.insert(1, User { id: 1, name: "Alice".into() });
        let service = UserService { repo: mock };
        assert_eq!(service.get_user(1).unwrap().name, "Alice");
    }

    #[test]
    fn get_user_missing_returns_not_found() {
        let service = UserService { repo: MockUserRepo { users: HashMap::new() } };
        assert!(matches!(service.get_user(999), Err(Error::NotFound)));
    }
}
# #[derive(Clone)] struct User { id: u64, name: String }
# #[derive(Debug)] enum Error { NotFound }
```

A hand-written **fake** that always errors (`struct FailingClient;` returning `Err(HttpError::Timeout)`) is the simplest
way to exercise error/timeout paths — no framework needed. When you don't want generics threaded everywhere, store a
`Box<dyn UserRepository>` and accept `impl UserRepository + 'static` in the constructor (small runtime cost, cleaner API).

### `mockall` for richer expectations (rule `test-mockall-mocking`)

When you need call-count verification, argument matchers, or per-call return sequences, `#[automock]` generates a
`MockYourTrait` with an expectation API. Expectations are verified on **drop**.

```rust
use mockall::{automock, predicate::*};

#[automock]
trait Database {
    fn get_user(&self, id: u64) -> Option<User>;
    fn save_user(&self, user: &User) -> Result<(), Error>;
}

#[test]
fn find_user_queries_by_id() {
    let mut mock = MockDatabase::new();
    mock.expect_get_user()
        .with(eq(42))                 // argument predicate
        .times(1)                     // call-count assertion
        .returning(|_| Some(User { id: 42, name: "Alice".into() }));

    let service = UserService::new(mock);
    assert_eq!(service.find_user(42).unwrap().name, "Alice");
}
# #[derive(Clone)] struct User { id: u64, name: String }
# #[derive(Debug)] struct Error;
# struct UserService<D>(D);
# impl<D: Database> UserService<D> { fn new(d: D) -> Self { Self(d) } fn find_user(&self, id: u64) -> Option<User> { self.0.get_user(id) } }
```

`mockall` essentials: predicates `eq`, `function(|x| …)`, `.withf(|a, b| …)` for multiple args; `.times(n)` /
`.times(3..)`; `.returning(|x| …)` (compute from input) or `.return_const(v)`; `mockall::Sequence` +`.in_sequence(&mut seq)`
to assert call ordering. For **foreign** traits you don't own, apply `#[cfg_attr(test, automock)]` so the attribute is
test-only. Guidance: reach for `mockall` only when the dependency graph is genuinely complex — hand-written trait doubles
cover most cases and read more clearly. Test philosophy: real dependencies in integration tests, trait-based doubles in
unit tests.

---

## Async tests (rule `test-tokio-async`)

Async fns need a runtime. `#[tokio::test]` provides one per test — do not hand-roll `Runtime::new().block_on(...)`, and a
bare `#[test] async fn` does not compile.

```rust
#[tokio::test]
async fn fetch_data_succeeds() {
    let result = fetch_data().await;
    assert!(result.is_ok());
}
# async fn fetch_data() -> Result<(), ()> { Ok(()) }
```

Runtime flavors and knobs:

| Attribute | Effect |
|-----------|--------|
| `#[tokio::test]` | current-thread runtime (single-threaded, default) |
| `#[tokio::test(flavor = "current_thread")]` | current-thread runtime, deterministic (same as the default, stated explicitly) |
| `#[tokio::test(flavor = "multi_thread")]` | multi-threaded runtime (opt-in) |
| `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` | multi-threaded runtime with a fixed worker count |
| `#[tokio::test(start_paused = true)]` | paused clock — `tokio::time::advance(dur).await` jumps time without waiting |

Note the default: `#[tokio::test]` spins up a **current_thread** (single-threaded) runtime, so tests are deterministic
by default; opt into `flavor = "multi_thread"` only when you need real parallelism. This is the opposite of
`#[tokio::main]`, which defaults to the multi-threaded runtime.

`start_paused = true` is the key tool for testing timeouts/retries/backoff **instantly**: advance virtual time instead of
sleeping. Test timeouts with `tokio::time::timeout(dur, fut).await` (assert `.is_ok()` / `.is_err()`); test channels by
spawning a producer and asserting on `rx.recv().await`. `mockall` mocks async traits via `#[automock] #[async_trait]`.

---

## Concurrency model checking with loom (rule `test-loom-concurrency`)

Stress tests can run a billion iterations and still miss a race that needs one specific interleaving. `loom`
**exhaustively** explores every thread schedule and memory reordering the C11 model permits — a proof for the
interleavings within the model, not a probability. Tokio uses it to verify its own synchronization primitives.

Gate the primitive so it uses loom's instrumented types under `--cfg loom` and std types otherwise:

```rust
#[cfg(loom)]
use loom::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Flag(AtomicBool);
impl Flag {
    pub fn new() -> Self { Self(AtomicBool::new(false)) } // not `const fn`: loom's atomic constructors register state at runtime and aren't const
    pub fn set(&self) { self.0.store(true, Ordering::Release); }
    pub fn is_set(&self) -> bool { self.0.load(Ordering::Acquire) }
}
```

```rust
#[cfg(loom)]
#[test]
fn flag_visible_across_threads() {
    loom::model(|| {
        let flag = loom::sync::Arc::new(Flag::new());
        let f2 = loom::sync::Arc::clone(&flag);
        let writer = loom::thread::spawn(move || f2.set());
        writer.join().unwrap();
        assert!(flag.is_set()); // holds in every interleaving loom explores
    });
}
```

Run with `RUSTFLAGS="--cfg loom" cargo test --test loom_flag`. Keep model closures **tiny** — the state space explodes
with each atomic op and thread; test one primitive at a time. loom replaces `std::sync::atomic`, `Mutex`, `thread`,
`cell` with instrumented equivalents; it checks the C11 model only and finds no logic bugs unrelated to concurrency.
See `conc-atomic-ordering` for choosing orderings.

---

## Benchmarking with criterion (rule `test-criterion-bench`)

`Instant::now()` timing is noise-dominated and easy to fool. `criterion` does warmup, many iterations, outlier
detection, statistical analysis, and baseline comparison, with HTML reports in `target/criterion/` (Patterns Book ch14
§Benchmarking; rule `test-criterion-bench`). Profile before you optimize (`perf-profile-first`).

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "my_benchmark"
harness = false          # required: disables libtest so criterion owns main()
```

```rust
// benches/my_benchmark.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn fibonacci(n: u64) -> u64 {
    match n { 0 => 0, 1 => 1, n => fibonacci(n - 1) + fibonacci(n - 2) }
}

fn bench_fibonacci(c: &mut Criterion) {
    c.bench_function("fib 20", |b| b.iter(|| fibonacci(black_box(20))));
}

criterion_group!(benches, bench_fibonacci);
criterion_main!(benches);
```

### `black_box` is not optional

Without `black_box`, the optimizer can see the input is a constant and the result is unused, and **delete the whole
computation** — you benchmark nothing and get an implausibly fast number. `black_box` is an opaque barrier: wrap the
**input** so it isn't constant-folded, and wrap the **result** if it would otherwise be dead (rule `test-criterion-bench`
§black_box; `perf-black-box-bench`).

```rust
// WRONG: result unused, input constant → may be optimized to nothing
b.iter(|| fibonacci(20));

// RIGHT: hide the input; hide the result too if it could be eliminated
b.iter(|| fibonacci(black_box(20)));
b.iter(|| black_box(fibonacci(black_box(20))));
```

### Comparing implementations, parameter sweeps, throughput

```rust
fn bench_concat(c: &mut Criterion) {
    let mut group = c.benchmark_group("String concat");
    let data = "hello";
    group.bench_function("format!",  |b| b.iter(|| format!("{}{}", black_box(data), " world")));
    group.bench_function("push_str", |b| b.iter(|| { let mut s = String::from(black_box(data)); s.push_str(" world"); s }));
    group.bench_function("concat",   |b| b.iter(|| [black_box(data), " world"].concat()));
    group.finish();
}

fn bench_vec_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("Vec::push");
    for size in [100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut v = Vec::new();
                for i in 0..size { v.push(black_box(i)); }
                v
            });
        });
    }
    group.finish();
}
```

Report throughput with `group.throughput(criterion::Throughput::Bytes(input.len() as u64))` to get MB/s instead of just
time. For setup that must not be timed, use `b.iter_batched(|| make_input(), |input| process(input),
BatchSize::SmallInput)` — the setup closure runs outside the measured region (avoids the anti-pattern of timing your
`clone`/allocation instead of the work).

### Regression tracking & running

```bash
cargo bench                              # run all; criterion prints change vs last run
cargo bench -- fib                       # filter by name
cargo bench -- --save-baseline main      # save a named baseline
cargo bench -- --baseline main           # compare current against that baseline
```

Save a baseline on `main`, then compare a feature branch to catch regressions in CI. For async code, build a runtime
once outside `iter` and `rt.block_on(...)` inside it (tokio's own `benches/sync_mpsc.rs` pattern).

### Micro vs macro benchmarks

- **Micro** — one function in isolation (`fib 20`). Fast feedback, but wins can be illusory: a hot loop the optimizer
  handles differently in context, or a change that helps micro yet regresses cache behavior at scale.
- **Macro** — a realistic end-to-end workload (parse a real file, serve N requests). Slower and noisier but reflects
  reality. Use micro to iterate, macro to confirm the win survives integration.

---

## Coverage

Coverage tells you which lines tests exercised, not whether assertions are meaningful — treat it as a gap-finder, not a
target. `cargo-llvm-cov` is the current standard (LLVM source-based instrumentation, includes doctests):

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --all-features --workspace              # summary
cargo llvm-cov --html                                  # browsable report
cargo llvm-cov --lcov --output-path lcov.info          # for CI upload
```

`cargo-tarpaulin` is the older Linux-focused alternative. Chase uncovered **branches** (error arms, edge cases), not a
percentage — 100% line coverage with no `Err`-path assertions proves nothing.

---

## Rules & anti-patterns checklist

- **test-cfg-test-module** — DO put unit tests in `#[cfg(test)] mod tests { use super::*; … }`; a bare `mod tests`
  ships in release builds and can't strip test-only deps.
- **test-use-super** — DO `use super::*` in the test module; it's a child module, so this reaches **private** items
  (and `super::super::*` reaches the grandparent). Spelling out `crate::…` paths can't see privates.
- **test-integration-dir** — DO put public-API tests in `tests/*.rs` (each file = a separate crate); shared helpers go in
  `tests/common/mod.rs` (a `tests/common.rs` would be built as its own empty test binary).
- **test-arrange-act-assert** — DO structure each test Arrange→Act→Assert and test one behavior; multi-concern tests are
  unnameable and hard to debug.
- **test-descriptive-names** — DO name tests `function_condition_expected` (`parse_returns_error_for_empty_input`); drop
  the `test_` prefix. `test_parse` says nothing when it fails.
- **test-should-panic** — DO use `#[should_panic(expected = "...")]` for genuine panics; DON'T panic to test recoverable
  errors — return `Result` and assert the `Err`. Bare `#[should_panic]` passes on any panic (false green).
- **test-doctest-examples** — DO keep public examples as runnable doctests with `# Examples`, `?` not `unwrap`, and hidden
  `# ` setup lines; pick `no_run`/`compile_fail`/`should_panic` fences deliberately. Untested examples rot.
- **test-fixture-raii** — DO clean up via `Drop`/`tempfile`, not a trailing cleanup line that a panic skips. `env::set_var`
  is `unsafe` since edition 2024 — run env tests single-threaded.
- **test-mock-traits** — DO inject dependencies behind a trait and swap in a hand-written double in tests; a concrete
  external type (DB/HTTP) makes error paths and edge cases untestable.
- **test-mockall-mocking** — DO use `#[automock]` when you need call-count/argument/sequence verification; expectations
  verify on drop. Use `#[cfg_attr(test, automock)]` for foreign traits. Prefer hand-written doubles for simple cases.
- **test-proptest-properties** — DO assert invariants over generated inputs with `proptest` and `prop_assert!`; commit
  `proptest-regressions/` so shrunk counterexamples stick. Start with a serialization roundtrip.
- **test-snapshot-testing** — DO use `insta` for large/structured output; review with `cargo insta review`, commit
  `.snap`, and gate CI with `INSTA_UPDATE=no`. `assert_eq!` on giant literals is brittle.
- **test-tokio-async** — DO use `#[tokio::test]`; DON'T hand-roll a runtime or write `#[test] async fn` (won't compile).
  Use `start_paused = true` + `time::advance` to test timeouts instantly.
- **test-loom-concurrency** — DO model-check lock-free code with `loom` under `#[cfg(loom)]`; keep model closures tiny to
  bound the state explosion. Stress tests can't prove absence of races.
- **test-criterion-bench** — DO benchmark with `criterion` (`harness = false`) and `black_box` every input/result;
  `Instant` timing and un-black-boxed benches measure the optimizer, not your code. Save baselines to track regressions.

---

## Gotchas / footguns

- **`harness = false` is mandatory for criterion.** Omit it and libtest owns `main()`, so `criterion_main!` never runs and
  `cargo bench` does nothing useful.
- **Overflow checks differ by profile.** `#[should_panic(expected = "overflow")]` on integer arithmetic passes in debug
  (overflow checks on) but **fails in release** (`cargo test --release` wraps silently). Gate such a test with
  `#[cfg(debug_assertions)]`, or use `checked_*` / set `overflow-checks = true` in the profile (Patterns Book ch14).
- **Bare `#[should_panic]` = false green.** Any panic passes, including an `unwrap` in your setup. Always pass
  `expected = "..."` (substring match).
- **Missing `black_box` deletes the benchmark.** The optimizer folds a constant input and drops the unused result;
  you time an empty loop and read an impossibly low number.
- **`tests/common.rs` becomes a phantom test binary.** Cargo compiles every top-level file in `tests/` as a test crate
  and reports it. Put shared code in `tests/common/mod.rs` instead.
- **Trailing cleanup runs only on success.** A `remove_file`/`remove_var` at the end of a test body is skipped when an
  earlier assertion panics, leaking state into the next test. Use RAII (`Drop`).
- **Test parallelism + shared global state.** `cargo test` runs tests in parallel threads by default; tests touching env
  vars, the filesystem at fixed paths, or process-wide singletons race. Use unique temp paths, or `cargo test --
  --test-threads=1` for env-sensitive suites (and remember env writes are `unsafe` in edition 2024).
- **`assert_eq!` on floats.** NaN != NaN and rounding makes exact equality flaky; assert a tolerance.
- **Integration tests can't see `pub(crate)` / private items.** If a `tests/` file needs them, either the item should be
  public, or the test belongs in a `#[cfg(test)]` unit module.
- **Doctests run against the *public* crate.** A doctest using a private helper won't compile; hidden `# use …` lines still
  only reach public paths.
- **`no_run` still compiles.** It catches type errors but not runtime bugs — don't use it to hide a broken example.
- **proptest `prop_filter` that rejects most inputs** slows or aborts generation (too many rejections). Prefer a
  constructive strategy (`prop_map`) that only produces valid values.
- **`unwrap()` in a test body is fine, in library code isn't.** Tests may `unwrap`; the `anti-unwrap-abuse` /
  `err-expect-bugs-only` guidance targets non-test code (see `design-patterns.md`).

---

## Cheat-sheet

| Need | Use |
|------|-----|
| Unit test with private access | `#[cfg(test)] mod tests { use super::*; #[test] fn … }` |
| Test the public contract | file in `tests/`, `use my_crate::…` |
| Example that stays correct | `///` doctest with `# Examples` + `?` |
| Return `Result` from a test | `#[test] fn t() -> Result<(), Box<dyn Error>> { …; Ok(()) }` |
| Assert enum variant / shape | `assert!(matches!(v, Err(E::X)))` |
| Expect a panic | `#[should_panic(expected = "msg")]` |
| Parameterized cases | loop over `[(input, expected)]` with input in the message; or `rstest` |
| Random-input properties | `proptest! { #[test] fn p(x in strat) { prop_assert!(…) } }` |
| Large/structured output | `insta::assert_debug_snapshot!` / `assert_json_snapshot!` |
| Auto cleanup | `tempfile::{NamedTempFile, TempDir}` or a `Drop` guard |
| Inject a dependency | trait + generic/`Box<dyn …>`; hand-written double or `#[automock]` |
| Mock with call verification | `mockall::automock` → `mock.expect_x().with(eq(..)).times(1).returning(..)` |
| Async test | `#[tokio::test]`; `start_paused = true` for time control |
| Fake time / timeouts | `tokio::time::{advance, timeout}` |
| Model-check lock-free code | `loom::model(|| …)` under `#[cfg(loom)]`, run `--cfg loom` |
| Benchmark | `criterion`, `harness = false`, `b.iter(|| f(black_box(x)))` |
| Untimed bench setup | `b.iter_batched(setup, routine, BatchSize::SmallInput)` |
| Track regressions | `cargo bench -- --save-baseline main` then `--baseline main` |
| Coverage | `cargo llvm-cov --html` |

**Commands:** `cargo test` (all) · `--lib` (unit) · `--doc` (doctests) · `--test '*'` (all integration) · `--test NAME` ·
`cargo test NAME_SUBSTRING` (filter) · `cargo test -- --test-threads=1` (serial) · `cargo test -- --nocapture` (show
`println!`) · `cargo test -- --ignored` (run `#[ignore]`d) · `cargo bench` · `cargo insta review`.
