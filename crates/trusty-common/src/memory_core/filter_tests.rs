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

// ---- Issue #1667: false-positive fixes for path/slug/key=value tokens ----

/// Why (issue #1667): the `has_b64_sym` branch in `looks_like_secret` was
/// flagging `/` (path separator) and `=` (key=value, semver operator) as
/// base64 indicators, causing legitimate technical tokens to be rejected as
/// credentials. `is_structural_token` now guards that branch.
/// What: asserts each known-false-positive token is NOT flagged as a secret
/// by `looks_like_secret`, and that the full gate accepts them.
/// Test: itself.
#[test]
fn structural_tokens_are_not_flagged() {
    // slash-path tokens (issue #1667 primary cases)
    let slash_tokens = [
        "verdict/grade/prose-summary", // 27 chars — the exact reported FP
        "duettoresearch/duetto",       // org/repo slug
        "duettoresearch/projects/19",  // org/repo/id path
        "crates/trusty-review/src/pipeline/mapreduce/synthesis.rs", // file path
    ];
    for tok in slash_tokens {
        assert!(
            !looks_like_secret(tok),
            "slash-path token should NOT be flagged as secret: {tok}"
        );
    }

    // key=value / semver-operator tokens
    let eq_tokens = [
        ">=2-medium->REQUEST_CHANGES", // semver + status slug
    ];
    for tok in eq_tokens {
        assert!(
            !looks_like_secret(tok),
            "key=value/semver token should NOT be flagged as secret: {tok}"
        );
    }

    // version/crate tag strings
    let version_tokens = [
        "trusty-review-v0.6.0", // crate release tag — has hyphens, no b64 syms
    ];
    for tok in version_tokens {
        assert!(
            !looks_like_secret(tok),
            "version tag should NOT be flagged as secret: {tok}"
        );
    }
}

/// Why (issue #1667): end-to-end gate must ACCEPT content containing
/// the real false-positive tokens that were rejected before this fix.
/// What: passes each FP token through `FilterConfig::apply` and asserts Ok.
/// Test: itself.
#[test]
fn gate_accepts_fp_content() {
    let cfg = FilterConfig::default();
    let cases = [
        // The exact token from the rejection report:
        "Use verdict/grade/prose-summary as the synthesized output key",
        // Org/repo slug references:
        "See duettoresearch/duetto for the upstream fork and duettoresearch/projects/19 for tracking",
        // Semver / review-decision token:
        "Review verdict: >=2-medium->REQUEST_CHANGES must block merge",
        // File path reference:
        "The synthesis module lives at crates/trusty-review/src/pipeline/mapreduce/synthesis.rs",
        // Crate release tag:
        "Released trusty-review-v0.6.0 with map-reduce support",
        // Bare 40-char git SHA (regression lock from #1481):
        "Merged 0fda534e0fda534e0fda534e0fda534e0fda534e into main",
    ];
    for content in cases {
        assert!(
            cfg.apply(content, false).is_ok(),
            "gate must ACCEPT: {content:?}\ngot: {:?}",
            cfg.apply(content, false)
        );
    }
}

