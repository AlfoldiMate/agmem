# Newtype, Type-State & PhantomData

Techniques for making illegal states *unrepresentable at compile time* with zero
runtime cost: the **newtype** pattern (wrapping for invariants, units, IDs, orphan-rule
escapes, FFI layout), the **type-state** pattern (encoding a state machine in the type so
invalid transitions do not compile), and **PhantomData** (variance, drop-check, lifetime
branding, unit-of-measure tags). Consult this file when designing a domain type, a
protocol/state-machine API, a builder that enforces required fields, a zero-cost unit or
handle abstraction, or any wrapper over a raw pointer. Canonical rule sources live in
`api-guidelines.md` (C-* codes), `microsoft-guidelines.md` (M-* codes), and
`design-patterns.md`; this file adds the deep engineering material and a rule catalogue.

---

## 1. Newtype: zero-cost type safety

A newtype is a single-field tuple struct wrapping an existing type to create a **distinct**
type. It compiles to the same layout as the inner type — pure compile-time safety, no
runtime overhead (Patterns Book ch03 §Newtype). Two motivations dominate: *distinguishing
otherwise-identical values* (IDs, units) and *enforcing invariants* (validated types).

### 1.1 Distinguishing values the compiler would otherwise conflate

Raw primitives carry no semantics; a function taking `(u64, u64)` invites argument swaps
that compile fine and fail at runtime (`api-newtype-safety`, `type-newtype-ids`).

```rust
// WRONG — swappable, compiles, wrong at runtime:
fn create_user(name: String, email: String, age: u32, employee_id: u32) {}
// create_user(name, email, employee_id, age); // compiles, silent bug

// RIGHT — distinct types, mistakes are compile errors:
struct UserName(String);
struct Email(String);
struct Age(u32);
struct EmployeeId(u32);

fn create_user2(name: UserName, email: Email, age: Age, id: EmployeeId) {}
// create_user2(name, email, EmployeeId(42), Age(30)); // ❌ expected Age, found EmployeeId
```

**ID newtypes** should derive the full "value-type" trait set so they work as map keys and
sort (`type-newtype-ids`). Prefer `#[serde(transparent)]` so JSON stays `123`, not `{"0":123}`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub u64);

impl UserId {
    pub const fn new(id: u64) -> Self { Self(id) }
    pub const fn get(self) -> u64 { self.0 }
}

impl From<u64> for UserId {
    fn from(id: u64) -> Self { Self(id) }
}
```

For families of ID types, a small declarative macro removes boilerplate while keeping each
type distinct (`type-newtype-ids`):

```rust
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub u64);
        impl $name {
            pub const fn new(id: u64) -> Self { Self(id) }
            pub const fn get(self) -> u64 { self.0 }
        }
        impl From<u64> for $name {
            fn from(id: u64) -> Self { Self(id) }
        }
    };
}
define_id!(PostId);
define_id!(CommentId);
```

> **Zero-cost argument** (`api-newtype-safety` §Zero-Cost): `size_of::<Miles>() ==
> size_of::<f64>()`. The wrapper vanishes after monomorphization; only the *type-checking*
> survives. This is the core "zero-cost abstraction" claim — safety you do not pay for.

### 1.2 Enforcing invariants — parse, don't validate

A **validated newtype** can only be constructed through a fallible constructor, so once you
hold one its invariant is guaranteed everywhere — no re-checking, no "did someone validate
this?" (`type-newtype-validated`, `api-parse-dont-validate`; see `api-guidelines.md`
C-VALIDATE / "parse don't validate"). The private field is the enforcement mechanism.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String); // field is private → cannot bypass `parse`

#[derive(Debug)]
pub enum EmailError { Invalid }

impl Email {
    pub fn parse(s: impl Into<String>) -> Result<Self, EmailError> {
        let s = s.into();
        if s.contains('@') && s.len() > 3 { Ok(Email(s)) } else { Err(EmailError::Invalid) }
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

// Downstream code needs no checks — the type *is* the proof of validity:
fn send(to: &Email, _body: &str) { let _ = to.as_str(); }
```

Use `Option` for a pure yes/no invariant, `Result` when the failure carries context
(`api-parse-dont-validate` more-examples): `Port::new(u16) -> Option<Port>`,
`Percentage::new(f64) -> Result<Percentage, RangeError>` for a `0.0..=100.0` bound.

Validate at the serde boundary by hand-writing `Deserialize` (or `#[serde(try_from = ...)]`)
so deserialization runs the same constructor (`type-newtype-validated` §serde):

```rust
# use serde::Deserialize;
# #[derive(Clone)] pub struct Email(String);
# impl Email { fn new(s: &str) -> Result<Self, String> { Ok(Email(s.into())) } }
impl<'de> Deserialize<'de> for Email {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Email::new(&s).map_err(serde::de::Error::custom)
    }
}
```

### 1.3 `Deref` on a newtype — power and pitfall

Implementing `Deref` auto-coerces the wrapper to `&Inner`, giving *every* inner method for
free. This **punches a hole through the abstraction boundary** — the whole point of the
newtype (Patterns Book ch03 §impl Deref; `type-deref-coercion`; `api-guidelines.md` C-DEREF).

