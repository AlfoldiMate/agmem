# Unsafe Rust & Macros

Deep reference for two features that let you step outside — or extend — the ordinary Rust surface: `unsafe` (raw pointers, FFI, `MaybeUninit`, manual `Send`/`Sync`, and building *sound* safe abstractions over unverifiable operations) and macros (`macro_rules!` fragment specifiers/hygiene/recursion, plus procedural macros with `syn`/`quote`). Consult before writing or reviewing any `unsafe` block, any `extern`/FFI boundary, or any macro. For the canonical style/API rules these build on, see `api-guidelines.md` (C-*), `microsoft-guidelines.md` (M-*), `design-patterns.md`, and `style-guide.md`.

---

# Part I — Unsafe Rust: Controlled Danger

## The five unsafe superpowers

`unsafe` unlocks exactly five operations the compiler cannot verify. It does **not** turn off the borrow checker, the type system, or any other rule — all safe-Rust rules still apply inside an `unsafe` block. (Patterns Book ch12 §The Five Unsafe Superpowers)

1. **Dereference a raw pointer** (`*ptr`) — the pointer may be dangling, null, misaligned, or aliased.
2. **Call an `unsafe fn`** — including FFI functions and intrinsics.
3. **Access or modify a mutable static** (`static mut`) — unsynchronized access is a data race.
4. **Implement an `unsafe trait`** (`unsafe impl Send for T {}`).
5. **Access fields of a `union`** — reinterpreting bits can yield an invalid value.

```rust
static mut COUNTER: u32 = 0;

unsafe fn call_me() {}

fn main() {
    let x = 42;
    let ptr: *const i32 = &x;
    // SAFETY: ptr was just taken from a live local `x` on this stack frame.
    let value = unsafe { *ptr }; // (1) deref raw pointer
    assert_eq!(value, 42);

    // SAFETY: call_me has no preconditions.
    unsafe { call_me() }; // (2) call unsafe fn

    // SAFETY: single-threaded main, no concurrent access to COUNTER.
    unsafe { COUNTER += 1 }; // (3) mutable static (prefer AtomicU32 instead)
}
```

> **Key principle**: `unsafe` is a promise *you* make to the compiler, not a permission the compiler grants you. You assert the invariants hold; the compiler trusts you and optimizes accordingly.

## The three rules of sound unsafe code

Every use of `unsafe` should satisfy all three (Patterns Book ch12 §Writing Sound Abstractions):

1. **Document invariants** — a `// SAFETY:` comment above each block; a `# Safety` doc section on each public `unsafe fn`.
2. **Encapsulate** — wrap the unsafe in a safe API so no caller can trigger UB with safe code. A type is *sound* iff no safe use of its public API can cause UB.
3. **Minimize** — the smallest possible span is `unsafe`; nothing safe leaks into the block.

```rust
use std::mem::MaybeUninit;

/// Fixed-capacity, stack-allocated buffer. Public API is entirely safe;
/// all `unsafe` is encapsulated with invariant: data[0..len] are initialized.
pub struct StackBuf<T, const N: usize> {
    data: [MaybeUninit<T>; N],
    len: usize,
}

impl<T, const N: usize> StackBuf<T, N> {
    pub fn new() -> Self {
        // `[const { MaybeUninit::uninit() }; N]` (Rust 1.79+) needs no unsafe
        // and works for any T (no Copy bound).
        StackBuf { data: [const { MaybeUninit::uninit() }; N], len: 0 }
    }

    pub fn push(&mut self, value: T) -> Result<(), T> {
        if self.len >= N {
            return Err(value); // full — hand the value back, don't panic
        }
        self.data[self.len] = MaybeUninit::new(value); // safe: MaybeUninit::new
        self.len += 1;
        Ok(())
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index < self.len {
            // SAFETY: index < len, and data[0..len] are all initialized.
            Some(unsafe { self.data[index].assume_init_ref() })
        } else {
            None
        }
    }
}

impl<T, const N: usize> Drop for StackBuf<T, N> {
    fn drop(&mut self) {
        for i in 0..self.len {
            // SAFETY: data[0..len] are initialized; drop each exactly once.
            unsafe { self.data[i].assume_init_drop() };
        }
    }
}
```

## `MaybeUninit<T>` — the only correct uninitialized memory (rule `unsafe-maybeuninit`)

`mem::uninitialized()` is `#[deprecated(since = "1.39")]` and is **instant UB** for any type with validity invariants (`bool` is only `0`/`1`; `&T`/`NonZero*`/`char`/enums have forbidden bit patterns). Even `mem::zeroed()` is UB for references and `NonZero*`. Use `MaybeUninit<T>`, which holds possibly-invalid bytes without ever producing an invalid `T`.

