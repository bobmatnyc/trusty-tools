//! Tests for the span API (`find_secret_spans`) and its rule set (#8623).
//!
//! Why: the span API must never disagree with `check_secret` — a span list
//! that misses a hit the verdict refuses would let a masking or review tool
//! admit a credential. These tests pin that parity over every string literal
//! and line of the existing filter suite plus generated input, and pin the
//! two new rules and the version fingerprint.
//! What: wired in from `spans.rs` as its `tests` module.
//! Test: this is the test file.

use super::super::private_key::{PEM_BEGIN, PEM_END, PEM_PRIVATE_KEY, private_key_blocks};
use super::super::*;
use super::{SecretRule, SecretSpan, classify_token, find_secret_spans};
use crate::engine::filter::{GIT_SHA_MAX_LEN, GIT_SHA_MIN_LEN};

const GHP: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"; // pragma: allowlist secret
const AWS: &str = "AKIAIOSFODNN7EXAMPLE"; // pragma: allowlist secret
const B64: &str = "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"; // pragma: allowlist secret
const MIXED: &str = "aB3dE5fG7hJ9kL1mN2pQ4rS6tU8vW0xYz1"; // pragma: allowlist secret
const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"; // pragma: allowlist secret
const PEM_BODY: &str = "MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHB7MhgHcTz6sE2I2yPB"; // pragma: allowlist secret

/// A PEM private key: armor, one body line that trips the base64 rule, armor.
fn pem() -> String {
    format!("-----BEGIN RSA PRIVATE KEY-----\n{PEM_BODY}\n-----END RSA PRIVATE KEY-----")
}

/// The first 60 chars of `s`, so a failure names its input without printing
/// a whole file.
fn head(s: &str) -> String {
    s.chars().take(60).collect()
}

/// Span invariants: non-empty, disjoint, ascending, on char boundaries, and a
/// token span's text re-classifies to its own rule.
fn assert_span_invariants(s: &str, spans: &[SecretSpan]) {
    let mut prev_end = 0;
    for span in spans {
        let r = span.range.clone();
        assert!(r.start < r.end, "empty span {r:?} in {:?}", head(s));
        assert!(
            prev_end <= r.start,
            "spans overlap or descend in {:?}",
            head(s)
        );
        assert!(
            s.is_char_boundary(r.start) && s.is_char_boundary(r.end),
            "span {r:?} is not on char boundaries in {:?}",
            head(s)
        );
        if span.rule != SecretRule::PrivateKeyBlock {
            assert_eq!(
                classify_token(&s[r.clone()]),
                Some(span.rule),
                "{:?}",
                head(s)
            );
        }
        prev_end = r.end;
    }
}

/// The parity contract: no span exactly when `check_secret` accepts, and the
/// first span is the hit `find_secret_token` previews. Returns the spans.
fn assert_parity(s: &str) -> Vec<SecretSpan> {
    let spans = find_secret_spans(s);
    assert_eq!(
        spans.is_empty(),
        check_secret(s).is_ok(),
        "span/verdict parity broken for {:?} (spans {spans:?})",
        head(s)
    );
    let first = spans.first().map(|sp| redact_token(&s[sp.range.clone()]));
    assert_eq!(
        first,
        find_secret_token(s),
        "first span ≠ preview for {:?}",
        head(s)
    );
    assert_span_invariants(s, &spans);
    spans
}

