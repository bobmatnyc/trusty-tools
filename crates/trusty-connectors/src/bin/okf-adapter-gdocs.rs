//! `okf-adapter-gdocs`: the trusty-okf import adapter for `gdocs` stores
//! (SPEC-OKF-01 §3 (m) A). Slice S13 of #10746.
//!
//! Why: the trusty-okf CLI starts one adapter program per source kind with
//! `--adapter PATH` and exactly the environment list its caller supplies
//! (B10); this program holds the Google credential, trusty-okf never does.
//! What: passes its arguments to [`trusty_connectors::gdocs::main`] and
//! exits with the code it returns. It speaks the acquire protocol on stdin
//! and stdout; stderr is diagnostic only.
//! Test: `crates/trusty-connectors/tests/conformance.rs`.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(trusty_connectors::gdocs::main(&args));
}
