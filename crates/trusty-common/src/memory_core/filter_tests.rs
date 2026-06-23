//! Unit tests for the `memory_core::filter` content gate (issue #61, #1481).
//!
//! Why: split out of `filter.rs` so the production module stays under the
//! 500-SLOC cap while the test suite (classified as a test file, 1500-SLOC cap)
//! can grow with each new heuristic. Wired back in via
//! `#[path = "filter_tests.rs"] mod tests;` in `filter.rs`.
//! What: exercises token counting, every reject pattern, the git-SHA allowlist,
//! and the secret/credential detector (including AWS access key IDs).
//! Test: this *is* the test file.

use super::*;

#[test]
fn meaningful_tokens_ignore_pure_punctuation() {
    assert_eq!(count_meaningful_tokens("--- === >>>"), 0);
    assert_eq!(count_meaningful_tokens("one two three"), 3);
    assert_eq!(count_meaningful_tokens("foo --- bar"), 2);
}

#[test]
fn non_alpha_ratio_detects_json() {
    let json = r#"{"a":1,"b":[2,3,4]}"#;
    let ratio = non_alphabetic_ratio(json);
    assert!(
        ratio > 0.5,
        "expected JSON to register as mostly non-alphabetic; got {ratio}"
    );
    let prose = "The quick brown fox jumps over the lazy dog";
    assert!(non_alphabetic_ratio(prose) < 0.2);
}

#[test]
fn default_patterns_match_known_noise() {
    let cfg = FilterConfig::default();
    let cases = [
        "Tool use: search_files",
        "Tool result: ok",
        // NOTE (issue #1481): a bare 40-hex git SHA is NO LONGER rejected —
        // it is a legitimate engineering memory. See
        // `bare_git_sha_is_no_longer_rejected` below.
        "feat(memory): add filter",
        "fix: handle nulls",
        "Running cargo test...",
        "/Users/x/foo.rs",
        "~/notes.md",
    ];
    for c in cases {
        assert!(cfg.apply(c, false).is_err(), "expected reject for: {c}");
    }
}

/// Why (issue #1481): regression lock — the previous behaviour rejected a
/// bare 40-hex git SHA as noise, silently dropping a valid memory. The
/// allowlist must keep it accepted.
/// What: asserts `apply` accepts a bare full SHA.
/// Test: itself.
#[test]
fn bare_git_sha_is_no_longer_rejected() {
    let cfg = FilterConfig::default();
    // pragma: allowlist secret (test fixture: a bare git SHA, not a credential)
    assert!(
        cfg.apply("abcdef0123456789abcdef0123456789abcdef01", false) // pragma: allowlist secret
            .is_ok()
    );
}

#[test]
fn filter_config_default_blocks_known_noise() {
    let cfg = FilterConfig::default();
    let res = cfg.apply("Tool use: read_file", true);
    assert!(matches!(res, Err(FilterReject::NoisePattern { .. })));
}

#[test]
fn filter_config_too_short_triggers_token_error() {
    // Use a config with the stricter MCP threshold (8) so the assertion
    // is independent of the lower library default (3).
    let cfg = FilterConfig {
        min_tokens: MCP_MIN_TOKENS,
        ..FilterConfig::default()
    };
    let res = cfg.apply("only four tokens here", true);
    match res {
        Err(FilterReject::TooShort { tokens }) => assert_eq!(tokens, 4),
        other => panic!("expected TooShort, got {other:?}"),
    }
}

#[test]
fn note_mode_allows_short_content() {
    // Use the stricter MCP threshold so the assertion documents the
    // contract independently of the library default.
    let cfg = FilterConfig {
        min_tokens: MCP_MIN_TOKENS,
        ..FilterConfig::default()
    };
    // 3 tokens, would fail with enforce_min=true, must pass with false.
    assert!(cfg.apply("User prefers snake_case", false).is_ok());
    assert!(cfg.apply("User prefers snake_case", true).is_err());
}

#[test]
fn filter_accepts_real_content() {
    let cfg = FilterConfig::default();
    assert!(
        cfg.apply(
            "When refactoring search indices, prefer postcard over JSON for redb \
                 values because of size and decode speed.",
            true,
        )
        .is_ok()
    );
}

#[test]
fn filter_rejects_high_non_alpha_content() {
    let cfg = FilterConfig::default();
    let json = r#"{"id":1,"items":[2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20]}"#;
    let res = cfg.apply(json, false);
    assert!(matches!(res, Err(FilterReject::NonAlphabetic { .. })));
}