```rust
use std::mem::MaybeUninit;

// WRONG — instant UB, optimizer may miscompile:
// let b: bool = unsafe { std::mem::uninitialized() };
// let r: &u32 = unsafe { std::mem::zeroed() };

// RIGHT — single value
let mut x = MaybeUninit::<u32>::uninit();
x.write(42);
// SAFETY: we wrote a valid u32 via `write`, so it is initialized.
let value: u32 = unsafe { x.assume_init() };
assert_eq!(value, 42);

// RIGHT — grow a Vec into its spare capacity
fn fill_vec(v: &mut Vec<u8>, extra: usize) {
    v.reserve(extra);
    let spare = v.spare_capacity_mut(); // &mut [MaybeUninit<u8>]
    for slot in spare.iter_mut().take(extra) {
        slot.write(0u8);
    }
    // SAFETY: we initialized `extra` elements of spare capacity.
    unsafe { v.set_len(v.len() + extra) };
}
```

- **`assume_init` is sound only after *every* byte is initialized.** Calling it on partially-initialized memory is UB.
- Arrays: `[const { MaybeUninit::uninit() }; N]` builds the uninit array (any `T`). To convert a fully-init `[MaybeUninit<T>; N]` to `[T; N]` on stable, use `unsafe { core::mem::transmute_copy::<_, [T; N]>(&arr) }` (the two arrays share layout; `transmute_copy` sidesteps the const-generic size check that blocks plain `transmute`). The ergonomic `MaybeUninit::array_assume_init` / `MaybeUninit::transpose` helpers are still nightly-only — don't rely on them in stable code.
- `mem::zeroed()` is technically sound only where all-zero is a valid bit pattern for every field (plain integers, C structs without references) — but `MaybeUninit` is still clearer.

## `// SAFETY:` comments & `# Safety` sections (rules `unsafe-safety-comment`, `lint-unsafe-doc`, cross-link `doc-safety-section.md`)

Two *distinct* obligations, both required:

| Where | Audience | Says |
|-------|----------|------|
| `# Safety` doc section on an `unsafe fn` | the **caller** | preconditions the caller must uphold for the call to be sound |
| `// SAFETY:` comment above each `unsafe {}` block | the **auditor** of this code | why *this* operation upholds the required invariants right here |

```rust
/// Returns the byte at `ptr + offset`.
///
/// # Safety
/// - `ptr` must be valid for reads for at least `offset + 1` bytes.
/// - `ptr` must be non-null and properly aligned for `u8`.
/// - The memory must not be mutated for the duration of this call.
pub unsafe fn read_at(ptr: *const u8, offset: usize) -> u8 {
    // SAFETY: caller guarantees ptr is valid for offset+1 bytes,
    // so ptr.add(offset) is in bounds and dereferenceable.
    unsafe { *ptr.add(offset) }
}
```

Enforce mechanically: `#![warn(clippy::undocumented_unsafe_blocks)]` (add `multiple_unsafe_ops_per_block = "warn"` for one op per block). A good SAFETY comment states (1) what invariants are upheld, (2) why they hold, (3) what breaks if violated.

## `unsafe_op_in_unsafe_fn` — minimize scope (rule `unsafe-minimize-scope`)

In Rust 2024 the `unsafe_op_in_unsafe_fn` lint is a **hard error**: an `unsafe fn` no longer implicitly makes its body unsafe — each unsafe operation still needs its own `unsafe {}`. This is a *feature*: it isolates exactly which lines are dangerous.

```rust
// WRONG — whole body unsafe; safe arithmetic and asserts look equally suspect.
// unsafe fn sum_at(ptr: *const i32, len: usize, index: usize) -> i32 {
//     assert!(index < len);
//     let value = *ptr.add(index); // the ONLY unsafe op, buried in noise
//     value + 1
// }

// RIGHT — safe wrapper, single isolated unsafe op.
fn sum_at(ptr: *const i32, len: usize, index: usize) -> i32 {
    assert!(index < len, "index out of bounds");
    // SAFETY: index < len guarantees ptr.add(index) is within the allocation.
    let value = unsafe { *ptr.add(index) };
    value + 1
}
```

Rules of thumb: prefer a *safe wrapper around a small `unsafe {}`* over an `unsafe fn`. Only make the fn `unsafe` when the caller genuinely must uphold preconditions the type cannot guarantee internally. A single block covering multiple ops is acceptable only when they share the *exact same* precondition.

## Raw pointers

- `*const T` / `*mut T` — no borrow checking, no lifetime, may be null/dangling/unaligned. Creating one is safe; *dereferencing* is unsafe.
- Offset with `ptr.add(n)` / `ptr.wrapping_add(n)` (`add` is UB if it leaves the allocation; `wrapping_add` never UB but result may be unusable).
- Read/write without moving: `ptr.read()`, `ptr.write()`, `ptr.read_unaligned()`, `ptr.write_unaligned()`.
- Build slices/strings from parts: `std::slice::from_raw_parts(ptr, len)`, `str::from_utf8_unchecked`.
- **Provenance matters**: a pointer carries provenance (which allocation it may access). Casting through `usize` and back loses it — Miri with `-Zmiri-strict-provenance` flags this. Prefer `ptr.addr()` / `ptr.with_addr()` over `as usize` when you must inspect addresses.

