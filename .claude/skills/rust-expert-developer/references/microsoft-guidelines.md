# Microsoft Rust Guidelines — Universal

Distilled reference of the **Universal** section of Microsoft's *Pragmatic Rust Guidelines* — rules that apply across all Rust projects regardless of domain. Each item keeps its upstream code (`M-*`) so you can cross-reference the source.

**Source:** https://microsoft.github.io/rust-guidelines/guidelines/universal/index.html

> Note on normative language: the source uses "should" / "must" prescriptively but does **not** define severity levels or badges on the universal page. Treat "must" as hard requirements and "should" as strong defaults with narrow, justified exceptions. All 11 universal items live on the single index page as anchored sections.

---

## Consistency & Tooling

### M-UPSTREAM-GUIDELINES — Follow the upstream guidelines
- **Rule:** Apply the established community guidelines *in addition to* this book; they are complementary, not replaced.
- **Why:** Produces a codebase that reflects community lessons and does not surprise users or contributors.
- **Consult:**
  - Rust API Guidelines — esp. `C-CONV`, `C-GETTER`, `C-COMMON-TRAITS`, `C-CTOR`, `C-FEATURE`
  - Rust Style Guide
  - Rust Design Patterns
  - Rust Reference — Undefined Behavior

### M-STATIC-VERIFICATION — Use static verification
- **Rule:** Systematically run static analysis on developer machines *and* at check-in gates.
- **Why:** Maintains consistency and freedom from common issues cheaply.
- **Toolset:**
  - Compiler lints — enable e.g. `ambiguous_negative_literals`, `missing_debug_implementations`, `redundant_imports`, `unsafe_op_in_unsafe_fn`, `unused_lifetimes`.
  - Clippy — all major groups: cargo, complexity, correctness, pedantic, perf, style, suspicious.
  - `rustfmt` — formatting consistency.
  - `cargo-audit` — dependency vulnerabilities.
  - `cargo-hack` — feature-combination validation.
  - `cargo-udeps` — unused dependencies.
  - `miri` — unsafe-code verification.

### M-LINT-OVERRIDE-EXPECT — Lint overrides should use `#[expect]`
- **Rule:** Prefer `#[expect(...)]` over `#[allow(...)]` for local lint overrides; always give a `reason`.
- **Why:** `#[expect]` warns when the lint *stops* firing, so stale suppressions can't accumulate — keeps the lint set current and tidy.
- **Exception:** `#[allow]` is fine for generated / macro-generated code.

```rust
#[expect(clippy::unused_async, reason = "API fixed, will use I/O later")]
pub async fn ping_server() {
    // Stubbed out for now
}
```

---

## Public API Surface

### M-PUBLIC-DEBUG — Public types are `Debug`
- **Rule:** Every public type a crate exposes should implement `Debug` (usually via `#[derive(Debug)]`).
- **Why:** Enables easy debugging by consumers without leaking sensitive data.
- **Sensitive data:** Write a custom `Debug` that masks secrets, and add a unit test asserting the secret never appears.

```rust
#[derive(Debug)]
struct Endpoint(String);

// Secret-bearing type: mask + test
impl std::fmt::Debug for UserSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UserSecret(...)")
    }
}

#[test]
fn secret_is_masked() {
    let s = UserSecret("token".to_string());
    assert!(!format!("{s:?}").contains("token"));
}
```

### M-PUBLIC-DISPLAY — Public types meant to be read are `Display`
- **Rule:** Public types intended to be read by humans should implement `Display`.
- **Why:** Improves usability.
- **Applies to:** error types (required by `std::error::Error`), string-like wrappers, any human-readable type.
- **Notes:** Redact sensitive data as in M-PUBLIC-DEBUG; follow Rust conventions for newlines / escape sequences.

---

## Crate & Module Structure

### M-SMALLER-CRATES — If in doubt, split the crate
- **Rule:** Err toward too many crates rather than too few. If a submodule can be used independently, move it into its own crate.
- **Why:** Faster compile times and better modularity.
- **Side benefit:** Splitting restricts `pub(crate)` access across the boundary, which pressures you toward a cleaner, more flexible *public* API.
- **Crate vs. feature:** A **crate** = independently useful functionality; a **feature** = optional extra capability of a crate. Umbrella crates may re-export split pieces for convenience (proc macros, runtimes), but keep re-exports sparse otherwise.

---

## Naming

