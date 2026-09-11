//! End-to-end proof that the reported `pm_guard` FALSE POSITIVES are gone from
//! the real binary (#7498, #7479, #7499) — and that two more (#7477, #7436)
//! never reached it (issue #6982's finding, re-confirmed here).
//!
//! Why: the unit rows in
//! `commands::pm_guard_bash::false_positive_tests` prove the policy, but the
//! thing an agent actually hits is `tm hook --pm-guard` reading a `PreToolUse`
//! payload on stdin. Every one of these five issues was reported against a
//! LIVE binary, so the fix is only credible when the same stdin → classify →
//! stdout path answers ALLOW. This file is separate from the older
//! `tm_hook_pm_guard.rs` so the false-positive corpus stays one readable
//! catalogue rather than an append to a 4,000-line neighbour.
//! What: spawns the built `tm` binary against an unreachable daemon URL, writes
//! one `PreToolUse` payload, and asserts ALLOW (empty stdout) or DENY (one JSON
//! line carrying `permissionDecision: "deny"`). Each issue contributes the
//! reported command verbatim plus the deny that bounds the fix.
//! Test: `cargo test -p trusty-mpm --test tm_hook_pm_guard_false_positives`.

use std::io::Write;
use std::process::{Command, Stdio};

/// The daemon URL every spawn here pins: nothing listens on port 1, so the
/// best-effort audit POST on a deny path fails fast on a refused connection
/// rather than on a timeout. Same value and same reason as
/// `tm_hook_pm_guard.rs`; an integration test binary cannot import a sibling
/// test binary's helpers, so the minimal spawn shape is restated rather than
/// shared.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// Run `tm hook --pm-guard` over one `PreToolUse` payload and return stdout.
///
/// Why: the one place this file scrubs the environment. Each of the five
/// operator escape hatches below would turn every assertion in this file into a
/// tautology if it leaked in from the runner.
/// What: spawns the built binary, writes `stdin_json`, and asserts the
/// fail-open contract (`exit 0`, always) before handing back stdout. `HOME` is
/// pinned to a caller-supplied temporary directory so the per-turn budget
/// counter can never touch the developer's real `$HOME`.
/// Test: every assertion below routes through it.
fn run_pm_guard(stdin_json: &str, home: &std::path::Path) -> String {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut command = Command::new(bin);
    command
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .env("HOME", home)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .env_remove("TM_MANAGED_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .expect("failed to spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let output = child
        .wait_with_output()
        .expect("wait for tm hook --pm-guard");
    assert!(
        output.status.success(),
        "tm hook --pm-guard must always exit 0 (fail-open): status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf8")
}

/// A `PreToolUse` Bash payload carrying `command`.
fn bash_payload(command: &str) -> String {
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    input.to_string()
}

/// Assert the guard ALLOWED `command` — an allow prints nothing at all.
fn assert_allowed(command: &str) {
    let home = tempfile::tempdir().expect("tempdir");
    let stdout = run_pm_guard(&bash_payload(command), home.path());
    assert_eq!(
        stdout.trim(),
        "",
        "expected ALLOW (empty stdout) for: {command}\ngot: {stdout}"
    );
}

/// Assert the guard DENIED `command`, with a non-empty reason.
fn assert_denied(command: &str) {
    let home = tempfile::tempdir().expect("tempdir");
    let stdout = run_pm_guard(&bash_payload(command), home.path());
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "a deny must print exactly one JSON line for {command}, got: {stdout:?}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("deny stdout must be valid JSON");
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(
        parsed["hookSpecificOutput"]["permissionDecision"], "deny",
        "expected a DENY for: {command}\ngot: {stdout}"
    );
    assert!(
        parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "deny must carry a non-empty reason for: {command}"
    );
}

/// #7498: the issue's repro commands allow through the real binary.
#[test]
fn pm_guard_allows_a_one_character_glob_fragment_in_a_search_argument() {
    assert_allowed(r#"gh issue list --search "\s* guard" --state all"#);
    assert_allowed(r"grep -cvE '^\s*(//|\*|/\*|\*/|$)' crates/trusty-mpm/src/lib.rs");
    assert_allowed("gh issue list --search 's* guard' --state all");
}

/// #7479: a placeholder env file is readable and appendable through the real
/// binary.
#[test]
fn pm_guard_allows_reading_and_appending_a_placeholder_env_file() {
    assert_allowed("cat apps/advisor/.env.example");
    assert_allowed(r"printf 'KEY=\n' >> apps/advisor/.env.example");
    assert_allowed("cat .env.sample");
    assert_allowed("cat .env.template");
}

/// #7499: a Go-template `--format` argument allows through the real binary.
#[test]
fn pm_guard_allows_a_go_template_format_argument() {
    assert_allowed("docker images --format '{{.Repository}}:{{.Tag}}'");
    assert_allowed("docker ps --format '{{.ID}}'");
    assert_allowed("docker inspect --format '{{.Created}}' abc123");
}

/// #7477 and #7436: the shapes both issues report as refused ALLOW here.
///
/// Why: the refusal text those issues quote is emitted by the Claude Code
/// binary, not by `tm` — the string "too complex to verify that it stays inside
/// the worktree" is absent from the `tm` binary and present in the harness. A
/// change to this guard could not have fixed either issue, and this row is what
/// a future reporter should re-run before routing a third one here (#6982).
#[test]
fn pm_guard_allows_every_shape_the_harness_refused_as_unverifiable() {
    assert_allowed("ls apps");
    assert_allowed("ls -la");
    assert_allowed("grep -rl healthz services");
    assert_allowed("grep -n healthz docs/specs/domain-service-shell.md");
    assert_allowed(
        r#"grep -E "^test result" /private/tmp/claude-502/proj/sess/scratchpad/gate-7359.txt"#,
    );
}

/// #7477 and #7436: one command gets the same verdict across SEPARATE
/// `tm hook --pm-guard` processes.
///
/// Why: both issues report a verdict FLIPPING between tool calls, which are
/// separate `tm hook` invocations — separate processes, separate `RandomState`
/// seeds, separate environments. A single-process loop cannot speak to that, so
/// this row spawns the real binary repeatedly and asserts the answer is stable
/// across process boundaries. Twelve spawns is the most this can cost without
/// the row becoming the slowest test in the crate; each takes ~60ms.
/// What: runs one allowed and one denied command six times each and asserts
/// every run matches the first.
#[test]
fn pm_guard_gives_one_command_the_same_verdict_across_separate_processes() {
    let home = tempfile::tempdir().expect("tempdir");
    for command in ["grep -rl healthz services", "cat .env"] {
        let payload = bash_payload(command);
        let first = run_pm_guard(&payload, home.path());
        for round in 1..6 {
            let again = run_pm_guard(&payload, home.path());
            assert_eq!(
                again, first,
                "verdict changed in process {round} for: {command}"
            );
        }
    }
}

/// #7479 review round: the placeholder exemption covers the dotenv family only.
///
/// Why: the first round exempted `*.example`/`*.sample`/`*.template` on every
/// family, so `cat id_rsa.sample` and `cat secrets.example` went DENY to ALLOW
/// — a convention neither #7479 nor its reporter asked for, on the two families
/// where a misnamed file leaks a key rather than a variable name.
#[test]
fn pm_guard_exempts_only_the_dotenv_family_as_a_placeholder() {
    assert_allowed("cat .env.example");
    assert_denied("cat id_rsa.sample");
    assert_denied("cat secrets.example");
}

/// #7499 review round: a Bash sequence group is expanded, not read as literal.
///
/// Why: the first round read every comma-free group as literal text, which
/// dropped the one coverage `origin/main` had of a degenerate sequence —
/// `cat .en{v..v}`, which a shell expands to `cat .env`.
#[test]
fn pm_guard_denies_a_sequence_group_that_expands_onto_a_secret() {
    assert_denied("cat .en{v..v}");
    assert_denied("cat .en{u..v}");
    assert_denied("cat id_rs{a..c}");
    assert_allowed("for i in {1..5}; do echo $i; done");
    assert_allowed("echo {a..e}");
}

/// The denies these three fixes must not weaken, through the real binary.
///
/// Why: every row above withdraws a refusal, and a withdrawal is only correct
/// while the target it was protecting still denies. Keeping the pair in ONE
/// test is what stops a later edit from relaxing the allow side alone.
#[test]
fn pm_guard_still_denies_a_real_secret_file_and_a_real_brace_alternation() {
    // #7479: every `.env` spelling that is not a placeholder.
    assert_denied("cat .env");
    assert_denied("cat .env.local");
    assert_denied("cat .env.production");
    assert_denied("cat .env.example.bak");
    // #7498: a glob that really does target a family by name, and a literal.
    assert_denied("cat *.env");
    assert_denied("cat ./secrets*");
    assert_denied("cat id_rsa");
    assert_denied("cat terraform.tfvars");
    // #7499: a brace group that is a real alternation still resolves, and a
    // shape the expander cannot read still fails closed.
    assert_denied("cat {.env,.env.prod}");
    assert_denied("cat .{e:x,{y,env}}");
}
