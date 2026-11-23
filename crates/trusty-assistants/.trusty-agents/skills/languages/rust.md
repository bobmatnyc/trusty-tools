---
name: rust
description: Ownership, lifetimes, error handling, async tokio, iterators, Arc/Mutex
tags: [rust, ownership, lifetimes, async, tokio, error-handling, iterators]
---

# Rust Language Skill

## Ownership and the Borrow Checker Mental Model

Each value has exactly one owner. Ownership is transferred (moved) on assignment
unless the type implements `Copy`. References borrow the value: `&T` is a shared
(read-only) borrow; `&mut T` is an exclusive (read-write) borrow. The compiler
enforces that at any point, you have either one `&mut T` **or** any number of
`&T` — never both.

```rust
// Move semantics — s1 is no longer valid after this line
let s1 = String::from("hello");
let s2 = s1;             // s1 moved into s2
// println!("{s1}");     // compile error: value used after move

// Borrow — s1 still valid
let s1 = String::from("hello");
let len = calculate_length(&s1);   // borrow, s1 still owned here
```

When a function needs to return data it also received, clone only what you
must — prefer designing APIs to take owned values when the caller no longer
needs them.

## Lifetime Annotations

Most lifetimes are **elided** (inferred by the compiler). Annotate explicitly
only when the compiler cannot determine the relationship between input and output
lifetimes.

```rust
// Elision applies — no annotation needed
fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

// Explicit lifetime required: output borrows from ONE of two inputs
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {
    if x.len() > y.len() { x } else { y }
}

// Struct holding a reference needs a lifetime parameter
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}
```

## Error Handling: thiserror (library) vs anyhow (application)

- **thiserror**: use in library crates. Generates `std::error::Error` impls from
  enum variants with `#[derive(thiserror::Error)]`. Callers can match on variants.
- **anyhow**: use in binary/application crates. `anyhow::Result<T>` + the `?`
  operator gives cheap error propagation with context. Avoid it in library APIs
  that callers need to handle programmatically.

```rust
// Library crate — thiserror
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent not found: {name}")]
    NotFound { name: String },
    #[error("IPC error: {0}")]
    Ipc(#[from] std::io::Error),
}

// Application crate — anyhow
use anyhow::{Context, Result};

async fn run() -> Result<()> {
    let cfg = load_config("config.toml")
        .context("failed to load config")?;
    Ok(())
}
```

## Async with Tokio

Always use `async fn` for IO-bound operations. Avoid blocking calls (`std::fs`,
`std::thread::sleep`) inside async functions — they block the tokio thread pool.

```rust
use tokio::fs;
use tokio::time::{sleep, Duration};

async fn read_skill(path: &std::path::Path) -> anyhow::Result<String> {
    let content = fs::read_to_string(path).await?;
    Ok(content)
}

// Spawn concurrent tasks
let (r1, r2) = tokio::join!(fetch_users(), fetch_products());
```

Use `tokio::sync::Mutex` (not `std::sync::Mutex`) when the guarded value must
be held across `.await` points.

## Iterator Chains

Prefer iterator chains over manual loops. They compose, are lazy by default,
and signal intent clearly.

```rust
let top_scores: Vec<u32> = scores
    .iter()
    .filter(|&&s| s > 0)
    .map(|&s| s * 2)
    .take(10)
    .collect();

// Summing with a fold
let total: u32 = items.iter().map(|i| i.value).sum();

// Finding with short-circuit
let first_match = entries.iter().find(|e| e.active && e.score > threshold);
```

## When to Use Arc<Mutex<T>> vs Channels

- **`Arc<Mutex<T>>`**: shared state read/written from multiple tasks where
  contention is low and the lock is held briefly. Do NOT hold across `.await`.
- **`tokio::sync::mpsc` channels**: producer-consumer pipelines; preferred when
  one task owns the data and others send it commands. Reduces lock contention.
- **`tokio::sync::RwLock`**: when reads dominate and writes are rare.

```rust
// Arc<Mutex<T>> — shared counter, never held across await
let counter = Arc::new(Mutex::new(0u64));
let c = counter.clone();
tokio::spawn(async move {
    let mut guard = c.lock().await;  // tokio Mutex, OK across .await point
    *guard += 1;
});

// Channel — task owns a registry, others send update commands
let (tx, mut rx) = tokio::sync::mpsc::channel::<Command>(32);
tokio::spawn(async move {
    while let Some(cmd) = rx.recv().await {
        registry.handle(cmd).await;
    }
});
```

## #[derive(Debug, Clone)] Conventions

Add `#[derive(Debug)]` to every struct and enum unless it holds a non-Debug
field. Add `Clone` when callers need copies (e.g. for Arc sharing or test
assertions). Derive order: `Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize`.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AgentId(String);
```

## The ? Operator and From Conversions

`?` is syntactic sugar for `return Err(From::from(e))`. Wire up automatic
conversions with `#[from]` in `thiserror` enums or `impl From<E> for MyError`.

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

fn load(path: &Path) -> Result<Config, MyError> {
    let raw = std::fs::read_to_string(path)?;   // io::Error → MyError::Io
    let cfg = serde_json::from_str(&raw)?;       // serde::Error → MyError::Json
    Ok(cfg)
}
```

## Cargo Workspace Conventions

In a workspace, keep all dependency versions in the root `Cargo.toml` under
`[workspace.dependencies]` and reference them from member crates with
`dep = { workspace = true }`. This prevents version drift.

```toml
# root Cargo.toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }

# member crate Cargo.toml
[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
```

## Anti-patterns

- Never use `unwrap()` in production code paths — use `?` or explicit error handling.
- Never use `clone()` to silence borrow checker errors without understanding why.
- Never use `unsafe` without a detailed safety comment explaining the invariants.
- Never use `std::sync::Mutex` held across `.await` — it can deadlock.
- Never mix `std::thread::sleep` with async code — use `tokio::time::sleep`.
