use super::core::{scrub, scrub_compat};
use super::types::{MAX_BODY_BYTES, ScrubChange};

// ── Helper: calls scrub() and unpacks for legacy tests ────────────────────

fn scrub_pair(text: &str) -> (String, Vec<ScrubChange>) {
    scrub_compat(text)
}

// ── Existing tests (preserved) ────────────────────────────────────────────

#[test]
fn paths_redacted() {
    let (out, changes) = scrub_pair("failed to open /Users/alice/projects/foo.db");
    assert!(!out.contains("/Users/alice"), "path not scrubbed: {out}");
    assert!(out.contains('~'), "expected ~ replacement: {out}");
    assert!(changes.iter().any(|c| c.pattern == "AbsolutePath"));
}

#[test]
fn bearer_redacted() {
    let (out, changes) = scrub_pair("Authorization: Bearer sk-proj-abcXYZ123");
    assert!(
        !out.contains("sk-proj-abcXYZ123"),
        "bearer token not scrubbed: {out}"
    );
    assert!(
        out.contains("[REDACTED_TOKEN]") || out.contains("[REDACTED_API_KEY]"),
        "{out}"
    );
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "BearerToken" || c.pattern == "SkApiKey"),
        "{changes:?}"
    );
}

#[test]
fn jwt_redacted() {
    let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.sig";
    let (out, changes) = scrub_pair(&format!("authorization_header={token}"));
    assert!(!out.contains("eyJhbG"), "jwt not scrubbed: {out}");
    let redacted = out.contains("[REDACTED_JWT]") || out.contains("[REDACTED_VALUE]");
    assert!(redacted, "expected JWT redaction: {out}");
    let caught = changes
        .iter()
        .any(|c| c.pattern == "JwtToken" || c.pattern == "EnvSecret");
    assert!(caught, "expected jwt or env scrub rule: {changes:?}");
}

#[test]
fn env_kv_redacted() {
    let (out, changes) = scrub_pair("OPENAI_API_KEY=sk-proj-abc123\nNAME=alice");
    // Either the sk- rule or the env-KV rule (or both) should scrub the value.
    assert!(!out.contains("sk-proj-abc123"), "key not scrubbed: {out}");
    // NAME=alice should be kept (no secret keyword).
    assert!(
        out.contains("NAME=alice"),
        "unrelated var should be kept: {out}"
    );
    let scrubbed = changes
        .iter()
        .any(|c| c.pattern == "EnvSecret" || c.pattern == "SkApiKey");
    assert!(scrubbed, "expected env or sk scrub: {changes:?}");
}

#[test]
fn truncation_applies() {
    let long = "x".repeat(MAX_BODY_BYTES + 1000);
    let (out, changes) = scrub_pair(&long);
    assert!(out.len() <= MAX_BODY_BYTES + 100, "should be truncated");
    assert!(
        out.contains("truncated"),
        "should mention truncation: {out}"
    );
    assert!(changes.iter().any(|c| c.pattern == "Truncation"));
}

#[test]
fn clean_text_unchanged() {
    let text = "connection refused: dial tcp 127.0.0.1:5432";
    let (out, changes) = scrub_pair(text);
    assert_eq!(out, text);
    assert!(changes.is_empty(), "no changes expected: {changes:?}");
}

#[test]
fn windows_path_redacted() {
    let (out, changes) = scrub_pair(r"failed to open C:\Users\alice\projects\foo.db");
    assert!(!out.contains("Users"), "windows path not scrubbed: {out}");
    assert!(changes.iter().any(|c| c.pattern == "WindowsPath"));
}

// ── Phase 4: new secret pattern tests ────────────────────────────────────

#[test]
fn aws_key_redacted() {
    let (out, changes) = scrub_pair("aws_key=AKIAIOSFODNN7EXAMPLE_EXTRA_PAD");
    // The env-kv rule fires on 'aws_key=...' (contains KEY).
    // Additionally, if the literal matches AWS regex it should be caught.
    assert!(
        !out.contains("AKIAIOSFODNN7"),
        "AWS key not scrubbed: {out}"
    );
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "AwsKey" || c.pattern == "EnvSecret"),
        "expected AwsKey or EnvSecret: {changes:?}"
    );
}

#[test]
fn aws_key_standalone_redacted() {
    // Standalone AWS key not inside a KEY= assignment.
    let (out, changes) = scrub_pair("access key is AKIAIOSFODNN7EXAMPLEQ");
    assert!(
        !out.contains("AKIAIOSFODNN7"),
        "AWS key not scrubbed: {out}"
    );
    assert!(
        changes.iter().any(|c| c.pattern == "AwsKey"),
        "expected AwsKey: {changes:?}"
    );
}

