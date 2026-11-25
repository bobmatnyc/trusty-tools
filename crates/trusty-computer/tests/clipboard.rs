//! Clipboard read and write (issue #11696 Phase C): the argument checks, the
//! two separate grant flags, the size cap, the redaction of a read, and the
//! broker pipeline on a fake pasteboard, including the refusal of every call
//! without its flag before the clipboard is touched.

mod common;

use std::sync::atomic::Ordering;

use common::rig::{Rig, WINDOW, consent, grant_req};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use trusty_computer::clipboard::{
    FLAGS, MAX_BYTES, Pasteboard, READ_FLAG, READ_TOOL, SystemPasteboard, WRITE_FLAG, WRITE_TOOL,
    grant_flag,
};
use trusty_computer::clipboard_redact::{MARKER, redact_text};
use trusty_computer::grants::DEFAULT_TOOLS;
use trusty_computer::policy::{Scope, Verdict, check_call, rule};

/// The daemon's own clipboard tools; DENY, and never called by the broker.
const DRIVER_TOOLS: [&str; 2] = ["clipboard_read", "clipboard_write"];

fn code(tool: &str, args: Value) -> Option<&'static str> {
    check_call(tool, &args, &Scope::default())
        .err()
        .map(|e| e.code)
}

#[test]
fn redaction_strips_passwords_and_keys() {
    for (input, want) in [
        ("password=hunter2", "password=[REDACTED]"),
        ("db_password: s3cret!", "db_password: [REDACTED]"),
        ("PWD:abc123 next", "PWD:[REDACTED] next"),
        ("user pass = letmein", "user pass = [REDACTED]"),
        (
            r#"{"api_key": "abc123def"}"#,
            r#"{"api_key": "[REDACTED]"}"#,
        ),
        ("x?token=abcd&user=bob", "x?token=[REDACTED]&user=bob"),
        (
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG",
            "AWS_SECRET_ACCESS_KEY=[REDACTED]",
        ),
        (
            "Authorization: Bearer eyJhbGciOi.payload.sig",
            "Authorization: [REDACTED] [REDACTED]",
        ),
        ("curl -H Bearer abc.def.ghi", "curl -H Bearer [REDACTED]"),
        (
            "Authorization: Basic dXNlcjpwYXNz",
            "Authorization: [REDACTED] [REDACTED]",
        ),
        ("id AKIAIOSFODNN7EXAMPLE end", "id [REDACTED] end"),
        // URL credentials: scheme and host stay, `user:pass` goes.
        (
            "DATABASE_URL=postgres://app:s3cr3t@db.example.com:5432/prod",
            "DATABASE_URL=postgres://[REDACTED]@db.example.com:5432/prod",
        ),
        (
            "clone https://bob:ghtoken@git.example.org/o/r.git now",
            "clone https://[REDACTED]@git.example.org/o/r.git now",
        ),
        (
            "redis://:hunter2@cache:6379/0",
            "redis://[REDACTED]@cache:6379/0",
        ),
        (
            "mongodb+srv://u:p%40ss@cluster0.example.net/db",
            "mongodb+srv://[REDACTED]@cluster0.example.net/db",
        ),
        ("key sk-ant-api03-abcdefghijklmnop0123", "key [REDACTED]"),
        ("ghp_0123456789abcdefABCDEF here", "[REDACTED] here"),
        ("xoxb-1234567890-abcdefghijkl", "[REDACTED]"),
        ("AIzaSyA1234567890abcdefghij", "[REDACTED]"),
        (
            "jwt eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4",
            "jwt [REDACTED]",
        ),
        (
            "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nb",
            "a\n[REDACTED]\nb",
        ),
        ("-----BEGIN PRIVATE KEY-----\nMIIE cut", "[REDACTED]"),
    ] {
        let red = redact_text(input);
        assert_eq!(red.text, want, "{input}");
        assert_eq!(red.removed, red.text.matches(MARKER).count(), "{input}");
    }
}

#[test]
fn redaction_keeps_plain_text() {
    for input in [
        "",
        "Meeting notes: ship Phase C on Monday.",
        "the password field is empty",
        "the token is expired; Basic setup done",
        "compass=north bypass:yes",
        "https://git@git.example.org/o/r.git https://example.com/a@b ssh://host:22/x",
        "sk-short ghp_tiny AKIA123",
        "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
        "héllo wörld 日本語",
    ] {
        let red = redact_text(input);
        assert_eq!((red.text.as_str(), red.removed), (input, 0), "{input}");
    }
}

