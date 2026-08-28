# Serialization, Zero-Copy & Binary Data

Engineering patterns for `serde` (data model, derives, attributes, custom impls, zero-copy/borrowing deserialization), the format ecosystem (JSON/TOML/bincode/MessagePack/postcard/CBOR), binary layout & endianness (`repr(C)`, `zerocopy`, `bytemuck`, `bytes::Bytes`), and numeric conversions & overflow (`as` vs `TryFrom`, `checked`/`saturating`/`wrapping`, `NonZero`, float comparison). Consult before writing or reviewing any (de)serialization boundary, binary-protocol parser, wire-format DTO, or arithmetic on untrusted numbers. Distilled from Patterns Book ch11 and the `serde-*`/`num-*` rust-skills rules. For canonical conversion/newtype/error guidance cross-linked below, see `api-guidelines.md` (C-*), `microsoft-guidelines.md` (M-*), and `design-patterns.md`.

## serde Fundamentals

`serde` separates the **data model** (your structs/enums) from the **format** (JSON, TOML, bincode, …). Derive `Serialize`/`Deserialize` once; the type then works with every serde-compatible format with no code changes (Patterns Book ch11 §serde Fundamentals).

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
struct ServerConfig {
    name: String,
    port: u16,
    #[serde(default)] // use Default::default() if the key is missing
    max_connections: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_cert_path: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let json_input = r#"{ "name": "hw-diag", "port": 8080 }"#;
    let config: ServerConfig = serde_json::from_str(json_input)?;
    // ServerConfig { name: "hw-diag", port: 8080, max_connections: 0, tls_cert_path: None }
    let _output = serde_json::to_string_pretty(&config)?;
    Ok(())
}
```

Two axes of the model:
- `Serialize` — turn a value into any `Serializer`.
- `Deserialize<'de>` — build a value from any `Deserializer`, possibly **borrowing** from the input with lifetime `'de`.

### Common serde Attributes

Fine-grained control lives in container attributes (on the `struct`/`enum`) and field attributes.

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")] // field_name -> fieldName on the wire
#[serde(deny_unknown_fields)]      // reject extra keys (strict parsing)
struct DiagResult {
    test_name: String,  // "testName"
    pass_count: u32,    // "passCount"
    fail_count: u32,    // "failCount"
}

fn default_threshold() -> f64 { 1.0 }

#[derive(Serialize, Deserialize, Default)]
struct Metadata { vendor: String, model: String }

#[derive(Serialize, Deserialize)]
struct Sensor {
    #[serde(rename = "sensor_id")]      // override this one field's name
    id: u64,
    #[serde(default)]                   // Default if missing
    enabled: bool,
    #[serde(default = "default_threshold")]
    threshold: f64,
    #[serde(skip)]                      // never ser/de; needs Default
    cached_value: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(flatten)]                   // inline Metadata's fields at top level
    metadata: Metadata,
}
```

| Attribute | Level | Effect |
|-----------|-------|--------|
| `rename_all = "camelCase"` | Container | Rename all fields/variants (camelCase, snake_case, kebab-case, PascalCase, SCREAMING_SNAKE_CASE, UPPERCASE, lowercase) |
| `deny_unknown_fields` | Container | Error on unexpected keys |
| `tag` / `content` / `untagged` | Container (enum) | Enum representation (below) |
| `rename = "..."` | Field | Custom wire name (also for keywords like `type`) |
| `default` / `default = "fn"` | Field | Fill missing key from `Default` or a `fn() -> T` |
| `skip` / `skip_serializing` / `skip_deserializing` | Field | Exclude from both/one direction |
| `skip_serializing_if = "fn"` | Field | Conditionally drop from output (`fn(&T) -> bool`) |
| `flatten` | Field | Inline a nested struct or a map catch-all |
| `with = "module"` | Field | Custom ser+de functions |
| `serialize_with` / `deserialize_with = "fn"` | Field | Custom one direction |
| `alias = "..."` | Field | Accept an alternate name on deserialize only |
| `try_from = "Raw"` / `into = "Raw"` | Container | Validate/convert through `TryFrom`/`Into` |

### Enum Representations