| `Deref` is appropriate | `Deref` is an anti-pattern |
|---|---|
| Smart pointers: `Box<T>`, `Arc<T>`, `MutexGuard<T>` — the wrapper's *purpose* is to behave like `T` | Domain types with invariants: `Email` derefing to `&str` leaks `.trim()`, `.split_at()` — none preserve "contains @" |
| Transparent owned→borrowed: `String`→`str`, `Vec<T>`→`[T]`, `PathBuf`→`Path` | Restricted APIs: `Password` derefing to `str` leaks `.as_bytes()`, `Debug`, exactly what you hide |
| Your newtype genuinely *is* the inner type with no restriction | Fake OOP inheritance: `AdminUser` deref→`User` (explicitly discouraged) |

`DerefMut` **doubles the risk** — callers mutate the inner value directly, bypassing every
constructor check (`*port = 0;`). Only add it when the inner type has no invariants.

> **Rule of thumb** (Patterns Book ch03): if the newtype exists to *add type safety* or
> *restrict the API*, do **not** implement `Deref`. If it exists to *add capabilities* while
> keeping the full inner surface (smart pointer), `Deref` is right.

**Prefer explicit delegation** — expose only the methods that preserve the invariant, and
implement `AsRef<str>` / `Borrow<str>` for trait compatibility without coercion surprises:

```rust
# pub struct Email(String);
impl Email {
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn domain(&self) -> &str { self.0.split('@').nth(1).unwrap_or("") }
    // .trim(), .replace() deliberately NOT exposed
}
impl AsRef<str> for Email {
    fn as_ref(&self) -> &str { &self.0 }
}
```

Decision tree (Patterns Book ch03):

```text
Want ALL inner methods callable on the wrapper?
  ├─ YES → Does the type enforce invariants / restrict the API?
  │         ├─ NO  → impl Deref ✅   (smart-pointer / transparent wrapper)
  │         └─ YES → don't impl Deref ❌ (invariant leaks)
  └─ NO  → don't impl Deref ❌         (use AsRef / explicit delegation)
```

### 1.4 Newtype for the orphan rule (coherence)

Coherence forbids `impl ForeignTrait for ForeignType` — one of the two must be local
(`trait-coherence-newtype`; `design-patterns.md` "Newtype"). Wrap the foreign type locally,
then implement the foreign trait on the wrapper. Add `From`/`Into` + `inner()`/`into_inner()`:

```rust
use std::fmt;

#[repr(transparent)]
struct CommaSeparated(Vec<i32>);

impl fmt::Display for CommaSeparated {
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
impl From<CommaSeparated> for Vec<i32> { fn from(w: CommaSeparated) -> Self { w.0 } }
```

The orphan rule rejects `impl<T> ForeignTrait for ForeignType<T>` even with a type
parameter — the newtype wrapper is the standard escape hatch, and also the way to add impls
to types from transitive dependencies you do not control.

### 1.5 `#[repr(transparent)]` — layout guarantee for FFI and niches

`#[repr(transparent)]` guarantees the newtype has the **same size, alignment, and ABI** as
its single non-zero-sized field (`type-repr-transparent`). Required for passing newtypes
across `extern "C"`, for pointer casts / `transmute`, and to inherit niche optimizations.
`PhantomData` fields are zero-sized and allowed alongside the one real field.

```rust
use std::marker::PhantomData;
use std::mem::size_of;

#[repr(transparent)]
struct FileDescriptor(i32);           // safe to pass to extern "C" fns expecting c_int

#[repr(transparent)]
struct TypedHandle<T> { raw: u64, _marker: PhantomData<T> }

fn _checks<T>() {
    assert_eq!(size_of::<TypedHandle<T>>(), size_of::<u64>()); // PhantomData is free
}
```

| Scenario | `#[repr(transparent)]`? |
|---|---|
| FFI newtype wrappers, type-safe handles | Yes (required) |
| `NonZero*` wrapper wanting `Option<T>` niche | Yes |
| Pure-Rust newtypes | Optional (harmless) |
| Multi-field structs | N/A (only single non-ZST field) |

### 1.6 `NonZero*` — forbid zero, get a free `Option` niche

`std::num::NonZeroU32` (and all integer siblings) make zero unrepresentable and push the
zero-check to construction (`num-nonzero`). The zero bit-pattern becomes a niche, so
`Option<NonZeroU32>` is the same size as `u32`.

```rust
use std::num::NonZeroU32;
use std::mem::size_of;

fn divide(numerator: u32, denominator: NonZeroU32) -> u32 {
    numerator / denominator.get() // always safe — cannot be zero
}
const _: () = assert!(size_of::<Option<NonZeroU32>>() == size_of::<u32>());
```

`NonZero*` has no `Add`/`Sub` (results could be zero): extract with `.get()`, compute,
reconstruct with `NonZeroU32::new(result)?`.

### 1.7 Numeric-format and Display/Debug hygiene for newtypes