#[test]
fn clipboard_args_are_checked() {
    assert_eq!(code(READ_TOOL, json!({})), None);
    assert_eq!(code(WRITE_TOOL, json!({"text": "hi"})), None);
    for (tool, args, want) in [
        (READ_TOOL, json!({"include_text": true}), "clipboard_arg"),
        (WRITE_TOOL, json!({}), "clipboard_text_required"),
        (WRITE_TOOL, json!({"text": 5}), "clipboard_text_required"),
        (
            WRITE_TOOL,
            json!({"text": "a", "image_path": "/etc/hosts"}),
            "clipboard_arg",
        ),
        (
            WRITE_TOOL,
            json!({"file_path": "/Users/x/.ssh/id_rsa"}),
            "clipboard_arg",
        ),
    ] {
        assert_eq!(code(tool, args.clone()), Some(want), "{tool} {args}");
    }
    for tool in [READ_TOOL, WRITE_TOOL] {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Filter));
    }
    // The driver's own clipboard tools stay DENY.
    for tool in DRIVER_TOOLS {
        assert_eq!(rule(tool).map(|r| r.verdict), Some(Verdict::Deny), "{tool}");
    }
}

#[test]
fn oversize_write_is_refused() {
    let at_cap = "a".repeat(MAX_BYTES);
    assert_eq!(code(WRITE_TOOL, json!({"text": at_cap})), None);
    // A multi-byte text is capped in bytes, not characters.
    let over = "é".repeat(MAX_BYTES / 2 + 1);
    assert_eq!(
        code(WRITE_TOOL, json!({"text": over})),
        Some("clipboard_too_large")
    );
}

/// The clipboard accesses of `rig`, and the daemon's clipboard calls (always none).
fn uses(rig: &Rig) -> u64 {
    let daemon = rig.daemon(|d| d.calls());
    assert!(
        !daemon
            .iter()
            .any(|(n, _)| DRIVER_TOOLS.contains(&n.as_str())),
        "{daemon:?}"
    );
    rig.pasteboard.uses.load(Ordering::SeqCst)
}

fn audit_of(rig: &Rig, tool: &str) -> Vec<Value> {
    rig.audit()
        .into_iter()
        .filter(|r| r["tool"] == tool)
        .collect()
}

fn done_line(rig: &Rig, tool: &str) -> Value {
    audit_of(rig, tool)
        .into_iter()
        .rfind(|r| r["phase"] == "done")
        .expect("done line")
}

/// The refusal tests: no flag, the other flag, the driver name, or a
/// grant naming a broker tool itself; none touches the clipboard.
#[test]
fn clipboard_tools_need_their_own_flag() {
    assert_eq!(grant_flag(READ_TOOL), Some(READ_FLAG));
    assert_eq!(grant_flag(WRITE_TOOL), Some(WRITE_FLAG));
    for flag in FLAGS {
        assert!(!DEFAULT_TOOLS.contains(&flag), "{flag}");
    }
    // No flag: both refused.
    let mut rig = Rig::new();
    rig.pasteboard.set(b"password=hunter2");
    let reply = rig.call(READ_TOOL, "r0", json!({}));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    let reply = rig.call(WRITE_TOOL, "w0", json!({"text": "x"}));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    // The write flag does not admit a read, nor the read flag a write.
    let mut w = Rig::with(&[WRITE_FLAG], WINDOW);
    let reply = w.call(READ_TOOL, "r1", json!({}));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    let mut r = Rig::with(&[READ_FLAG], WINDOW);
    let reply = r.call(WRITE_TOOL, "w1", json!({"text": "x"}));
    assert_eq!(reply["code"], "tool_not_granted", "{reply}");
    // The driver tool names are DENY whatever the grant holds.
    for (i, tool) in DRIVER_TOOLS.into_iter().enumerate() {
        let reply = r.call(tool, &format!("d{i}"), json!({"include_text": true}));
        assert_eq!(reply["code"], "denied_tool", "{reply}");
    }
    // Neither broker tool is grantable by its own name.
    for tool in [READ_TOOL, WRITE_TOOL] {
        let reply = consent(
            &rig.server.operator,
            &rig.dialog,
            &grant_req(&[tool], WINDOW),
        );
        assert_eq!(reply["code"], "bad_tool", "{tool}: {reply}");
    }
    for rig in [&rig, &w, &r] {
        assert_eq!(uses(rig), 0);
    }
    assert_eq!(rig.pasteboard.get(), b"password=hunter2");
}

#[test]
fn clipboard_tools_drive_text_only() {
    let mut rig = Rig::with(&[WRITE_FLAG, READ_FLAG], WINDOW);
    let reply = rig.call(WRITE_TOOL, "w1", json!({"text": "héllo"}));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        reply["result"],
        json!({"structuredContent": {"written_bytes": 6}})
    );
    assert_eq!(rig.pasteboard.get(), "héllo".as_bytes());
    let reply = rig.call(READ_TOOL, "r1", json!({}));
    assert_eq!(
        reply["result"]["structuredContent"]["text"], "héllo",
        "{reply}"
    );
    assert_eq!(uses(&rig), 2);
}

