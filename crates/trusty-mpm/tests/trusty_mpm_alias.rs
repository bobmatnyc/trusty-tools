//! The `trusty-mpm` alias behaves as `tm` (owner ruling 2026-09-27).
//!
//! Why: `trusty-mpm` is now a shim that execs `tm`, kept because installed
//! launchd plists, hook commands, and `.mcp.json` entries still name it. Those
//! callers pass arguments, pipe stdin (hooks, MCP stdio), and read the exit
//! status, so each of the three must reach `tm` unchanged.
//! What: runs the same invocation through the alias and through `tm`, and
//! compares stdout and exit status.
//! Test: `cargo test -p trusty-mpm --test integration trusty_mpm_alias::`.

use crate::common;

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use trusty_agents_common::compress::ENV_COMPRESS_NO_RTK;

/// Run `cmd` with `args` and `stdin`, returning its captured output.
fn run(mut cmd: Command, args: &[&str], stdin: &str) -> Output {
    let mut child = cmd
        .args(args)
        .env(ENV_COMPRESS_NO_RTK, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

#[test]
fn the_alias_prints_what_tm_prints() {
    let alias = run(common::alias_command(), &["--version"], "");
    let tm = run(common::tm_command(), &["--version"], "");
    assert!(alias.status.success(), "alias --version failed: {alias:?}");
    assert!(!alias.stdout.is_empty(), "alias printed nothing");
    assert_eq!(alias.stdout, tm.stdout);
}

#[test]
fn the_alias_forwards_stdin_and_the_exit_status() {
    // An unmatched tool name is a byte-for-byte passthrough of stdin.
    let args = ["compress", "--tool", "alias-passthrough"];
    let alias = run(common::alias_command(), &args, "alias stdin\n");
    assert!(alias.status.success(), "alias compress failed: {alias:?}");
    assert_eq!(alias.stdout, b"alias stdin\n");

    let bad = ["no-such-subcommand-for-the-alias-test"];
    let alias = run(common::alias_command(), &bad, "");
    let tm = run(common::tm_command(), &bad, "");
    assert!(!tm.status.success(), "tm accepted a bogus subcommand");
    assert_eq!(alias.status.code(), tm.status.code());
}