- A numeric newtype should forward `LowerHex`/`UpperHex`/`Octal`/`Binary` to its inner value
  so `{:x}` etc. keep working (`type-numeric-fmt`; C-NUM-FMT). Forward via
  `fmt::LowerHex::fmt(&self.0, f)` so width/`#`/`0` flags are honoured.
- Always `#[derive(Debug)]` (C-DEBUG). **Hand-write `Display`; never derive it and never make
  it call `{:?}`** (`type-display-vs-debug`). `Debug` is for developers/logs; `Display` is
  the human-facing message and is required by `std::error::Error`.

---

## 2. Type-state: compile-time protocol enforcement

The type-state pattern encodes a state machine in the type parameter of a struct. Each
transition **consumes `self`** and returns a *different* type, so operations are only
callable in the correct state — invalid transitions are compile errors, not runtime panics
(Patterns Book ch03 §Type-State; `api-typestate`; `design-patterns.md`). Compare to
C++/C# runtime guards (`if (!authenticated) throw`): Rust moves the check to compile time.

### 2.1 Marker states + `PhantomData<State>`

States are zero-sized marker structs; the wrapper carries a `PhantomData<State>` so the
parameter is "used". Data lives once, in the wrapper; only the marker changes per transition.

```rust
use std::marker::PhantomData;

struct Disconnected;
struct Connected;
struct Authenticated;

struct Connection<State> {
    address: String,
    _state: PhantomData<State>,
}

impl Connection<Disconnected> {
    fn new(address: &str) -> Self {
        Connection { address: address.to_string(), _state: PhantomData }
    }
    fn connect(self) -> Connection<Connected> {
        Connection { address: self.address, _state: PhantomData }
    }
}
impl Connection<Connected> {
    fn authenticate(self, _token: &str) -> Connection<Authenticated> {
        Connection { address: self.address, _state: PhantomData }
    }
}
impl Connection<Authenticated> {
    fn request(&self, path: &str) -> String { format!("GET {} from {}", path, self.address) }
}

fn demo() {
    let conn = Connection::new("api.example.com");
    // conn.request("/data"); // ❌ no method `request` on Connection<Disconnected>
    let conn = conn.connect();
    let conn = conn.authenticate("secret");
    let _ = conn.request("/data"); // ✅ only after Authenticated
}
```

Because each transition consumes `self`, the old handle is *moved out* — you cannot use a
stale state, and calling `connect()` twice is a use-after-move compile error.

### 2.2 State-as-data variant (payload per state)

When each state owns different data, put the payload *in* the state struct instead of a
`PhantomData` marker (`api-typestate`, `type-enum-states` contrast). No `PhantomData` needed
because the type parameter appears in a real field:

```rust
# struct TcpStream; struct Session; #[derive(Debug)] struct Error;
# impl TcpStream { fn connect(_: &str) -> Result<Self, Error> { Ok(TcpStream) }
#   fn write_all(&mut self, _: &[u8]) -> Result<(), Error> { Ok(()) } }
# fn do_auth(_: &TcpStream, _: &str) -> Result<Session, Error> { Ok(Session) }
struct Disconnected;
struct Connected { socket: TcpStream }
struct Authenticated { socket: TcpStream, session: Session }

struct Connection<S> { state: S }

impl Connection<Disconnected> {
    fn new() -> Self { Connection { state: Disconnected } }
    fn connect(self, addr: &str) -> Result<Connection<Connected>, Error> {
        Ok(Connection { state: Connected { socket: TcpStream::connect(addr)? } })
    }
}
impl Connection<Connected> {
    fn authenticate(self, pw: &str) -> Result<Connection<Authenticated>, Error> {
        let session = do_auth(&self.state.socket, pw)?;
        Ok(Connection { state: Authenticated { socket: self.state.socket, session } })
    }
}
impl Connection<Authenticated> {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> { self.state.socket.write_all(data) }
}
```

### 2.3 Builder with type-state — enforce required fields

A type-state builder makes "forgot a required field" a compile error rather than a runtime
`unwrap` panic (Patterns Book ch03 §Builder; `api-typestate` §Builder;
`design-patterns.md` builder). Marker types track which required fields are set; `build()`
exists only on the terminal `Ready` state, so the `.unwrap()`s inside it can never fire.

```rust
use std::marker::PhantomData;

struct NeedsName;
struct NeedsPort;
struct Ready;

struct ServerConfig<State> {
    name: Option<String>,
    port: Option<u16>,
    max_connections: usize,      // optional, defaulted
    _state: PhantomData<State>,
}

impl ServerConfig<NeedsName> {
    fn new() -> Self {
        ServerConfig { name: None, port: None, max_connections: 100, _state: PhantomData }
    }
    fn name(self, name: &str) -> ServerConfig<NeedsPort> {
        ServerConfig { name: Some(name.into()), port: self.port,
                       max_connections: self.max_connections, _state: PhantomData }
    }
}
impl ServerConfig<NeedsPort> {
    fn port(self, port: u16) -> ServerConfig<Ready> {
        ServerConfig { name: self.name, port: Some(port),
                       max_connections: self.max_connections, _state: PhantomData }
    }
}
impl ServerConfig<Ready> {
    fn max_connections(mut self, n: usize) -> Self { self.max_connections = n; self }
    fn build(self) -> Server {
        Server { name: self.name.unwrap(), port: self.port.unwrap(),
                 max_connections: self.max_connections } // unwraps provably safe
    }
}
struct Server { name: String, port: u16, max_connections: usize }

fn demo() {
    let _ = ServerConfig::new().name("srv").port(8080).max_connections(500).build();
    // ServerConfig::new().port(8080);       // ❌ no `port` on NeedsName
    // ServerConfig::new().name("x").build(); // ❌ no `build` on NeedsPort
}
```

