//! Unit tests for [`super`] (`tm hook --divert-check`, #6887).
//!
//! Why: split out of `divert_check.rs` so the production module stays well
//! under the 500-SLOC cap, mirroring the `project_hooks.rs`/`project_hooks_tests.rs`
//! split.
//! What: covers the classifier ([`super::divert_targets`]), the credential
//! predicate ([`super::worker_credentials_present`]), and — the pair the design
//! calls out as BLOCKING — the two fail-open branches of [`super::decide`].
//!
//! The two fail-open tests are written so that an implementation which blocks
//! on SIZE ALONE fails them: `divert_check_allows_when_no_worker_credentials`
//! feeds an oversized file with `worker_available = false` and demands `Allow`,
//! which a size-only implementation answers with `Block`.
//! Test: this module IS the test suite for `super`.

use super::*;

/// A `line_count` stub returning the same count for every path.
fn fixed(n: u32) -> impl Fn(&str) -> Option<u32> {
    move |_| Some(n)
}

/// A registry with every credential present.
fn full_registry() -> ProviderRegistry {
    ProviderRegistry {
        anthropic_api_key: Some("sk-test".to_string()),
        aws_credentials_available: true,
        openrouter_api_key: Some("or-test".to_string()),
    }
}

fn read_input(path: &str) -> Value {
    serde_json::json!({ "file_path": path })
}

// ─── §9 test 6: fail-open with no worker credential ────────────────────────────

/// Why (#6887, BLOCKING): the hook must never deny a read AND deny the
/// replacement. With no credential anywhere, `tm divert bulk-read` cannot run,
/// so blocking would strand the agent with no next move.
/// What: an oversized file (900 lines vs a 350 threshold) with
/// `worker_available = false` must ALLOW. An implementation that blocks on size
/// alone returns `Block` here and fails.
#[test]
fn divert_check_allows_when_no_worker_credentials() {
    let decision = decide(
        "Read",
        Some(&read_input("/repo/huge.rs")),
        350,
        false,
        &fixed(900),
    );
    assert_eq!(
        decision,
        DivertDecision::Allow,
        "an oversized read with no reachable worker must fail OPEN"
    );

    // The same must hold for the Bash route, which reaches the same bytes.
    let bash = serde_json::json!({ "command": "cat /repo/huge.rs" });
    assert_eq!(
        decide("Bash", Some(&bash), 350, false, &fixed(900)),
        DivertDecision::Allow
    );
}

/// Why: [`worker_credentials_present`] is what test 6 depends on, so its own
/// truth table needs asserting rather than inferring.
/// What: `auto` accepts any single credential; an explicit provider accepts
/// only its own; a fully-empty registry is false for every selector.
#[test]
fn worker_credentials_present_requires_a_matching_credential() {
    let empty = ProviderRegistry::default();
    for provider in ["auto", "anthropic", "openrouter", "bedrock", "nonsense"] {
        assert!(
            !worker_credentials_present(&empty, provider),
            "an empty registry must report no worker for {provider}"
        );
    }

    let anthropic_only = ProviderRegistry {
        anthropic_api_key: Some("sk-test".to_string()),
        ..ProviderRegistry::default()
    };
    assert!(worker_credentials_present(&anthropic_only, "auto"));
    assert!(worker_credentials_present(&anthropic_only, "anthropic"));
    assert!(
        !worker_credentials_present(&anthropic_only, "openrouter"),
        "an explicit provider must not borrow another provider's credential"
    );

    let openrouter_only = ProviderRegistry {
        openrouter_api_key: Some("or-test".to_string()),
        ..ProviderRegistry::default()
    };
    assert!(worker_credentials_present(&openrouter_only, "auto"));
    assert!(worker_credentials_present(&openrouter_only, "openrouter"));

    // An unrecognised selector falls back to `auto`, not to "no worker".
    assert!(worker_credentials_present(&full_registry(), "NoNsEnSe"));
}