```rust
let v = vec![1u8, 2, 3, 4];
let ptr = v.as_ptr();
// SAFETY: ptr is valid for v.len() initialized bytes, aligned, not mutated here.
let slice: &[u8] = unsafe { std::slice::from_raw_parts(ptr, v.len()) };
assert_eq!(slice, &[1, 2, 3, 4]);
```

## Interior mutability through `&self` needs `UnsafeCell`

Casting `&self.field` to `*mut` and writing through it is UB unless the field sits inside an `UnsafeCell` — that is the *only* legal way to mutate through a shared reference. This is why `Cell`, `RefCell`, `Mutex`, and atomics are built on `UnsafeCell`. (Patterns Book ch12 §Implementing a Minimal Arena)

```rust
use std::cell::{Cell, UnsafeCell};
use std::alloc::Layout;

/// A bump allocator that allocates through `&self`.
pub struct FixedArena<const N: usize> {
    // UnsafeCell is REQUIRED: without it, mutating via &self is UB.
    buf: UnsafeCell<[u8; N]>,
    offset: Cell<usize>,
}

impl<const N: usize> FixedArena<N> {
    pub const fn new() -> Self {
        FixedArena { buf: UnsafeCell::new([0; N]), offset: Cell::new(0) }
    }

    pub fn alloc<T>(&self, value: T) -> Option<&mut T> {
        let layout = Layout::new::<T>();
        let base = self.buf.get() as *mut u8;
        let current = self.offset.get();
        // Align the *absolute address*, not just the offset: `[u8; N]` has
        // alignment 1, so aligning the offset alone would not guarantee the
        // resulting pointer is aligned for T. `.addr()` reads the address
        // without discarding provenance (strict-provenance clean); we then
        // recover the pointer via `base.add(aligned)`, which keeps provenance.
        let start = base.addr() + current;
        let aligned_addr = (start + layout.align() - 1) & !(layout.align() - 1);
        let aligned = aligned_addr - base.addr();
        let new_offset = aligned + layout.size();
        if new_offset > N {
            return None;
        }
        self.offset.set(new_offset);
        // SAFETY:
        // - `aligned + size <= N` (checked above), so the region is in bounds.
        // - `aligned_addr` is aligned for T, so `base.add(aligned)` is too.
        // - Each alloc returns a unique, non-overlapping region (no aliasing).
        // - UnsafeCell grants permission to mutate through &self.
        unsafe {
            let ptr = base.add(aligned) as *mut T;
            ptr.write(value);
            Some(&mut *ptr)
        }
    }
}
```

## FFI: `unsafe extern` blocks (rule `unsafe-extern-block`)

In **Rust 2024**, `extern` blocks must be written `unsafe extern`, and each item is annotated `safe` or `unsafe` (default). The block's `unsafe` means "I assert these declarations faithfully describe the external ABI" — it does not make calls safe.

```rust
// Rust 2024 style
unsafe extern "C" {
    // Genuinely unsafe: caller must pass a valid null-terminated pointer.
    pub unsafe fn strlen(s: *const std::ffi::c_char) -> usize;
    // Non-overlapping, valid regions required.
    pub unsafe fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
}

/// Safe wrapper — the encapsulation the ecosystem expects.
fn safe_strlen(s: &str) -> usize {
    let c = std::ffi::CString::new(s).expect("string contains null byte");
    // SAFETY: c is a valid null-terminated string, alive for the call.
    unsafe { strlen(c.as_ptr()) }
}
```

Marking an item `safe` is itself a promise: if it is actually unsafe to call, `safe` is unsound and the compiler will not catch it. `bindgen` 0.70+ and `cbindgen` emit `unsafe extern` for 2024. Run `cargo fix --edition` to migrate, then review each item.

**Common FFI type map** (Patterns Book ch12 §FFI Patterns):

| Rust | C | Notes |
|------|---|-------|
| `i32`/`u32` | `int32_t`/`uint32_t` | fixed-width, safe |
| `*const T`/`*mut T` | `const T*`/`T*` | raw pointers |
| `std::ffi::CStr` | `const char*` (borrowed) | null-terminated |
| `std::ffi::CString` | `char*` (owned) | null-terminated |
| `std::ffi::c_void` | `void` | opaque target |
| `Option<extern "C" fn(...)>` | nullable fn pointer | `None` = NULL |

Use `#[repr(C)]` on structs crossing the boundary and `#[repr(transparent)]` on FFI newtypes (see `type-repr-transparent`).

## Exported symbols: `#[unsafe(no_mangle)]` (rule `unsafe-no-mangle-unsafe`)

In Rust 2024, `#[no_mangle]`, `#[export_name]`, and `#[link_section]` are **hard errors** unless wrapped as `#[unsafe(...)]`. Reason: duplicate exported symbol names cause the linker to silently pick one — type-level UB with no diagnostic. The wrapper documents that you accept responsibility for symbol uniqueness; it does **not** require an `unsafe {}` at any call site.