/// Every string literal in Rust source `src`, unescaped. Raw strings,
/// escapes and line continuations are handled; `//` comments and char
/// literals are skipped. Accuracy only widens the corpus: any string is a
/// valid parity input.
fn string_literals(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = src[i..].find('\n').map_or(b.len(), |n| i + n);
            }
            // An escaped char literal (`'\''`, `'\n'`) closes at or after i + 3.
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i = src
                    .get(i + 3..)
                    .and_then(|r| r.find('\''))
                    .map_or(b.len(), |n| i + 3 + n + 1);
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 3,
            b'r' if matches!(b.get(i + 1), Some(b'"' | b'#'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let hashes = src[i + 1..].bytes().take_while(|&c| c == b'#').count();
                let open = i + 1 + hashes;
                if b.get(open) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let close = format!("\"{}", "#".repeat(hashes));
                let Some(n) = src[open + 1..].find(&close) else {
                    break;
                };
                out.push(src[open + 1..open + 1 + n].to_string());
                i = open + 1 + n + close.len();
            }
            b'"' => {
                let mut lit = String::new();
                let mut chars = src[i + 1..].char_indices();
                let mut end = b.len();
                while let Some((at, c)) = chars.next() {
                    match c {
                        '"' => {
                            end = i + 1 + at + 1;
                            break;
                        }
                        '\\' => match chars.next().map(|(_, e)| e) {
                            Some('n') => lit.push('\n'),
                            Some('t') => lit.push('\t'),
                            Some('r') => lit.push('\r'),
                            Some('0') => lit.push('\0'),
                            Some('\n') => {
                                // Line continuation: skip the next line's indent.
                                let rest = chars.as_str();
                                let skip = rest.len() - rest.trim_start().len();
                                for _ in 0..rest[..skip].chars().count() {
                                    chars.next();
                                }
                            }
                            Some(other) => lit.push(other),
                            None => {}
                        },
                        _ => lit.push(c),
                    }
                }
                out.push(lit);
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Why (#8623): parity over every input shape the existing filter suite
/// asserts on. The suite's literals, adjacent literal pairs (its
/// `format!("{}{}", a, b)` fixtures), each literal inside prose, every source
/// line and the whole file are screened both ways.
/// Test: itself; shown red when `classify_token` drops one rule.
#[test]
fn spans_match_check_secret_over_the_filter_corpus() {
    let src = include_str!("../../filter_tests.rs");
    let literals = string_literals(src);
    assert!(
        literals.len() > 1000,
        "corpus extraction broke: {}",
        literals.len()
    );
    let mut inputs: Vec<String> = Vec::new();
    for pair in literals.windows(2) {
        inputs.push(format!("{}{}", pair[0], pair[1]));
    }
    for lit in &literals {
        inputs.push(lit.clone());
        inputs.push(format!("note {lit} end"));
    }
    inputs.extend(src.lines().map(str::to_string));
    inputs.push(src.to_string());
    inputs.push(pem());
    let mut seen = std::collections::BTreeSet::new();
    let (mut refused, mut accepted) = (0, 0);
    for input in &inputs {
        let spans = assert_parity(input);
        if spans.is_empty() {
            accepted += 1;
        } else {
            refused += 1;
        }
        seen.extend(spans.iter().map(|sp| sp.rule));
    }
    assert!(
        refused > 200 && accepted > 200,
        "{refused} refused, {accepted} accepted"
    );
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        SecretRule::ALL.to_vec()
    );
}

/// Why (#8623): parity, invariants and no panic on input built to stress the
/// tokenizer and the block scan — multibyte whitespace, emoji, partial PEM
/// armor, backticks and provider prefixes in random order.
/// Test: itself.
#[test]
fn spans_match_check_secret_over_generated_input() {
    let fragments = [
        "sk-",
        "ghp_",
        "AKIA",
        "eyJ",
        PEM_BEGIN,
        PEM_END,
        PEM_PRIVATE_KEY,
        "-----",
        " ",
        "\n",
        "`",
        "=",
        "/",
        "+",
        ".",
        ":",
        "é",
        "日本",
        "🔑",
        "\u{2003}",
        "\u{a0}",
        "Ab3",
        "xY9z",
        "0123456789",
        "abcdefghij",
        "QWERTYUIOP",
        "docs",
        "spreadsheet",
        GHP,
        AWS,
        B64,
        MIXED,
        JWT,
        "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms",
    ];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (mut refused, mut accepted) = (0, 0);
    for _ in 0..4000 {
        let len = 1 + next() % 24;
        let s: String = (0..len)
            .map(|_| fragments[(next() % fragments.len() as u64) as usize])
            .collect();
        if assert_parity(&s).is_empty() {
            accepted += 1;
        } else {
            refused += 1;
        }
    }
    assert!(
        refused > 500 && accepted > 100,
        "{refused} refused, {accepted} accepted"
    );
}

/// Why (#8623): the rule comes from the branch that fired. A provider key
/// written after `=` is a provider key, though it carries a base64 symbol.
/// Test: itself.
#[test]
fn each_rule_is_labelled_by_the_branch_that_fired() {
    let key_eq = format!(
        "OPENAI_API_KEY={}",
        "sk-proj-abcdefghijklmnopqrstuvwxyz0123"
    ); // pragma: allowlist secret
    for (tok, rule) in [
        (GHP, SecretRule::ProviderPrefix),
        (key_eq.as_str(), SecretRule::ProviderPrefix),
        (AWS, SecretRule::AwsKeyId),
        (JWT, SecretRule::Jwt),
        (B64, SecretRule::Base64Blob),
        (MIXED, SecretRule::MixedCaseAlnum),
    ] {
        assert_eq!(classify_token(tok), Some(rule), "{tok}");
        let text = format!("note {tok} end");
        let at = text.find(tok).expect("fixture");
        assert_eq!(
            find_secret_spans(&text),
            vec![SecretSpan {
                range: at..at + tok.len(),
                rule
            }]
        );
    }
    assert_eq!(classify_token("ordinary"), None);
    let labels: std::collections::BTreeSet<&str> =
        SecretRule::ALL.iter().map(|r| r.label()).collect();
    assert_eq!(
        labels.len(),
        SecretRule::ALL.len(),
        "labels must be distinct"
    );
}

/// Why (#8623): spans are byte ranges a caller slices with, so they must be
/// ascending, disjoint and on char boundaries, including beside multibyte text
/// and between hits one delimiter apart.
/// Test: itself.
#[test]
fn spans_are_ascending_char_boundary_ranges() {
    let text = format!("日本語 {GHP} 🔑`{AWS}``{MIXED}`\u{2003}{B64}é");
    let spans = assert_parity(&text);
    let want: Vec<SecretSpan> = [
        (GHP, SecretRule::ProviderPrefix),
        (AWS, SecretRule::AwsKeyId),
        (MIXED, SecretRule::MixedCaseAlnum),
        (B64, SecretRule::Base64Blob),
    ]
    .into_iter()
    .map(|(tok, rule)| {
        let at = text.find(tok).expect("fixture");
        SecretSpan {
            range: at..at + tok.len(),
            rule,
        }
    })
    .collect();
    assert_eq!(spans, want);
    assert!(find_secret_spans("").is_empty());
    assert!(find_secret_spans("日本語のテキスト 🔑 é").is_empty());
}

/// Why (#8623): a key block is one hit. Its body line flags on its own, but
/// inside the block it is not reported again; hits around it still are.
/// Test: itself.
#[test]
fn private_key_block_is_one_span() {
    assert!(
        classify_token(PEM_BODY).is_some(),
        "the body line flags alone"
    );
    let key = pem();
    let text = format!("deploy {GHP} then\n{key}\nafter {AWS}");
    let spans = assert_parity(&text);
    let rules: Vec<SecretRule> = spans.iter().map(|s| s.rule).collect();
    assert_eq!(
        rules,
        [
            SecretRule::ProviderPrefix,
            SecretRule::PrivateKeyBlock,
            SecretRule::AwsKeyId
        ]
    );
    assert_eq!(&text[spans[1].range.clone()], key);

    // Two keys are two spans; a JSON-escaped key on one line is one span.
    let two = format!("{key}\n\n{key}");
    assert_eq!(private_key_blocks(&two).len(), 2);
    assert_eq!(assert_parity(&two).len(), 2);
    let json = format!(
        r#"{{"private_key": "-----BEGIN PRIVATE KEY-----\n{PEM_BODY}\n-----END PRIVATE KEY-----\n"}}"#
    );
    let spans = assert_parity(&json);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].rule, SecretRule::PrivateKeyBlock);

    // An edge inside a chunk widens to the chunk, so no token is cut.
    let flush = format!("x={key}y");
    assert_eq!(assert_parity(&flush)[0].range, 0..flush.len());
}

