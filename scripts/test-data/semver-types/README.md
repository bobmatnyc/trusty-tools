# semver-types fixtures

Inputs for `scripts/check_semver_types_selftest.sh`, which tests
`scripts/check_semver_types.sh`.

## The probe pair

`probe-base.json` and `probe-cur.json` are the real rustdoc JSON of the 9-break
probe crate that established the gap the differ exists to close. Their sources
are committed beside them as `probe-base.rs` and `probe-cur.rs`, one file each,
26 lines: nine deliberate public-API breaks between them.

Run against that pair at `--release-type patch` — the strictest setting it has —
`cargo-semver-checks` 0.50.0 caught 2 and missed 7. The two it caught are a
removed `pub fn` and an added enum variant. The seven it missed are all type
substitutions:

| break | probe-base | probe-cur |
|---|---|---|
| method return | `S::method_ret -> u64` | `-> Result<u64, String>` |
| method parameter | `S::method_param(x: u64)` | `(x: String)` |
| free-fn return | `free_ret() -> u64` | `-> Result<u64, String>` |
| free-fn parameter | `free_param(x: u64)` | `(x: String)` |
| struct field | `F.f: u64` | `F.f: i64` |
| `pub const` | `C: u64` | `C: i64` |
| trait-method return | `T::tm(&self) -> u64` | `-> Result<u64, String>` |

Those seven rows are the differ's regression suite. They fail against the tool
that missed them, which is what makes them tests rather than a description of
current behaviour.

## How the pair was captured

The probe crate was built twice, at 0.1.0 and 0.2.0, and
`cargo semver-checks --package lintprobe --baseline-version 0.1.0` was run to
produce the rustdoc JSON it compares. The document taken is the one
cargo-semver-checks caches:

```
target/semver-checks/local-lintprobe-<version>-<target>-<hash>/target/doc/lintprobe.json
```

To regenerate: build a crate from `probe-base.rs` at 0.1.0 and from
`probe-cur.rs` at 0.2.0, run `cargo semver-checks` over each, and take that file.

**Two reductions were applied**, both to what the differ never reads:

- `span`, `docs` and `links` are nulled on every item. `span` holds the absolute
  path of the machine that built it, which has no business in a fixture.
- `paths` is filtered to this crate's own entries. It is rustdoc's whole-universe
  path map — 219 KB of the 250 KB original — and `check_semver_types.sh` renders
  types from the type nodes themselves, never from that map.

Everything the differ does read is verbatim, including the 45 synthetic and
blanket impls, which is what proves it skips them.

## The format-61 pair

`probe-v61-base.json` / `probe-v61-cur.json`, sources beside them as
`probe-v61-base.rs` / `probe-v61-cur.rs`. Same nine breaks as the pair above plus
a tenth — `S::async_ret`, `Vec<u64>` -> `Result<Vec<u64>, String>` — at
rustdoc-JSON `format_version` 61.

**Why a second pair exists.** The differ shipped with
`SUPPORTED_FORMAT_VERSIONS = (57,)` while every rustdoc on the machine emitted
61, so `--crate <anything>` exited 3 and it compared nothing on any real crate.
Its self-test stayed green the whole time, because the format-57 fixtures above
were the only documents it ever read. A differ pinned to a schema version needs a
fixture at the version the toolchain actually emits, or "the tests pass" and "the
tool works" come apart with nothing to say so.

**Why the async row.** rustdoc records an `async fn` UN-DESUGARED: `sig.output`
holds the inner type, not the `impl Future` the source implies. So an async
return is an ordinary type position and needs no special handling — verified, not
assumed. Pinning it means a future schema that starts recording the desugared
future instead fails here, rather than silently comparing every async return
equal.

**To regenerate**, or to add a pair at the next format version:

```
cargo +nightly rustdoc -- -Zunstable-options --output-format json
```

over a crate named `lintprobe` built from each `.rs`, then apply the same two
reductions described above. `scripts/check_semver_types_selftest.sh` case 15
asserts every version in `SUPPORTED_FORMAT_VERSIONS` has a pair behind it, so
adding a version to that tuple without a fixture fails the self-test.

## Derived fixtures

`mutate.py` derives the fail-closed inputs from `probe-base.json` at test time —
one field each, so the mutation is readable rather than buried in a minified
copy. Modes: `additive`, `bad-format`, `unknown-type`, `empty`.

## Hand-written fixtures

- `not-rustdoc.json` — valid JSON, not a rustdoc document.
- `malformed.json` — truncated mid-object; not valid JSON at all.