> Contrast with the *non-consuming* builder (`api-builder-pattern`, `api-builder-must-use`)
> where every setter takes `&mut self` and `build()` returns `Result` — that validates at
> runtime. Reach for type-state builders when forgetting a field must be a *compile* error;
> use the ordinary builder when fields are genuinely optional or order is free.

### 2.4 Type-state vs enum-state — pick the right axis

Both make illegal states unrepresentable; they differ on *when* the state is known
(`type-enum-states` vs `api-typestate`):

| | Type-state (`Foo<State>`) | Enum state (`enum State { ... }`) |
|---|---|---|
| State known at | compile time (static) | runtime (dynamic) |
| Invalid *transition* | compile error | must be handled in `match` |
| Can store heterogeneous handles in a `Vec` | No (different types) | Yes (one type) |
| Runtime dispatch on current state | No | Yes (`match self.state`) |
| Cost | zero (markers erased) | one discriminant + branch |
| Use when | protocol order enforced by API, state flows linearly through moves | state changes based on runtime events, need collections/serialization |

Enum states also eliminate impossible *combinations* (the boolean-flag soup of
`is_connected && is_disconnected`) — use them when the state itself is data. Use type-state
when the *sequence of calls* is the thing to police.

### 2.5 Config-trait pattern — taming generic-parameter explosion

When a struct accumulates many trait-constrained generics (`Foo<S: SpiBus, C: ComPort,
I: I3cBus, ...>`), every `impl` block, function, and caller must repeat the whole list;
adding one bus edits every mention (Patterns Book ch03 §Config Trait). Bundle the associated
types into **one** trait so the struct has a **single** generic parameter forever:

```rust
# #[derive(Debug)] enum BusError { Timeout }
trait SpiBus { fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> Result<(), BusError>; }
trait ComPort { fn send(&self, data: &[u8]) -> Result<usize, BusError>; }

// One associated type per component:
trait BoardConfig {
    type Spi: SpiBus;
    type Com: ComPort;
}

// Exactly ONE generic parameter, no matter how many components:
struct DiagController<Cfg: BoardConfig> {
    spi: Cfg::Spi,
    com: Cfg::Com,
}

impl<Cfg: BoardConfig> DiagController<Cfg> {
    fn new(spi: Cfg::Spi, com: Cfg::Com) -> Self { DiagController { spi, com } }
    fn read_flash_id(&self) -> Result<u32, BusError> {
        let mut id = [0u8; 4];
        self.spi.transfer(&[0x9F], &mut id)?;
        Ok(u32::from_be_bytes(id))
    }
}

// One impl selects the whole concrete hardware layer:
struct ProductionBoard;
struct RealSpi; struct RealCom;
# impl SpiBus for RealSpi { fn transfer(&self, _: &[u8], rx: &mut [u8]) -> Result<(), BusError> { rx.copy_from_slice(&[0xEF,0x40,0x18,0x00]); Ok(()) } }
# impl ComPort for RealCom { fn send(&self, _: &[u8]) -> Result<usize, BusError> { Ok(0) } }
impl BoardConfig for ProductionBoard {
    type Spi = RealSpi;
    type Com = RealCom;
}
```

Adding a bus later touches only `BoardConfig` (one associated type) and `DiagController`
(one field) — **no downstream signature changes**. Swap the entire hardware layer for tests
by defining `TestBoard` with mock impls, no `#[cfg]`. Fully static dispatch (no vtables).
This is Substrate/Polkadot's `Config`-trait technique for 20+ associated types.

| Use config trait | Prefer alternative |
|---|---|
| 3+ trait-constrained generics on one struct | 1–2 generics → direct generics |
| Swap whole hardware/platform layer | Runtime polymorphism → `dyn Trait` |
| Components form a natural group (board, platform) | Open-ended plugin system → type-map / `Any` |

### 2.6 Dual-axis type-state — capability × state via marker traits

Real systems vary on two axes: *who provides it* (vendor) and *what state it is in*. Encode
both with a `Handle<V, S>` where `impl` blocks are gated on a vendor **trait bound** and a
state **marker trait** (Patterns Book ch03 §Dual-Axis). Each `impl` block is one cell of the
(vendor × state) capability matrix.

