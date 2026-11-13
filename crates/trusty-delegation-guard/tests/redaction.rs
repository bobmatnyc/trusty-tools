//! Credential redaction and the excerpt cap. All secrets are synthetic.
#![cfg(feature = "types")]

use trusty_delegation_guard::redact::{EXCERPT_MAX_BYTES, excerpt, redact};

#[test]
fn each_detector_redacts_its_shape() {
    let cases = [
        (
            "private_key",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBsynthetic\n-----END RSA PRIVATE KEY-----",
        ),
        ("aws_access_key", "AKIAABCDEFGHIJKLMNOP"),
        ("github_token", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        ("github_pat", "github_pat_11ABCDEFG0123456789_abcdefghij"),
        ("anthropic_key", "sk-ant-api03-abcdefghijklmnopqrstuv"),
        ("sk_key", "sk-or-v1-abcdefghijklmnopqrstuvwxyz"),
        ("slack_token", "xoxb-1234567890-abcdefghij"),
        (
            "jwt",
            "eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4",
        ),
        ("bearer", "Bearer abcdefghijklmnopqrstuvwx"),
        ("assignment", "password: hunter2hunter2"),
    ];
    for (kind, secret) in cases {
        let text = format!("before {secret} after");
        let out = redact(&text);
        assert!(out.redacted, "{kind}: no hit");
        assert!(
            !out.text.contains(secret),
            "{kind}: secret survived: {}",
            out.text
        );
        assert!(out.text.contains("[REDACTED:"), "{kind}: no marker");
        assert!(out.text.starts_with("before "), "{kind}: prefix lost");
    }
}

#[test]
fn json_quoted_and_env_style_assignments_redact() {
    // (text, secret value that must not survive)
    let cases = [
        (r#"{"password": "hunter2hunter2"}"#, "hunter2hunter2"),
        (r#"{"api_key": "abcd1234efgh5678"}"#, "abcd1234efgh5678"),
        ("API_TOKEN=tok0synthetic0value", "tok0synthetic0value"),
        ("access_token=acc0synthetic0value", "acc0synthetic0value"),
        ("client_secret=cls0synthetic0value", "cls0synthetic0value"),
        (
            "AWS_SECRET_ACCESS_KEY=wJalrSYNTHETICbPxRfiCYEXAMPLE",
            "wJalrSYNTHETICbPxRfiCYEXAMPLE",
        ),
        ("github_token=gtk0synthetic0value", "gtk0synthetic0value"),
    ];
    for (text, value) in cases {
        let out = redact(&format!("before {text} after"));
        assert!(out.redacted, "{text}: no hit");
        assert!(
            !out.text.contains(value),
            "{text}: value survived: {}",
            out.text
        );
    }
}

#[test]
fn prose_mentioning_a_keyword_is_untouched() {
    for text in [
        "the token budget is 8000",
        "rotate the client secret next week",
        "password policy requires twelve characters",
    ] {
        let out = redact(text);
        assert!(!out.redacted, "false positive on {text:?}: {}", out.text);
        assert_eq!(out.text, text);
    }
}

#[test]
fn clean_text_is_untouched() {
    let text = "Index the parser module; start at most 2 nested workers in docs/.";
    let out = redact(text);
    assert!(!out.redacted);
    assert_eq!(out.text, text);
}

#[test]
fn excerpt_is_capped_after_redaction() {
    // The secret straddles the cap: capping first would leave a fragment.
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let prompt = format!(
        "{} {secret}{}",
        "a".repeat(EXCERPT_MAX_BYTES - 10),
        "b".repeat(50)
    );
    let ex = excerpt(&prompt);
    assert!(ex.redacted);
    assert!(ex.truncated);
    assert!(ex.text.len() <= EXCERPT_MAX_BYTES);
    assert!(
        !ex.text.contains("ghp_"),
        "credential fragment survived the cap"
    );

    let short = excerpt("short prompt");
    assert!(!short.truncated && !short.redacted);
    assert_eq!(short.text, "short prompt");
}

#[test]
fn excerpt_cap_respects_char_boundary() {
    // 3-byte chars: 8192 is not a multiple of 3, so the cut must step back.
    let prompt = "\u{20ac}".repeat(EXCERPT_MAX_BYTES);
    let ex = excerpt(&prompt);
    assert!(ex.truncated);
    assert!(ex.text.len() <= EXCERPT_MAX_BYTES);
    assert_eq!(ex.text.len() % 3, 0);
}