#[test]
fn read_returns_redacted_text_only() {
    let mut rig = Rig::with(&[READ_FLAG], WINDOW);
    let secret = "login bob password=hunter2 key AKIAIOSFODNN7EXAMPLE";
    rig.pasteboard.set(secret.as_bytes());
    let reply = rig.call(READ_TOOL, "r1", json!({}));
    let want = "login bob password=[REDACTED] key [REDACTED]";
    assert_eq!(
        reply["result"],
        json!({"content": [{"type": "text", "text": want}],
            "structuredContent": {"text": want, "redacted": 2}}),
        "{reply}"
    );
    // The raw text reaches neither the agent nor the audit.
    let all = format!("{reply}{:?}", rig.audit());
    for leak in ["hunter2", "AKIAIOSFODNN7EXAMPLE"] {
        assert!(!all.contains(leak), "{leak} leaked: {all}");
    }
    let done = done_line(&rig, READ_TOOL);
    assert_eq!(done["clipboard_bytes"], secret.len());
    assert_eq!(done["redacted"], 2);
    assert_eq!(done["verdict"], "FILTER");
}

#[test]
fn oversize_read_is_refused() {
    let mut rig = Rig::with(&[READ_FLAG], WINDOW);
    let at_cap = "a".repeat(MAX_BYTES);
    rig.pasteboard.set(at_cap.as_bytes());
    let reply = rig.call(READ_TOOL, "r0", json!({}));
    assert_eq!(reply["ok"], true, "a text at the cap is returned");
    let big = format!("password=hunter2 {}", "a".repeat(MAX_BYTES));
    rig.pasteboard.set(big.as_bytes());
    let reply = rig.call(READ_TOOL, "r1", json!({}));
    assert_eq!(reply["code"], "clipboard_too_large", "{reply}");
    assert!(reply["result"].is_null(), "{reply}");
    assert!(!reply.to_string().contains("hunter2"));
    // The broker asked for one byte over the cap, never the whole text.
    assert_eq!(done_line(&rig, READ_TOOL)["clipboard_bytes"], MAX_BYTES + 1);
    // Not UTF-8: refused, not lossily decoded.
    rig.pasteboard.set(&[0xff, 0xfe, b'a']);
    let reply = rig.call(READ_TOOL, "r2", json!({}));
    assert_eq!(reply["code"], "clipboard_not_text", "{reply}");
}

#[test]
fn write_is_audited_without_its_text() {
    let mut rig = Rig::with(&[WRITE_FLAG], WINDOW);
    let reply = rig.call(WRITE_TOOL, "w1", json!({"text": "topsecret"}));
    assert_eq!(reply["ok"], true, "{reply}");
    let lines = audit_of(&rig, WRITE_TOOL);
    let phases: Vec<&Value> = lines.iter().map(|r| &r["phase"]).collect();
    assert_eq!(phases, [&json!("forward"), &json!("done")], "{lines:?}");
    assert_eq!(lines[1]["clipboard_bytes"], 9);
    assert!(!format!("{lines:?}").contains("topsecret"));
    // An oversize write is refused before the clipboard is touched, and audited.
    let reply = rig.call(WRITE_TOOL, "w2", json!({"text": "a".repeat(MAX_BYTES + 1)}));
    assert_eq!(reply["code"], "clipboard_too_large", "{reply}");
    assert_eq!(uses(&rig), 1);
    let refused = audit_of(&rig, WRITE_TOOL);
    assert_eq!(
        refused.last().expect("line")["refused"],
        "clipboard_too_large"
    );
    assert_eq!(rig.pasteboard.get(), b"topsecret");
}

#[test]
fn pasteboard_failure_returns_no_text() {
    let mut rig = Rig::with(&[READ_FLAG, WRITE_FLAG], WINDOW);
    rig.pasteboard.set(b"password=hunter2");
    rig.pasteboard.broken.store(true, Ordering::SeqCst);
    let reply = rig.call(READ_TOOL, "r1", json!({}));
    assert_eq!(reply["code"], "clipboard_error", "{reply}");
    assert!(!reply.to_string().contains("hunter2"));
    let reply = rig.call(WRITE_TOOL, "w1", json!({"text": "x"}));
    assert_eq!(reply["code"], "clipboard_error", "{reply}");
}

/// A pasteboard command that never finishes is killed at its deadline, for a
/// read and for a write; `/bin/sleep` stands in for a hung `pbpaste`/`pbcopy`.
#[test]
fn pasteboard_command_is_killed_at_its_deadline() {
    let hung = vec!["/bin/sleep".to_owned(), "30".to_owned()];
    let pb = SystemPasteboard {
        paste: hung.clone(),
        copy: hung,
        timeout: Duration::from_millis(300),
    };
    let start = Instant::now();
    let read = pb.read(MAX_BYTES + 1);
    assert!(
        read.as_ref().is_err_and(|e| e.contains("timed out")),
        "{read:?}"
    );
    let write = pb.write("x");
    assert!(
        write.as_ref().is_err_and(|e| e.contains("timed out")),
        "{write:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    // A command that ends in time is not cut off.
    let quick = SystemPasteboard {
        paste: vec!["/bin/echo".to_owned(), "-n".to_owned(), "hi".to_owned()],
        copy: vec!["/bin/cat".to_owned()],
        timeout: Duration::from_secs(5),
    };
    assert_eq!(quick.read(MAX_BYTES + 1).as_deref(), Ok(&b"hi"[..]));
    assert_eq!(quick.write("x"), Ok(()));
}