```rust
use std::marker::PhantomData;

struct Locked; struct Unlocked; struct ExtendedUnlocked;

// Marker traits express *which capability* a state grants — extensible:
trait HasRegAccess {}
impl HasRegAccess for Unlocked {}
impl HasRegAccess for ExtendedUnlocked {}
trait HasMemAccess {}
impl HasMemAccess for ExtendedUnlocked {}

trait JtagVendor {
    fn raw_unlock(&mut self);
    fn raw_read_reg(&self, addr: u32) -> u32;
}
trait JtagMemoryVendor: JtagVendor {          // super-trait = "strictly more capable"
    fn raw_extended_unlock(&mut self);
    fn raw_read_memory(&self, addr: u64, buf: &mut [u8]);
}

struct Jtag<V, S = Locked> { vendor: V, _state: PhantomData<S> }

impl<V: JtagVendor> Jtag<V, Locked> {
    fn new(vendor: V) -> Self { Jtag { vendor, _state: PhantomData } }
    fn unlock(mut self) -> Jtag<V, Unlocked> {
        self.vendor.raw_unlock();
        Jtag { vendor: self.vendor, _state: PhantomData }
    }
}
// Register I/O: ANY vendor, ANY state granting register access:
impl<V: JtagVendor, S: HasRegAccess> Jtag<V, S> {
    fn read_reg(&self, addr: u32) -> u32 { self.vendor.raw_read_reg(addr) }
}
// Extended unlock: only memory-capable vendors, only from Unlocked:
impl<V: JtagMemoryVendor> Jtag<V, Unlocked> {
    fn extended_unlock(mut self) -> Jtag<V, ExtendedUnlocked> {
        self.vendor.raw_extended_unlock();
        Jtag { vendor: self.vendor, _state: PhantomData }
    }
}
// Memory I/O: only memory-capable vendors, only ExtendedUnlocked:
impl<V: JtagMemoryVendor, S: HasMemAccess> Jtag<V, S> {
    fn read_memory(&self, addr: u64, buf: &mut [u8]) { self.vendor.raw_read_memory(addr, buf) }
}
```

Marker traits (not concrete states) in the bound let you add a new state (`DebugHalted`) and
grant it register access with one line — every register method works automatically. Generic
functions bind only the axes they care about: `fn read_idcode<V: JtagVendor, S:
HasRegAccess>(j: &Jtag<V, S>)` accepts either register-capable state. When you reach a third
independent axis (`Handle<V, S, D, T>`), collapse the vendor axis into a config trait
(§2.5), keeping only the state axis generic: `Handle<Cfg, S>`.

---

## 3. PhantomData — types that carry no data

`PhantomData<T>` is a zero-sized type telling the compiler "this struct is logically
associated with `T`" without storing one. It affects **variance**, **drop-check**, and
**auto-trait inference** at zero runtime cost (Patterns Book ch04). Its three jobs
(`type-phantom-marker`):

| Job | Marker | Effect |
|---|---|---|
| Lifetime binding | `PhantomData<&'a T>` | struct is treated as borrowing `'a` |
| Ownership simulation | `PhantomData<T>` | drop-check assumes struct owns a `T` |
| Variance control | `PhantomData<fn(T)>` | makes struct contravariant over `T` |

Reach for it whenever a type parameter (or lifetime) does not appear in a real field — e.g.
type-state markers, typed FFI handles, iterators over raw pointers. Never fake it with
`Option<T>` (adds a discriminant/size and forces `match`/`unwrap` at every use).

```rust
use std::marker::PhantomData;

struct User; struct Order;
struct Handle<T> { id: u64, _marker: PhantomData<T> }
impl<T> Handle<T> {
    fn new(id: u64) -> Self { Handle { id, _marker: PhantomData } }
}
// Handle<User> and Handle<Order> are incompatible types over the same u64.
```

### 3.1 Drop-check and ownership semantics

The compiler's drop-checker uses the `PhantomData` variant to decide whether the struct's
destructor *might* touch a `T`, which constrains lifetimes (Patterns Book ch04 §Drop Check):

```rust
use std::marker::PhantomData;

// "I logically OWN a T" — drop-check requires T to outlive the struct:
struct Owning<T> { ptr: *const T, _marker: PhantomData<T> }

// "I just POINT to a T" — more permissive, T need not outlive us:
struct NonOwning<T> { ptr: *const T, _marker: PhantomData<*const T> }
```

Practical rule: a container that **owns** its data → `PhantomData<T>`; a **view/reference**
type → `PhantomData<&'a T>` or `PhantomData<*const T>`. A `Drop` impl that calls
`drop_in_place(self.ptr)` must use `PhantomData<T>` so drop-check knows a `T` is dropped.

### 3.2 Variance — why the marker's type matters

Variance decides whether a generic type substitutes with a longer- or shorter-lived
parameter. Getting it wrong either rejects good code or accepts unsound code (Patterns Book
ch04 §Variance).

| Variance | Meaning | Rust examples |
|---|---|---|
| **Covariant** | longer lifetime usable where shorter expected | `&'a T`, `Box<T>`, `Vec<T>`, `Rc<T>` |
| **Contravariant** | flows against — shorter where longer expected | `fn(T)` argument position |
| **Invariant** | no substitution either way | `&mut T`, `Cell<T>`, `UnsafeCell<T>` |

`&mut T` is **invariant over `T`**: if it were covariant you could write a shorter-lived
`&str` into a `&'static str` slot and create a dangling reference. `PhantomData<X>` gives your
struct **the same variance as `X`**:

