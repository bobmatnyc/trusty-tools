//! The secret gate accepts ordinary paths and URLs and still refuses a secret
//! carried inside one (issue #9748, the paths-and-URLs half).
//!
//! Why: `memory_remember` refused bodies holding a file reference with a
//! `:line` suffix, such as `architect/.claude/skills/instance-evo/SKILL.md:38`,
//! while a URL carrying a token in its query string or fragment was admitted.
//! What: drives the gate (`check_secret`, `FilterConfig::apply`,
//! `find_secret_spans`) only. Every fixture is synthetic.
//! Test: this is the test file.

use super::{check_secret, find_secret_spans};
use crate::engine::filter::{FilterConfig, FilterReject};

/// Synthetic 64-hex AWS SigV4 signature.
const AMZ_SIG: &str = "3f1c0a9b8e7d6c5b4a39281706f5e4d3c2b1a0f9e8d7c6b5a493827160f5e4d3"; // pragma: allowlist secret

/// True when every gate path refuses `content`.
fn refused(content: &str) -> bool {
    check_secret(content).is_err()
        && FilterConfig::default().apply(content, false).is_err()
        && !find_secret_spans(content).is_empty()
}

/// Why: the acceptance half — ordinary file references and URLs are stored,
/// bare and in prose. The first row is the live refusal the coordinator
/// reported on 2026-10-26.
/// What: asserts `check_secret` and `find_secret_spans` admit each shape and
/// `apply` raises no secret refusal for it. Rows that a `:line` suffix used
/// to sink are red before the fix.
/// Test: itself; red on origin/main, which refuses the `:line` rows.
#[test]
fn ordinary_paths_and_urls_are_accepted_after_9748() {
    let blob = "https://github.com/bobmatnyc/trusty-tools/blob/\
                5eaad1934c0e3f1d2a4b6c8e9f0a1b2c3d4e5f60/crates/trusty-memory/src/engine/filter/secret.rs";
    let blob_anchor = format!("{blob}#L1446-L1562");
    for shape in [
        "architect/.claude/skills/instance-evo/SKILL.md:38",
        "/Users/x/proj/src/lib.rs",
        "crates/trusty-memory/src/x.rs:42",
        "https://github.com/bobmatnyc/trusty-tools/issues/9748",
        blob,
        &blob_anchor,
        "crates/trusty-memory/CLAUDE.md:12:5",
        "docs/reference/README.md:120",
        "file:///Users/masa/proj/SKILL.md:38",
        "ImplementationPlan.md:120",
        "https://github.com/bobmatnyc/trusty-tools/issues?q=is%3Aopen+label%3Abug",
        "https://docs.example.com/guide/setup?section=install&page=2#prerequisites",
    ] {
        for content in [
            shape.to_string(),
            format!("See {shape} for the gate wiring."),
            format!("The rule lives at `{shape}` until the release ships."),
        ] {
            assert_eq!(check_secret(&content), Ok(()), "refused: {content}");
            // A bare path can trip the noise-pattern gate; only the secret
            // verdict is under test here.
            assert!(
                !matches!(
                    FilterConfig::default().apply(&content, false),
                    Err(FilterReject::PotentialSecret { .. })
                ),
                "apply refused as a secret: {content}"
            );
            assert!(find_secret_spans(&content).is_empty(), "span in: {content}");
        }
    }
}

/// Why: the refusal half — the path and URL structure stops counting as a
/// secret, nothing else does. A credential in the userinfo, a query value, a
/// fragment value or a path segment is still refused, and a `:line` suffix
/// does not launder a credential-shaped file name.
/// What: one row per credential placement; each must be refused by every gate
/// path, bare and in prose.
/// Test: itself; shown red by removing the query-value check.
#[test]
fn secrets_inside_paths_and_urls_are_refused_after_9748() {
    let presigned = format!(
        "https://bucket.s3.amazonaws.com/report.csv?X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Date=20261026T120000Z&X-Amz-Expires=3600&X-Amz-SignedHeaders=host\
         &X-Amz-Signature={AMZ_SIG}"
    );
    let rows: &[(&str, &str)] = &[
        (
            "basic auth",
            "https://admin:Hunter2Hunter2@db.example.com/app",
        ), // pragma: allowlist secret
        (
            "basic auth with query",
            "https://user:pass@example.com/path?x=1",
        ), // pragma: allowlist secret
        (
            "token query",
            "https://api.example.com/v1/items?token=Zq7RtY2uWx9Kp4Ls8Nm3",
        ), // pragma: allowlist secret
        (
            "lowercase access_token",
            "https://api.example.com/v1/items?access_token=abcdefghijklmnopqrstuvwx",
        ), // pragma: allowlist secret
        (
            "api_key query",
            "https://maps.example.com/api/js?api_key=AbCd1234EfGh5678IjKl9012&callback=init",
        ), // pragma: allowlist secret
        (
            "aws credential query",
            "https://bucket.s3.amazonaws.com/a.csv?X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20261026%2Fus-east-1%2Fs3%2Faws4_request",
        ), // pragma: allowlist secret
        ("aws signature query", &presigned),
        (
            "fragment token",
            "https://callback.example.com/done#access_token=Zq7RtY2uWx9Kp4Ls8Nm3Bv&token_type=bearer",
        ), // pragma: allowlist secret
        (
            "ghp_ in path",
            "https://hooks.example.com/ghp_abcdefghijklmnopqrstuvwxyz0123456789/run",
        ), // pragma: allowlist secret
        (
            "github_pat_ in path",
            "https://hooks.example.com/github_pat_11ABCDEFG0abcdefghijklmnop/run",
        ), // pragma: allowlist secret
        (
            "sk- in path",
            "https://proxy.example.com/v1/sk-abcdef0123456789abcdef01/chat",
        ), // pragma: allowlist secret
        (
            "xoxb- in path",
            "https://relay.example.com/xoxb-1234-5678-abcdEFGH/post",
        ), // pragma: allowlist secret
        (
            "AKIA in path",
            "https://s3.example.com/keys/AKIAIOSFODNN7EXAMPLE/config",
        ), // pragma: allowlist secret
        (
            "credential file name with line",
            "notes/AbCd1234EfGh5678IjKl9012.md:12",
        ), // pragma: allowlist secret
        (
            "credential with line, no extension",
            "AbCd1234EfGh5678IjKl9012:42",
        ), // pragma: allowlist secret
    ];
    for (label, shape) in rows {
        assert!(refused(shape), "{label}: admitted {shape}");
        assert!(
            refused(&format!("Callback set to {shape} for the job.")),
            "{label}: admitted in prose"
        );
    }
}