serde offers four ways to encode enums; choose deliberately — the wrong one silently mismatches the external schema (rule `serde-enum-representation`; Patterns Book ch11 §Enum Representations).

```rust
use serde::{Serialize, Deserialize};

// 1. Externally tagged (DEFAULT): {"Circle":{"radius":5.0}}
//    Good for Rust-to-Rust where the variant name IS the key.
#[derive(Serialize, Deserialize)]
enum ShapeExternal { Circle { radius: f64 }, Rectangle { width: f64, height: f64 } }

// 2. Internally tagged: {"type":"Circle","radius":5.0}
//    Good for REST APIs with a discriminator. ALL variants must be structs/maps.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum ShapeInternal { Circle { radius: f64 }, Rectangle { width: f64, height: f64 } }

// 3. Adjacently tagged: {"t":"Circle","c":{"radius":5.0}}
//    Use when variants may hold primitives/tuples (internally tagged cannot).
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
enum ShapeAdjacent { Circle { radius: f64 }, Count(u32) }

// 4. Untagged: 42 -> Integer(42), "hi" -> Text("hi")
//    Tried IN DECLARATION ORDER; first structural match wins.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Value { Integer(i64), Float(f64), Text(String) }
```

| Strategy | Attribute | Wire form (Circle) | Tuple/primitive variant? |
|---|---|---|---|
| Externally tagged | (default) | `{"Circle":{"radius":5}}` | yes |
| Internally tagged | `#[serde(tag = "type")]` | `{"type":"Circle","radius":5}` | **no** |
| Adjacently tagged | `#[serde(tag="t", content="c")]` | `{"t":"Circle","c":{"radius":5}}` | yes |
| Untagged | `#[serde(untagged)]` | `{"radius":5}` | yes |

Rule of thumb: internally tagged for most JSON APIs (readable, matches Go/Python/TS conventions); untagged only for small, structurally-distinct union types. See `design-patterns.md` (state as enum) and `api-guidelines.md` C-NON-EXHAUSTIVE for future-proofing public enums.

## Custom (De)serialization

### `with` / `serialize_with` / `deserialize_with`

When the natural Rust type differs from the wire shape (a `Duration` as whole seconds, bytes as base64, a timestamp as ISO-8601), point serde at conversion functions instead of polluting the domain model with wire-shaped fields (rule `serde-custom-with`).

WRONG — leaks the wire representation into the type:

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct Task {
    name: String,
    timeout_secs: u64, // callers must convert to/from Duration everywhere
}
```

RIGHT — a `with` module keeps the field a `Duration`:

```rust
use serde::{Serialize, Deserialize, Serializer, Deserializer};
use std::time::Duration;

mod duration_secs {
    use super::*;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs()) // note: receives &T
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

#[derive(Serialize, Deserialize)]
struct Task {
    name: String,
    #[serde(with = "duration_secs", rename = "timeout")]
    timeout: Duration,
    #[serde(serialize_with = "duration_secs::serialize")] // one-sided
    elapsed: Duration,
}
```

The `with` module must expose both `serialize<S>(&T, S)` and `deserialize<'de, D>(D)` with those exact signatures. For a representation used widely, prefer a **newtype with its own impls** over repeating `#[serde(with)]` (see the `HumanDuration` example below and `api-guidelines.md` C-NEWTYPE).

### Manual `Serialize`/`Deserialize` (newtype wrapper)

A parse-friendly wrapper that reads a human string like `"30s"`, `"5m"`, `"2h"` and round-trips (Patterns Book ch11 exercise). Delegates to `String::deserialize` then converts, mapping failures through `serde::de::Error::custom`.