```rust
use std::marker::PhantomData;

struct Ref<'a, T>    { ptr: *const T, _m: PhantomData<&'a T> }     // covariant in 'a and T
struct MutRef<'a, T> { ptr: *mut T,   _m: PhantomData<&'a mut T> } // covariant 'a, INVARIANT T
struct Consumer<T>   { _m: PhantomData<fn(T)> }                     // contravariant in T
```

Full cheat sheet (Patterns Book ch04):

| PhantomData type | Variance over `T` | Variance over `'a` | Use when |
|---|---|---|---|
| `PhantomData<T>` | covariant | — | you own a `T` |
| `PhantomData<&'a T>` | covariant | covariant | you borrow a `T` for `'a` |
| `PhantomData<&'a mut T>` | **invariant** | covariant | you mutably borrow `T` |
| `PhantomData<*const T>` | covariant | — | non-owning `*const` |
| `PhantomData<*mut T>` | **invariant** | — | non-owning `*mut` |
| `PhantomData<fn(T)>` | **contravariant** | — | `T` in argument position |
| `PhantomData<fn() -> T>` | covariant | — | `T` in return position |
| `PhantomData<fn(T) -> T>` | **invariant** | — | `T` in both positions |

> **Decision rule** (Patterns Book ch04): start with `PhantomData<&'a T>` (covariant). Switch
> to `PhantomData<&'a mut T>` (invariant) only if the abstraction hands out mutable access.
> Use `PhantomData<fn(T)>` (contravariant) almost never — only for callback-storage.
> `fn(T)`/`fn() -> T` markers are also the standard `Send + Sync` trick: they make the struct
> unconditionally `Send`/`Sync` regardless of `T` (a raw `PhantomData<T>` would inherit `T`'s
> auto-traits).

### 3.3 Unit-of-measure — dimension-safe arithmetic

`PhantomData<Unit>` tags a scalar with its unit so mixing incompatible units fails to compile,
with `Quantity<Meters>` laid out identically to a bare `f64` — "pure type-system magic"
(Patterns Book ch04 §Unit-of-Measure):

```rust
use std::marker::PhantomData;
use std::ops::{Add, Div};

struct Meters; struct Seconds; struct MetersPerSecond;

#[derive(Debug, Clone, Copy)]
struct Quantity<U> { value: f64, _unit: PhantomData<U> }
impl<U> Quantity<U> { fn new(value: f64) -> Self { Quantity { value, _unit: PhantomData } } }

impl<U> Add for Quantity<U> {                     // same unit only
    type Output = Quantity<U>;
    fn add(self, rhs: Self) -> Self { Quantity::new(self.value + rhs.value) }
}
impl Div<Quantity<Seconds>> for Quantity<Meters> { // Meters / Seconds = m/s
    type Output = Quantity<MetersPerSecond>;
    fn div(self, rhs: Quantity<Seconds>) -> Quantity<MetersPerSecond> {
        Quantity::new(self.value / rhs.value)
    }
}

fn demo() {
    let dist = Quantity::<Meters>::new(100.0);
    let time = Quantity::<Seconds>::new(9.58);
    let _speed = dist / time;         // Quantity<MetersPerSecond>
    // let bad = dist + time;         // ❌ cannot add Meters + Seconds
}
```

### 3.4 Lifetime branding — unforgeable handles

An **invariant** lifetime brand ties a handle to one specific arena/session so a handle from
one context cannot be used with another. The `for<'arena>` HRTB in `with_arena` gives each
call a fresh, opaque lifetime that cannot be unified (Patterns Book ch04 §Lifetime Branding):

```rust
use std::cell::RefCell;
use std::marker::PhantomData;

struct ArenaHandle<'arena> {
    index: usize,
    _brand: PhantomData<*mut &'arena ()>, // invariant → prevents cross-arena mixing
}
struct Arena<'arena> {
    data: RefCell<Vec<String>>,
    _phantom: PhantomData<&'arena ()>,
}
fn with_arena<R>(f: impl for<'arena> FnOnce(&Arena<'arena>) -> R) -> R {
    let arena = Arena { data: RefCell::new(Vec::new()), _phantom: PhantomData };
    f(&arena)
}
impl<'arena> Arena<'arena> {
    fn alloc(&self, value: String) -> ArenaHandle<'arena> {
        let mut d = self.data.borrow_mut();
        let index = d.len();
        d.push(value);
        ArenaHandle { index, _brand: PhantomData }
    }
    fn get(&self, h: &ArenaHandle<'arena>) -> String { self.data.borrow()[h.index].clone() }
}
```

Note the contrast: a *session token* meant to be passed to functions needing a shorter borrow
must be **covariant** (`PhantomData<&'a ()>`), so callers can shorten `'a`. Use invariance
(`PhantomData<*mut &'a ()>`) only when forging/mixing must be blocked.

---

## 4. Rules & anti-patterns checklist

Distilled from the `type-*` rust-skills rules (cross-links to `api-guidelines.md` C-codes and
sibling rules noted):

