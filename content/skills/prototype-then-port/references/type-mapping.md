# Type mapping: Python to Rust

Map types before writing logic. Each row names the default Rust choice and
the case that changes it.

## Records: dataclass and pydantic to struct + serde

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Job {
    pub job_id: String,           // python: job_id -> "jobId" on the wire
    #[serde(default)]
    pub retries: u32,             // python: retries: int = 0
    pub timeout_s: Option<f64>,   // python: timeout_s: float | None = None
}
```

- `rename_all` sets the wire name for every field. Use it instead of one
  `rename` per field when the whole model uses one convention.
- `deny_unknown_fields` rejects extra keys. Python pydantic defaults to
  ignoring them (`extra="ignore"`), so set this only when the Python
  rejects them too. Check the prototype's config before choosing.
- `#[serde(default)]` fills a missing field from `Default`. Use it for a
  Python field with a default value. Use `default = "fn_name"` for a
  non-trivial default.

### Option, missing and null

| Input | Rust field type | Result |
|---|---|---|
| key missing | `Option<T>` | `None` |
| key is `null` | `Option<T>` | `None` |
| key missing | `Option<T>` with `deserialize_with` | error, unless `#[serde(default)]` is also set |
| key missing | `T` with `#[serde(default)]` | `T::default()` |
| key missing | `T` (no default) | error |

The derive treats a missing `Option<T>` field as `None` on its own. A
`deserialize_with` function turns that off: the field is required again
until you add `#[serde(default)]`.

Option cannot tell "missing" from "null". When the Python code treats them
differently, use `Option<Option<T>>` with
`#[serde(default, deserialize_with = "fn_name")]`, where the function wraps
the inner value in `Some`. Missing then gives `None`, `null` gives
`Some(None)`, and a value gives `Some(Some(v))`. Without `default`, a missing
key is an error. Test both inputs against the golden corpus.

## Enums and Literal

A Python `Literal["fast", "slow"]` or a string enum maps to a Rust enum with
a rename rule:

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode { Fast, Slow }
```

A tagged union (a discriminator field such as `"type"`) maps to an internally
tagged enum: `#[serde(tag = "type")]`. Use `untagged` only as a last resort.
It tries each variant in order and gives poor error messages.

## Dict to HashMap or BTreeMap

- Python dicts keep insertion order. `HashMap` has no order, so iteration
  order changes between runs.
- `BTreeMap` iterates in sorted key order. Use it when output order matters
  and the Python sorts keys (`sort_keys=True`).
- `serde_json::Map` sorts keys by default (BTreeMap). Enable the
  `preserve_order` feature for `IndexMap`-style insertion order when the
  Python output relies on insertion order.
- When a dict has a fixed set of keys, use a struct instead. A struct
  serializes its fields in declaration order.

## list, tuple, set

- `list` maps to `Vec<T>`.
- A fixed-length `tuple` maps to a Rust tuple `(A, B)` or a small struct.
  Serde writes tuples as JSON arrays. Python `json.dumps` also writes arrays.
- `set` maps to `HashSet<T>` (unordered) or `BTreeSet<T>` (sorted). Sort
  before output if the Python sorts.

## datetime, zoneinfo

- Naive `datetime` (no offset) maps to `chrono::NaiveDateTime` or
  `jiff::civil::DateTime`.
- Aware `datetime` maps to `chrono::DateTime<FixedOffset>` or
  `chrono::DateTime<Utc>`, or to `jiff::Zoned` for a named zone.
- `zoneinfo.ZoneInfo("Europe/Paris")` needs a tz database: `chrono-tz`
  (compiled in), or jiff, which reads the system tz database and can bundle
  one through a crate feature.
- A naive value has no zone. Converting it needs an explicit zone, so the
  port must name one.

## pathlib to Path and PathBuf

- `Path` and `PathBuf` hold OS strings. They can hold non-UTF-8 names.
- `to_str()` returns `Option<&str>` and fails on non-UTF-8 names.
  `to_string_lossy()` replaces bad bytes, which loses data. Use it for
  display only.
- Keep paths as `PathBuf` through the code. Convert to `String` only at a
  JSON or text boundary, and decide the failure behavior there.

## bytes versus str

- `str` maps to `String` (owned) or `&str` (borrowed). Always valid UTF-8.
- `bytes` maps to `Vec<u8>` or `&[u8]`. It may not be valid UTF-8.
- `bytes.decode()` maps to `String::from_utf8`, which returns an error on
  bad input. Python's `errors="replace"` maps to `from_utf8_lossy`.
- Python's `surrogateescape` has no direct Rust equivalent. Keep such data as
  bytes.

## int overflow

- Python ints do not overflow. Rust `i64` and `u64` wrap in release builds
  and panic in debug builds, under the default `overflow-checks` setting.
- Use `checked_add`, `checked_mul` and similar, and map `None` to an error.
- For values that can exceed 64 bits, use `i128` or `num-bigint`.

## Decimal

- Python `Decimal` is exact decimal arithmetic. Do not map it to `f64`.
- Use `rust_decimal` or `bigdecimal`. Check that the precision and rounding
  mode match the Python context. Python's default rounding is
  `ROUND_HALF_EVEN`.