/// Why (#8623): a truncated paste is still a key. A BEGIN line with no END,
/// or closed only by another label's END, is flagged to the end of the
/// string, and `check_secret` refuses it though no token in it flags.
/// Test: itself.
#[test]
fn unterminated_private_key_is_flagged() {
    for text in [
        "-----BEGIN PRIVATE KEY-----".to_string(),
        "key: -----BEGIN OPENSSH PRIVATE KEY".to_string(),
        "-----BEGIN EC PRIVATE KEY-----\nabc\n-----END CERTIFICATE-----\ntail".to_string(),
        format!("-----BEGIN ENCRYPTED PRIVATE KEY-----\n{}", &PEM_BODY[..30]),
    ] {
        assert!(check_secret(&text).is_err(), "must refuse {text:?}");
        let spans = assert_parity(&text);
        assert_eq!(spans.len(), 1, "{text:?}");
        assert_eq!(spans[0].rule, SecretRule::PrivateKeyBlock);
        assert_eq!(spans[0].range.end, text.len(), "{text:?}");
    }
}

/// Why (#8623): only a private key's armor opens a block — not a public key,
/// a certificate, unarmored prose or lowercase armor.
/// Test: itself.
#[test]
fn private_key_near_misses_are_not_blocks() {
    for text in [
        "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----",
        "-----BEGIN CERTIFICATE-----",
        "paste the BEGIN PRIVATE KEY line here",
        "-----begin private key-----",
        "----BEGIN PRIVATE KEY-----",
        "-----BEGIN-PRIVATE-KEY-----",
    ] {
        assert!(private_key_blocks(text).is_empty(), "{text:?}");
        assert!(assert_parity(text).is_empty(), "{text:?}");
    }
}