```rust
#[unsafe(no_mangle)]
pub extern "C" fn rust_add(a: i32, b: i32) -> i32 {
    a + b
}

#[unsafe(export_name = "plugin_entry")]
pub extern "C" fn plugin_main() {}
```

## Manual `Send`/`Sync` (rule `unsafe-send-sync-manual`)

`Send`/`Sync` are `unsafe` auto-traits derived correctly for most types. **Auto-derive first** — reach for manual impls only when raw pointers / `Cell` / non-auto fields block the derive.

```rust
use std::marker::PhantomData;

// Opt OUT: PhantomData<*const T> is !Send + !Sync — no unsafe impl needed.
struct IntrusiveRef<T> {
    ptr: *const T,
    _marker: PhantomData<*const T>,
}

// Opt IN with a documented, load-bearing unsafe impl.
struct OwnedBuffer {
    ptr: *mut u8,
    len: usize,
}
// SAFETY: OwnedBuffer owns its allocation exclusively (no aliasing); the
// pointer is valid for the buffer's whole lifetime.
unsafe impl Send for OwnedBuffer {}
// SAFETY: all mutation requires &mut self; shared &refs cannot mutate.
unsafe impl Sync for OwnedBuffer {}
```

- `PhantomData<*const T>` → `!Send + !Sync`. `PhantomData<*mut T>` → also invariant over `T`.
- Every `unsafe impl Send/Sync` needs a `// SAFETY:` comment. An `unsafe impl Send` on a type containing `Rc`/`Cell` is almost certainly unsound — use `Arc`/`Mutex` and let the compiler derive.

## Undefined behavior catalog — never do these (Patterns Book ch12 §Common UB Pitfalls)

| UB | Example | Why |
|----|---------|-----|
| Null deref | `*std::ptr::null::<i32>()` | dereferencing null is always UB |
| Dangling deref | deref after `drop()`/free | memory may be reused |
| Data race | two threads write `static mut` | unsynchronized concurrent writes |
| Uninit read | `MaybeUninit::<String>::uninit().assume_init()` | reads garbage; use `[const { … }; N]` instead |
| `&mut` aliasing | two live `&mut` to same data | violates the aliasing model |
| Invalid value | `transmute::<u8, bool>(2)` | `bool` may only be 0 or 1 |
| Misaligned deref | reading `*const u64` at odd address | use `read_unaligned` |
| Provenance loss | `ptr as usize as *mut T` then deref | pointer has no allocation provenance |
| `mem::uninitialized()` | for `bool`/`&T`/enums | immediate UB (deprecated 1.39) |

## Miri in CI (rule `unsafe-miri-ci`)

Miri is the only tool that **dynamically** detects UB — out-of-bounds, use-after-free, uninit reads, bad provenance, data races, and Stacked/Tree Borrows aliasing violations. std, tokio, and serde all run it. Every crate containing `unsafe` should run `cargo miri test` in CI.

```yaml
# .github/workflows/miri.yml (excerpt)
- run: |
    rustup toolchain install nightly --component miri
    rustup override set nightly
    cargo miri setup
- env: { MIRIFLAGS: "-Zmiri-strict-provenance" }
  run: cargo miri test --all-features
```

- Nightly-only; 100–1000× slower (interpreted) — run a targeted subset if the full suite is impractical.
- `-Zmiri-strict-provenance` catches provenance-violating casts; add `-Zmiri-tree-borrows` for the newer aliasing model.
- Pure-safe crates gain nothing — skip them.

## When to use `unsafe` in production (Patterns Book ch12)

FFI boundaries · performance-critical inner loops (eliding bounds checks) · building primitives (`Vec`, `HashMap`, arenas). **Never in application logic if you can avoid it.** For allocation patterns, arenas (`bumpalo`, `typed-arena`) and slabs (`slab`) encapsulate the unsafe for you and give lifetime-scoped, use-after-free-proof allocation (rule `mem-arena-allocator`; Patterns Book ch12 §Custom Allocators).

---

# Part II — Macros: Code That Writes Code

## Declarative macros (`macro_rules!`)

A `macro_rules!` matches patterns on *syntax* (token trees) and expands to code at compile time — before type checking. (Patterns Book ch13 §Declarative Macros)

```rust
macro_rules! hashmap {
    // key => value pairs, comma-separated, optional trailing comma
    ( $( $key:expr => $value:expr ),* $(,)? ) => {{
        let mut map = std::collections::HashMap::new();
        $( map.insert($key, $value); )*
        map
    }};
}

let scores = hashmap! { "Alice" => 95, "Bob" => 87, };
assert_eq!(scores["Alice"], 95);
```

## Fragment specifiers (rule `macro-fragment-specifiers`)

Capture with the *most precise* specifier, not raw `:tt` — precise specifiers yield targeted errors ("expected expression"), better IDE support, and prevent ambiguous parses.