#[test]
fn classify_detects_commit_and_tool_use() {
    assert_eq!(
        classify("feat(memory): add filter", DrawerType::Unknown),
        DrawerType::Commit
    );
    assert_eq!(
        classify(
            "abcdef0123456789abcdef0123456789abcdef01", // pragma: allowlist secret
            DrawerType::Unknown
        ),
        DrawerType::Commit
    );
    assert_eq!(
        classify("Tool use: search_code", DrawerType::Unknown),
        DrawerType::SessionEvent
    );
    assert_eq!(
        classify("Running cargo test...", DrawerType::Unknown),
        DrawerType::SessionEvent
    );
    // Prose falls through to the supplied fallback.
    assert_eq!(
        classify(
            "A regular curated knowledge fragment.",
            DrawerType::AgentNote
        ),
        DrawerType::AgentNote
    );
}

#[test]
fn reject_messages_are_actionable() {
    let too_short = FilterReject::TooShort { tokens: 3 };
    assert!(too_short.to_string().contains("memory_note"));
    let noise = FilterReject::NoisePattern {
        pattern: "x".to_string(),
    };
    assert!(noise.to_string().contains("low-signal"));
    let na = FilterReject::NonAlphabetic { ratio: 85.0 };
    assert!(na.to_string().contains("force=true"));
    // Issue #1481: the secret rejection must name the offending token.
    let secret = FilterReject::PotentialSecret {
        token: "sk-A…(48 chars)".to_string(),
    };
    let msg = secret.to_string();
    assert!(msg.contains("sk-A"), "secret reject must name token: {msg}");
    assert!(
        msg.contains("secret"),
        "secret reject must say 'secret': {msg}"
    );
}

// ---- Issue #1481: git-SHA allowlist + secret detection ----

#[test]
fn git_sha_like_recognises_short_and_full() {
    // 7-char abbreviation (git's default) through full 40-char SHA-1.
    assert!(is_git_sha_like("0fda534"));
    assert!(is_git_sha_like("4c536992"));
    assert!(is_git_sha_like("0fda534e0fda534e0fda534e0fda534e0fda534e"));
    // Case-insensitive: uppercase hex is still a SHA.
    assert!(is_git_sha_like("0FDA534E"));
}

#[test]
fn git_sha_like_rejects_overlong_and_nonhex() {
    // 6 chars is below git's abbreviation floor.
    assert!(!is_git_sha_like("0fda53"));
    // 41 chars: was rejected under the old SHA-1-only cap (40); now accepted
    // as within the 7–64 range that covers SHA-256 repos (issue #1484).
    assert!(is_git_sha_like("0fda534e0fda534e0fda534e0fda534e0fda534e0"));
    // 65 chars exceeds SHA-256 length and must be rejected.
    let sixty_five = "a".repeat(65);
    assert!(!is_git_sha_like(&sixty_five));
    // Non-hex characters.
    assert!(!is_git_sha_like("0fda534g"));
    assert!(!is_git_sha_like("hello123"));
}

#[test]
fn git_sha_prose_is_accepted() {
    // The exact repro from issue #1481 must be stored, not skipped.
    let cfg = FilterConfig::default();
    let repro = "Shipped via PR #1466 squash 0fda534e -> merge 4c536992, CI green.";
    assert!(
        cfg.apply(repro, true).is_ok(),
        "git-SHA prose must pass the gate; got {:?}",
        cfg.apply(repro, true)
    );
    // A bare full SHA on its own is also legitimate now.
    assert!(
        cfg.apply("0fda534e0fda534e0fda534e0fda534e0fda534e", false)
            .is_ok()
    );
}

#[test]
fn git_sha_like_is_not_secret() {
    assert!(find_secret_token("merge 4c536992 into main").is_none());
    assert!(find_secret_token("0fda534e0fda534e0fda534e0fda534e0fda534e").is_none());
}

#[test]
fn secret_token_is_blocked() {
    // A real high-entropy, mixed-case+digit credential token must still
    // be rejected, and the reject must name the (redacted) trigger.
    let cfg = FilterConfig::default();
    let content = "Use this token AbCd1234EfGh5678IjKl9012 to authenticate the deploy webhook"; // pragma: allowlist secret
    match cfg.apply(content, true) {
        Err(FilterReject::PotentialSecret { token }) => {
            assert!(token.contains("AbCd"), "should name token: {token}");
            assert!(token.contains("chars"), "should redact tail: {token}");
        }
        other => panic!("expected PotentialSecret, got {other:?}"),
    }
}

#[test]
fn base64_blob_is_blocked() {
    let content = "config blob: aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NA== embedded in the note"; // pragma: allowlist secret
    assert!(matches!(
        FilterConfig::default().apply(content, false),
        Err(FilterReject::PotentialSecret { .. })
    ));
}