```rust
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, time::Duration};

#[derive(Debug, Clone, PartialEq)]
struct HumanDuration(Duration);

impl HumanDuration {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() { return Err("empty duration".into()); }
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, suffix) = s.split_at(split);
        let v: u64 = num.parse().map_err(|_| format!("invalid number: {num}"))?;
        let d = match suffix {
            "s" | "sec" => Duration::from_secs(v),
            "m" | "min" => v.checked_mul(60).map(Duration::from_secs)
                            .ok_or_else(|| "duration overflow".to_string())?,
            "h" | "hr"  => v.checked_mul(3600).map(Duration::from_secs)
                            .ok_or_else(|| "duration overflow".to_string())?,
            "ms"        => Duration::from_millis(v),
            other       => return Err(format!("unknown suffix: {other}")),
        };
        Ok(HumanDuration(d))
    }
}

impl fmt::Display for HumanDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.as_secs();
        if secs == 0 { write!(f, "{}ms", self.0.as_millis()) }
        else if secs % 3600 == 0 { write!(f, "{}h", secs / 3600) }
        else if secs % 60 == 0 { write!(f, "{}m", secs / 60) }
        else { write!(f, "{}s", secs) }
    }
}

impl Serialize for HumanDuration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        HumanDuration::parse(&s).map_err(serde::de::Error::custom)
    }
}
```

### Validate at the boundary with `try_from`/`into`

"Parse, don't validate": wire the deserializer through `TryFrom` so an invalid value is **never constructed**, not even briefly (rule `serde-try-from-validate`; cross-links `api-guidelines.md` C-CONV-TRAITS and `design-patterns.md`).

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct Email(String);

impl TryFrom<String> for Email {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.contains('@') && !s.starts_with('@') && !s.ends_with('@') {
            Ok(Email(s))
        } else {
            Err(format!("invalid email address: {s}"))
        }
    }
}
impl From<Email> for String {
    fn from(e: Email) -> String { e.0 }
}
```

`try_from`/`into` **replace** the derived field-by-field impls; `into` requires `Clone` (serde may clone before converting). Reuse the same `TryFrom` in CLI/form paths.

## Zero-Copy / Borrowing Deserialization

serde can deserialize `&'de str`/`&'de [u8]` fields that **borrow directly from the input buffer** — zero allocation. This is the key to high-throughput parsing (Patterns Book ch11 §Zero-Copy Deserialization).

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct OwnedRecord { name: String, value: String }        // 2 heap allocations

#[derive(Deserialize)]
struct BorrowedRecord<'a> { name: &'a str, value: &'a str } // 0 allocations

fn demo() {
    let input = r#"{"name":"cpu_temp","value":"72.5"}"#;
    let _owned: OwnedRecord = serde_json::from_str(input).unwrap();
    let borrowed: BorrowedRecord = serde_json::from_str(input).unwrap();
    // `borrowed` cannot outlive `input`.
    println!("{}: {}", borrowed.name, borrowed.value);
}
```

**`Deserialize<'de>` vs `DeserializeOwned`:**

```rust
use serde::Deserialize;
use serde::de::DeserializeOwned;

// Requires an owned result — input may be temporary, result outlives it.
fn parse_owned<T: DeserializeOwned>(input: &str) -> T {
    serde_json::from_str(input).unwrap()
}

// Allows borrowing from input — result is lifetime-bound to `'a`.
fn parse_borrowed<'a, T: Deserialize<'a>>(input: &'a str) -> T {
    serde_json::from_str(input).unwrap()
}
```

`DeserializeOwned` is the alias `for<'de> Deserialize<'de>` — use it as the bound when a function must **store or return** the result independently of the input. Use `Deserialize<'a>` only when you can keep the input alive.

**`Cow<'a, str>` — borrow when possible, allocate only when needed** (e.g., JSON escape sequences that must be unescaped). Requires `#[serde(borrow)]` to enable borrowing:

```rust
use serde::Deserialize;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(borrow)]
    name: Cow<'a, str>, // Borrowed if no escapes; Owned if unescaping needed
}
```

Use zero-copy for: large files where you read few fields, high-throughput pipelines (packets, log lines), memory-mapped inputs. Avoid it when: the input buffer is ephemeral/reused, the result must outlive the input, or fields need transformation.

## The Format Ecosystem

| Format | Crate | Human-readable | Size | Speed | Self-describing | Use case |
|--------|-------|:---:|:---:|:---:|:---:|----------|
| JSON | `serde_json` | yes | large | good | yes | Config, REST, logging |
| TOML | `toml` | yes | medium | good | yes | Config (Cargo.toml style) |
| YAML | `serde_yaml`* | yes | medium | good | yes | Config, complex nesting |
| bincode | `bincode` | no | small | fast | **no** | Rust-to-Rust IPC, caches |
| postcard | `postcard` | no | tiny | very fast | no | Embedded, `no_std` (varint) |
| MessagePack | `rmp-serde` | no | small | fast | yes | Cross-language binary |
| CBOR | `ciborium` | no | small | fast | yes | IoT, constrained env |