- **type-newtype-ids** — DO wrap IDs in newtypes (`UserId(u64)`), never pass raw `u64`. Reason:
  swapped-argument bugs become compile errors. Derive `Debug, Clone, Copy, PartialEq, Eq, Hash`
  (+ `Ord` for `BTreeMap` keys); add `#[serde(transparent)]`.
- **type-newtype-validated** — DO make validated newtypes constructible only via a fallible
  constructor with a **private** field. Reason: "parse, don't validate" — holding the type
  proves the invariant, no re-checking. (See `api-parse-dont-validate`.)
- **api-newtype-safety** — DO use distinct newtypes for semantically different values (IDs,
  units, validated strings). DON'T create `struct X(i32)` where no confusion is possible.
- **type-repr-transparent** — DO add `#[repr(transparent)]` to newtypes crossing FFI, used in
  pointer casts, or wrapping `NonZero*`. Reason: guarantees identical size/align/ABI. DON'T
  rely on layout of a plain newtype in `extern` blocks.
- **type-deref-coercion** — DON'T implement `Deref`/`DerefMut` for invariant-bearing or
  API-restricting newtypes (leaks the inner surface, fakes inheritance). DO implement it only
  for smart-pointer / transparent-wrapper types. Prefer `AsRef`/`Borrow`/explicit delegation.
  (C-DEREF.)
- **api-typestate** — DO encode state-machine invariants in the type (`Foo<State>`), consuming
  `self` per transition. Reason: invalid transitions become compile errors, no runtime state
  guards. Fix if you see `if self.state != X { return Err(...) }` protocol checks.
- **type-phantom-marker** — DO use `PhantomData<T>` for a type parameter absent from fields.
  DON'T use `Option<T>` as a workaround (wastes memory, needs `T: Default`). Choose the marker
  type to get the right variance/ownership.
- **type-enum-states** — DO model mutually-exclusive runtime states with an enum carrying
  per-state data. DON'T use parallel booleans (`is_running`, `is_completed`) that permit
  impossible combinations. Reason: `match` forces exhaustive handling.
- **type-no-stringly** — DON'T accept `&str` for a fixed set of values or structured data. DO
  use an enum / newtype / validated type. Reason: typos and wrong formats become compile
  errors, not runtime panics. Parse strings into typed values at boundaries via `FromStr`.
- **type-option-nullable** — DO use `Option<T>` for "might not exist"; DON'T use sentinel /
  empty values. Reason: the compiler forces handling `None`.
- **type-result-fallible** — DO return `Result<T, E>` for fallible operations; DON'T panic,
  return `Option` (loses context), or use magic sentinels. Propagate with `?`.
- **type-never-diverge** — DO annotate never-returning fns `-> !` (loops, `exit`, `panic`).
  Reason: `!` coerces to any type (clean `match` arms) and documents control flow. On stable,
  use `std::convert::Infallible` where `!` as a *type argument* is needed.
- **type-generic-bounds** — DON'T put trait bounds on struct definitions (`struct C<T: Clone>`);
  DO put them on the impls/functions that need them, in `where` clauses. Enables conditional
  impls (`impl<T: Clone> Clone for Wrapper<T>`).
- **type-display-vs-debug** — DO `#[derive(Debug)]` on every public type; hand-write `Display`
  for user-facing text. DON'T derive/synthesize `Display` from `Debug`, or show `{:?}` to end
  users. `Error` requires `Display`. (C-DEBUG.)
- **type-numeric-fmt** — DO forward `LowerHex`/`UpperHex`/`Octal`/`Binary` to the inner value
  for numeric newtypes (masks, addresses, IDs). Reason: `{:x}` on a newtype should not be a
  compile error. (C-NUM-FMT.) One-liner per trait via `fmt::LowerHex::fmt(&self.0, f)`.
- **trait-coherence-newtype** — DO wrap a foreign type in a local newtype to implement a
  foreign trait (orphan rule). Add `From`/`Into` + `inner()`/`into_inner()`; `#[repr(transparent)]`
  if ABI matters.
- **num-nonzero** — DO use `NonZeroU32` etc. when zero is invalid. Reason: pushes the check to
  construction *and* makes `Option<NonZero>` free via the niche. No direct `Add`/`Sub`.
- **api-sealed-trait** — DO seal a trait (`: private::Sealed`) when it must not be implemented
  downstream (state markers, capability markers, driver sets), so you can add methods without a
  breaking change and guarantee invariants.

---

## 5. Gotchas / footguns

- **`PhantomData<T>` inherits `T`'s auto-traits.** A struct with `PhantomData<T>` is `Send`
  only if `T: Send`, and drop-check assumes it owns a `T`. If you hold a `*mut T` you do **not**
  own, use `PhantomData<*const T>`/`*mut T` to avoid over-constraining lifetimes — but note
  `*mut`/`*const` are **not** `Send`/`Sync`, so you may then need `PhantomData<fn() -> T>`
  (covariant, unconditionally `Send + Sync`) to decouple auto-traits from `T`.
- **`&mut T` invariance surprises.** Wrapping a `*mut T` view with `PhantomData<&'a T>`
  (covariant) when you actually mutate through it is **unsound**. Use `PhantomData<&'a mut T>`.