### M-WEASEL-WORDS — Names are free of weasel words
- **Rule:** Drop generic, information-free suffixes from type/trait names. Common offenders: `Service`, `Manager`, `Factory`.
- **Why:** Weasel words obscure what the type actually does; removing them improves readability.
- **Do / Don't:**
  - `BookingService` → `Bookings`
  - `Manager` → a descriptive role, e.g. `BookingDispatcher`
  - `Factory` → `Builder` (the builder pattern), or accept `impl Fn() -> Foo` instead of a `FooBuilder` for repeatable instantiation.

### M-SHORT-NAMES — Names of items are short
- **Rule:** Keep identifiers short: don't compound more than ~2 short words, don't bake module/crate info into prefixes, and prefer accepted abbreviations.
- **Why:** Matches idiomatic Rust; the module path already provides context.
- **Do / Don't:**
  - `AppConfig` not `GlobalApplicationConfig`
  - `foo::Id` not `foo::FooId`
  - `CallbackFn` not `CallbackFunction`

### M-REGULAR-FN — Prefer regular over associated functions
- **Rule:** Use associated functions primarily for *instance creation* (constructors), not general-purpose computation. Move unrelated computation to free functions.
- **Why:** Free functions are first-class in Rust and read more clearly than namespaced helpers.

```rust
struct Database {}

impl Database {
    fn new() -> Self { Self {} }   // Ok: constructs an instance
    fn query(&self) {}             // Ok: method with a receiver
    // fn check_parameters(p: &str) {}  // Not ok: unrelated computation
}

fn check_parameters(p: &str) {}     // Correct: a regular free function
```

---

## Documentation & Observability

### M-DOCUMENTED-MAGIC — Magic values are documented
- **Rule:** Every hardcoded magic value in production code must carry a comment covering: why the value was chosen, non-obvious side effects if it changes, and any external systems that depend on it.
- **Why:** Enables safe refactoring and maintenance. Prefer a named `const` over an inline literal.

```rust
// Bad
wait_timeout(60 * 60 * 24).await; // Wait at most a day

// Best
/// Large enough to let the upstream server finish; too low aborts valid
/// requests. Based on api.foo.com timeout policy.
const UPSTREAM_SERVER_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 24);
```

### M-LOG-STRUCTURED — Use structured logging with message templates
- **Rule:** Log structured events with named properties and message templates (per the message-templates spec), not ad-hoc formatted strings.
- **Why:** Cheap to emit and strongly filterable downstream.
- **Practices:**
  - **No string formatting** — use templates, not `format!()` allocations.
  - **Name events hierarchically** — `<component>.<operation>.<state>`.
  - **Follow OpenTelemetry** attribute names — e.g. `file.path`, `http.request.method`.
  - **Redact sensitive data** — never log raw emails, tokens, or PII.

```rust
// Bad: eager formatting, no structure
tracing::info!("file opened: {}", path);

// Good: named props + message template
event!(name: "file.open.success", Level::INFO,
       file.path = path.display(), "file opened: {{file.path}}");

// Redaction
// Bad:  event!(Level::INFO, user.email = user.email, ...);
// Good: event!(Level::INFO, user.email.redacted = redact_email(user.email), ...);
```

---

## Quick checklist

- [ ] Also follow the **upstream** Rust API Guidelines, Style Guide, and Design Patterns (M-UPSTREAM-GUIDELINES).
- [ ] Run clippy (all groups), rustfmt, cargo-audit, cargo-hack, cargo-udeps, and miri in CI *and* locally; enable the recommended compiler lints (M-STATIC-VERIFICATION).
- [ ] Suppress lints with `#[expect(..., reason = "...")]`, not `#[allow]` (M-LINT-OVERRIDE-EXPECT).
- [ ] Derive `Debug` on all public types; custom-mask secrets and test the masking (M-PUBLIC-DEBUG).
- [ ] Implement `Display` on human-readable public types, including all errors (M-PUBLIC-DISPLAY).
- [ ] Split independently usable modules into their own crates; know crate-vs-feature (M-SMALLER-CRATES).
- [ ] Strip weasel words (`Service`/`Manager`/`Factory`) from names (M-WEASEL-WORDS).
- [ ] Keep identifiers short (≤2 words, no path prefixes, prefer abbreviations) (M-SHORT-NAMES).
- [ ] Reserve associated fns for constructors; put other computation in free functions (M-REGULAR-FN).
- [ ] Document every magic value (rationale + side effects + external deps); prefer named `const` (M-DOCUMENTED-MAGIC).
- [ ] Emit structured, hierarchically-named, OTel-aligned log events with redaction — no `format!` logging (M-LOG-STRUCTURED).