/// Why (#8623): a JWT gets its own label; a token that only resembles one
/// keeps the label of the branch that flagged it, or is not flagged at all.
/// Test: itself.
#[test]
fn jwt_is_labelled_jwt() {
    let unsigned = "eyJhbGciOiJub25lIn0.eyJzdWIiOiIxMjM0NTY3ODkwIn0."; // pragma: allowlist secret
    for text in [
        JWT.to_string(),
        format!("Bearer {JWT}"),
        unsigned.to_string(),
    ] {
        let spans = assert_parity(&text);
        assert_eq!(spans.len(), 1, "{text}");
        assert_eq!(spans[0].rule, SecretRule::Jwt, "{text}");
    }
    let (header, rest) = JWT.split_once('.').expect("fixture");
    assert_eq!(classify_token(header), Some(SecretRule::MixedCaseAlnum));
    let foreign_header = format!("Zq7RtY2uWx9Kp4Ls8Nm3Bv.{rest}"); // pragma: allowlist secret
    let four_parts = format!("{JWT}.eyJ0");
    for near in [foreign_header, four_parts] {
        let rule = classify_token(&near);
        assert!(
            rule.is_some() && rule != Some(SecretRule::Jwt),
            "{near}: {rule:?}"
        );
    }
    assert_eq!(classify_token("eyJa.eyJb.c"), None, "too short to flag");
}