/// Why (issue #1667 hardening): verifying the fix did NOT weaken real-secret
/// detection. Prefix-based secrets (AKIA, ghp_, sk-, AIza) are caught by the
/// `SECRET_PREFIXES` / `find_secret_token` layer that runs BEFORE
/// `looks_like_secret`'s entropy heuristics; the assertions here go through
/// the REAL public entry points (`find_secret_token` and `FilterConfig::apply`)
/// so that the test proves the actual end-to-end blocking guarantee rather than
/// only a lower-level heuristic.
/// What: asserts every secret class (prefix-based and entropy-based) is rejected
/// by both `find_secret_token` (the intermediate public guard) and `apply` (the
/// full gate). For the GCP key (`AIzaSy…`), which is caught by the mixed-case+
/// digit fallback rather than a prefix, `looks_like_secret` is also verified
/// directly since that is the authoritative guard for that class.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_1667_fix() {
    let cfg = FilterConfig::default();

    // --- Prefix-based secrets: verified through the public guard ---
    // `find_secret_token` is the REAL layer that catches these (the
    // `SECRET_PREFIXES` check inside `looks_like_secret` is reached only via
    // `find_secret_token`; calling `looks_like_secret` directly on a bare
    // prefix token tests a sub-layer but not the actual end-to-end path).
    let prefix_cases = [
        ("AKIAIOSFODNN7EXAMPLE", "AWS long-term key"), // pragma: allowlist secret
        ("ASIAY34FZKBOKMUTVV7A", "AWS STS temp cred"), // pragma: allowlist secret
        ("ghp_abcdefghijklmnopqrstuvwxyz012345", "GitHub PAT"), // pragma: allowlist secret
        ("sk-abcdefghijklmnopqrstuvwxyz01234567890123", "OpenAI key"), // pragma: allowlist secret
    ];
    for (tok, label) in prefix_cases {
        // Layer 1: the intermediate public guard must find the secret token.
        assert!(
            find_secret_token(tok).is_some(),
            "{label} must be caught by find_secret_token: {tok}"
        );
        // Layer 2: the full gate (apply) must return PotentialSecret when the
        // token appears in prose, which is the actual blocking guarantee.
        let prose = format!("Deployment secret: {tok}");
        assert!(
            matches!(
                cfg.apply(&prose, false),
                Err(FilterReject::PotentialSecret { .. })
            ),
            "{label} must be rejected by apply end-to-end; got {:?}",
            cfg.apply(&prose, false)
        );
    }

    // --- GCP API key: caught by the mixed-case+digit fallback, not a prefix ---
    // For this class, `looks_like_secret` is the primary guard; verify all
    // three layers: looks_like_secret, find_secret_token, and apply.
    let gcp_key = "AIzaSyDdI0hCZtE6vySjMm-WEfRq3CPzqKqqsHI"; // pragma: allowlist secret
    assert!(
        looks_like_secret(gcp_key),
        "GCP API key must be flagged by looks_like_secret: {gcp_key}"
    );
    assert!(
        find_secret_token(gcp_key).is_some(),
        "GCP API key must be caught by find_secret_token: {gcp_key}"
    );
    let gcp_prose = format!("API key: {gcp_key} — keep secret");
    assert!(
        matches!(
            cfg.apply(&gcp_prose, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "GCP API key must be rejected by apply end-to-end; got {:?}",
        cfg.apply(&gcp_prose, false)
    );

    // --- High-entropy base64 blob (with `+`): caught by the b64-symbol branch ---
    // `+` is unambiguously base64 and is not a structural char, so this blob
    // must be rejected even though it also contains `/` (which would be a
    // structural indicator if `+` weren't present).
    let b64_blob = "aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NAaGVsbG8rd29ybGQ="; // pragma: allowlist secret
    let b64_content = format!("Config blob: {b64_blob} stored in env");
    assert!(
        find_secret_token(&b64_content).is_some(),
        "base64 blob must be caught by find_secret_token"
    );
    assert!(
        matches!(
            cfg.apply(&b64_content, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "content with base64 blob must reject end-to-end; got {:?}",
        cfg.apply(&b64_content, false)
    );
}

// ---- Issue #1676: key=value tokens where the value is a slash-path ----

/// Why (issue #1676): `is_structural_token` routed tokens containing both `=`
/// and `/` to the slash-path branch first, where the first `/`-segment (e.g.
/// `reviewer_model=openrouter`) contains `=` and fails `is_word_segment`,
/// causing false positives. The fix: evaluate `=` before `/` and use a
/// compositional check — LHS must be a word segment AND the RHS must be
/// either a word segment or a slash-path.
/// What: asserts that `key=slash/path/value` tokens are NOT flagged as secrets.
/// Test: itself.
#[test]
fn key_equals_slashpath_not_flagged() {
    // The exact token from the live false positive (issue #1676).
    assert!(
        !looks_like_secret("reviewer_model=openrouter/openai/gpt-5.4-mini-20260317"),
        "reviewer_model=openrouter/openai/... must NOT be flagged as secret"
    );

    // Additional key=slash-path shapes.
    let allowed = [
        "reviewer_model=openrouter/openai/gpt-5.4-mini-20260317",
        "model=anthropic/claude-3-5-sonnet",
        "provider=openai/gpt-4o",
        "config=org/repo/settings.json",
        "base_path=usr/local/bin",
        // Plain key=word (unchanged existing behaviour).
        "timeout=30s",
        "env=production",
        // key=version-tag
        "version=v1.2.3",
    ];
    for tok in allowed {
        assert!(
            !looks_like_secret(tok),
            "key=value token should NOT be flagged as secret: {tok}"
        );
    }
}

/// Why (issue #1676): end-to-end gate must ACCEPT content containing
/// `key=slash/path` tokens.
/// What: passes the confirmed live false positive and additional representative
/// cases through `FilterConfig::apply` and asserts Ok.
/// Test: itself.
#[test]
fn gate_accepts_key_equals_slashpath() {
    let cfg = FilterConfig::default();
    let cases = [
        // The exact rejection from the issue report:
        "reviewer_model=openrouter/openai/gpt-5.4-mini-20260317",
        // Representative variations:
        "Using model=anthropic/claude-3-5-sonnet for code review",
        "Set provider=openai/gpt-4o in your config",
        "Deploy with config=org/repo/settings.json",
    ];
    for content in cases {
        assert!(
            cfg.apply(content, false).is_ok(),
            "gate must ACCEPT: {content:?}\ngot: {:?}",
            cfg.apply(content, false)
        );
    }
}

// ---- Issue #2442: blocklist prefix-anchoring + secret-heuristic FP fixes ----

/// Why (issue #2442): `blocklist_match` replaces the old substring-anywhere
/// `str::contains` check with a `starts_with` anchor. Known auto-capture
/// prefixes must still be caught at the start of (whitespace-trimmed)
/// content.
/// What: asserts the exact write-path prefixes are matched.
/// Test: itself.
#[test]
fn blocklist_match_blocks_known_prefixes() {
    assert_eq!(blocklist_match("Tool use: Bash"), Some("Tool use: "));
    assert_eq!(
        blocklist_match("   Tool use: Read"),
        Some("Tool use: "),
        "leading whitespace must not let it through"
    );
    assert_eq!(
        blocklist_match("Claude Code session ended: abc"),
        Some("Claude Code session")
    );
}

/// Why (issue #2442): the "sharper problem" from the issue report — a coding
/// agent's turn text routinely QUOTES phrases like `"Tool use: "` mid-prose
/// when recapping tool output. The old `contains`-based match silently
/// dropped these legitimate memories. Anchoring to the start of the content
/// must let them through.
/// What: asserts content that merely mentions the pattern (not framed by it)
/// is NOT matched.
/// Test: itself.
#[test]
fn blocklist_match_ignores_quoted_mid_text() {
    assert_eq!(blocklist_match("I used Tool use: Bash here"), None);
    assert_eq!(
        blocklist_match("The transcript shows Claude Code session lifecycle events firing twice"),
        None
    );
    assert_eq!(blocklist_match("an ordinary engineering note"), None);
}

/// Why (issue #2442, live false positive #1): a Rust source-location
/// reference of the shape `crate/module/file.rs::function_name` was rejected
/// by `looks_like_secret` because the `::` inside the final slash-segment
/// broke `is_word_segment`, so the token fell through to the entropy
/// heuristics — `/` tripped `has_b64_sym` and the token was flagged.
/// What: asserts the exact real-world rejected token (and the full gate,
/// end-to-end) now passes.
/// Test: itself.
#[test]
fn path_like_token_with_rust_module_separator_not_flagged() {
    let tok = "client/http_client/error.rs::response_or_body_error";
    assert!(
        !looks_like_secret(tok),
        "Rust path::fn reference must NOT be flagged as secret: {tok}"
    );
    let cfg = FilterConfig::default();
    let content = format!(
        "Milestone: fixed the retry loop in {tok} so transient 5xxs no longer abort the batch"
    );
    assert!(
        cfg.apply(&content, true).is_ok(),
        "gate must ACCEPT the path::fn reference; got {:?}",
        cfg.apply(&content, true)
    );
}

/// Why (issue #2442, live false positive #2): a compact issue/PR/SHA ledger
/// reference like `#2486→PR#2491(e993c18a)` mixes uppercase (`PR`), digits,
/// and lowercase hex — enough to trip the old unconditional mixed-case+digit
/// fallback — even though no real credential format contains `#`, arrows, or
/// parentheses.
/// What: asserts the exact real-world rejected token (and the full gate,
/// end-to-end) now passes.
/// Test: itself.
#[test]
fn ledger_reference_token_not_flagged() {
    let tok = "#2486→PR#2491(e993c18a)";
    let content = format!("Shipped {tok} closing the retry-loop regression");
    assert!(
        find_secret_token(&content).is_none(),
        "issue/PR/SHA ledger token must NOT be flagged as secret: {content}"
    );
    let cfg = FilterConfig::default();
    assert!(
        cfg.apply(&content, true).is_ok(),
        "gate must ACCEPT the ledger reference; got {:?}",
        cfg.apply(&content, true)
    );
}

/// Why (issue #2442 hardening): the ledger/path false-positive fixes must not
/// weaken detection of real secrets — including ones that happen to sit next
/// to ledger-shaped punctuation in the same content.
/// What: asserts a known-prefix secret is still rejected even when the
/// surrounding content also contains a ledger reference and a path::fn
/// reference (both now allowlisted individually).
/// Test: itself.
#[test]
fn real_secrets_still_blocked_alongside_2442_fp_fixes() {
    let cfg = FilterConfig::default();
    let content = "See #2486->PR#2491(e993c18a) and crates/foo/bar.rs::baz — deploy secret \
                    sk-abcdefghijklmnopqrstuvwxyz01234567890123"; // pragma: allowlist secret
    assert!(
        matches!(
            cfg.apply(content, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "real credential must still be rejected end-to-end; got {:?}",
        cfg.apply(content, false)
    );
}

/// Why (issue #2442 hardening, path segment): confirm the `:` addition to
/// `is_word_segment` does not let arbitrary secrets slip through when
/// wrapped in a slash-path with colons — the wrapping segment must still be
/// alnum/`-`/`_`/`.`/`:` ONLY. A segment containing `@` (as in a connection
/// string `user:pass@host`) still fails structural detection.
/// What: asserts a connection-string-shaped token with an embedded `@` is
/// still flagged by the base64-symbol fallback.
/// Test: itself.
#[test]
fn connection_string_shaped_token_still_flagged() {
    let tok = "postgres://user:pass@host/dbname12345678901234567890"; // pragma: allowlist secret
    assert!(
        looks_like_secret(tok),
        "connection-string-shaped token must still be flagged: {tok}"
    );
}

/// Why (issue #1676): the compositional fix must NOT weaken detection of real
/// secrets embedded as values in `key=value` tokens. The critical guard is the
/// `+` early-exit in `is_structural_token`: a `key=base64blob` where the
/// encoded blob itself contains `+` (a standard base64 symbol) is NEVER
/// structural, regardless of the `=` reordering introduced by this fix.
///
/// Note: `key=alnum_only_blob` (no `+`, no `/` in value, all alnum) is treated
/// as structural by `is_word_segment` — this is a KNOWN FALSE NEGATIVE that
/// predates issue #1667 (the `=` branch already returned true for these), and
/// the #1676 compositional fix does not change it. Real-world API keys with a
/// slash in them are not emitted by any known provider. The `+` guard and the
/// known-prefix layer (`sk-`, `ghp_`, `AKIA`, …) remain the primary defences.
///
/// What: verifies that `key=<blob-with-plus>` tokens are still flagged and that
/// the known prefix layer (`AKIA…`) is still operative end-to-end when the
/// token arrives as a standalone whitespace-delimited token (not embedded in
/// `key=value` form, where the prefix check is bypassed).
/// Test: itself.
#[test]
fn key_equals_secret_still_blocked_after_1676_fix() {
    let cfg = FilterConfig::default();

    // A `key=` token where the base64-ENCODED value itself contains `+` —
    // the `+` early-exit in `is_structural_token` fires before the `=` branch,
    // so the token is correctly non-structural even after the reordering.
    // Base64 of arbitrary bytes that encodes to contain `+`:
    // bytes [3, 224, 200] × 10 → "A+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DI"
    let b64_value = "A+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DI"; // pragma: allowlist secret
    let b64_kv = format!("token={b64_value}");
    assert!(
        find_secret_token(&b64_kv).is_some(),
        "key=base64blob (with literal + in encoding) must still be flagged: {b64_kv}"
    );
    assert!(
        matches!(
            cfg.apply(&b64_kv, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "key=base64blob (with +) must be rejected by apply; got {:?}",
        cfg.apply(&b64_kv, false)
    );

    // A standalone known-prefix secret — the prefix check runs BEFORE
    // `is_structural_token`, so moving `=` before `/` in the structural check
    // does NOT affect it.
    let cases = [
        ("AKIAIOSFODNN7EXAMPLE", "AWS long-term key"), // pragma: allowlist secret
        ("ghp_abcdefghijklmnopqrstuvwxyz012345", "GitHub PAT"), // pragma: allowlist secret
        ("sk-abcdefghijklmnopqrstuvwxyz01234567890123", "OpenAI key"), // pragma: allowlist secret
    ];
    for (tok, label) in cases {
        assert!(
            find_secret_token(tok).is_some(),
            "{label} must still be caught by find_secret_token after #1676 fix: {tok}"
        );
        let prose = format!("Config: {tok}");
        assert!(
            matches!(
                cfg.apply(&prose, false),
                Err(FilterReject::PotentialSecret { .. })
            ),
            "{label} must still be rejected by apply end-to-end; got {:?}",
            cfg.apply(&prose, false)
        );
    }
}

/// Why (issue #2520 review, BLOCKER): `is_plausible_credential_charset`'s
/// allowed set (alnum + `-`/`_`/`.`) excluded `:`, so ANY colon anywhere in a
/// token — even a colon that is not part of any structural shape recognised
/// by `is_structural_token` — flunked the `.all()` check and disabled the
/// mixed-case+digit fallback entirely. On `origin/main` (pre-#2442, no
/// charset gate at all) `token:aBc123XyZ987uvW456QrS` was correctly flagged;
/// after #2442 added the charset gate it silently stopped being flagged
/// (`find_secret_token` returned `None`) — a real false-negative regression.
/// The `path::fn` false positive this PR fixes is handled by a DIFFERENT,
/// earlier code path (`is_structural_token`'s slash-path branch, which
/// short-circuits `looks_like_secret` before the charset gate is ever
/// reached), so the two functions do not need to (and must not be made to)
/// share behaviour here — restoring `:` to the credential charset closes the
/// regression without reopening the path::fn or ledger-reference false
/// positives.
/// What: asserts a bare colon-bearing credential-shaped token is flagged by
/// both `find_secret_token` and the end-to-end `FilterConfig::apply` gate.
/// Test: itself.
#[test]
fn colon_bearing_credential_is_flagged() {
    let tok = "key:aBc123XyZ987uvW456QrS"; // pragma: allowlist secret
    assert!(
        find_secret_token(tok).is_some(),
        "bare colon-bearing credential-shaped token must be flagged: {tok}"
    );
    let cfg = FilterConfig::default();
    let content = format!("Config: {tok}");
    assert!(
        matches!(
            cfg.apply(&content, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "colon-bearing credential must be rejected end-to-end; got {:?}",
        cfg.apply(&content, false)
    );
}

// ---- Issue #2800: slash-separated issue/PR-number lists ----

/// Why (issue #2800, observed live twice): a PM session checkpoint enumerating
/// PRs as `#2763/#2774/#2780/#2782/#2790` was rejected as a secret. The token
/// carries no letters at all, but the `/` separators set `has_b64_sym` and the
/// digits set `has_digit`, so the base64-blob branch of `looks_like_secret`
/// fired. `is_structural_token`'s slash-path branch does not rescue it because
/// the `#` in each segment fails `is_word_segment`.
/// What: asserts the EXACT token from the issue body passes both the token
/// predicate and the end-to-end gate, in the prose shape it was written in.
/// Test: itself.
#[test]
fn slash_separated_pr_number_list_not_flagged() {
    let tok = "#2763/#2774/#2780/#2782/#2790";
    assert!(
        !looks_like_secret(tok),
        "slash-separated PR-number list must NOT be flagged as secret: {tok}"
    );
    // `find_secret_token` trims the leading `#`, yielding the 28-char token
    // quoted in the issue's rejection message — cover that shape too.
    let content = format!("Merged {tok} closing the eviction epic");
    assert!(
        find_secret_token(&content).is_none(),
        "PR-number list in prose must NOT be flagged; got {:?}",
        find_secret_token(&content)
    );
    let cfg = FilterConfig::default();
    assert!(
        cfg.apply(&content, true).is_ok(),
        "gate must ACCEPT the PR-number list; got {:?}",
        cfg.apply(&content, true)
    );
}

/// Why (issue #2800): the digit/separator exemption must be a keyhole, not a
/// hole. Anything with an alphabetic character is outside the exemption, so
/// every real-credential shape the module already blocks must still be blocked
/// — including ones that sit next to a PR-number list in the same content, and
/// including the adversarial case of a credential that is *mostly* digits and
/// slashes with only a few letters smuggled in.
/// What: asserts known-prefix, base64-blob, connection-string, and
/// digits-plus-letters credentials all remain flagged after the exemption.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_2800_exemption() {
    let cfg = FilterConfig::default();

    // 1. Known-prefix credential sharing content with an allowlisted PR list —
    //    the exemption must not shadow other tokens in the same memory.
    let mixed = "Checkpoint #2763/#2774/#2780/#2782/#2790 deploy key \
                 sk-abcdefghijklmnopqrstuvwxyz01234567890123"; // pragma: allowlist secret
    assert!(
        matches!(
            cfg.apply(mixed, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "known-prefix credential must still be rejected beside a PR list; got {:?}",
        cfg.apply(mixed, false)
    );

    // 2. LOAD-BEARING: a token that is `#`/digit/slash-shaped EXCEPT for a
    //    smuggled alphabetic segment. One letter must take it out of the
    //    #2800 exemption. If the exemption were written as "contains only
    //    digits and separators, ignoring letters" — or applied before the
    //    charset was fully checked — this is the token that would slip through.
    //
    //    #4312 changed what this asserts against, and the change is
    //    deliberate. The invariant has always been "the #2800 exemption is a
    //    keyhole, not a hole"; the original assertion probed it INDIRECTLY via
    //    `looks_like_secret == true`. That proxy stopped holding once the
    //    base64 branch gained `is_plausible_b64_charset`, which declines any
    //    `#`-bearing token — `#` appears in no credential format, so declining
    //    it loses no real detection, and this token is prose in every reading.
    //    So assert the invariant itself rather than a downstream verdict that
    //    a second, stricter gate now also owns. Coverage is not reduced: the
    //    same token is still tested, for the same property.
    let sneaky = "#2763/#2774/#abcd/#2782/#2790"; // pragma: allowlist secret
    assert!(
        !is_issue_number_list(sneaky),
        "a `#`/digit/slash token containing letters must NOT be exempted by the \
         #2800 issue-list allowlist: {sneaky}"
    );

    // 3. LOAD-BEARING: `+` is the unambiguous base64 indicator. It is outside
    //    the exemption charset, so a digit-and-slash token carrying one must
    //    stay flagged even though it has no letters at all.
    let with_plus = "1234/5678/9012+3456/7890/1234"; // pragma: allowlist secret
    assert!(
        looks_like_secret(with_plus),
        "`+` (base64 indicator) must defeat the exemption: {with_plus}"
    );

    // 4. Base64 blob with `=` padding — the same `has_b64_sym` branch the
    //    exemption short-circuits. Letters present, so still flagged.
    let b64 = "dGhpcyBpcyBhIHZlcnkgbG9uZyBzZWNyZXQgdG9rZW4="; // pragma: allowlist secret
    assert!(
        looks_like_secret(b64),
        "padded base64 blob must still be flagged: {b64}"
    );

    // 5. Connection string — `/`-bearing and digit-bearing, letters present.
    let conn = "postgres://user:pass@host/dbname12345678901234567890"; // pragma: allowlist secret
    assert!(
        looks_like_secret(conn),
        "connection-string-shaped token must still be flagged: {conn}"
    );
}

// ---- Issue #4312: backtick-joined Markdown inline-code spans ----

/// Why (issue #4312, fourth recurrence of this shape after #1667, #2800,
/// #4216): `find_secret_token` split only on whitespace, so two Markdown
/// inline-code spans joined by a bare `/` — `` `foo`/`bar` `` — arrived at the
/// heuristic as ONE token. `trim_matches` strips punctuation only from the
/// outer boundary, so the interior backticks survived, `is_word_segment`
/// rejected every `/`-segment (backtick is in neither its charset nor
/// `is_issue_number_list`'s), and the token fell to the base64 branch: `/`
/// sets `has_b64_sym`, a lowercase letter sets `has_lower` → flagged.
/// What: asserts the three shapes from the issue — a backtick-joined
/// identifier pair, a backtick-joined path, and a backtick-wrapped issue list
/// (the #4216 regression) — all pass `find_secret_token` and the end-to-end
/// gate.
/// Test: itself.
#[test]
fn backtick_joined_spans_are_not_flagged() {
    let cfg = FilterConfig::default();
    for content in [
        // 1. Two backtick-joined identifiers, 34 chars combined.
        "`stale_skills`/`doctor_staleness.rs`",
        // 2. Two backtick-joined path spans, ~51 chars combined.
        "`crates/trusty-common/src/memory_core`/`filter_tests.rs`",
        // 3. Backtick-wrapped issue-number list — straight regression of the
        //    #4216 exemption, which recurs the moment the list is quoted.
        "`#4601`/`#4602`/`#4603`",
    ] {
        assert!(
            find_secret_token(content).is_none(),
            "backtick-joined span must NOT be flagged: {content}; got {:?}",
            find_secret_token(content)
        );
        let prose = format!("Checkpoint: touched {content} in this pass");
        assert!(
            find_secret_token(&prose).is_none(),
            "backtick-joined span in prose must NOT be flagged: {prose}; got {:?}",
            find_secret_token(&prose)
        );
        assert!(
            cfg.apply(&prose, true).is_ok(),
            "gate must ACCEPT backtick-joined prose; got {:?}",
            cfg.apply(&prose, true)
        );
    }
}

/// Why (issue #4312): treating the backtick as a token delimiter must not open
/// a hole. A backtick never appears in any credential alphabet (base64,
/// base64url, hex, base32) nor in any known provider key format, so no genuine
/// credential can be split by it — but that claim has to be proven, not
/// asserted. These cases pin the three detection layers (known prefix, base64
/// blob, mixed-case entropy) while a backtick is present in the same content.
/// What: asserts a GitHub token, an OpenAI-style key, an AWS access key ID, a
/// padded base64 blob, and a bare mixed-case credential all remain rejected
/// when quoted in backticks or sitting beside backtick-quoted prose.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_4312_backtick_split() {
    let cfg = FilterConfig::default();

    // 1. Credentials wrapped in backticks — the commonest way a leaked key
    //    actually reaches a memory write (someone quotes it as code).
    for tok in [
        "`ghp_abcdefghijklmnopqrstuvwxyz0123456789`", // pragma: allowlist secret
        "`sk-abcdefghijklmnopqrstuvwxyz01234567890123`", // pragma: allowlist secret
        "`AKIAIOSFODNN7EXAMPLE`",                     // pragma: allowlist secret
        "`dGhpcyBpcyBhIHZlcnkgbG9uZyBzZWNyZXQgdG9rZW4=`", // pragma: allowlist secret
        "`AbCd1234EfGh5678IjKl9012`",                 // pragma: allowlist secret
    ] {
        assert!(
            find_secret_token(tok).is_some(),
            "backtick-quoted credential must STILL be flagged: {tok}"
        );
    }

    // 2. LOAD-BEARING: a real credential sharing content with the exact
    //    backtick-joined shape #4312 exempts. The delimiter change must not
    //    shadow other tokens in the same memory.
    let mixed = "Touched `stale_skills`/`doctor_staleness.rs`; deploy key \
                 ghp_abcdefghijklmnopqrstuvwxyz0123456789"; // pragma: allowlist secret
    assert!(
        matches!(
            cfg.apply(mixed, false),
            Err(FilterReject::PotentialSecret { .. })
        ),
        "credential beside a backtick-joined span must still reject; got {:?}",
        cfg.apply(mixed, false)
    );

    // 3. LOAD-BEARING (detection TIGHTENED, not loosened): a credential
    //    written flush against surrounding text and a backtick. Under
    //    whitespace-only splitting this arrived as one token whose backtick
    //    defeated `is_plausible_credential_charset`, silently disabling the
    //    mixed-case fallback — the secret was NOT flagged. Splitting on the
    //    backtick isolates the 24-char credential and catches it.
    let adjacent = "token`AbCd1234EfGh5678IjKl9012`)"; // pragma: allowlist secret
    assert!(
        find_secret_token(adjacent).is_some(),
        "credential flush against a backtick must be flagged: {adjacent}"
    );

    // 4. LOAD-BEARING, and the ONE direction this change can regress: a
    //    backtick INSIDE a credential-bearing token rather than adjacent to
    //    it. Machine-generated credentials cannot contain one, but a
    //    user-chosen password in a connection-string URL can — the charset
    //    there is unbounded. The split divides it, the left fragment falls
    //    under the 20-char floor, and the right fragment loses the `/` that
    //    set `has_b64_sym`, so the credential is MISSED. That is a real
    //    limitation, it is documented on `find_secret_token`, and it is
    //    pinned here so a future reader meets the behaviour rather than the
    //    paragraph. If this assertion ever starts failing, the limitation was
    //    closed — update the doc comment, do not delete the test.
    let interior = "mongodb://user:pa`ss@cluster0.mongodb.net"; // pragma: allowlist secret
    assert!(
        find_secret_token(interior).is_none(),
        "KNOWN LIMITATION (#4312): a backtick inside a connection-string \
         password splits the token and the credential is missed. Got {:?}",
        find_secret_token(interior)
    );
    // The same connection string WITHOUT the interior backtick must still be
    // caught — this is what bounds the limitation to the split, and proves the
    // detector has not simply stopped seeing connection strings.
    let clean = "mongodb://user:passw0rdX@cluster0.mongodb.net"; // pragma: allowlist secret
    assert!(
        find_secret_token(clean).is_some(),
        "connection-string credential must still be flagged: {clean}"
    );
}

/// Why (issue #4312 round 3): the first cut of `is_url_credential_shaped`
/// exempted any token containing `://` or `@`, which is monotone-WIDENING — it
/// held open 14 URL-shaped prose false positives that the entropy floor would
/// otherwise have closed. Bare documentation links, `git@…` remotes and plain
/// addresses are not credentials, and this project's own always-clickable-links
/// convention puts one of these tokens in essentially every session checkpoint.
/// Narrowing the predicate to `scheme://user:pass@host` — a colon-bearing
/// userinfo before the first `@` — closes 10 of the 14 while exempting zero
/// prose and keeping every connection string (asserted in
/// `real_secrets_still_blocked_after_4312_charset_gate`).
/// What: asserts the ten URL-shaped prose tokens the narrowing fixes. All ten
/// are false positives on `origin/main`.
/// Test: itself.
#[test]
fn url_shaped_prose_is_not_flagged() {
    for tok in [
        "https://crates.io/crates/trusty-common/versions",
        "https://docs.rs/trusty-common/latest/trusty_common",
        "https://api.github.com/repos/bobmatnyc/trusty-tools",
        "https://example.com/docs/getting-started-guide",
        "https://bobmatnyc.github.io/trusty-tools/reference",
        "git@github.com:bobmatnyc/trusty-tools.git",
        "contact@example.com/support-team-list",
        "amqp://rabbitmq.svc.cluster.local/vhost-production",
        "redis://cache.internal/keyspace-notifications",
        "postgres://localhost/trusty_dev",
    ] {
        assert!(
            find_secret_token(tok).is_none(),
            "URL-shaped prose must NOT be flagged: {tok}; got {:?}",
            find_secret_token(tok)
        );
        // The narrowed predicate must not exempt prose — exempting it would
        // mask the result above rather than earn it.
        assert!(
            !is_url_credential_shaped(tok),
            "prose must not be exempted by is_url_credential_shaped: {tok}"
        );
    }
}

/// Why (issue #4312 round 3): four URL-shaped prose tokens still false-positive
/// after the narrowing, because they carry a digit or an uppercase letter and so
/// clear the entropy floor on their own. They are **pre-existing** — all four are
/// flagged identically on `origin/main` — and this PR fixes ten of their
/// fourteen siblings, but the residue includes the single most common shape in
/// this project's checkpoints: a bare GitHub issue/PR link.
///
/// They are NOT fixed here because the obvious blanket rule is unsafe. A
/// userinfo-free URL can still carry a secret in its PATH — the webhook URL in
/// `real_secrets_still_blocked_after_4312_charset_gate` is exactly that shape,
/// and exempting bare URLs would lose it. Separating "URL path that is a path"
/// from "URL path that is a credential" is a distinct design problem and belongs
/// in its own change.
/// What: pins the residue as change-detectors. If one starts passing, the
/// follow-up landed — update this test, do not delete it.
/// Test: itself.
#[test]
fn url_prose_residue_is_a_known_pre_existing_bound() {
    for tok in [
        "https://github.com/bobmatnyc/trusty-tools/pull/4723",
        "https://github.com/bobmatnyc/trusty-tools/issues/4312",
        "file:///Users/masa/trusty-tools/crates/common",
        "mongodb://cluster0.mongodb.net/analytics-store",
    ] {
        assert!(
            find_secret_token(tok).is_some(),
            "KNOWN BOUND (#4312): this URL-shaped prose is still flagged, as it \
             is on origin/main. If it now passes, the follow-up landed — move it \
             into `url_shaped_prose_is_not_flagged`. Token: {tok}"
        );
    }
}

// ---- Issue #4312: the base64-branch charset gate + entropy floor ----

/// Why (issue #4312): the delimiter change alone closes the backtick shape but
/// not its cause. The base64 branch of `looks_like_secret` fired on ANY ≥20-char
/// token containing `/` plus one letter that `is_structural_token` declined to
/// rescue — and `is_word_segment`'s charset is deliberately narrow, so EVERY
/// Markdown decoration (backtick, `**`, `"`, `'`, `[`, `(`, `|`, `%`, `,`) put
/// ordinary prose on the credential path. Backtick was simply the fourth
/// character to be noticed. Gating the branch on the charset a real blob is made
/// of, plus an entropy floor, attacks the cause instead of enumerating shapes.
/// What: asserts all FOUR reproductions enumerated in #4312 comment 2 — which
/// the issue explicitly asks to be presented as must-not-flag regression cases —
/// plus the three backtick shapes and eleven further Markdown/prose shapes that
/// reproduce the identical defect with different punctuation.
/// Test: itself.
#[test]
fn four_4312_acceptance_cases_are_not_flagged() {
    let cfg = FilterConfig::default();
    // The four reproductions from #4312 comment 2, verbatim.
    let acceptance = [
        ("file:line citation", "credentials/secret.rs:106,:118"),
        (
            "cargo feature list",
            "--features credentials,inference-client",
        ),
        ("hyphenated English", "old-leaks/new-doesn't"),
        ("plus-joined phrase", "ticker+shutdown-channel"),
    ];
    for (label, tok) in acceptance {
        assert!(
            find_secret_token(tok).is_none(),
            "#4312 acceptance case ({label}) must NOT be flagged: {tok}; got {:?}",
            find_secret_token(tok)
        );
        let prose = format!("Checkpoint: noted {tok} during the pass");
        assert!(
            cfg.apply(&prose, true).is_ok(),
            "gate must ACCEPT #4312 acceptance case ({label}); got {:?}",
            cfg.apply(&prose, true)
        );
    }
}

/// Why (issue #4312): the same defect, with the backtick swapped for every
/// other decoration a Markdown-shaped memory write actually contains. A
/// Markdown link to a source file and a Markdown table row are not exotic —
/// they are what these writes are made of, and each was a live false positive
/// before the charset gate. Pinning them here is what makes the fix a fix
/// rather than a fifth per-shape exemption.
/// What: asserts eleven decorated path/issue-list shapes pass the token
/// predicate.
/// Test: itself.
#[test]
fn markdown_decorated_paths_are_not_flagged() {
    for tok in [
        "**stale_skills**/**doctor_staleness.rs**",
        "\"stale_skills\"/\"doctor_staleness.rs\"",
        "'stale_skills'/'doctor_staleness.rs'",
        "[filter.rs](crates/trusty-common/src/filter.rs)",
        "|crates/common|src/memory_core/filter.rs|",
        "[stale_skills]/[doctor_staleness.rs]",
        "(stale_skills)/(doctor_staleness.rs)",
        "\"#4601\"/\"#4602\"/\"#4603\"",
        "**#4601**/**#4602**/**#4603**",
        "docs/my%20file/spec%20v2.md",
        "82%-100%/idle-across-the-window",
    ] {
        assert!(
            find_secret_token(tok).is_none(),
            "Markdown-decorated path must NOT be flagged: {tok}; got {:?}",
            find_secret_token(tok)
        );
    }
}

/// Why (issue #4312): the charset gate and the entropy floor both NARROW the
/// base64 branch, which is the one direction that can lose detection. Over-
/// blocking is the reported bug; under-blocking would be a security regression,
/// so the narrowing has to be pinned against every credential family the branch
/// is responsible for.
///
/// The entropy floor (`has_upper || has_digit`) is the riskier of the two: an
/// all-lowercase, digit-free connection string is a genuine credential that the
/// floor alone would drop. `is_url_credential_shaped` is what keeps it, and the
/// lowercase connection strings below are what prove that exemption carries its
/// weight rather than being decorative.
///
/// Known and deliberate: `wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY` (a
/// canonical AWS *secret access key*) is NOT caught — `is_structural_token`'s
/// slash-path branch swallows it before either gate runs. That miss is
/// pre-existing on `origin/main`, unchanged by #4312, and out of scope here; it
/// is asserted so the baseline is explicit rather than assumed.
/// What: asserts every provider-prefix, base64, base64url, JWT, bare-blob and
/// connection-string shape stays flagged after the narrowing.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_4312_charset_gate() {
    for (label, tok) in [
        ("GitHub PAT", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"), // pragma: allowlist secret
        ("OpenAI key", "sk-abcdefghijklmnopqrstuvwxyz01234567890123"), // pragma: allowlist secret
        ("AWS key id", "AKIAIOSFODNN7EXAMPLE"),                     // pragma: allowlist secret
        ("AWS STS id", "ASIAY34FZKBOKMUTVV7A"),                     // pragma: allowlist secret
        ("Slack token", "xoxb-1234-5678-abcdEFGH"),                 // pragma: allowlist secret
        (
            "padded base64",
            "dGhpcyBpcyBhIHZlcnkgbG9uZyBzZWNyZXQgdG9rZW4=",
        ), // pragma: allowlist secret
        (
            "base64 with +/",
            "aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NA==",
        ), // pragma: allowlist secret
        ("base64 with +", "A+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DI"), // pragma: allowlist secret
        ("bare mixed-case blob", "AbCd1234EfGh5678IjKl9012"),       // pragma: allowlist secret
        ("base64url token", "AbCd1234EfGh5678IjKl9012_MnOp-QrSt="), // pragma: allowlist secret
        ("digits/slash/plus", "1234/5678/9012+3456/7890/1234"),     // pragma: allowlist secret
        (
            "JWT",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ), // pragma: allowlist secret
        (
            "postgres conn",
            "postgres://user:pass@host/dbname12345678901234567890",
        ), // pragma: allowlist secret
        (
            "hyphenated host conn",
            "postgres://user:pass@db-primary.internal/appdb1234567890",
        ), // pragma: allowlist secret
        (
            "basic-auth URL",
            "https://admin:Sup3rS3cret@internal.example.com/api",
        ), // pragma: allowlist secret
        ("mysql conn", "mysql://root:s3cr3tP4ss@127.0.0.1:3306/appdb"), // pragma: allowlist secret
        // LOAD-BEARING for the `is_url_credential_shaped` exemption: these
        // carry no uppercase and no digit, so the entropy floor alone would
        // drop every one of them.
        (
            "lowercase postgres conn",
            "postgres://user:password@host/database",
        ), // pragma: allowlist secret
        (
            "lowercase redis conn",
            "redis://default:supersecretpass@cache.local/one",
        ), // pragma: allowlist secret
        (
            "lowercase mongo srv",
            "mongodb+srv://svcuser:hunterhunter@cluster.mongodb.net",
        ), // pragma: allowlist secret
        (
            "lowercase ftp conn",
            "ftp://deployer:deploypassword@files.internal/pub",
        ), // pragma: allowlist secret
        (
            "lowercase mysql conn",
            "mysql://admin:letmein@localhost/mydb",
        ), // pragma: allowlist secret
        // LOAD-BEARING for the narrowed `is_url_credential_shaped` (#4312
        // round 3): a userinfo-free URL whose PATH carries the secret. This is
        // why "a bare URL is not a credential" cannot be a blanket exemption —
        // it would lose every webhook secret. Caught here by the entropy floor
        // (uppercase + digits), not by the URL predicate. Host is deliberately
        // generic: the real vendor form trips GitHub push protection, which is
        // itself a second opinion that this shape reads as a credential.
        (
            "webhook URL with secret path",
            "https://webhook.example.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX",
        ), // pragma: allowlist secret
    ] {
        assert!(
            find_secret_token(tok).is_some(),
            "{label} must STILL be flagged after the #4312 narrowing: {tok}"
        );
    }

    // Pre-existing baseline miss, asserted so it is explicit. NOT caused by
    // #4312 — `origin/main` misses it identically via the slash-path bypass in
    // `is_structural_token`, which runs before either new gate.
    let aws_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"; // pragma: allowlist secret
    assert!(
        find_secret_token(aws_secret).is_none(),
        "baseline drift: this AWS secret-key miss is pre-existing on origin/main. \
         If it now flags, the structural bypass changed — re-check scope. Got {:?}",
        find_secret_token(aws_secret)
    );

    // KNOWN MISS INTRODUCED BY #4312, asserted rather than left implicit.
    //
    // Narrowing `is_url_credential_shaped` to require `user:pass@` removed a
    // shield that the round-2 broad predicate had incidentally given to
    // userinfo-free URLs. The class that moved is exactly: a URL whose PATH
    // carries a secret that is all-lowercase and digit-free, so the entropy
    // floor reads it as English. Measured against origin/main, where both
    // flag:
    //     https://webhook.example.com/services/abcdefghij/…   flagged -> missed
    //
    // This is the entropy floor's already-accepted bound, not a new class of
    // hole, and the exposure is narrow: real webhook tokens are near-universally
    // mixed-case or digit-bearing, which is why the uppercase form above is
    // still caught in the must-flag battery. Pinned here so a future change that
    // fixes OR worsens it is visible instead of silent. If these start flagging,
    // the follow-up landed — move them into the battery above.
    for missed in [
        "https://webhook.example.com/services/abcdefghij/klmnopqrst/uvwxyzabcdefghij", // pragma: allowlist secret
        "https://hooks.example.org/t/aaaaaaaaaa/bbbbbbbbbb/cccccccccc", // pragma: allowlist secret
    ] {
        assert!(
            find_secret_token(missed).is_none(),
            "KNOWN MISS (#4312): an all-lowercase, digit-free URL path secret is \
             not flagged after the userinfo narrowing. If it now flags, the bound \
             changed — update the doc on `is_url_credential_shaped`. Token: {missed}"
        );
    }
}

// ---- Issue #4739: the mixed-case branch, capitalised identifier segments ----

/// Why (issue #4739, the FIFTH recurrence in this detector after #1667, #2800,
/// #4216 and #4312): `Agents.app.bak-20260729-000028` — an ordinary macOS app
/// bundle backup — was rejected as `Agen…(30 chars)`. It carries no `+`, `/` or
/// `=`, so the base64 branch #4312 narrowed never runs; it reaches the mixed-case
/// branch and `is_structural_token`'s segmented-identifier branch declines to
/// rescue it, because that branch split on `-` alone and admitted only
/// all-lowercase and ALL-UPPERCASE segments. A leading capital — the single most
/// ordinary thing about an English word or an app bundle name — read as internal
/// mixed case.
///
/// What this pins: the reproduction verbatim, plus fourteen further shapes of the
/// same class (release artifacts, screenshots, plist backups, snapshots, branch
/// and milestone labels) that reproduce the identical defect with different
/// delimiters. Each is asserted at BOTH levels — the token predicate and the full
/// `FilterConfig::apply` gate — because the gate is what a memory write actually
/// hits.
/// Test: itself.
#[test]
fn dotted_capitalised_filenames_are_not_flagged() {
    let cfg = FilterConfig::default();
    for (label, tok) in [
        // The #4739 reproduction, verbatim from the issue body.
        ("4739 repro", "Agents.app.bak-20260729-000028"),
        (
            "4739 repro, full line",
            "/Applications/Trusty Agents.app.bak-20260729-000028",
        ),
        ("release artifact", "Trusty-Agents-v1.3.5-darwin-arm64"),
        ("macOS screenshot", "Screenshot-2026-08-04-at-10.31.22.png"),
        ("lockfile backup", "Cargo.lock-backup-20260803-091500"),
        ("draft doc", "README-Draft-20260804-final.md"),
        ("instructions backup", "INSTRUCTIONS.md.bak-20260731-000028"),
        ("session snapshot", "Session-Snapshot-20260804-093000.json"),
        (
            "reverse-dns plist backup",
            "com.apple.Finder.plist.bak-20260101",
        ),
        ("app with build number", "Xcode.app-15.4.0-build-2026"),
        (
            "double-extension backup",
            "Agents.app.bak-20260729-000028.zip",
        ),
        ("milestone label", "Milestone-1.3.5-Release-Candidate"),
        ("person-dated note", "Bob.Matsuoka-20260804-notes.md"),
        // Underscore is an identifier delimiter too (#4739): these were false
        // positives on origin/main for the same reason, one delimiter over.
        ("snake_case capitalised", "Trusty_Agents_20260804"),
        ("dotted snake backup", "Session_Log.2026.08.04.txt"),
    ] {
        assert!(
            find_secret_token(tok).is_none(),
            "#4739 ({label}): a capitalised, delimiter-segmented filename must \
             NOT be flagged: {tok}; got {:?}",
            find_secret_token(tok)
        );
        let prose = format!("Checkpoint: rolled back to {tok} after the bounce");
        assert!(
            cfg.apply(&prose, true).is_ok(),
            "#4739 ({label}): the gate must ACCEPT prose carrying {tok}; got {:?}",
            cfg.apply(&prose, true)
        );
    }
}

/// Why (issue #4739): widening `is_human_word_segment` widens
/// `is_structural_token`, and `is_structural_token` returning `true` makes
/// `looks_like_secret` return `false` — so this change NARROWS flagging and can
/// only lose detection, never add it. #4723 shipped a doc claiming the opposite
/// direction for a neighbouring predicate and a measurement disproved it, so the
/// narrowing is pinned here against every credential family the mixed-case branch
/// is responsible for.
///
/// The load-bearing cases are the delimiter-segmented MIXED-CASE blobs. A
/// careless widening — anything that admits a segment with two uppercase letters
/// — loses every one of them, because run-of-two case alternation (`AbCd`,
/// `EfGh`) is exactly the signature of an encoded blob. The predicate admits at
/// most ONE uppercase letter in a non-uppercase segment, and only as its first
/// letter, which is what keeps these caught.
/// What: asserts the mixed-case credential families stay flagged, then pins the
/// CamelCase residue that this change deliberately did NOT fix.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_4739_capitalised_segments() {
    for (label, tok) in [
        // LOAD-BEARING: delimiter-segmented mixed-case blobs. If the segment
        // predicate ever admits two uppercase letters, every one of these is lost.
        ("hyphenated mixed-case cred", "AbCd1234-EfGh5678-IjKl9012"), // pragma: allowlist secret
        ("dotted mixed-case cred", "AbCd1234.EfGh5678.IjKl9012"),     // pragma: allowlist secret
        ("underscored mixed-case cred", "AbCd1234_EfGh5678_IjKl9012"), // pragma: allowlist secret
        ("camelCase blob segments", "xYzAbc123-qRsTuv456-mNoPqr789"), // pragma: allowlist secret
        ("slug-prefixed api key", "apikey-Xy7Kp2Qm9Rt4Vw8Nz3Bc6Fj"),  // pragma: allowlist secret
        ("capitalised head, blob tail", "Prod-AbCd1234EfGh5678IjKl"), // pragma: allowlist secret
        (
            "dotted capitalised head, blob tail",
            "Prod.Key.AbCd1234EfGh5678",
        ), // pragma: allowlist secret
        // Unchanged families, re-asserted because the widened predicate sits on
        // the path every one of them takes.
        ("bare mixed-case blob", "AbCd1234EfGh5678IjKl9012"), // pragma: allowlist secret
        ("base64url token", "AbCd1234EfGh5678IjKl9012_MnOp-QrSt="), // pragma: allowlist secret
        ("colon-bearing credential", "token:aBc123XyZ987uvW456QrS"), // pragma: allowlist secret
        (
            "JWT",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ), // pragma: allowlist secret
        ("GitHub PAT", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"), // pragma: allowlist secret
        ("AWS key id", "AKIAIOSFODNN7EXAMPLE"),               // pragma: allowlist secret
        (
            "padded base64",
            "dGhpcyBpcyBhIHZlcnkgbG9uZyBzZWNyZXQgdG9rZW4=",
        ), // pragma: allowlist secret
        (
            "basic-auth URL",
            "https://admin:Sup3rS3cret@internal.example.com/api",
        ), // pragma: allowlist secret
        (
            "lowercase postgres conn",
            "postgres://user:password@host/database",
        ), // pragma: allowlist secret
    ] {
        assert!(
            find_secret_token(tok).is_some(),
            "{label} must STILL be flagged after the #4739 widening: {tok}"
        );
    }

    // KNOWN RESIDUE, deliberately not fixed by #4739, asserted rather than left
    // implicit. A CamelCase segment has TWO capitalised runs, so it is not a word
    // under `is_human_word_segment` and these stay flagged. Admitting CamelCase
    // would mean admitting multi-uppercase segments, which is the exact signature
    // of `AbCdEfGhIjKlMnOpQrSt-1234` — i.e. it would buy two prose cases at the
    // cost of a credential miss. Measured at both revisions: 17 -> 2 prose false
    // positives with 0 -> 0 credential misses. If these start passing, the
    // follow-up landed — move them into
    // `dotted_capitalised_filenames_are_not_flagged`.
    for residue in [
        "TrustyMemory.app.bak-20260801-120000",
        "MyDocument.Backup.2026.tar.gz",
    ] {
        assert!(
            find_secret_token(residue).is_some(),
            "KNOWN RESIDUE (#4739): CamelCase segments are still not words, so \
             this is still flagged. If it now passes, update the bound stated on \
             `is_human_word_segment`. Token: {residue}"
        );
    }
}

/// Why (issue #4739, review round 1): the battery above probes the REJECTED
/// side of `is_human_word_segment`'s bound — shapes that must stay flagged.
/// Nothing probed the ACCEPTED side at token scale, which left the widening
/// reading as a stronger guarantee than it gives. What `is_segmented_identifier`
/// actually admits is a token whose every segment is independently case-uniform,
/// and at short segment lengths that includes shapes which are not human words
/// at all. These moved FLAG -> pass in this PR; they are neither prose nor
/// credentials the module is expected to catch, and they are accepted (see the
/// bound stated on `is_human_word_segment`) rather than fixed.
///
/// This block is modelled on the #4312 known-miss pin it sits beside: that one
/// is the reason the URL-path misses stayed visible across two PRs instead of
/// being rediscovered. The accepted side of this bound gets the same treatment,
/// so any future movement — in either direction — is caught rather than found.
/// What: pins two of the admitted shapes, plus the corrected doc example.
/// Test: itself.
#[test]
fn per_segment_case_uniformity_is_a_known_accepted_bound() {
    for (label, tok) in [
        (
            "capitalised 3-char groups",
            "Xy7-Kp2-Qm9-Rt4-Vw8-Nz3-Bc6-Fj0",
        ),
        ("capitalised 4-char groups", "A1b2-C3d4-E5f6-G7h8-I9j0-K1l2"),
        // The corrected doc example. The earlier revision cited
        // `Abcd-1234-Efgh-5678`, which at 19 chars is below the 20-char floor in
        // `looks_like_secret` and so never reaches this branch at all — it could
        // not illustrate the bound it was cited for. This form does.
        ("corrected doc example", "Abcd-1234-Efgh-5678-Ijkl"),
    ] {
        assert!(
            tok.len() >= 20,
            "{label}: this pin is meaningless below the 20-char secret floor — \
             the token would never reach the segmented-identifier branch. \
             Token: {tok} ({} chars)",
            tok.len()
        );
        assert!(
            find_secret_token(tok).is_none(),
            "KNOWN ACCEPTED BOUND (#4739, {label}): every segment is \
             independently case-uniform, so this is admitted even though it is \
             not a human-readable identifier. If it now FLAGS, the bound \
             tightened — update the doc on `is_human_word_segment`. Token: {tok}"
        );
    }
}

// ---- Issue #4898: plus-joined phrases, internal-caps segments, length floor ----

/// Why (issue #4898, the SIXTH recurrence in this detector after #1667, #2800,
/// #4216, #4312 and #4739): three separate shapes of ordinary memory-palace
/// content were rejected as credentials in one session.
///
/// 1. `PM+instructions+subagents` — an English phrase joined by `+`.
///    `is_structural_token` refused every `+`-bearing token outright, so the
///    token fell to the base64 branch, where the `has_upper || has_digit`
///    entropy floor was satisfied by the two capitals in `PM`. The doc on
///    `is_plausible_b64_charset` already named this gap: the floor excluded only
///    the all-lowercase case.
/// 2. `fix-3696-slice1-gapA-emit` — a git branch name. `is_human_word_segment`
///    admitted a capital only in first position, so the segment `gapA` was not a
///    word and the whole token missed the structural rescue.
/// 3. A four-character token equal to a `SECRET_PREFIXES` entry (`Asia`, the
///    continent). The prefix test ran BEFORE the 20-char floor, so length never
///    entered into it.
///
/// What: pins all three reproductions verbatim, at both the token predicate and
/// the `FilterConfig::apply` gate a memory write actually hits, plus further
/// shapes of each class.
/// Test: itself.
#[test]
fn three_4898_reproductions_are_not_flagged() {
    let cfg = FilterConfig::default();
    for (label, tok) in [
        // (1) plus-joined English phrase carrying an uppercase run.
        ("4898 repro 1", "PM+instructions+subagents"),
        ("plus phrase, lowercase", "ticker+shutdown-channel"),
        ("plus phrase, capitalised", "Instructions+Agents+Skills"),
        ("plus phrase with digits", "milestone+1.3.5+release"),
        // (2) branch name with an internal capital inside one segment.
        ("4898 repro 2", "fix-3696-slice1-gapA-emit"),
        ("branch with trailing cap", "feat-4172-l0-scoping-partB"),
        ("issue slug with inner cap", "docs-4898-secretScanner-notes"),
        ("snake segment with cap", "trusty_mpm_phase2B_rollout"),
        // (3) short tokens that merely start with a credential prefix.
        ("4898 repro 3", "Asia"),
        ("prefix word, uppercase", "ASIA"),
        ("prefix word, hyphenated", "Asia-Pacific"),
        ("prefix word in a slug", "asia-pacific-rollout-notes"),
        ("truncated sk prefix", "sk-1"),
    ] {
        assert!(
            find_secret_token(tok).is_none(),
            "#4898 ({label}): ordinary content must NOT be flagged: {tok}; got {:?}",
            find_secret_token(tok)
        );
        let prose = format!("Checkpoint: the {tok} work landed on main this morning");
        assert!(
            cfg.apply(&prose, true).is_ok(),
            "#4898 ({label}): the gate must ACCEPT prose carrying {tok}; got {:?}",
            cfg.apply(&prose, true)
        );
    }
}

/// Why (issue #4898): every one of the three changes NARROWS flagging —
/// widening `is_human_word_segment` and `is_structural_token` makes
/// `looks_like_secret` return `false` sooner, and moving the length floor above
/// the prefix test removes matches outright. A narrowing can only lose
/// detection, so the true-positive corpus is re-asserted in full against the
/// post-change code rather than assumed intact.
///
/// The load-bearing entries are the `+`-bearing blobs and the delimiter-
/// segmented mixed-case blobs. `A+DIA+DIA+…` is what pins the word-length
/// requirement inside `is_plus_joined_word_phrase`: every one of its segments is
/// case-uniform, so segment shape ALONE would have exempted it.
/// What: asserts each provider-prefix, base64, base64url, JWT, bare-blob,
/// connection-string and AWS shape stays flagged.
/// Test: itself.
#[test]
fn real_secrets_still_blocked_after_4898_narrowing() {
    for (label, tok) in [
        // LOAD-BEARING for `is_plus_joined_word_phrase`'s word-length floor.
        ("base64 with +", "A+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DI"), // pragma: allowlist secret
        // LOAD-BEARING for its alphanumeric-segment charset gate: the `token=A`
        // first segment clears the word-length floor on its own, and the
        // `srv://svcuser:hunterhunter@cluster` segment is uniformly lowercase.
        // Both passed before the charset gate was added.
        (
            "key= base64 with +",
            "token=A+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DIA+DI",
        ), // pragma: allowlist secret
        (
            "base64 with +/",
            "aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NA==",
        ), // pragma: allowlist secret
        ("digits/slash/plus", "1234/5678/9012+3456/7890/1234"), // pragma: allowlist secret
        // LOAD-BEARING for the one-uppercase-anywhere segment predicate: run-of-
        // two case alternation carries TWO capitals per segment and stays out.
        ("hyphenated mixed-case cred", "AbCd1234-EfGh5678-IjKl9012"), // pragma: allowlist secret
        ("dotted mixed-case cred", "AbCd1234.EfGh5678.IjKl9012"),     // pragma: allowlist secret
        ("underscored mixed-case cred", "AbCd1234_EfGh5678_IjKl9012"), // pragma: allowlist secret
        ("camelCase blob segments", "xYzAbc123-qRsTuv456-mNoPqr789"), // pragma: allowlist secret
        ("slug-prefixed api key", "apikey-Xy7Kp2Qm9Rt4Vw8Nz3Bc6Fj"),  // pragma: allowlist secret
        ("capitalised head, blob tail", "Prod-AbCd1234EfGh5678IjKl"), // pragma: allowlist secret
        // LOAD-BEARING for the prefix length floor and the AWS shape gate.
        ("AWS key id", "AKIAIOSFODNN7EXAMPLE"), // pragma: allowlist secret
        ("AWS STS id", "ASIAY34FZKBOKMUTVV7A"), // pragma: allowlist secret
        ("GitHub PAT", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"), // pragma: allowlist secret
        ("OpenAI key", "sk-abcdefghijklmnopqrstuvwxyz01234567890123"), // pragma: allowlist secret
        ("OpenAI key, short form", "sk-abcdef0123456789abcdef01"), // pragma: allowlist secret
        ("Slack token", "xoxb-1234-5678-abcdEFGH"), // pragma: allowlist secret
        // Unchanged families, re-asserted because the changed predicates all sit
        // on the path these take.
        ("bare mixed-case blob", "AbCd1234EfGh5678IjKl9012"), // pragma: allowlist secret
        ("base64url token", "AbCd1234EfGh5678IjKl9012_MnOp-QrSt="), // pragma: allowlist secret
        ("colon-bearing credential", "token:aBc123XyZ987uvW456QrS"), // pragma: allowlist secret
        (
            "padded base64",
            "dGhpcyBpcyBhIHZlcnkgbG9uZyBzZWNyZXQgdG9rZW4=",
        ), // pragma: allowlist secret
        (
            "JWT",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ), // pragma: allowlist secret
        (
            "basic-auth URL",
            "https://admin:Sup3rS3cret@internal.example.com/api",
        ), // pragma: allowlist secret
        (
            "lowercase postgres conn",
            "postgres://user:password@host/database",
        ), // pragma: allowlist secret
        (
            "lowercase mongo srv",
            "mongodb+srv://svcuser:hunterhunter@cluster.mongodb.net",
        ), // pragma: allowlist secret
        (
            "webhook URL with secret path",
            "https://webhook.example.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX",
        ), // pragma: allowlist secret
    ] {
        assert!(
            find_secret_token(tok).is_some(),
            "{label} must STILL be flagged after the #4898 narrowing: {tok}"
        );
    }

    // End-to-end, not just the token predicate: the gate is what a write hits.
    let cfg = FilterConfig::default();
    for content in [
        "Deploy creds leaked: access key AKIAIOSFODNN7EXAMPLE in the log", // pragma: allowlist secret
        "Use this token AbCd1234EfGh5678IjKl9012 to authenticate the webhook", // pragma: allowlist secret
        "config blob: aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NA== embedded here", // pragma: allowlist secret
    ] {
        assert!(
            matches!(
                cfg.apply(content, false),
                Err(FilterReject::PotentialSecret { .. })
            ),
            "the gate must still REJECT leaked-credential prose; got {:?}",
            cfg.apply(content, false)
        );
    }
}

/// Why (issue #4898): the `+` exemption is gated on segment shape AND on one
/// segment reaching word length, and the prefix floor is a flat 20 characters.
/// Both bounds admit shapes that are not prose. They are accepted rather than
/// fixed, and pinned here so future movement in either direction is visible —
/// the same treatment the #4312 and #4739 bounds already get above.
/// What: pins the accepted side of each new bound.
/// Test: itself.
#[test]
fn known_accepted_bounds_after_4898() {
    // A `+`-joined token whose every segment is case-uniform AND long enough to
    // read as a word is admitted, even though no English dictionary is
    // consulted. Base64 does not produce this: its `+` density is ~1.6%, so its
    // segments run ~60 chars and cannot be case-uniform.
    for tok in ["Abcdefgh+Ijklmnop+Qrstuvwx", "alphabet+bravocode+charlie"] {
        assert!(
            find_secret_token(tok).is_none(),
            "KNOWN ACCEPTED BOUND (#4898): a `+`-joined token of case-uniform \
             word-length segments is admitted. If it now FLAGS, the bound \
             tightened — update the doc on `is_plus_joined_word_phrase`. \
             Token: {tok}"
        );
    }
    // The prefix floor is length-only for the punctuation-bearing prefixes, so a
    // 20+-char token that merely STARTS with one is still flagged. That is the
    // pre-#4898 behaviour for every length at or above the floor, unchanged.
    assert!(
        find_secret_token("sk-eleton-key-for-the-front-door").is_some(),
        "KNOWN ACCEPTED BOUND (#4898): the prefix test is length-gated, not \
         shape-gated, for `sk-`/`ghp_`/`xoxb-`. A long prose token that starts \
         with one is still flagged."
    );
    // The AWS prefixes ARE shape-gated (all-uppercase alphanumeric), which is
    // what removes the `Asia…` prose class entirely rather than by length alone.
    assert!(
        find_secret_token("ASIA-PACIFIC-ROLLOUT-NOTES").is_none(),
        "#4898: an all-uppercase but punctuation-bearing `ASIA…` token is not \
         an AWS key id shape and must not be flagged"
    );
}
