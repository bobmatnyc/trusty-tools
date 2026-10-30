# Pitfalls: silent behavior changes

Each entry names the Python behavior, the Rust behavior, and the test that
catches the difference. Add a golden case for each one that applies.

## Float formatting

- Python `repr(1e16)` is `1e+16`. Rust `Display` for `f64` is
  `10000000000000000`. Rust `Debug` is `1e16`. serde_json output depends on
  its version (see the JSON table in `idioms.md`).
- Both use shortest round-trip digits, so `0.1 + 0.2` prints the same in both.
- Format floats through one function, with the precision the contract states.
- Test: a golden case with values at `1e15`, `1e16`, `1e-5`, `-0.0`, `nan`
  and `inf`.

## Dict and JSON key order

- Python dicts keep insertion order. `HashMap` order changes between runs.
- `serde_json::Map` sorts keys unless `preserve_order` is enabled.
- Test: a golden case whose keys are inserted out of alphabetical order.

## Unicode normalization and case folding

- Python compares strings by code point. Rust `String` comparison is by bytes,
  which gives the same order for valid UTF-8.
- Canonically equivalent strings differ in bytes: `é` as one code point or as
  `e` plus a combining accent. Python does not normalize unless asked. Rust
  does not either. Apply `unicode-normalization` only where the Python does.
- `str.casefold()` in Python does full case folding. Rust `to_lowercase()`
  does not match it for every character. Use a folding crate where the
  Python folds.
- Test: `ß`, `İ`, combining marks and emoji in names and keys.

## Path separators and non-UTF-8 names

- Python `os.path` and `pathlib` use the platform separator. Rust `Path` does
  too. A hard-coded `/` in a string breaks on Windows.
- A file name can hold bytes that are not UTF-8. Python `os.fsdecode` maps them
  with `surrogateescape`. Rust `to_str()` returns `None`.
- Test: a file named with a Latin-1 byte, and a name with a space.

## Timezone and DST

- A naive `datetime` has no zone. Converting it to a time needs a zone the
  Python code assumes, often the system zone.
- `datetime.now()` returns local time. `Utc::now()` returns UTC. Mixing them
  shifts values by the offset.
- Ambiguous and skipped local times during a DST change differ between
  libraries. Test one timestamp in each gap.
- Test: the hour before and after a DST change, in the zone the Python uses.

## Integer division and rounding

- Python `//` floors toward negative infinity. Rust `/` truncates toward zero.
  `-7 // 2` is `-4` in Python and `-3` in Rust. `div_euclid` gives the Python
  result only when the divisor is positive: `7 // -2` is `-4` in Python, and
  `7.div_euclid(-2)` is `-3`.
- Python `%` takes the sign of the divisor. Rust `%` takes the sign of the
  dividend. `-7 % 3` is `2` in Python and `-1` in Rust. `rem_euclid` gives
  the Python result only when the divisor is positive: `7 % -3` is `-2` in
  Python, and `7.rem_euclid(-3)` is `1`. For a negative divisor, write a
  floor-mod helper and test it.
- Python 3 `round(x)` on floats rounds half to even. Rust `f64::round` rounds
  half away from zero. Use `f64::round_ties_even` (stable since Rust 1.77).
- `int(x)` on floats truncates in both languages.
- Test: `-7 // 2`, `7 // -2`, `-7 % 3`, `7 % -3`, `round(0.5)`, `round(1.5)`,
  `round(2.5)`.

## Sort stability and key functions

- Python `sort` is stable. Rust `sort` and `sort_by_key` are stable.
  `sort_unstable` is not. Use the stable form whenever the Python sorts.
- Python compares tuples element by element. Rust tuples compare the same
  way, but a key that is a `Vec` or a struct needs `Ord` derived in the
  right field order.
- Floats are not `Ord` in Rust. Use `total_cmp` or `partial_cmp().unwrap()`
  only where NaN cannot appear. Python `sorted` with NaN gives an order that
  depends on the input.
- Test: equal keys in the input, and NaN in a float column.

## Mutable default arguments

- A Python default such as `def f(items=[])` is one shared list for every
  call. Code that depends on that sharing is a bug. Find it in the prototype
  before the port. Do not port the sharing.
- Rust has no default arguments. Each call gets a fresh value, so the port
  changes behavior only where the Python relied on the shared object.
- Test: call the function twice in one process and compare the second result.

## Environment variables and locale

- `std::env::var` returns `Err` on a non-UTF-8 value. Use `var_os` to read
  the raw value, as the Python `os.environb` mapping does.
- Python reads the locale for some formatting and for the file system
  encoding. Rust does not read the locale. A date or number printed with a
  locale-dependent format changes output.
- Test: run the golden cases once with `LANG=C` and once with a UTF-8 locale.

## stdout buffering and partial writes

- Python stdout is block-buffered when it is a pipe. Rust stdout is
  line-buffered. The interleaving of stdout and stderr can change.
  Compare them as separate streams.
- `println!` panics when stdout is a closed pipe. Python raises
  `BrokenPipeError`. Write with `writeln!` and handle the error.
- A crash mid-write leaves a partial file. Write to a temp file in the same
  directory, `fsync`, then rename over the target.
- Test: pipe the output into `head -1` and check the exit status.

## Signals and exit codes

- Uncaught Python exceptions exit with 1 and print a traceback to stderr.
  A Rust panic exits with 101. Return an error from `main` for exit 1.
- Python `KeyboardInterrupt` exits with a code the caller can see as
  signal-terminated, usually 130 from a shell. A Rust process that does not
  install a handler dies from SIGINT with no chance to clean up. Install a
  handler where the Python cleaned up.
- Rust ignores SIGPIPE by default and reports `EPIPE` as an error. Python does
  the same. Check the exit code of a pipeline through the shell.
- Test: exit codes for a usage error, a missing file, and an interrupt.
