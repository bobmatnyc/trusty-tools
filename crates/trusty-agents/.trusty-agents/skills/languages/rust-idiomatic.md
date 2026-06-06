---
name: rust-idiomatic
tags: [language, idioms, rust]
summary: Idiomatic Rust coding guidelines — 2024-2025
---

# Idiomatic Rust — 2024-2025

## Core Philosophy
Idiomatic Rust in 2024-2025 is ownership-clear, `thiserror`/`anyhow`-typed, `tokio`-async, `clippy`-clean, iterator-first, and visibility-scoped (`pub(crate)`). The community optimizes for zero-cost abstractions, fearless concurrency through ownership, and compile-time correctness — favor making invalid states unrepresentable.

## Idioms: DO / DON'T / WHY

**DO** use `thiserror` for library error types (derive `Error` on enum variants) and `anyhow` for application-level error propagation with context. **DON'T** mix both in the same crate, and never use `Box<dyn Error>` as a public library return type. **WHY**: `thiserror` generates `Display`/`From` impls; `anyhow::Error` gives rich context chains for binaries. They are different tools for different layers.

**DO** use the `?` operator for error propagation everywhere. **DON'T** call `.unwrap()` or `.expect()` in library code paths that can reasonably fail. **WHY**: `unwrap` panics — it's only acceptable when you can prove statically the variant is impossible, and a comment should explain why.

**DO** use return-position `impl Trait` in trait methods (RPITIT, stable since Rust 1.75) for async / Future-returning trait methods. **DON'T** reach for the `async-trait` proc macro for new code. **WHY**: RPITIT is zero-cost; `async-trait` allocates a `Box<dyn Future>` per call.

**DO** annotate fallible `Result`-returning functions with `#[must_use]`. **DON'T** silently let callers ignore returned `Result`s. **WHY**: `#[must_use]` makes ignored results a compile-time warning.

**DO** prefer iterator chains (`.iter().map(...).filter(...).collect()`) for transformations. **DON'T** write manual `for` loops with `push` for transformations expressible as a chain. **WHY**: iterator chains are lazy, composable, and LLVM tends to vectorize them well. Keep manual `for` only when you need early `break`/`continue` or in-loop mutation.

**DO** use `Arc<dyn Trait>` for runtime polymorphism across threads, and generic bounds (`impl Trait` / `<T: Trait>`) for compile-time dispatch. **DON'T** default to `Arc` everywhere — check whether `Rc` or owned values suffice. **WHY**: `Arc` adds atomic ref-counting overhead; generics let the compiler monomorphize and inline.

**DO** scope visibility with `pub(crate)` and `pub(super)` for items shared inside a crate but not part of the public API. **DON'T** mark everything `pub` and rely on docs to indicate "internal". **WHY**: visibility is enforced by the compiler; documentation is not, and `pub` is a semver commitment.

**DO** use the sealed-trait pattern for non-exhaustive trait hierarchies meant only for in-crate impls: a private `Sealed` supertrait. **DON'T** leave a public trait open if it represents an internal abstraction. **WHY**: sealed traits allow semver-compatible evolution without breaking downstream users.

**DO** put unit tests in `#[cfg(test)] mod tests { ... }` inside the source file; integration tests go in a top-level `tests/` directory. **DON'T** mix the two — they have different access scopes. **WHY**: in-module unit tests can access private items; `tests/` files cannot.

**DO** use `tokio::spawn` for independent tasks and `tokio::join!` / `tokio::try_join!` for concurrent subtasks of the same logical operation. **DON'T** call blocking I/O (`std::fs`, `std::thread::sleep`) inside an async function — use `tokio::fs` or `tokio::task::spawn_blocking`. **WHY**: blocking inside an async task starves the runtime worker pool.

**DO** clone deliberately and explicitly when you need owned data — and look for ways to avoid it (borrow, `Cow`, restructure ownership). **DON'T** sprinkle `.clone()` to silence borrow-check errors without thinking. **WHY**: cloning a `Vec`/`String` allocates; the borrow checker is usually telling you the lifetime model needs adjusting.

**DO** run `cargo clippy --all-targets --all-features -- -D warnings` in CI. **DON'T** add `#[allow(clippy::...)]` without a comment justifying the suppression. **WHY**: clippy's lint categories include real bug classes, not just style; silencing without a reason hides debt.

## Toolchain
- **Edition**: `2024` (note: reserves `gen` keyword)
- **Formatter**: `cargo fmt` (rustfmt)
- **Linter**: `cargo clippy --all-targets --all-features`
- **Test runner**: `cargo nextest run` for parallel/UX-improved test runs (replaces `cargo test` in CI)
- **Build**: `cargo build --release`; benchmarks via `criterion`; profiling via `cargo flamegraph`

## Anti-Patterns to Reject
- `.unwrap()` / `.expect()` in library code paths — use `?` or document the invariant.
- `Box<dyn Error>` as a public library return type — use `thiserror`-derived enums.
- `async-trait` macro on new code — use RPITIT.
- `clone()` to escape the borrow checker without examining ownership — re-think the lifetime model first.
- `pub` on every item — use `pub(crate)` for internal API.
- Catch-all `.unwrap_or_default()` that silently hides errors callers should know about.
- Mixing `tokio` and `async-std` runtimes in the same workspace.

## 2024-2025 Updates
- Edition 2024 stabilized: reserves `gen`, tightens `impl Trait` lifetime capture rules.
- RPITIT (`-> impl Future` in trait methods) stable since 1.75; `async-trait` is largely obsolete for new code.
- `cargo-nextest` is the de facto standard test runner over `cargo test` for CI.
- `let-else` and `if-let` chains stabilized in 2024 — use them for early-return patterns.
- `std::sync::LazyLock` / `OnceLock` replaced the `lazy_static` / `once_cell` crates for stdlib needs.