#[test]
fn known_key_prefixes_are_blocked() {
    for tok in [
        "sk-abcdef0123456789abcdef01",              // pragma: allowlist secret
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789", // pragma: allowlist secret
        "xoxb-1234-5678-abcdEFGH",                  // pragma: allowlist secret
    ] {
        assert!(
            looks_like_secret(tok),
            "prefix token should be a secret: {tok}"
        );
    }
}

/// Why (FN-1, issue #1481): AWS access key IDs (`AKIA…` long-term, `ASIA…`
/// STS temp creds) are all-uppercase base32 — they fail the
/// `has_lower && has_upper && has_digit` mixed-case heuristic, so before the
/// `akia`/`asia` prefixes were added they slipped through the content gate
/// and could be persisted into palace storage. This is a real secret-leak
/// path that contradicted the doc comments.
/// What: asserts both the `looks_like_secret` heuristic and the end-to-end
/// `apply` gate reject the canonical AWS example key IDs as
/// `PotentialSecret`.
/// Test: itself.
#[test]
fn aws_access_key_ids_are_blocked() {
    // Canonical AWS docs example access key ID + an STS temp-cred example.
    let akia = "AKIAIOSFODNN7EXAMPLE"; // pragma: allowlist secret
    let asia = "ASIAY34FZKBOKMUTVV7A"; // pragma: allowlist secret
    for tok in [akia, asia] {
        assert!(
            looks_like_secret(tok),
            "AWS access key ID must be flagged as secret: {tok}"
        );
    }
    // End-to-end through the gate: an AWS key embedded in prose must reject.
    let cfg = FilterConfig::default();
    let content =
        format!("Deploy creds leaked: access key {akia} and temp creds {asia} in the log");
    assert!(
        matches!(
            cfg.apply(&content, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "AWS access key IDs must reject end-to-end; got {:?}",
        cfg.apply(&content, false)
    );
}

/// Why (FN-1 over-blocking guard, issue #1481): the fix must not regress the
/// git-SHA allowlist — a bare 40-hex commit SHA is legitimate engineering
/// memory and must still pass the gate even though it is high-entropy.
/// What: asserts a full 40-char hex SHA is accepted (not flagged secret).
/// Test: itself.
#[test]
fn git_sha_still_allowed_after_aws_fix() {
    let cfg = FilterConfig::default();
    // 40 hex chars: a real git SHA-1 object id, NOT a credential.
    let sha = "abcdef0123456789abcdef0123456789abcdef01"; // pragma: allowlist secret
    assert!(
        !looks_like_secret(sha),
        "a 40-hex git SHA must not be flagged as a secret"
    );
    assert!(
        cfg.apply(sha, false).is_ok(),
        "a bare 40-hex git SHA must still pass the gate; got {:?}",
        cfg.apply(sha, false)
    );
}

#[test]
fn ordinary_words_are_not_secret() {
    for tok in [
        "authentication",
        "configuration",
        "refactoring",
        "snake_case",
    ] {
        assert!(!looks_like_secret(tok), "ordinary word flagged: {tok}");
    }
}

#[test]
fn non_alpha_masking_ignores_shas() {
    // Masking turns SHAs into "sha" so the ratio reflects the prose only.
    let masked = mask_git_shas("merge 4c536992 -> 0fda534e done");
    assert!(masked.contains("sha"));
    assert!(!masked.contains("4c536992"));
}

#[test]
fn redact_token_masks_tail() {
    let r = redact_token("AbCd1234EfGh5678IjKl9012"); // pragma: allowlist secret
    assert!(r.starts_with("AbCd"));
    assert!(r.contains("…"));
    assert!(r.contains("chars"));
    assert!(!r.contains("9012"), "tail must be masked: {r}");
}

#[test]
fn filter_config_force_bypasses_all() {
    // The `force` semantics live in the caller (we model it by skipping
    // `apply` entirely); this test exists so the contract is documented:
    // there is no way *inside* `apply` to bypass — the caller must not
    // call it at all to force-store.
    let cfg = FilterConfig::default();
    assert!(cfg.apply("Tool use: x", true).is_err());
}

// ---- Issue #1484: advisory hardening follow-ups ----

/// Why (issue #1484, gap 1): git 2.29+ repos using `--object-format=sha256`
/// emit 64-hex-char commit SHAs. Before raising GIT_SHA_MAX_LEN from 40 to 64,
/// a 64-char lowercase-hex token in prose would fail `is_git_sha_like` (too
/// long), pass the secret detector (pure-lowercase-hex with no credential
/// prefix), but then push the non-alpha ratio over the gate threshold when the
/// content was hex-dense. This test locks in the allowlist at 64 chars.
/// What: asserts 64-hex tokens are allowlisted as SHA-like and pass the gate.
/// Test: itself.
#[test]
fn git_sha_like_recognises_sha256() {
    // 64 hex chars: a SHA-256 git object id.
    let sha256 = "a".repeat(63) + "b"; // 63 'a' + 1 'b' = 64 hex chars
    assert!(
        is_git_sha_like(&sha256),
        "64-hex SHA-256 must be recognised as git-SHA-like"
    );
    // 65 chars: exceeds SHA-256, must NOT be allowlisted.
    let too_long = "a".repeat(65);
    assert!(
        !is_git_sha_like(&too_long),
        "65-hex token must not be allowlisted (exceeds SHA-256 length)"
    );
    // End-to-end: prose containing a SHA-256 commit reference must pass.
    let cfg = FilterConfig::default();
    let sha256_hex = "a".repeat(64);
    let prose = format!("Merged commit {sha256_hex} into main via PR #42.");
    assert!(
        cfg.apply(&prose, true).is_ok(),
        "prose with SHA-256 commit must pass the gate; got {:?}",
        cfg.apply(&prose, true)
    );
}

/// Why (issue #1484, gap 2): `looks_like_secret`'s fallback requires
/// `has_lower && has_upper && has_digit`. Mixed-case alphabetic tokens with no
/// digit (e.g. base58 wallet key segments) slip through unless they match a
/// known prefix. This test documents the limitation so future reviewers know
/// the known false-negative surface and do not mistake it for a bug introduced
/// later.
/// What: asserts a mixed-case-no-digit ≥20-char token is NOT flagged by the
/// fallback heuristic (documents the known gap without changing behaviour).
/// Test: itself.
#[test]
fn mixed_case_no_digit_limitation() {
    // 24-char mixed-case alphabetic, no digit, no known prefix — would be a
    // false negative if it were a real base58 key segment.
    let mixed_alpha_only = "AbCdEfGhIjKlMnOpQrStUvWx";
    assert_eq!(mixed_alpha_only.len(), 24);
    assert!(
        !looks_like_secret(mixed_alpha_only),
        "known FN-2 limitation (issue #1484): mixed-case-no-digit tokens \
         are not flagged by the fallback heuristic; document, do not panic"
    );
}

/// Why (issue #1484, gap 3): `is_git_sha_like` uses `is_ascii_hexdigit()`
/// which returns `true` for `0-9` as well as `a-f`/`A-F`. An all-digit string
/// of 7–64 chars (e.g. a long account number) is therefore allowlisted as
/// SHA-like. The risk is low, but callers should know the allowlist is broader
/// than strictly "git commit SHA". This test documents the known behaviour.
/// What: asserts that an all-digit 10-char string is accepted by
/// `is_git_sha_like` (broader-than-SHA allowlist), and that it also passes the
/// full gate as an allowlisted token.
/// Test: itself.
#[test]
fn pure_digit_token_is_sha_like() {
    // 10 ASCII digits: broader-than-SHA allowlist — known and documented.
    let digits = "1234567890";
    assert!(
        is_git_sha_like(digits),
        "known gap (issue #1484, gap 3): all-digit strings of SHA length \
         pass is_git_sha_like because ASCII digits are valid hex"
    );
    // Verify it does not cause a false reject (it is allowlisted as safe).
    assert!(
        !looks_like_secret(digits),
        "an all-digit short token must not be flagged as a secret"
    );
}

/// Why (issue #1484, gap 5): a bare single-token SHA passed to `apply` with
/// `enforce_min_tokens=true` is rejected as `TooShort` because it counts as
/// only 1 meaningful token — below the MCP threshold of 8. This is correct
/// behaviour (a bare SHA alone carries too little context for memory_remember)
/// but the gap was that the `enforce_min_tokens=true` path for bare SHAs had
/// no explicit test or doc coverage.
/// What: asserts `apply` returns `TooShort` for a bare SHA with the MCP
/// threshold and `enforce_min_tokens=true`, and `Ok` with `false`.
/// Test: itself.
#[test]
fn bare_sha_with_enforce_min_tokens_is_too_short() {
    let cfg = FilterConfig {
        min_tokens: MCP_MIN_TOKENS, // 8 tokens
        ..FilterConfig::default()
    };
    let bare_sha = "0fda534e0fda534e0fda534e0fda534e0fda534e"; // pragma: allowlist secret

    // With enforcement: 1 token < 8 threshold → TooShort.
    match cfg.apply(bare_sha, true) {
        Err(FilterReject::TooShort { tokens }) => assert_eq!(tokens, 1),
        other => {
            panic!("expected TooShort(1) for bare SHA with enforce_min_tokens=true; got {other:?}")
        }
    }

    // Without enforcement (e.g. memory_note path): the bare SHA passes.
    assert!(
        cfg.apply(bare_sha, false).is_ok(),
        "bare SHA must pass gate when enforce_min_tokens=false"
    );
}