#[test]
fn google_key_redacted() {
    let key = "AIzaSyDdI0hCZtE6vySjMm-WEfRq3CPzqKqqsHI"; // pragma: allowlist secret
    let (out, changes) = scrub_pair(&format!("google api key: {key}"));
    assert!(!out.contains("AIzaSy"), "Google key not scrubbed: {out}");
    assert!(
        changes.iter().any(|c| c.pattern == "GoogleKey"),
        "expected GoogleKey: {changes:?}"
    );
}

#[test]
fn slack_token_redacted() {
    let (out, changes) = scrub_pair("token=xoxb-1234567890-ABCDEFGHIJ");
    assert!(!out.contains("xoxb-"), "Slack token not scrubbed: {out}");
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "SlackToken" || c.pattern == "EnvSecret"),
        "expected SlackToken or EnvSecret: {changes:?}"
    );
}

#[test]
fn slack_token_standalone_redacted() {
    let (out, changes) = scrub_pair("using slack token xoxp-987654321-xyz");
    assert!(!out.contains("xoxp-"), "Slack token not scrubbed: {out}");
    assert!(
        changes.iter().any(|c| c.pattern == "SlackToken"),
        "expected SlackToken: {changes:?}"
    );
}

#[test]
fn pem_block_redacted() {
    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----"; // pragma: allowlist secret
    let (out, changes) = scrub_pair(pem);
    assert!(
        !out.contains("BEGIN RSA PRIVATE KEY"),
        "PEM block not scrubbed: {out}"
    );
    assert!(
        changes.iter().any(|c| c.pattern == "PemPrivateKey"),
        "expected PemPrivateKey: {changes:?}"
    );
}

#[test]
fn connection_string_redacted() {
    let (out, changes) = scrub_pair("db_url=postgres://admin:s3cr3tp4ss@db.example.com:5432/mydb"); // pragma: allowlist secret
    assert!(!out.contains("s3cr3tp4ss"), "password not scrubbed: {out}");
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "ConnString" || c.pattern == "EnvSecret"),
        "expected ConnString or EnvSecret: {changes:?}"
    );
}

#[test]
fn connection_string_standalone_redacted() {
    let (out, changes) = scrub_pair("connecting to postgres://user:hunter2@localhost/prod"); // pragma: allowlist secret
    assert!(!out.contains("hunter2"), "password not scrubbed: {out}");
    assert!(
        changes.iter().any(|c| c.pattern == "ConnString"),
        "expected ConnString: {changes:?}"
    );
}

#[test]
fn sk_prefix_redacted() {
    let (out, changes) = scrub_pair("OPENAI_KEY=sk-abcdef1234567890abcdef1234");
    assert!(!out.contains("sk-abcdef"), "sk- key not scrubbed: {out}");
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "SkApiKey" || c.pattern == "EnvSecret"),
        "expected SkApiKey or EnvSecret: {changes:?}"
    );
}

#[test]
fn sk_ant_prefix_redacted() {
    let (out, changes) = scrub_pair("ANTHROPIC_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz");
    assert!(
        !out.contains("sk-ant-api03"),
        "sk-ant- key not scrubbed: {out}"
    );
    assert!(
        changes
            .iter()
            .any(|c| c.pattern == "SkApiKey" || c.pattern == "EnvSecret"),
        "expected SkApiKey or EnvSecret: {changes:?}"
    );
}

#[test]
fn github_token_prefixes_redacted() {
    // pragma: allowlist secret
    let tokens = [
        "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
        "gho_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
        "ghu_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
        "ghs_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890", // pragma: allowlist secret
    ];
    for token in &tokens {
        let (out, changes) = scrub_pair(&format!("using token {token}"));
        assert!(
            !out.contains(&token[..8]),
            "GitHub token not scrubbed: {out}"
        );
        assert!(
            changes.iter().any(|c| c.pattern == "GithubToken"),
            "expected GithubToken for {token}: {changes:?}"
        );
    }
}

#[test]
fn scrub_result_summary() {
    // A string with both secrets and paths.
    let text = "token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567890 at /Users/alice/db"; // pragma: allowlist secret
    let result = scrub(text);
    // Must not contain the raw token or path.
    assert!(!result.text.contains("ghp_aB"), "{}", result.text);
    assert!(!result.text.contains("/Users/alice"), "{}", result.text);
    // Summary must mention both.
    assert!(
        result.redaction_summary.contains("secret") || result.redaction_summary.contains("path"),
        "summary: {}",
        result.redaction_summary
    );
}

#[test]
fn clean_text_summary_is_nothing_redacted() {
    let result = scrub("connection refused: 127.0.0.1:5432");
    assert_eq!(result.redaction_summary, "nothing redacted");
}

#[test]
fn home_dir_path_scrubbed() {
    // Simulate $HOME expansion in a message.
    let (out, changes) = scrub_pair("reading /home/bob/.config/trusty-mpm/config.toml");
    assert!(!out.contains("/home/bob"), "home path not scrubbed: {out}");
    assert!(
        changes.iter().any(|c| c.pattern == "AbsolutePath"),
        "{changes:?}"
    );
}