| Specifier | Matches | Uses |
|-----------|---------|------|
| `:expr` | an expression | values, arithmetic, closures |
| `:ty` | a type | generic helpers, aliases |
| `:ident` | an identifier | field/var names (not paths) |
| `:pat` | a pattern (incl. `A \| B` in 2021+) | match arms |
| `:pat_param` | a pattern, no top-level `\|` | fn param patterns |
| `:path` | a path `a::b::c` | trait bounds, type paths |
| `:literal` | a literal | `42`, `"hi"`, `true` |
| `:block` | a `{ … }` block | inline code injection |
| `:stmt` | a statement | statement-level macros |
| `:meta` | a meta item | `#[derive(Clone)]` content |
| `:vis` | a visibility qualifier | `pub`, `pub(crate)` |
| `:lifetime` | a lifetime `'a` | generic lifetimes |
| `:tt` | any single token tree | last resort; forwarding to other macros |

```rust
macro_rules! debug_val {
    // :expr captures ONE expression; follow-set allows `=>`, `,`, `;` after it.
    ($e:expr) => {
        println!("{} = {:?}", stringify!($e), $e);
    };
}
debug_val!(1 + 2); // correctly rejects `debug_val!(let x = 1)` at the call site
```

**Follow-set restriction**: after `:expr`, `:ty`, `:pat` (and a few others), only a limited set of tokens may follow — most commonly `=>`, `,`, `;`, `|`. Plan separators accordingly. The `$(,)?` trailing-comma pattern is legal because the comma appears as a *separator/terminator*, not in follow position.

## Repetition

`$( … )SEP*` = zero-or-more, `$( … )SEP+` = one-or-more, `$( … )?` = optional. `SEP` is an optional separator token.

```rust
// Generate #[test] functions — a macro use a function CANNOT replace.
fn process(s: &str) -> String { s.to_uppercase() }

macro_rules! test_cases {
    ( $( $name:ident: $input:expr => $expected:expr ),* $(,)? ) => {
        $(
            #[test]
            fn $name() { assert_eq!(process($input), $expected); }
        )*
    };
}

test_cases! {
    test_hello: "hello" => "HELLO",
    test_empty: "" => "",
}
```

## Hygiene & `$crate` (rule `macro-rules-hygiene`)

`macro_rules!` is hygienic for **local bindings** — a `let` introduced inside the macro lives in its own namespace and cannot clash with the caller's identifiers. It is **not** automatic for **item paths**: `crate::helper()` resolves relative to the *caller's* crate and breaks when the macro is used elsewhere. Always use `$crate::` — it expands to the *defining* crate regardless of call site.

```rust
macro_rules! swap {
    ($a:expr, $b:expr) => {{
        let tmp = $a; // does NOT collide with any `tmp` in the caller
        $a = $b;
        $b = tmp;
    }};
}

fn main() {
    let tmp = "outer";
    let (mut x, mut y) = (1, 2);
    swap!(x, y);
    assert_eq!((x, y), (2, 1));
    assert_eq!(tmp, "outer"); // untouched
}
```

```rust
// In a library — WRONG uses `crate::`, RIGHT uses `$crate::`.
pub fn log_value(v: &str) { println!("[log] {v}"); }

#[macro_export]
macro_rules! log {
    ($val:expr) => {
        // $crate always resolves to THIS crate, even after re-export/rename.
        $crate::log_value(&format!("{:?}", $val));
    };
}
```

Hygiene does **not** protect identifiers you pass in as `$name:ident` — those intentionally come from the caller's scope.

## Exporting macros with a clean path (rule `macro-export-crate-path`)

`#[macro_export]` lifts the macro to the crate root; callers use ordinary path imports (`use mycrate::my_macro;`) since Rust 2018 — *not* the legacy, namespace-polluting `#[macro_use] extern crate`. To expose under a module path, add a `pub use` re-export. Document macros with `///` like any public item.

```rust
#[macro_export]
macro_rules! greet {
    ($name:expr) => { println!("hello, {}", $name); };
}
// Optional: re-export under a module for `use mycrate::macros::greet;`
pub mod macros {
    pub use crate::greet;
}
```

## Hiding helpers behind `__private` (rule `macro-private-helpers`)

Exported macros often call helper items. Placing those helpers in the public API pollutes the surface and freezes them under semver. Route all generated references through a `#[doc(hidden)] pub mod __private` (the `serde`/`thiserror` pattern) so internals stay evolvable.

```rust
#[doc(hidden)]
pub mod __private {
    pub use crate::helpers::format_value;
}
mod helpers {
    pub fn format_value(v: &dyn std::fmt::Debug) -> String { format!("{v:?}") }
}

#[macro_export]
macro_rules! debug_print {
    ($val:expr) => {
        // Reference through __private, never through the bare crate root.
        println!("{}", $crate::__private::format_value(&$val));
    };
}
```

## Recursion & `tt` munching (Patterns Book ch13 §Recursive Macros)

Recursive macros consume input one token tree at a time — **`tt` munching**. A base case ends the recursion.

```rust
macro_rules! count {
    () => { 0usize };                                  // base
    ($head:expr $(, $tail:expr)* $(,)?) => {           // recursive
        1usize + count!($($tail),*)
    };
}

fn main() {
    assert_eq!(count!("a", "b", "c", "d"), 4);
    const N: usize = count!(1, 2, 3); // works at compile time
    assert_eq!(N, 3);
}
```

