//! Tests for `classify_secret` and `mask_content` (#8624).
//!
//! Why: a marker that leaks any part of a secret, or a masker that panics or
//! returns raw text on a bad span list, defeats the review export it feeds.
//! What: wired in from `mask.rs` as its `tests` module.
//! Test: this is the test file.

use super::super::spans::{SecretRule, SecretSpan, find_secret_spans};
use super::{MaskError, SecretShape, classify_secret, mask_content};

/// A provider-style token, assembled at runtime so no scanner reads the
/// source as a credential. 40 chars: `ghp_` plus 36 mixed alphanumerics.
fn provider_token() -> String {
    format!("{}{}", "ghp_", "Zq7RtY2uWx9Kp4Ls8Nm3Bv6Hc1Df5Gj0AeXw") // pragma: allowlist secret
}

/// One token per token rule, each assembled at runtime.
fn rule_tokens() -> Vec<String> {
    vec![
        provider_token(),
        format!("{}{}", "AKIA", "QX7RT2UW9KP4LS8N"), // pragma: allowlist secret
        format!("{}{}", "aGVsbG8+d29ybGQ/", "Zm9vYmFyQmF6UXV4MTIzNDU2"), // pragma: allowlist secret
        format!("{}{}", "aB3dE5fG7hJ9kL1m", "N2pQ4rS6tU8vW0xYz1"), // pragma: allowlist secret
    ]
}

fn span(range: std::ops::Range<usize>, rule: SecretRule) -> SecretSpan {
    SecretSpan { range, rule }
}

/// Every window of 4 chars in `s`.
fn windows4(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(4).map(|w| w.iter().collect()).collect()
}

/// Why (#8624): criterion 1. A marker that previews even a 4-char prefix of
/// the token fails here. The masked output must also re-screen clean, so the
/// export can be checked for secrets again.
/// Test: itself.
#[test]
fn marker_and_output_carry_no_four_char_window_of_the_token() {
    for tok in rule_tokens() {
        let content = format!("deploy key: {tok} (rotated)");
        let spans = find_secret_spans(&content);
        assert_eq!(spans.len(), 1, "fixture must be one span: {}", spans.len());
        let out = mask_content(&content, &spans).expect("valid spans");
        for w in windows4(&tok) {
            assert!(!out.contains(&w), "output leaks {w:?}: {out}");
        }
        assert!(out.starts_with("deploy key: ⟦S0:"), "{out}");
        assert!(out.ends_with("⟧ (rotated)"), "{out}");
        assert!(find_secret_spans(&out).is_empty(), "re-screen: {out}");
    }
    let tok = provider_token();
    let out = mask_content(&tok, &find_secret_spans(&tok)).expect("valid spans");
    assert!(out.starts_with("⟦S0:provider_prefix:40:ulds:"), "{out}");
}

/// Why (#8624): criterion 2. Bytes outside the spans, including multibyte
/// UTF-8 touching a span on either side, are copied unchanged.
/// Test: itself.
#[test]
fn text_outside_spans_passes_through_byte_for_byte() {
    let tok = provider_token();
    let content = format!("é→{tok}日本\u{2003}🔑 tail");
    let spans = find_secret_spans(&content);
    assert_eq!(spans.len(), 1);
    let r = spans[0].range.clone();
    assert_eq!(
        &content[r.clone()],
        tok,
        "span sits flush against é→ and 日"
    );
    let out = mask_content(&content, &spans).expect("valid spans");
    let marker_end = out.find('⟧').expect("marker") + '⟧'.len_utf8();
    assert_eq!(out.as_bytes()[..r.start], content.as_bytes()[..r.start]);
    assert_eq!(out.as_bytes()[marker_end..], content.as_bytes()[r.end..]);

    // Hand-built: two abutting spans between multibyte chars, and no spans.
    let text = "日aaaa€bb🔑";
    let a = "日".len();
    let b = a + 4;
    let c = b + "€".len();
    let spans = [
        span(a..b, SecretRule::Base64Blob),
        span(b..c, SecretRule::Jwt),
    ];
    let out = mask_content(text, &spans).expect("valid spans");
    assert_eq!(
        out,
        "日⟦S0:base64_blob:4:l:0.0⟧⟦S1:jwt:1:s:0.0⟧bb🔑".to_string()
    );
    assert_eq!(mask_content(text, &[]).expect("no spans"), text);
}