```rust
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct DiagConfig { name: String, tests: Vec<String>, timeout_secs: u64 }

fn encode(config: &DiagConfig) {
    let _json = serde_json::to_string(config).unwrap(); // e.g. 67 bytes, field names present
    let _bin = bincode::serialize(config).unwrap();     // ~40 bytes, no field names
    // let _post = postcard::to_allocvec(config).unwrap(); // even smaller, varints
}
```

Choose: humans edit → TOML/JSON; Rust-to-Rust IPC/cache → bincode; cross-language binary → MessagePack/CBOR; embedded/`no_std` → postcard. **Non-self-describing formats (bincode, postcard) don't carry field names**, so `#[serde(flatten)]`, untagged enums, and unknown-key handling either don't work or behave differently — reserve those attributes for JSON/TOML/YAML.

## Library serde: gate behind a feature

For general-purpose libraries, make serde **optional** so users who don't serialize don't pay for it (rule `api-serde-optional`; aligns with `microsoft-guidelines.md` M-* on lean dependencies). Keep it required only when the crate is *about* serialization (config parsers, API clients, data formats).

```toml
# Cargo.toml
[dependencies]
serde = { version = "1.0", features = ["derive"], optional = true }

[features]
default = []
serde = ["dep:serde"]
```

```rust
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub struct Point { pub x: f64, pub y: f64 }
```

Test the matrix: `cargo test`, `cargo test --features serde`, `cargo test --all-features`.

## Binary Data & `repr(C)`

For hardware/protocol parsing, `#[repr(C)]` guarantees fields lay out in declaration order with C padding rules — essential for matching register maps and protocol headers (Patterns Book ch11 §Binary Data). See `design-patterns.md` (FFI) and `reference-notation.md` for layout grammar.

```rust
use std::mem::size_of;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct IpmiHeader {
    rs_addr: u8, net_fn_lun: u8, checksum: u8,
    rq_addr: u8, rq_seq_lun: u8, cmd: u8,
}

impl IpmiHeader {
    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < size_of::<Self>() { return None; }
        Some(IpmiHeader {
            rs_addr: data[0], net_fn_lun: data[1], checksum: data[2],
            rq_addr: data[3], rq_seq_lun: data[4], cmd: data[5],
        })
    }
    fn net_fn(&self) -> u8 { self.net_fn_lun >> 2 }
    fn lun(&self) -> u8 { self.net_fn_lun & 0x03 }
}
```

### Endianness & byte order

Never `transmute` multi-byte integers from a buffer — decode explicitly with `from_le_bytes`/`from_be_bytes` (network byte order = big-endian). These take a fixed-size array, so bounds must be checked first.

```rust
fn read_u16_le(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}
fn read_u32_be(data: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}
// Encode: value.to_le_bytes() / value.to_be_bytes() -> [u8; N]
```

### `repr(C, packed)` — the unaligned-reference footgun

