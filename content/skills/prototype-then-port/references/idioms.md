# Idioms: Python constructs to Rust

## Exceptions to Result

Map each `except` branch to one enum variant. Keep the mapping in one place.

```rust
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("file not found: {0}")]          // except FileNotFoundError
    NotFound(PathBuf),
    #[error("invalid json: {0}")]            // except json.JSONDecodeError
    Json(#[from] serde_json::Error),
}
```

- Use `thiserror` in a library crate. Callers match on variants.
- Use `anyhow` in a binary crate, where the top level prints the error chain
  and sets the exit code. Add `.context("...")` at each boundary.
- Each variant maps to one exit code and one message. Keep the Python message
  text where a caller parses stderr.
- `try/finally` becomes a `Drop` guard or an explicit cleanup call on every
  return path.

## argparse and click to clap

Use `clap` with the derive feature.

```rust
#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 3)]
    retries: u32,
    input: PathBuf,
}
```

- Usage errors exit with code 2, as argparse does. `--help` and `--version`
  exit with 0.
- The help text layout differs from argparse. Treat help output as a
  normalized field in the parity harness, not an exact match.
- Keep flag names, short flags and defaults identical to the contract.

## logging to tracing

- Map `logging.getLogger` calls to `tracing::info!` and friends. Use
  `tracing-subscriber` to set the output format and level filter.
- Python logging writes to stderr by default. Configure `tracing-subscriber`
  to write to stderr too.
- Log lines are not usually part of the contract. Check whether any caller
  parses them before matching the format.

## subprocess to std::process::Command

- `subprocess.run(["git", "status"])` maps to
  `Command::new("git").arg("status").output()`.
- `shell=True` has no direct equivalent. Pipes and globs must be rebuilt in
  code, or the shell call stays in a thin wrapper. Do not reproduce a shell
  with string concatenation.
- `check=True` maps to `if !status.success()`.
- `capture_output=True` maps to `.output()`, which returns stdout, stderr and
  the status together.
- Set `env_clear()` and `env()` when the Python passes an explicit
  environment.

## asyncio to tokio, only when needed

- Start synchronous. A CLI that waits on one request does not need async.
- Use `tokio` only when the Python runs concurrent I/O that matters for
  latency. Use `tokio::join!` or `JoinSet` for the same fan-out.
- Do not mix `std::thread::sleep` into async code. Use `tokio::time::sleep`.

## requests and httpx

- Use `reqwest::blocking` for a synchronous port.
- Use async `reqwest` only with `tokio`.
- `requests` has no default timeout. The async `reqwest::Client` has none
  either, but `reqwest::blocking::Client` defaults to 30 seconds. Set
  `timeout` explicitly on every client, to the value the Python uses; pass
  `None` to the blocking builder for no timeout.
- Keep the TLS backend the same as the Python environment, or check
  certificate behavior with the golden corpus.

## re to regex

- The `regex` crate does not support backreferences or lookaround. Use
  `fancy-regex` if the Python pattern needs them. It backtracks, so check
  its speed on large inputs.
- `re.match` anchors at the start. `Regex::is_match` searches anywhere.
  Add `^` or `\A` to get Python's `match` behavior.
- `re.fullmatch` maps to `\A(?:pattern)\z`. In `regex`, `\z` and `$` (without
  the `m` flag) both match only at the end of the text. Python's `$` also
  matches before a trailing newline. `regex` rejects Python's `\Z`, so
  translate it to `\z`. Test inputs ending in `\n`.
- Python `\d` and `\w` match Unicode by default, and so do `regex`'s (`\d` is
  `\p{Nd}`). The `\w` classes are not identical; check them against the
  golden corpus, and test non-ASCII digits and letters.

## json.dumps to serde_json

Python defaults and serde_json defaults differ on each of these.

| Setting | Python | serde_json | Port choice |
|---|---|---|---|
| separators | `", "` and `": "` on one line | compact `,` and `:` | a custom `serde_json::ser::Formatter`; `to_string_pretty` writes indented multi-line output, which matches neither |
| key order | insertion | `Map` sorted (BTreeMap); structs in field order | `preserve_order` feature, or sort keys where the Python passes `sort_keys=True` |
| non-ASCII | `\uXXXX` escapes (`ensure_ascii`) | raw UTF-8 | write a formatter if the consumer needs ASCII |
| float `1e16` | `1e+16` | depends on the serde_json version: 1.0.143 writes `1e16`, 1.0.154 writes `1e+16` | pin the version and test with the corpus |
| NaN and Infinity | written as `NaN` and `Infinity` | written as `null` | decide the rule and test it |

Set the formatting explicitly in one function. Do not spread serializer
calls across the code.

## generators to iterators

- A generator with `yield` maps to an `impl Iterator<Item = T>`. Keep it
  lazy: no `collect()` until the caller needs a `Vec`.
- Use `std::iter::from_fn` for a generator with local state.

## context managers to Drop

- `with open(...)` maps to a value that closes in `Drop`.
- `Drop` cannot return an error. For a write that must report failure, expose
  an explicit `finish()` or `close()` that returns `Result`, and call it.
- `BufWriter` drops without reporting flush errors. Call `flush()` before drop.

## global state to explicit config

- Pass a config struct from `main` down through the functions. Do not use a
  global static for settings.
- Use `std::sync::OnceLock` or `LazyLock` only for a true process-wide
  singleton, such as a compiled regex or a lookup table.
- Module-level state read from environment variables should be read once at
  startup into the config struct.