`$($args:tt)*` is the "accept everything and forward it" pattern used by `println!`, `format!`, `vec!` — the one legitimate place for raw `:tt`.

## When a macro is the right tool (rule `macro-prefer-functions`; Patterns Book ch13 §When Not to Use Macros)

Macros run on tokens *before* type checking: they bypass inference, resist IDE navigation, produce opaque errors, slow incremental compilation, and cannot be passed as values. **Prefer a generic function or trait** unless you genuinely need one of the "yes" rows below.

| Situation | Macro? |
|-----------|--------|
| Fixed argument count, any types | No — generics |
| Truly variadic args (`vec!`, `println!`) | Yes |
| Implementing a trait for many unrelated types | Yes — `macro_rules!` impl |
| DSL / embedded syntax (SQL, HTML, regex) | Yes — proc-macro |
| Compile-time format/string validation | Yes |
| Boilerplate a `#[derive]` could generate | Yes — derive proc-macro |
| Simple computation / conversion | No — function or trait |

```rust
// WRONG — nothing variadic, no DSL, no trait impl; a macro adds only pain.
// macro_rules! double { ($x:expr) => { $x * 2 }; }

// RIGHT — a generic function is clearer, debuggable, equally fast.
#[inline]
fn double<T: std::ops::Add<Output = T> + Copy>(x: T) -> T { x + x }
```

Cross-link `anti-over-abstraction`, `type-generic-bounds.md`, `design-patterns.md`.

## Procedural macros — the three kinds (Patterns Book ch13 §Procedural Macros)

Proc macros are Rust functions that transform `TokenStream`s. They must live in a crate with `proc-macro = true`.

```rust
// 1. Derive:        #[derive(Serialize)] struct Config { … }
// 2. Attribute:     #[route(GET, "/users")] async fn list() { … }
// 3. Function-like: let q = sql!(SELECT * FROM users);
```

Workflow: `TokenStream` (raw) → `syn::parse` (typed AST) → inspect/transform → `quote!` (generate) → `TokenStream` (back to compiler).

| Crate | Role | Key types |
|-------|------|-----------|
| `proc-macro` | compiler interface | `TokenStream` |
| `syn` (v2) | parse Rust into an AST | `DeriveInput`, `ItemFn`, `Type` |
| `quote` | generate tokens from templates | `quote!{}`, `#var` interpolation |
| `proc-macro2` | span-aware bridge, testable | `TokenStream`, `Span` |

## Writing a derive macro with `syn` + `quote` (rules `macro-proc-syn-quote`, `macro-proc-two-crate`)

A `proc-macro = true` crate can **only** export proc macros — no regular types/traits/fns. If your library needs both, split into `mycrate-derive` (proc-macro) + `mycrate` (facade that re-exports and holds the runtime items). Generated code refers to the facade via `::mycrate::__private::…` so the impl-crate version stays invisible.

```toml
# mycrate-derive/Cargo.toml
[lib]
proc-macro = true
[dependencies]
syn = { version = "2", features = ["derive"] }  # "full" only if you need it
quote = "1"
proc-macro2 = "1"
```

```rust
// mycrate-derive/src/lib.rs (illustrative — requires a proc-macro crate)
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(Greet)]
pub fn derive_greet(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    // Forward generics correctly so `#[derive]` works on generic types.
    let (impl_g, ty_g, where_c) = input.generics.split_for_impl();
    let name_str = name.to_string();
    quote! {
        impl #impl_g ::mycrate::Greet for #name #ty_g #where_c {
            fn greet(&self) -> String {
                ::mycrate::__private::format_greeting(#name_str)
            }
        }
    }
    .into()
}
```

`split_for_impl()` is essential — it emits `impl<T: Bound> Trait for Ty<T> where …` so derives work on generic types. Because `proc-macro2::TokenStream` runs outside the compiler, factor generation into a `fn(&DeriveInput) -> proc_macro2::TokenStream` and unit-test it with `syn::parse_quote!`. Use `cargo expand` to see what any macro expands to — invaluable for debugging.

## Report proc-macro errors as spanned diagnostics (rule `macro-proc-error-spans`)

A `panic!`/`.unwrap()`/`.expect()` in a proc-macro produces "proc macro panicked" with no source location. Instead return a `syn::Error` converted to a compile error — it points directly at the offending span, like an ordinary compiler error.

```rust
// illustrative — proc-macro crate
use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error};

#[proc_macro_derive(MyTrait)]
pub fn derive_my_trait(input: TokenStream) -> TokenStream {
    inner(input).unwrap_or_else(|e| e.to_compile_error().into())
}