/// Why (#6887): `aws_credentials_available` says a credential exists, not that
/// this build can use it. Without the `bedrock` feature `construct_bedrock`
/// returns a config error, so counting AWS as a usable worker would produce
/// exactly the dead-end block the fail-open rule forbids.
/// What: an AWS-only registry reports a worker ONLY in a `--features bedrock`
/// build; the assertion is written against `cfg!` so it holds in both.
#[test]
fn worker_credentials_present_ignores_aws_without_the_bedrock_feature() {
    let aws_only = ProviderRegistry {
        aws_credentials_available: true,
        ..ProviderRegistry::default()
    };
    let expected = cfg!(feature = "bedrock");
    assert_eq!(worker_credentials_present(&aws_only, "bedrock"), expected);
    assert_eq!(worker_credentials_present(&aws_only, "auto"), expected);
}

// ─── §9 test 7: threshold behaviour when a worker IS available ─────────────────

/// Why (#6887): the positive case, and the exact boundary. A threshold applied
/// with the wrong comparison (`>` instead of `>=`) lets a file of exactly
/// `min_lines` through, which is the off-by-one this pins.
/// What: over threshold + no `offset`/`limit` + a worker → `Block`; under
/// threshold → `Allow`; exactly 350 → `Block`; 349 → `Allow`. The block reason
/// must name the replacement command.
#[test]
fn divert_check_blocks_when_worker_available_and_over_threshold() {
    let input = read_input("/repo/huge.rs");

    let over = decide("Read", Some(&input), 350, true, &fixed(900));
    let DivertDecision::Block(reason) = over else {
        panic!("an oversized read with a worker available must block, got {over:?}");
    };
    assert!(
        reason.contains("tm divert bulk-read"),
        "the reason must name the replacement command: {reason}"
    );

    assert_eq!(
        decide("Read", Some(&input), 350, true, &fixed(120)),
        DivertDecision::Allow,
        "a file under the threshold must not be diverted"
    );

    // Exact boundary: `min_lines` itself is over the line, one less is not.
    assert!(
        matches!(
            decide("Read", Some(&input), 350, true, &fixed(350)),
            DivertDecision::Block(_)
        ),
        "exactly min_lines must divert (the comparison is >=)"
    );
    assert_eq!(
        decide("Read", Some(&input), 350, true, &fixed(349)),
        DivertDecision::Allow,
        "one line under min_lines must not divert"
    );
    assert!(matches!(
        decide("Read", Some(&input), 350, true, &fixed(351)),
        DivertDecision::Block(_)
    ));
}

/// Why (#6887): `offset`/`limit` is the documented escape hatch the block
/// reason itself points the agent at. If a bounded re-read were diverted too,
/// the recovery path would loop forever.
/// What: an oversized `Read` carrying `offset`, `limit`, or both must ALLOW
/// even with a worker available; so must a `head -n`-bounded Bash read.
#[test]
fn divert_check_allows_a_bounded_read() {
    for extra in [
        serde_json::json!({"file_path": "/repo/huge.rs", "offset": 100}),
        serde_json::json!({"file_path": "/repo/huge.rs", "limit": 50}),
        serde_json::json!({"file_path": "/repo/huge.rs", "offset": 1, "limit": 50}),
    ] {
        assert_eq!(
            decide("Read", Some(&extra), 350, true, &fixed(900)),
            DivertDecision::Allow,
            "a bounded read must never be diverted: {extra}"
        );
    }

    for command in ["head -n 40 /repo/huge.rs", "tail -100 /repo/huge.rs"] {
        let bash = serde_json::json!({ "command": command });
        assert_eq!(
            decide("Bash", Some(&bash), 350, true, &fixed(900)),
            DivertDecision::Allow,
            "an already-bounded shell read must not be diverted: {command}"
        );
    }
}

// ─── classifier ───────────────────────────────────────────────────────────────

