//! The `trusty-okf` command line (SPEC-OKF-01 §4.10, 3g B).
//!
//! Why: a person imports a document tree and feeds a bundle without an
//! agent. The binary is a thin shell: every rule lives in the library, so
//! the CLI adds no second engine and no MCP surface (B7).
//! What: passes its arguments to [`trusty_okf::cli::run`] and exits with
//! the §4.10 code it returns. It reads its arguments and nothing else of
//! its environment (B3). Slice S8 of #10746.
//! Test: `cli_binary_exits_4_when_the_feed_cannot_reach_a_daemon`.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = trusty_okf::cli::run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