fn inner(input: TokenStream) -> Result<TokenStream, Error> {
    // NOT `parse_macro_input!` here: that macro expands to `return <bare
    // TokenStream>` on error, which mismatches this fn's `Result` return type.
    // `syn::parse` yields a `Result`, so `?` propagates the error correctly.
    let input = syn::parse::<DeriveInput>(input)?;
    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        // Attach the error to a specific span in the user's code.
        _ => return Err(Error::new_spanned(
            &input.ident, "MyTrait can only be derived on structs")),
    };
    let name = &input.ident;
    let _ = fields;
    Ok(quote! { impl MyTrait for #name {} }.into())
}
```

- Wrap logic in a `Result<TokenStream, syn::Error>` helper; convert at the entry with `.unwrap_or_else(|e| e.to_compile_error().into())`.
- `Error::new_spanned(tokens, msg)` attaches to a token/AST node; `Error::new(span, msg)` when you only have a `Span`; `Error::combine` reports multiple problems at once.
- Messages: lowercase, no trailing punctuation (compiler style; cf. `err-lowercase-msg`).

## Derive macros in practice (Patterns Book ch13 §Derive Macros)

Use derive macros *liberally* — they eliminate error-prone boilerplate. Writing your own is advanced; reach for existing ones first: `Debug`/`Clone`/`Copy`/`PartialEq`/`Eq`/`Hash` (std), `Serialize`/`Deserialize` (serde), `Error` (thiserror), `Parser` (clap), builders (derive_builder). Study `thiserror` or `derive_more` source before authoring your own.

---

## Rules & anti-patterns checklist

**Unsafe**
- `unsafe-safety-comment` — DO write a `// SAFETY:` comment on every `unsafe {}` block AND a `# Safety` doc section on every public `unsafe fn`. They target auditors vs callers respectively; both are required for soundness.
- `lint-unsafe-doc` — DO enable `#![warn(clippy::undocumented_unsafe_blocks)]` (and `multiple_unsafe_ops_per_block`). Mechanically catches undocumented/overstuffed unsafe.
- `unsafe-minimize-scope` — DO shrink each `unsafe {}` to the single operation that needs it; prefer a safe wrapper over an `unsafe fn`. Rust 2024's `unsafe_op_in_unsafe_fn` is a hard error — even inside `unsafe fn`, each op needs its own block.
- `unsafe-maybeuninit` — DON'T use `mem::uninitialized()`/`mem::zeroed()` for types with validity invariants; use `MaybeUninit<T>`. The old calls are instant UB. Build uninit arrays with `[const { MaybeUninit::uninit() }; N]`.
- `unsafe-extern-block` — DO write `unsafe extern "C" { … }` with per-item `safe`/`unsafe` (Rust 2024). Makes FFI contracts auditable; `safe` is a promise the compiler can't verify.
- `unsafe-no-mangle-unsafe` — DO write `#[unsafe(no_mangle)]`/`#[unsafe(export_name=…)]`/`#[unsafe(link_section=…)]` (Rust 2024). Duplicate symbols are silent linker UB; the wrapper flags the risk (no call-site `unsafe {}` needed).
- `unsafe-send-sync-manual` — DON'T `unsafe impl Send/Sync` without a `// SAFETY:` justification; prefer auto-derive or `PhantomData<*const T>` to opt out. A wrong impl is an uncatchable data race.
- `unsafe-miri-ci` — DO run `cargo miri test` in CI for every crate containing `unsafe`. Only tool that dynamically finds UB (aliasing, provenance, uninit reads).

**Macros**
- `macro-prefer-functions` — DON'T reach for a macro when a generic function/trait works. Macros bypass type inference, resist IDE tooling, and slow builds. Only for variadics, DSLs, cross-type impls, compile-time checks, or derive-style boilerplate.
- `macro-fragment-specifiers` — DO capture with precise specifiers (`:expr`, `:ty`, `:ident`…), not raw `:tt`, where possible. Better errors, fewer ambiguous parses. Mind the follow-set restriction after `:expr`/`:ty`/`:pat`.
- `macro-rules-hygiene` — DO use `$crate::` for every path to your crate's items. `crate::` resolves against the *caller's* crate and breaks on cross-crate use; hygiene covers local `let` bindings automatically.
- `macro-export-crate-path` — DO `#[macro_export]` + `use mycrate::my_macro;` path imports; DON'T use legacy `#[macro_use] extern crate`. Path imports are explicit and IDE-friendly.
- `macro-private-helpers` — DO route macro-generated helper references through `#[doc(hidden)] pub mod __private`. Keeps the public API clean and helpers exempt from semver.
- `macro-proc-two-crate` — DO split proc macros into a `proc-macro = true` crate + a facade that re-exports and holds runtime items. A proc-macro crate can export *only* proc macros.
- `macro-proc-syn-quote` — DO build proc macros with `syn` + `quote` + `proc-macro2`; DON'T hand-iterate raw tokens. Typed AST, quasi-quoting, and off-compiler unit tests. Enable only the `syn` features you use.
- `macro-proc-error-spans` — DON'T `panic!`/`.unwrap()` in a proc macro; return `syn::Error::to_compile_error()`. Panics give "proc macro panicked" with no location; spanned errors point at the user's code.

## Gotchas / footguns

- **`unsafe` is not a borrow-check escape hatch.** It unlocks only the five superpowers; aliasing/lifetime rules still apply — Miri enforces them at the pointer level.
- **`assume_init` on partially-initialized memory is UB**, even if you "know" you'll fill the rest later. Initialize *every* byte first.
- **`&self` + raw-pointer write without `UnsafeCell` is UB**, even when logically single-threaded. Only `UnsafeCell` legalizes mutation through a shared reference.
- **Arenas (`bumpalo`, `FixedArena`) do NOT run destructors** on allocated items — types with meaningful `Drop` (files, sockets) leak. Only arena-allocate `Drop`-free types, or drop manually first.
- **Provenance loss compiles clean but Miri fails**: `ptr as usize as *mut T` strips the allocation provenance; the resulting deref is UB under strict provenance.
- **`$x:expr` parses greedily**: `1 + 2` is ONE expression fragment, not three tokens. Likewise `$x:ty` swallows `Vec<String>` whole and can't be followed by `+`/`<`.
- **`:pat` vs `:pat_param`**: in Rust 2021+, `:pat` matches `A | B`; use `:pat_param` for a single top-level pattern (e.g. fn parameters).
- **Macro hygiene doesn't help identifiers you pass in**: a `$name:ident` argument is resolved in the *caller's* scope on purpose — that's how macros build accessible items.
- **`#[macro_export]` always places the macro at the crate root**, ignoring the module it's written in. Add a `pub use` re-export to expose it under a module path.
- **`crate::` inside a macro is a time bomb**: it works in the defining crate's own tests, then breaks the moment a downstream crate calls the macro. Use `$crate::`.
- **A wrong `safe` annotation in `unsafe extern` is unsound and uncatchable** — the compiler trusts you. Same for a wrong `unsafe impl Send/Sync`.
- **`cargo fix --edition` mechanizes the 2024 unsafe-attribute/extern migration** but you must still review each `safe`/`unsafe` and confirm exported symbols are unique.
- **`panic!` in a proc macro loses the span**; always convert `syn::Error` to a compile error instead.

## Cheat-sheet

| Task | Do this | Rule / source |
|------|---------|---------------|
| Uninitialized memory | `MaybeUninit<T>`, `[const { MaybeUninit::uninit() }; N]` | `unsafe-maybeuninit` |
| Justify unsafe | `// SAFETY:` block + `# Safety` doc on `unsafe fn` | `unsafe-safety-comment` |
| Minimize unsafe | smallest `unsafe {}`; safe wrapper over `unsafe fn` | `unsafe-minimize-scope` |
| FFI import (2024) | `unsafe extern "C" { unsafe fn … }` | `unsafe-extern-block` |
| Export symbol (2024) | `#[unsafe(no_mangle)] pub extern "C" fn …` | `unsafe-no-mangle-unsafe` |
| Mutate via `&self` | store field in `UnsafeCell<T>` | Patterns ch12 |
| Manual thread-safety | auto-derive; else documented `unsafe impl`; `PhantomData<*const T>` to opt out | `unsafe-send-sync-manual` |
| Verify unsafe | `cargo miri test` with `-Zmiri-strict-provenance` in CI | `unsafe-miri-ci` |
| Small code-gen | `macro_rules!` with precise fragment specifiers | `macro-fragment-specifiers` |
| Variadic / DSL / derive | macro (else use a generic fn) | `macro-prefer-functions` |
| Reference own crate in macro | `$crate::path` | `macro-rules-hygiene` |
| Export a macro | `#[macro_export]` + `use crate::mac;` path | `macro-export-crate-path` |
| Hide macro helpers | `#[doc(hidden)] pub mod __private` | `macro-private-helpers` |
| Count/recurse over tokens | `tt` munching with a base case | Patterns ch13 |
| Proc-macro crate | `proc-macro = true` + facade re-export | `macro-proc-two-crate` |
| Parse/generate in proc-macro | `syn` + `quote` + `proc-macro2`; `split_for_impl()` | `macro-proc-syn-quote` |
| Proc-macro errors | `syn::Error::to_compile_error()`, never `panic!` | `macro-proc-error-spans` |
| Inspect macro output | `cargo expand` | Patterns ch13 |

Fragment specifier quick pick: `:expr` values · `:ty` types · `:ident` names · `:pat`/`:pat_param` patterns · `:path` paths · `:literal` literals · `:block` blocks · `:stmt` statements · `:meta` attrs · `:vis` visibility · `:lifetime` lifetimes · `:tt` last-resort forwarding.

Repetition: `$( … )*` zero+ · `$( … )+` one+ · `$( … )?` optional · `$( … ),*` comma-separated · `$(,)?` optional trailing comma.

See also: `api-guidelines.md` (C-* — e.g. C-NEWTYPE for FFI newtypes, C-DEBUG for derives), `microsoft-guidelines.md` (M-*), `design-patterns.md` (FFI idioms, newtype), `style-guide.md` (Cargo.toml/rustfmt), `reference-notation.md` (grammar of the fragment/repetition syntax).