/// Why: `cat`/`head`/`tail`/`less`/`more` reach the same bytes `Read` would, so
/// leaving them uncovered makes the whole feature one keystroke to bypass.
/// What: each bare and path-qualified reader yields its file operand.
#[test]
fn divert_targets_matches_bulk_bash_readers() {
    for cmd in [
        "cat src/lib.rs",
        "/bin/cat src/lib.rs",
        "less src/lib.rs",
        "more src/lib.rs",
        "head src/lib.rs",
        "tail src/lib.rs",
    ] {
        let input = serde_json::json!({ "command": cmd });
        assert_eq!(
            divert_targets("Bash", Some(&input)),
            vec!["src/lib.rs".to_string()],
            "expected {cmd} to name one target"
        );
    }

    // Quoted operands are unwrapped; several operands all count.
    let input = serde_json::json!({ "command": "cat \"a.rs\" 'b.rs'" });
    assert_eq!(
        divert_targets("Bash", Some(&input)),
        vec!["a.rs".to_string(), "b.rs".to_string()]
    );
}

/// Why: a command the classifier cannot lex confidently must be left alone —
/// a false positive here blocks a call the agent legitimately needs, and a
/// composed pipeline is not a plain bulk read.
/// What: pipes, redirects, separators, and command substitution all yield no
/// targets, as does a non-reader command.
#[test]
fn divert_targets_skips_bounded_reads() {
    for cmd in [
        "cat src/lib.rs | head -20",
        "cat src/lib.rs > /tmp/out",
        "cat src/lib.rs; echo done",
        "cat $(ls)",
        "grep foo src/lib.rs",
        "rm -rf src",
        "cat",
    ] {
        let input = serde_json::json!({ "command": cmd });
        assert!(
            divert_targets("Bash", Some(&input)).is_empty(),
            "expected {cmd} to yield no divert targets"
        );
    }
}

/// Why: the hook is registered with matcher `Read` and matcher `Bash`, but
/// Claude Code matchers are regex — a future matcher change must not silently
/// start diverting edits. Scope is bulk READS only (#6887).
/// What: `Edit`, `Write`, `Grep`, and a missing `tool_input` all yield nothing.
#[test]
fn divert_targets_ignores_other_tools() {
    let input = read_input("/repo/huge.rs");
    for tool in ["Edit", "Write", "MultiEdit", "Grep", "Glob", "Task"] {
        assert!(
            divert_targets(tool, Some(&input)).is_empty(),
            "{tool} must never be diverted"
        );
    }
    assert!(divert_targets("Read", None).is_empty());
    assert!(
        divert_targets("Read", Some(&serde_json::json!({}))).is_empty(),
        "a Read with no file_path must yield nothing"
    );
}

/// Why: the block reason is the agent's ONLY view of what to do next, so its
/// two load-bearing strings are pinned.
/// What: the reason names the command and the fall-through signal.
#[test]
fn block_reason_names_the_worker_command() {
    let reason = block_reason("/repo/huge.rs", 900);
    assert!(reason.contains("tm divert bulk-read /repo/huge.rs"));
    assert!(reason.contains("divert: fall-through"));
    assert!(reason.contains("offset"));
    assert!(reason.contains("900"));
}

/// Why: an unreadable path must ALLOW rather than block — a missing file is
/// the tool call's problem to report, not the hook's to pre-empt.
/// What: a real temp file's lines are counted; a missing path yields `None`,
/// and `decide` then allows.
#[test]
fn count_lines_reads_a_real_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "a\nb\nc\n").expect("write");
    assert_eq!(count_lines(&path), Some(3));
    assert_eq!(count_lines(&dir.path().join("absent.txt")), None);

    // An unreadable target allows, even oversized-by-configuration.
    assert_eq!(
        decide("Read", Some(&read_input("/repo/x.rs")), 1, true, &|_| None),
        DivertDecision::Allow
    );
}