/// Why (#8589, #8623): the bare Google document-id skip applies to spans
/// exactly as to the verdict: named by context, no span; unnamed, a span.
/// Test: itself.
#[test]
fn bare_doc_id_named_by_context_is_not_a_span() {
    let id = "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms"; // pragma: allowlist secret
    assert!(assert_parity(&format!("spreadsheetId: {id}")).is_empty());
    let spans = assert_parity(&format!("note {id}"));
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].rule, SecretRule::MixedCaseAlnum);
}

/// FNV-1a, 64-bit: a stable hash with no dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Fingerprint of every named constant the rules read, and the rule labels.
fn rule_inputs_fingerprint() -> u64 {
    let labels: Vec<&str> = SecretRule::ALL.iter().map(|r| r.label()).collect();
    let parts = [
        format!("{SECRET_PREFIXES:?}"),
        format!("{AWS_KEY_ID_PREFIXES:?}"),
        format!("{PUBLIC_ID_PREFIXES:?} {PUBLIC_ID_TAIL_LEN}"),
        format!("{GITHUB_PROJECT_NODE_IDS:?}"),
        format!("{SECRET_MIN_LEN} {GIT_SHA_MIN_LEN} {GIT_SHA_MAX_LEN}"),
        format!("{IDENTIFIER_DELIMITERS:?} {MIN_PHRASE_WORD_LEN} {SYMBOL_PATH_SEPARATOR:?}"),
        format!("{SYMBOL_SEGMENT_SHORT_LEN} {SYMBOL_SEGMENT_SHORT_WORD_LEN} {MAX_PATH_WORD_LEN}"),
        format!("{MAX_ACRONYM_LEN} {MIN_MEAN_CAMEL_WORD_LEN} {MIN_VOWEL_PERCENT}"),
        format!("{GOOGLE_DOC_HOSTS:?} {GOOGLE_DOC_ID_LENS:?} {GOOGLE_DOC_CONTEXT_WORDS:?}"),
        format!("{GOOGLE_PUBLISHED_ID_PREFIX:?} {GOOGLE_PUBLISHED_ID_LEN}"),
        format!("{MAX_FILE_EXTENSION_LEN} {MAX_GITHUB_USER_ID_LEN} {MAX_GITHUB_LOGIN_LEN}"),
        format!("{GITHUB_NOREPLY_DOMAIN:?} {MAX_SHORT_STEM_RUN}"),
        format!("{PEM_BEGIN:?} {PEM_END:?} {PEM_PRIVATE_KEY:?}"),
        format!("{labels:?}"),
    ];
    fnv1a(parts.join("\n").as_bytes())
}

/// `(SECRET_FILTER_VERSION, fingerprint)` per released rule set, oldest first.
/// Append a row when the version is bumped; never edit an old row.
const PINNED_FINGERPRINTS: &[(u32, u64)] = &[
    (1, 0xa59c_686b_f67d_6f1b),
    // #9748: GitHub Projects v2 node-id shapes.
    (2, 0x5cef_e4b6_e8ed_c676),
];

/// Why (#8623): a ledger keyed on `SECRET_FILTER_VERSION` is only as good as
/// the discipline of bumping it. Any change to a prefix list, a length or a
/// label changes the fingerprint, and this test fails until the version is
/// bumped and a new row appended.
/// Test: itself; shown red when a provider prefix is added without a bump.
#[test]
fn secret_filter_version_pins_the_rule_inputs() {
    let fingerprint = rule_inputs_fingerprint();
    let versions: Vec<u32> = PINNED_FINGERPRINTS.iter().map(|(v, _)| *v).collect();
    assert!(
        versions.windows(2).all(|w| w[0] < w[1]),
        "versions must ascend"
    );
    assert_eq!(
        PINNED_FINGERPRINTS.last().copied(),
        Some((SECRET_FILTER_VERSION, fingerprint)),
        "the secret rule inputs changed (fingerprint {fingerprint:#018x}): bump \
         SECRET_FILTER_VERSION and append (version, fingerprint) to PINNED_FINGERPRINTS"
    );
}