- **Contravariance is almost always wrong.** `PhantomData<fn(T)>` breaks ergonomics (callers
  can't shorten lifetimes). Only correct for genuine callback storage. Default to covariant.
- **`Deref` makes method resolution surprising.** With `Deref`, `wrapper.is_empty()` may
  silently resolve to the inner type's method, not one you meant to shadow — can trip
  `clippy::wrong_self_convention`. Prefer explicit inherent methods on invariant newtypes.
- **`DerefMut` bypasses every constructor invariant.** `*port = 0;` compiles and destroys the
  invariant your `PortNumber::new` protected. Only add `DerefMut` for invariant-free wrappers.
- **Type-state balloons the binary / hurts dynamic use.** Each `Foo<State>` is a distinct
  monomorphized type — you cannot store mixed states in a `Vec<Foo<_>>` or deserialize into
  "current state." If state is chosen at runtime or needs serialization, use an **enum state**
  instead (§2.4).
- **Forgetting to consume `self` defeats type-state.** A transition taking `&self` (not `self`)
  leaves the old-state handle usable — the guarantee evaporates. Every transition must move.
- **`#[repr(transparent)]` requires exactly one non-ZST field.** Adding a second real field is a
  compile error; `PhantomData` fields are fine because they are zero-sized.
- **`#[serde(transparent)]` ≠ `#[repr(transparent)]`.** The former controls JSON shape (serialize
  as the inner value); the latter controls memory layout. They are orthogonal — you often want
  both on an ID newtype but for different reasons.
- **`NonZero*` arithmetic silently drops through `.get()`.** `a.get() + b.get()` is a plain
  integer that can overflow or hit zero; reconstruct via `NonZeroU32::new(result)?` and handle
  the `None`.
- **`!` as a type argument is still nightly.** `Result<(), !>` needs `#![feature(never_type)]`;
  use `std::convert::Infallible` on stable. `fn f() -> !` (return position) is stable since 1.41.
- **Validated newtype with public field is not validated.** `pub struct Email(pub String)` lets
  callers construct `Email("garbage".into())` directly, bypassing `parse`. Keep the field
  private for invariant types (public field is fine for pure ID/unit newtypes).

---

## 6. Cheat-sheet

| Goal | Tool | Key detail |
|---|---|---|
| Stop swapped-argument bugs | newtype (`UserId(u64)`) | distinct type, zero cost; derive value traits |
| Guarantee validity everywhere | validated newtype, **private field** | construct only via `parse`/`new -> Result`/`Option` |
| Type-safe scalar arithmetic | `PhantomData<Unit>` on a scalar | same layout as inner; unit mismatch = compile error |
| Never-zero integer + free `Option` | `NonZeroU32` | `new -> Option`; niche makes `Option<T>` free |
| Implement foreign trait on foreign type | local newtype wrapper | orphan-rule escape; add `From`/`into_inner` |
| FFI / pointer-cast layout match | `#[repr(transparent)]` | one non-ZST field + any `PhantomData` |
| Same wire format as inner value | `#[serde(transparent)]` | orthogonal to `repr` |
| Enforce call order at compile time | type-state `Foo<State>` | each transition consumes `self`, returns new type |
| Enforce required builder fields | type-state builder | `build()` only on terminal `Ready` state |
| Runtime-varying mutually-exclusive state | enum state | per-variant data; exhaustive `match` |
| Collapse many trait generics into one | config trait (associated types) | `Foo<Cfg: Config>`; one param forever, static dispatch |
| Vendor × state matrix | dual-axis `Handle<V, S>` + marker traits | each `impl` = one matrix cell |
| Type param not in any field | `PhantomData<T>` | never `Option<T>` |
| Control variance | choose `PhantomData<X>` | see §3.2 table; default `PhantomData<&'a T>` |
| Owning vs viewing raw pointer | `PhantomData<T>` vs `PhantomData<*const T>` | affects drop-check + auto-traits |
| Unforgeable per-context handle | invariant lifetime brand `PhantomData<*mut &'a ()>` | `for<'a>` HRTB gives fresh opaque lifetime |
| Block downstream trait impls | sealed trait (`: private::Sealed`) | add methods non-breakingly |
| Give inner methods transparently | `Deref` — **only** smart-pointer/transparent wrapper | never for invariant/restricted types |

**Marker → variance quick pick:** own→`PhantomData<T>` · borrow→`PhantomData<&'a T>` ·
mut-borrow→`PhantomData<&'a mut T>` · non-owning ptr→`PhantomData<*const T>` ·
decouple auto-traits / `Send+Sync`→`PhantomData<fn() -> T>` · contravariant callback→`PhantomData<fn(T)>`.

**Related references:** `api-guidelines.md` (C-NEWTYPE, C-DEREF, C-DEBUG, C-NUM-FMT,
"parse don't validate"), `microsoft-guidelines.md` (M-* pragmatic rules), `design-patterns.md`
(Newtype idiom, Builder, type-state), `style-guide.md` (derive ordering, `where`-clause
formatting).
