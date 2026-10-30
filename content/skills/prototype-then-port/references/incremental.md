# Incremental port: PyO3 for one hot spot, or a whole binary

## Choose the shape

| Situation | Choose |
|---|---|
| The cost sits in one function, and Python is the host | PyO3 extension for that function |
| Callers are CLIs, hooks or daemons, not Python code | whole Rust binary |
| Startup time or a Python-free install matters | whole Rust binary |
| Many functions are slow, or the whole program is the cost | whole Rust binary |
| The hot spot is I/O-bound | neither; fix the I/O first |

Measure the function before choosing. A PyO3 call adds a boundary cost on
every call. Pass large data once, not per item.

## PyO3 with maturin

Written against PyO3 0.30. The single-argument `&Bound<'_, PyModule>` module
form arrived in 0.21 and is the only form since 0.23. `Python::detach`
replaced `allow_threads` in 0.26, and 0.28 removed `allow_threads`.

```rust
use pyo3::prelude::*;

#[pyfunction]
fn score(py: Python<'_>, data: Vec<f64>) -> PyResult<f64> {
    py.detach(|| Ok(data.iter().sum()))   // release the GIL
}

#[pymodule]
fn fastpath(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(score, m)?)
}
```

- Release the GIL with `py.detach` around pure Rust work. Do not touch
  Python objects inside that closure.
- Convert errors with `impl From<MyError> for PyErr`. Raise a specific
  exception type such as `ValueError`, not a generic one.
- Build with `maturin develop` for local tests and `maturin build --release`
  for wheels. Use an `abi3-py3xx` feature to build one wheel for that Python
  version and later.
- Accept `bytes` for large binary input. Avoid copying it into a `Vec` when
  a slice works.
- Keep the Python wrapper: the module name, the signature and the exception
  types must match the old function, so callers do not change.

## Whole binary

- Build with `cargo build --release`. Ship one binary per platform.
- Keep the same CLI contract as the Python entry point (see the interface
  contract in SKILL.md).
- Use `[profile.release]` settings only after the parity run passes, so a
  profile change does not add a second variable to a failing run.

## Python shim during cutover

When callers invoke a Python path, keep that path as a thin shim during the
fallback period. The shim replaces its body with an exec of the binary.

```sh
#!/bin/sh
exec /path/to/tool-rs "$@"
```

- `exec` keeps the exit code and the signals unchanged. A child process
  wrapper can change both.
- In Python, `os.execv(BIN, [BIN, *sys.argv[1:]])` does the same job.
- Name the shim, its removal date and the ledger row that owns it.

## Removing the Python

1. Switch every caller to the binary in one change.
2. Run the parity harness once more against the removed Python tree, using
   the last pinned commit in a separate checkout.
3. Delete the Python module, its tests, its docs entries and the shim.
4. Update the ledger row: Python removed, date, commit.

Do not keep the Python in the tree "for reference". The ledger and the
pinned commit already record it.