`packed` removes padding (alignment 1). Taking a **reference** to a field of a packed struct is undefined behavior when unaligned; you must copy the field out (it's `Copy`).

```rust
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct PcieCapabilityHeader { cap_id: u8, next_cap: u8, cap_reg: u16 }

fn read(h: &PcieCapabilityHeader) -> u16 {
    let reg = h.cap_reg; // OK: copies the u16 out (field is Copy)
    reg
    // let r = &h.cap_reg; // UB: unaligned reference — never do this
}
```

### `zerocopy` and `bytemuck` — safe transmutation

Instead of `unsafe transmute`, use crates that verify layout at compile time and yield zero-copy views into a buffer.

```rust
// zerocopy = { version = "0.8", features = ["derive"] }
use zerocopy::{FromBytes, IntoBytes, KnownLayout, Immutable};

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Debug)]
#[repr(C)]
struct SensorReading { sensor_id: u16, flags: u8, _reserved: u8, value: u32 }

fn parse_sensor(raw: &[u8]) -> Option<&SensorReading> {
    SensorReading::ref_from_bytes(raw).ok() // checks size+alignment, borrows into raw
}
```

```rust
// bytemuck = { version = "1", features = ["derive"] }
use bytemuck::{Pod, Zeroable};

#[derive(Pod, Zeroable, Clone, Copy)]
#[repr(C)]
struct GpuRegister { address: u32, value: u32 }

fn cast_registers(data: &[u8]) -> &[GpuRegister] {
    bytemuck::cast_slice(data) // Pod guarantees every bit pattern is valid
}
```

| Approach | Safety | Overhead | Use when |
|----------|:---:|:---:|----------|
| Manual field-by-field | safe | copies fields | Small structs, complex/variable layouts |
| `zerocopy` | safe | zero-copy | Large buffers, many reads, compile-time checks |
| `bytemuck` | safe | zero-copy | Simple `Pod` types, slice casting |
| `unsafe transmute` | **unsafe** | zero-copy | Last resort — avoid in app code |

### `bytes::Bytes` — reference-counted zero-copy buffers

`Bytes` is to `Vec<u8>` what `Arc<[u8]>` is to owned slices: cheap clones and sub-slices that share one allocation. Used across tokio/hyper/tonic/axum.

```rust
use bytes::{Bytes, BytesMut, Buf, BufMut};

fn build_and_parse() {
    let mut buf = BytesMut::with_capacity(1024);
    buf.put_u8(0x01);
    buf.put_u16(0x1234);      // big-endian
    buf.put_slice(b"hello");
    let data: Bytes = buf.freeze(); // zero-cost freeze to immutable

    let _clone = data.clone();       // O(1): refcount bump, not deep copy
    let _sub = data.slice(3..8);     // zero-copy sub-slice

    let mut reader = &data[..];      // read via Buf
    let _byte = reader.get_u8();     // 0x01
    let _short = reader.get_u16();   // 0x1234

    let mut original = Bytes::from_static(b"HEADER\x00PAYLOAD");
    let _header = original.split_to(6); // "HEADER"; original keeps the rest — no copy
}
```

| Feature | `Vec<u8>` | `Bytes` |
|---------|-----------|---------|
| Clone | O(n) deep copy | O(1) refcount |
| Sub-slice | borrow w/ lifetime | owned, refcount-tracked |
| Shared ownership | `Vec<u8>` is already `Send + Sync`, but cheap sharing needs `Arc` (deep clone otherwise) | `Bytes` clone is O(1) refcount; `Send + Sync` built in |
| Mutability | direct `&mut` | split into `BytesMut` first |

Use `bytes` for network protocols and packet parsing where a buffer is split into parts handled by different components/threads — the zero-copy split is the killer feature.

## Numeric Conversions & Overflow

### `as` vs `From`/`TryFrom`

`as` silently truncates on narrowing (`300u32 as u8 == 44`) and saturates/zeroes on float→int (`NaN as usize == 0`). Prefer lossless `From`/`Into` for widening and fallible `TryFrom`/`TryInto` for narrowing (rule `num-cast-try-from`; `api-guidelines.md` C-CONV-TRAITS).

```rust
fn widen(x: u8) -> u32 { u32::from(x) }               // lossless; won't compile if lossy

fn narrow(x: u32) -> Result<u8, std::num::TryFromIntError> {
    u8::try_from(x)                                    // explicit failure on overflow
}

fn float_to_index(f: f64, len: usize) -> Option<usize> {
    if f.is_nan() || f < 0.0 || f >= len as f64 { return None; }
    Some(f as usize)                                   // `as` OK: range verified above
}
```

Reserve `as` for: pointer casts, float→int after an explicit range check, and `usize`↔pointer-sized conversions where the semantics are intended and documented. `.try_into()` often needs a type annotation for inference: `let n: u8 = x.try_into()?;`.

### Explicit overflow handling

Integer overflow **panics in debug, wraps silently in release** — relying on either default is a latent bug. Pick the variant that states intent (rule `num-overflow-explicit`).

```rust
fn add_score(cur: u32, delta: u32) -> Option<u32> { cur.checked_add(delta) }   // None on overflow
fn inc(c: u8) -> u8 { c.saturating_add(1) }                                     // clamps at 255
fn ring(n: u8) -> u8 { n.wrapping_add(1) }                                      // modular
fn carry(a: u32, b: u32) -> (u32, bool) { a.overflowing_add(b) }               // (result, overflowed)
```

| Family | Returns | Use when |
|---|---|---|
| `checked_*` | `Option<T>` | Overflow is an error the caller must handle |
| `saturating_*` | `T` | Clamping at the type bounds is correct |
| `wrapping_*` | `T` | Modular arithmetic is intended (checksums, ring buffers) |
| `overflowing_*` | `(T, bool)` | You need the result and a did-overflow flag |

The `checked_*`/`wrapping_*`/`overflowing_*` families exist for `add`/`sub`/`mul`/`div`/`shl`/`shr` on all integer primitives; `saturating_*` covers only `add`/`sub`/`mul` (plus `pow`/`neg`/`abs`) — there is no `saturating_div`/`saturating_shl`/`saturating_shr`.

### `clamp` and saturating bounds

`Ord::clamp(min, max)` expresses "constrain to range" in one call; combine with `saturating_*` for arithmetic that stops at the limits (rule `num-saturating-clamp`).

```rust
fn apply_damage(health: i32, damage: i32) -> i32 {
    health.saturating_sub(damage).clamp(0, i32::MAX) // stop at MIN, then floor at 0
}
fn clamp_volume(v: u8, lo: u8, hi: u8) -> u8 { v.clamp(lo, hi) }
fn normalize_alpha(a: f32) -> f32 { a.clamp(0.0, 1.0) } // NaN propagates -> NaN
```

`clamp` **panics if `min > max`** — validate order first if the bounds come from user input.

### `NonZero*` types

`NonZeroU32` & siblings make zero unrepresentable at the type level (constructed via `NonZeroU32::new(n) -> Option<_>`), pushing the zero-check to the boundary and unlocking the **niche optimization**: `Option<NonZeroU32>` is the same size as `u32` (rule `num-nonzero`; pairs with `api-guidelines.md` C-NEWTYPE for type-safe IDs).

```rust
use std::num::NonZeroU32;

fn divide(n: u32, d: NonZeroU32) -> u32 { n / d.get() } // always safe

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct WidgetId(NonZeroU32);
impl WidgetId {
    pub fn new(id: u32) -> Option<Self> { NonZeroU32::new(id).map(WidgetId) }
    pub fn get(self) -> u32 { self.0.get() }
}
// size_of::<Option<WidgetId>>() == size_of::<u32>() == 4
```

`NonZero*` don't implement `Add`/`Sub` (the result could be zero) — extract with `.get()`, compute, reconstruct with `NonZeroU32::new(result)?`.

### Comparing floats

Never `==` floats (`0.1 + 0.2 != 0.3`; `NaN != NaN`). Use an epsilon for approximate equality and `total_cmp` for ordering — `partial_cmp().unwrap()` **panics** on `NaN` in `sort_by` (rule `num-float-compare`).

```rust
fn approx_eq(a: f64, b: f64, eps: f64) -> bool { (a - b).abs() < eps }

fn sort_scores(scores: &mut [f64]) {
    scores.sort_by(|a, b| a.total_cmp(b)); // total order; NaN sorts last, never panics
}

fn safe_reciprocal(x: f64) -> Option<f64> {
    if x == 0.0 || x.is_nan() { None } else { Some(1.0 / x) }
}
```

An absolute epsilon is wrong for very large/small magnitudes — use a relative epsilon `(a-b).abs() / a.abs().max(b.abs()) < eps` (handle the zero case) for general code. `total_cmp` order: `-NaN < -inf < ... < -0.0 < +0.0 < ... < +inf < NaN`.

## Rules & anti-patterns checklist

- **api-serde-optional** — DO gate serde behind an optional feature (`serde = ["dep:serde"]`, `#[cfg_attr(feature="serde", derive(...))]`) in general-purpose libs; DON'T make every consumer pay compile time/binary size. Keep it required only for serialization-centric crates.
- **serde-rename-all** — DO put `#[serde(rename_all = "camelCase")]` on the container to match external naming in one line; DON'T scatter per-field `rename`. Field-level `rename` still wins for exceptions (keywords like `type`).
- **serde-default-compat** — DO mark new/optional fields `#[serde(default)]` (or `default = "fn"`) for backward compatibility; DON'T let a missing key hard-fail old payloads. Field-level is safer than container-level (container-level hides typos).
- **serde-skip-empty** — DO drop empty output with `#[serde(skip_serializing_if = "Option::is_none"|"Vec::is_empty")]`; DON'T emit `null`/`[]` noise. Pair with `#[serde(default)]` so the omission round-trips. `#[serde(skip)]` excludes both directions and requires `Default`.
- **serde-deny-unknown-fields** — DO add `#[serde(deny_unknown_fields)]` to config structs and DTOs so typos error loudly; DON'T silently drop unknown keys. Incompatible with `flatten` on the same struct.
- **serde-flatten** — DO `#[serde(flatten)]` shared field groups (pagination, metadata) or a `HashMap<String, Value>` catch-all; DON'T copy-paste field sets. Not compatible with `deny_unknown_fields`; uses a slower buffering path; won't work on non-self-describing formats (bincode).
- **serde-enum-representation** — DO pick tagging deliberately: externally (default, Rust-to-Rust), internally `tag` (REST, struct-only variants), adjacently `tag`+`content` (variants with primitives/tuples), untagged (small distinct unions); DON'T ship the default externally-tagged form to an API expecting a discriminator. Untagged tries variants in order and can silently mis-match.
- **serde-custom-with** — DO use `#[serde(with = "module")]` (or one-sided `serialize_with`/`deserialize_with`) to bridge type↔wire shape; DON'T reshape the domain type to fit the format. The `serialize` fn receives `&T`; for wide reuse prefer a newtype with its own impls.
- **serde-try-from-validate** — DO `#[serde(try_from = "Raw", into = "Raw")]` to validate during deserialize so invalid values never exist; DON'T validate after the fact. `into` needs `Clone`; these replace the derived impls.
- **num-cast-try-from** — DO `From`/`Into` for widening and `TryFrom`/`TryInto` for narrowing; DON'T use `as` for narrowing (silent truncation) or blind float→int (`NaN`→0). Range-check before an `as` float cast.
- **num-overflow-explicit** — DO choose `checked_`/`saturating_`/`wrapping_`/`overflowing_`; DON'T rely on bare `+`/`-`/`*` (debug panic, release silent wrap).
- **num-saturating-clamp** — DO `value.clamp(lo, hi)` and `saturating_*` for bounded arithmetic; DON'T hand-roll `if/min/max`. `clamp` panics if `lo > hi`; float `clamp` propagates `NaN`.
- **num-nonzero** — DO wrap never-zero values in `NonZero*` (rejects zero at construction, `Option<NonZero>` is free); DON'T encode "0 = invalid" as an untyped convention.
- **num-float-compare** — DO `approx_eq` with epsilon and `total_cmp` for sorting; DON'T `==` floats or `partial_cmp().unwrap()` (panics on `NaN`).

## Gotchas / footguns

- **Zero-copy lifetime leak.** A `struct Rec<'a> { name: &'a str }` cannot outlive the input buffer. If you need to store the result, use owned fields (`String`) / `DeserializeOwned`, or `Cow<'a, str>` with `#[serde(borrow)]`.
- **`Cow` doesn't borrow without `#[serde(borrow)]`.** Absent that attribute, serde deserializes `Cow` as always-owned. Escaped JSON strings force the `Owned` branch even with borrowing enabled — that's correct, not a bug.
- **`deny_unknown_fields` + `flatten` = compile/runtime conflict.** `flatten` must forward unmatched keys; `deny_unknown_fields` intercepts them. Split the struct or deserialize into `serde_json::Value` first.
- **Internally-tagged enums reject primitive/tuple variants.** `#[serde(tag="type")]` requires every variant to serialize as a map. A `Count(u32)` variant needs adjacently-tagged (`tag`+`content`).
- **Untagged order matters.** Variants are tried top-to-bottom; put the most specific first. `Float(f64)` before `Integer(i64)` can swallow integers into floats.
- **`#[serde(default)]` is deserialize-only.** It never omits defaults from output — pair with `skip_serializing_if` for symmetric behavior.
- **`#[serde(skip)]` needs `Default`.** Skipped fields are filled with `Default::default()` on the way in; the struct/field type must implement `Default` or it won't compile.
- **Packed-struct references are UB.** `&packed.field` on `#[repr(C, packed)]` is undefined behavior when unaligned. Always copy the field out (`let x = packed.field;`).
- **Endianness silence.** `from_le_bytes` vs `from_be_bytes` on the wrong-endian buffer compiles fine and yields byte-swapped garbage. Network protocols are big-endian; document the choice at every boundary.
- **`as` release-mode surprises.** `300u32 as u8 == 44` and `1e300 as u8 == 255` (saturating) never warn. `TryFrom` catches both at runtime; widening `From` catches lossy conversions at compile time.
- **`sort_by(partial_cmp().unwrap())` panics on `NaN`.** Use `total_cmp`. Same for `min`/`max` via `partial_cmp` — a `NaN` gives an inconsistent order.
- **`NonZero` arithmetic gap.** `NonZeroU32` has no `Add`; `.get()`, compute, `NonZeroU32::new(r)?`. Forgetting this and reaching for `as`/unsafe reintroduces the zero the type was meant to forbid.
- **bincode/postcard drop field names.** Renames, flatten, untagged enums, and unknown-field handling silently don't apply. Version binary formats explicitly; adding a field breaks old data unless the format handles it.
- **`clamp(min, max)` with reversed bounds panics.** Validate `min <= max` when bounds are dynamic/user-supplied.

## Cheat-sheet

| Need | Use | Notes |
|------|-----|-------|
| Optional/back-compat field | `#[serde(default)]` / `default = "fn"` | Deserialize-only |
| Omit empties from output | `#[serde(skip_serializing_if = "...")]` | Pair with `default` |
| Exclude field entirely | `#[serde(skip)]` | Needs `Default` |
| Strict parsing | `#[serde(deny_unknown_fields)]` | Not with `flatten` |
| Match wire naming | `#[serde(rename_all = "camelCase")]` | Per-field `rename` overrides |
| Inline / catch-all keys | `#[serde(flatten)]` | Not with `deny_unknown_fields`; slower |
| Discriminated enum | `#[serde(tag="type")]` | Struct-only variants |
| Enum w/ primitives | `#[serde(tag="t", content="c")]` | Adjacently tagged |
| Type↔wire mismatch | `#[serde(with = "mod")]` | `serialize` gets `&T` |
| Validate on deserialize | `#[serde(try_from="Raw", into="Raw")]` | `into` needs `Clone` |
| No-alloc parse | `&'de str` / `Cow` + `#[serde(borrow)]` | Result bound to input |
| Store result independently | `T: DeserializeOwned` | `for<'de> Deserialize<'de>` |
| Rust-to-Rust binary | `bincode` | No field names; not cross-lang |
| Embedded/`no_std` | `postcard` | Varint, tiny |
| Cross-language binary | `rmp-serde` / `ciborium` | MessagePack / CBOR |
| Fixed binary layout | `#[repr(C)]` + `zerocopy`/`bytemuck` | Compile-checked casts |
| Read integer from bytes | `u32::from_be_bytes([..])` | BE = network order |
| RC zero-copy buffer | `bytes::Bytes` / `BytesMut` | O(1) clone/split |
| Widen integer | `u32::from(x)` / `.into()` | Lossless, compile-checked |
| Narrow integer | `u8::try_from(x)?` | Explicit failure |
| Overflow as error | `.checked_add()` → `Option` | |
| Clamp arithmetic | `.saturating_add()` / `.clamp(lo,hi)` | `clamp` panics if `lo>hi` |
| Never-zero value | `NonZeroU32` | Free `Option`, niche |
| Approx float equality | `(a-b).abs() < eps` | Not `==` |
| Sort floats | `.total_cmp()` | Not `partial_cmp().unwrap()` |
