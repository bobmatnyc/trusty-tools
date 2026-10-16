//! Bundled `trusty-embedderd-py` shim — produced by `cargo install trusty-search`.
//!
//! Why (#9468): the daemon finds the Python/MPS sidecar launcher as a sibling
//! of its own executable, but no install path produced it while the binary
//! belonged to the trusty-embedderd-py package. Owning it here installs it
//! next to `trusty-search`, as `trusty-embedderd` already is.
//! What: forwards to `trusty_embedderd_py::run()`, which ensures the venv and
//! `exec`s `python -m trusty_embed_sidecar` with the forwarded arguments.
//! Test: `launcher_bin_ships_with_trusty_search_only` (integration target)
//! pins that trusty-search alone declares this binary;
//! `cargo_install_trusty_search_produces_both_binaries` (`#[ignore]`) checks
//! the installed bin dir.

// See the rationale on the library crate root.
#![deny(rustdoc::broken_intra_doc_links)]

fn main() -> std::process::ExitCode {
    trusty_embedderd_py::run()
}