/// Why (#8624): the index in `S{i}` is 0-based, matching `spans[i]`.
/// Test: itself.
#[test]
fn markers_are_numbered_from_zero() {
    let text = "x=aabb y=Ab3! z";
    let spans = [
        span(2..6, SecretRule::Base64Blob),
        span(9..13, SecretRule::MixedCaseAlnum),
    ];
    assert_eq!(
        mask_content(text, &spans).expect("valid spans"),
        "x=⟦S0:base64_blob:4:l:1.0⟧ y=⟦S1:mixed_case_alnum:4:ulds:2.0⟧ z"
    );
}

/// Why (#8624): criterion 3, the fail-open check. Every malformed list is an
/// error that names offsets only; none panics and none returns text. Shown
/// red against a variant that slices without checking.
/// Test: itself.
#[test]
fn malformed_span_lists_are_errors_not_panics() {
    let tok = provider_token();
    let content = format!("hé {tok} end");
    let t = content.find(&tok).expect("fixture");
    let ok = span(t..t + tok.len(), SecretRule::ProviderPrefix);
    let len = content.len();
    let rule = SecretRule::ProviderPrefix;
    let cases: Vec<(Vec<SecretSpan>, MaskError)> = vec![
        (
            vec![span(t..len + 1, rule)],
            MaskError::OutOfBounds {
                index: 0,
                range: t..len + 1,
                len,
            },
        ),
        // `é` is bytes 1..3, so 2 is inside it: start, then end.
        (
            vec![span(2..t, rule)],
            MaskError::NotCharBoundary {
                index: 0,
                range: 2..t,
            },
        ),
        (
            vec![span(0..2, rule)],
            MaskError::NotCharBoundary {
                index: 0,
                range: 0..2,
            },
        ),
        (
            vec![span(t..t, rule)],
            MaskError::Empty {
                index: 0,
                range: t..t,
            },
        ),
        (
            vec![span(t + 4..t, rule)],
            MaskError::Empty {
                index: 0,
                range: t + 4..t,
            },
        ),
        (
            vec![ok.clone(), span(t + 3..len, rule)],
            MaskError::Unordered {
                index: 1,
                range: t + 3..len,
                prev_end: t + tok.len(),
            },
        ),
        (
            vec![ok.clone(), span(0..1, rule)],
            MaskError::Unordered {
                index: 1,
                range: 0..1,
                prev_end: t + tok.len(),
            },
        ),
    ];
    for (spans, want) in cases {
        let got = mask_content(&content, &spans);
        assert_eq!(got, Err(want), "{spans:?}");
        let msg = got.err().map(|e| e.to_string()).unwrap_or_default();
        for w in windows4(&tok) {
            assert!(!msg.contains(&w), "error text leaks {w:?}: {msg}");
        }
    }
}

/// Why (#8624): `cls` known answers for all 16 class sets, each written out,
/// with the input chars in a scrambled order.
/// Test: itself.
#[test]
fn cls_names_each_class_combination() {
    for (input, want) in [
        ("", ""),
        ("QZ", "u"),
        ("qz", "l"),
        ("09", "d"),
        ("-é", "s"),
        ("qQ", "ul"),
        ("9Q", "ud"),
        ("9q", "ld"),
        ("9qQ", "uld"),
        (" Q", "us"),
        ("q.", "ls"),
        ("/qQ", "uls"),
        ("9=", "ds"),
        ("9_Q", "uds"),
        ("+q9", "lds"),
        ("9!qQ", "ulds"),
    ] {
        assert_eq!(classify_secret(input).cls, want, "{input:?}");
    }
}

/// Why (#8624): `ent` known answers in tenths of a bit per char.
/// Test: itself.
#[test]
fn ent_known_answers() {
    for (input, want) in [
        ("", 0),
        ("aaaa", 0),
        ("abab", 10),
        ("aab", 9), // 0.918 bits/char
        ("abcdefgh", 30),
        ("0123456789abcdef", 40),
    ] {
        assert_eq!(classify_secret(input).ent_tenths, want, "{input:?}");
    }
}

/// Why (#8624): `len` counts chars, so a multibyte span is not overstated.
/// Test: itself.
#[test]
fn len_counts_chars_not_bytes() {
    assert_eq!(
        classify_secret("é日🔑"),
        SecretShape {
            len: 3,
            cls: "s",
            ent_tenths: 16,
        }
    );
}
