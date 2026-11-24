//! Live check of the clipboard tools on the real pasteboard (issue #11696 Phase C).
//!
//! Why: the fake pasteboard cannot show that `pbcopy` / `pbpaste` under a
//! cleared environment round-trip UTF-8 text, nor that a read of an
//! oversized clipboard stops at the cap. cua-driver 0.23.2 is not used: its
//! bounded manifest has no clipboard resource and refuses both tools
//! (`clipboard resource kind 'system_clipboard' is outside the capability
//! manifest`), which is why the broker runs them itself.
//! What: saves the user's clipboard text with `pbpaste`, then through
//! [`clipboard::run`] on [`SystemPasteboard`]: writes a non-ASCII sentinel
//! carrying a password and reads it back with the password replaced by
//! `[REDACTED]`; writes one byte over the cap with `pbcopy` and checks the
//! read is refused `clipboard_too_large`. It restores the saved text with
//! `pbcopy` before any assertion. The saved text is never printed.
//! Run: `cargo test -p trusty-computer --test live_clipboard -- --ignored --nocapture`.
//! Test: this file.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};
use trusty_computer::clipboard::{self, MAX_BYTES, READ_TOOL, SystemPasteboard, WRITE_TOOL};

fn pbcopy(text: &[u8]) {
    let mut child = Command::new("/usr/bin/pbcopy")
        .env_clear()
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .spawn()
        .expect("pbcopy");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(text)
        .expect("write pbcopy");
    assert!(child.wait().expect("pbcopy exit").success());
}

fn run(tool: &str, args: Value) -> (Value, Map<String, Value>) {
    let args = args.as_object().expect("object").clone();
    clipboard::run(&SystemPasteboard, tool, &args)
}

#[test]
#[ignore = "live: replaces the system clipboard (restored after); needs a logged-in GUI session"]
fn live_clipboard_write_then_redacted_read() {
    let saved = Command::new("/usr/bin/pbpaste")
        .env_clear()
        .env("LANG", "en_US.UTF-8")
        .output()
        .expect("pbpaste");

    let sentinel = format!("tcomp-{} héllo password=hunter2", std::process::id());
    let (wrote, wrote_audit) = run(WRITE_TOOL, json!({"text": sentinel}));
    let (read, read_audit) = run(READ_TOOL, json!({}));
    pbcopy(&vec![b'a'; MAX_BYTES + 1]);
    let (big, big_audit) = run(READ_TOOL, json!({}));

    pbcopy(&saved.stdout);

    println!("write: {wrote} {wrote_audit:?}");
    println!("read audit: {read_audit:?}; oversize: {big} {big_audit:?}");
    assert_eq!(wrote["ok"], true, "{wrote}");
    let want = format!("tcomp-{} héllo password=[REDACTED]", std::process::id());
    assert_eq!(read["result"]["structuredContent"]["text"], want, "{read}");
    assert_eq!(read_audit["redacted"], 1);
    assert_eq!(big["code"], "clipboard_too_large", "{big}");
    assert_eq!(big_audit["clipboard_bytes"], MAX_BYTES + 1);
}
