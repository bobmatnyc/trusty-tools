//! Unit tests for the Claude Code runtime adapter's command builders
//! ([`spawn_command`], [`resume_command`]) and `RuntimeAdapter` impl.
//!
//! Why: split out of `claude_code.rs` (issue #3070) to bring that file back
//! toward its pre-#3040 SLOC budget, mirroring the sibling-`_tests.rs`
//! pattern already established by `claude_code_gh_env_tests.rs` for the same
//! file. Purely mechanical — no behavior or assertion changes.
//! What: exercises every command-builder flag combination (workdir, resume
//! selection, session id export, prompt file, OAuth token, `gh_env` file) and
//! the `ClaudeCodeAdapter::spawn`/`spawn_resume`/`identify` trait methods
//! against a `FakeTmux`.
//! Test: this file IS the test suite; see individual `#[test]` doc comments.

use super::super::test_helpers::FakeTmux;
use super::*;

/// Fixed managed-session UUID string reused across command-builder tests
/// (#2023 component B) — a representative id, not a real session.
const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

/// Fixed workdir reused across command-builder tests (#2250) — a
/// representative cwd, not a real workspace (these tests exercise the
/// string builders directly, never touching the filesystem).
const TEST_CWD: &str = "/tmp/ws";

/// RAII guard that redirects `$HOME` to a temp dir and restores it on drop
/// (including panic).
///
/// Why: the adapter's `spawn`/`spawn_resume` now provision the real managed
/// `CLAUDE_CONFIG_DIR` (resolved under `$HOME`). Tests that drive them must
/// redirect `$HOME` so the provisioning/trust-seed side effects land in a
/// throwaway dir instead of the developer's real `~/.trusty-tools`. Pair with
/// `#[serial_test::serial]` since it mutates process-global env.
struct HomeGuard {
    prev: Option<String>,
    _tmp: tempfile::TempDir,
}
impl HomeGuard {
    fn set() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var("HOME").ok();
        // SAFETY: callers are #[serial], so no other test thread reads HOME
        // concurrently; Drop restores the prior value even on panic.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        Self { prev, _tmp: tmp }
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.prev {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

#[test]
fn claude_code_adapter_identifies() {
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake);
    assert_eq!(adapter.identify(), "claude-code");
}

#[test]
fn spawn_command_contains_env_scrub() {
    // The spawn command must always strip the API key from the environment
    // so the session falls back to OAuth (keyed by CLAUDE_CONFIG_DIR).
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY"),
        "spawn command must contain env scrub: {cmd}"
    );
    assert!(
        cmd.contains(" claude "),
        "spawn command must invoke claude: {cmd}"
    );
}

#[test]
fn spawn_command_exports_managed_session_id() {
    // #2023 component B: the spawn command sent to the pane must export
    // TM_MANAGED_SESSION_ID (single-quoted, terminated by `;`) BEFORE the
    // claude invocation, so a command run in the pane after claude exits
    // can identify which managed session it belongs to. tmux
    // set-environment alone would NOT reach the pane's already-running
    // shell — the export must be part of the literal command line.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    let expected_prefix = format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; ");
    // #2250: the command now starts with `cd <workdir> && { ...` — the
    // export is the first statement INSIDE that brace group, not literally
    // the first bytes of the string.
    assert!(
        cmd.contains(&format!("{{ {expected_prefix}")),
        "spawn command must export the session id as the first statement \
             inside the cd-group: {cmd}"
    );
    let export_pos = cmd
        .find("export TM_MANAGED_SESSION_ID")
        .expect("export present");
    let claude_pos = cmd.find(" claude ").expect("claude invocation present");
    assert!(
        export_pos < claude_pos,
        "export must precede the claude invocation: {cmd}"
    );
}

#[test]
fn spawn_command_sets_claude_config_dir() {
    // DOC-34 / #1996: when a managed config dir is available the spawn
    // command MUST export it so the framework roster comes from tm's config
    // home, not the project's committed `.claude/`. It must co-exist with the
    // API-key scrub.
    let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains(
            "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
        ),
        "spawn command must scrub the key (via -u, BEFORE the NAME=VALUE assignment per \
             POSIX env grammar) then set (single-quoted) CLAUDE_CONFIG_DIR: {cmd}"
    );
    assert!(
        cmd.contains("--setting-sources project,local"),
        "isolation flag must still be present with a config dir: {cmd}"
    );
}

#[test]
fn env_bin_prefix_orders_unset_flag_before_config_dir_assignment() {
    // Regression guard (fatal bug): POSIX `env`'s grammar is
    // `env [OPTION]... [NAME=VALUE]... [COMMAND]...` — option flags like
    // `-u NAME` MUST precede any `NAME=VALUE` assignment. Putting
    // `CLAUDE_CONFIG_DIR=<dir>` before `-u` makes `env` stop parsing options
    // at the first NAME=VALUE and try to exec `-u` as a command
    // (`env: -u: No such file or directory`), fatally killing every managed
    // session spawn.
    let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    let u_pos = cmd
        .find("-u ANTHROPIC_API_KEY")
        .expect("cmd must contain -u ANTHROPIC_API_KEY");
    let config_pos = cmd
        .find("CLAUDE_CONFIG_DIR=")
        .expect("cmd must contain CLAUDE_CONFIG_DIR=");
    assert!(
        u_pos < config_pos,
        "-u ANTHROPIC_API_KEY must appear BEFORE CLAUDE_CONFIG_DIR= per POSIX env \
             option-then-assignment grammar: {cmd}"
    );
}

#[test]
fn spawn_command_without_config_dir_omits_it() {
    // When home is unresolvable there is no config dir to point at; the
    // command must fall back to the legacy scrub-only prefix (no bare
    // `CLAUDE_CONFIG_DIR=` token).
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("CLAUDE_CONFIG_DIR"),
        "no config dir → must not reference CLAUDE_CONFIG_DIR: {cmd}"
    );
}

// ── issue #2246: CLAUDE_CODE_OAUTH_TOKEN injection ──────────────────────

#[test]
fn spawn_command_sets_oauth_token_when_available() {
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        Some("sk-ant-oat01-fake-token"),
        None,
    );
    assert!(
        cmd.contains("CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-fake-token'"),
        "spawn command must inject a single-quoted CLAUDE_CODE_OAUTH_TOKEN when \
             one is available: {cmd}"
    );
    assert!(
        cmd.contains("-u ANTHROPIC_API_KEY"),
        "the API-key scrub must still be present alongside the token: {cmd}"
    );
}

#[test]
fn spawn_command_omits_oauth_token_when_absent() {
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        "no token available → must not reference CLAUDE_CODE_OAUTH_TOKEN: {cmd}"
    );
}

#[test]
fn spawn_command_without_token_is_byte_identical_to_pre_2246() {
    // Zero-regression guard: a caller with no config dir, no prompt file,
    // and no token must get EXACTLY the same command string this crate
    // produced before #2246 introduced the extra parameter.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    let expected = format!(
        "cd '/tmp/ws' && {{ export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; \
             env -u ANTHROPIC_API_KEY claude \
             --setting-sources project,local --dangerously-skip-permissions; \
             echo 'tm: run `tm` to relaunch this session'; }}"
    );
    assert_eq!(
        cmd, expected,
        "no-token command must be byte-identical to pre-#2246"
    );
}

#[test]
fn spawn_command_sets_both_config_dir_and_oauth_token() {
    // Both assignments must coexist, config dir first (matching the
    // established assignment order), each independently single-quoted.
    let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        TEST_SESSION_ID,
        None,
        Some("sk-ant-oat01-fake-token"),
        None,
    );
    assert!(
        cmd.contains(
            "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' \
                 CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-fake-token' claude"
        ),
        "both assignments must coexist, config dir before the token: {cmd}"
    );
}

#[test]
fn resume_command_sets_oauth_token_when_available() {
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        Some("sk-ant-oat01-fake-token"),
        None,
    );
    assert!(
        cmd.contains("CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-fake-token'"),
        "resume command must inject the token when available: {cmd}"
    );
    assert!(
        cmd.contains("--resume abc-123"),
        "--resume must still be present alongside the token: {cmd}"
    );
}

#[test]
fn resume_command_omits_oauth_token_when_absent() {
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        "no token available → must not reference CLAUDE_CODE_OAUTH_TOKEN: {cmd}"
    );
}

#[test]
fn resume_command_without_token_is_byte_identical_to_pre_2246() {
    // Zero-regression guard, resume-path counterpart to
    // spawn_command_without_token_is_byte_identical_to_pre_2246.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    let expected = format!(
        "cd '/tmp/ws' && {{ export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; \
             env -u ANTHROPIC_API_KEY claude \
             --setting-sources project,local --dangerously-skip-permissions --resume abc-123; \
             echo 'tm: run `tm` to relaunch this session'; }}"
    );
    assert_eq!(
        cmd, expected,
        "no-token resume command must be byte-identical to pre-#2246"
    );
}

#[test]
fn env_bin_prefix_quotes_config_dir_with_space() {
    // CRITICAL (DOC-34 review): a home dir with a space must NOT word-split
    // the pane command. The path must appear single-quoted and INTACT so
    // `env` receives one CLAUDE_CONFIG_DIR value, not two argv tokens.
    let dir = Path::new("/Users/John Doe/.trusty-tools/trusty-mpm/claude-config");
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains(
            "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config' claude"
        ),
        "config dir with a space must be single-quoted and intact: {cmd}"
    );
    // The bare unquoted form (which would word-split) must NOT appear.
    assert!(
        !cmd.contains("CLAUDE_CONFIG_DIR=/Users/John Doe"),
        "config dir must never be interpolated unquoted: {cmd}"
    );
    // Resume path shares env_bin_prefix, so it must quote identically.
    let resume = resume_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        None,
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        resume
            .contains("CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config'"),
        "resume command must also single-quote a spaced config dir: {resume}"
    );
}

#[test]
fn shell_single_quote_escapes_embedded_quote() {
    // A literal single quote in the path must be escaped via the POSIX
    // close-reopen sequence '\'' so the quoting stays balanced. (A macOS
    // home never contains one, but the escape must be correct regardless.)
    assert_eq!(shell_single_quote("/a/b"), "'/a/b'");
    assert_eq!(
        shell_single_quote("/Users/o'brien/cfg"),
        r"'/Users/o'\''brien/cfg'"
    );
}

#[test]
fn spawn_command_contains_isolation_flags() {
    // Why: the session_manager spawn path must isolate from the user's global
    // settings and run fully unattended (bypass all permission prompts) so
    // multi-agent orchestration never stalls on interactive approval dialogs.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("--setting-sources project,local"),
        "spawn command must isolate settings: {cmd}"
    );
    assert!(
        cmd.contains("--dangerously-skip-permissions"),
        "spawn command must bypass permissions for unattended orchestration: {cmd}"
    );
    // Must not carry the old acceptEdits flag.
    assert!(
        !cmd.contains("acceptEdits"),
        "old acceptEdits flag must not be present: {cmd}"
    );
}

#[test]
fn spawn_command_uses_resolved_binary() {
    // Why (#1298): under launchd the pane inherits a minimal PATH, so the
    // spawn command must invoke claude by the resolved (absolute) path
    // rather than a bare `claude` that the pane's PATH cannot find.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "/Users/me/.local/bin/claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY /Users/me/.local/bin/claude "),
        "spawn command must invoke the resolved absolute claude path: {cmd}"
    );
}

#[test]
fn claude_code_adapter_binary_check_returns_option() {
    // resolve_claude returns Some(path) or None without panicking; when it
    // resolves, the path must be a non-empty absolute-ish string.
    if let Some(p) = ClaudeCodeAdapter::resolve_claude() {
        assert!(!p.is_empty(), "resolved claude path must be non-empty");
    }
}

#[serial_test::serial]
#[test]
fn spawn_sends_env_scrub_when_binary_available() {
    // Patch: if claude is not available this test is a no-op (we cannot
    // install it in CI). We only assert the send when the binary exists.
    // HOME is redirected so the spawn's config-dir provisioning + trust seed
    // land in a throwaway dir, not the developer's real ~/.trusty-tools.
    let _home = HomeGuard::set();
    let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return;
    };
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn(
            "tmpm-test",
            Path::new("/tmp"),
            "some task",
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, "tmpm-test");
    let cmd = &sends[0].1;
    // Must carry the env scrub regardless (the option must precede any
    // intervening `CLAUDE_CONFIG_DIR=` assignment per POSIX env grammar).
    assert!(cmd.contains("-u ANTHROPIC_API_KEY"));
    // Issue #2125 item 3: the spawn command must inject the PM system
    // prompt via --append-system-prompt-file — the third fail-closed
    // carrier — so this, the default daemon on-ramp, can no longer
    // silently spawn vanilla Claude Code. The prompt file path itself is
    // non-deterministic (fresh UUID per call), so this asserts the flag's
    // presence rather than an exact command match.
    assert!(
        cmd.contains("--append-system-prompt-file"),
        "spawn command must inject the PM system prompt file: {cmd}"
    );
}

#[serial_test::serial]
#[test]
fn spawn_sends_oauth_token_when_available() {
    // #2246: the fresh-spawn path must inject CLAUDE_CODE_OAUTH_TOKEN when
    // resolve_oauth_token() finds one (here, via the process env var — the
    // higher-precedence source). HOME is redirected so the rest of the
    // provisioning stays hermetic.
    let _home = HomeGuard::set();
    let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return;
    };
    unsafe {
        std::env::set_var(
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR,
            "sk-ant-oat01-fake-token",
        );
    }
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    let result = adapter.spawn(
        "tmpm-test",
        Path::new("/tmp"),
        "some task",
        TEST_SESSION_ID,
        &[],
    );
    unsafe {
        std::env::remove_var(crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR);
    }
    result.expect("spawn");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        sends[0]
            .1
            .contains("CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-fake-token'"),
        "spawn must inject the resolved oauth token: {}",
        sends[0].1
    );
}

#[test]
fn publish_session_env_sets_id_and_config_dir() {
    // #2157 item 1: exercises publish_session_env directly (no HOME
    // redirection or real `claude` binary needed) so this call-shape
    // assertion runs unconditionally in CI, unlike the full-spawn tests
    // below which are gated on a real `claude` binary being present.
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter.publish_session_env("tmpm-test", TEST_SESSION_ID, Some("/tmp/config-dir"));
    let env_sets = fake.env_sets.lock().unwrap();
    assert_eq!(
        env_sets.len(),
        2,
        "expected id + config-dir sets: {env_sets:?}"
    );
    assert!(env_sets.contains(&(
        "tmpm-test".to_string(),
        "TM_MANAGED_SESSION_ID".to_string(),
        TEST_SESSION_ID.to_string()
    )));
    assert!(env_sets.contains(&(
        "tmpm-test".to_string(),
        "CLAUDE_CONFIG_DIR".to_string(),
        "/tmp/config-dir".to_string()
    )));
}

#[test]
fn publish_session_env_omits_config_dir_when_absent() {
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter.publish_session_env("tmpm-test", TEST_SESSION_ID, None);
    let env_sets = fake.env_sets.lock().unwrap();
    assert_eq!(
        env_sets.len(),
        1,
        "expected only the session-id set: {env_sets:?}"
    );
    assert_eq!(env_sets[0].1, "TM_MANAGED_SESSION_ID");
}

#[serial_test::serial]
#[test]
fn spawn_publishes_session_id_via_set_environment() {
    // #2157 item 1: spawn must durably publish TM_MANAGED_SESSION_ID via
    // `tmux set-environment`, not just the pane-shell export baked into the
    // command line — belt-and-suspenders so a sibling pane/window in the
    // same tmux session (which never ran the export line) can still resolve
    // it via `tmux show-environment`.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    }
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn(
            "tmpm-test",
            Path::new("/tmp"),
            "some task",
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn");
    let env_sets = fake.env_sets.lock().unwrap();
    assert!(
        env_sets.iter().any(|(name, key, value)| name == "tmpm-test"
            && key == "TM_MANAGED_SESSION_ID"
            && value == TEST_SESSION_ID),
        "spawn must call tmux set-environment with TM_MANAGED_SESSION_ID: {env_sets:?}"
    );
}

#[test]
fn spawn_command_with_prompt_file_contains_flag() {
    // #2125 item 3: when a prompt file is supplied, spawn_command must
    // inject --append-system-prompt-file pointing at it, alongside the
    // existing isolation flags — the third fail-closed carrier.
    let path = Path::new("/tmp/trusty-mpm-system-prompt-test.txt");
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        Some(path),
        None,
        None,
    );
    assert!(
        cmd.contains("--append-system-prompt-file '/tmp/trusty-mpm-system-prompt-test.txt'"),
        "spawn command must pass the prompt file via --append-system-prompt-file: {cmd}"
    );
    assert!(
        cmd.contains("--setting-sources project,local"),
        "isolation flags must still be present alongside the prompt file: {cmd}"
    );
}

#[test]
fn spawn_command_without_prompt_file_omits_flag() {
    // #2125 item 3: no prompt file (e.g. the write failed) → the flag must
    // be omitted entirely rather than passed with a bad/empty path.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("--append-system-prompt-file"),
        "no prompt file → flag must be absent: {cmd}"
    );
}

#[test]
fn build_prompt_file_writes_resolved_prompt_for_project() {
    // #2125 item 3: build_prompt_file must reuse the SAME
    // build_system_prompt_for_with_style_and_native seam the CLI/client
    // launch paths use, so the daemon adapter's injected prompt is never a
    // divergent copy — proven here by asserting the written file carries
    // the bundled PM_INSTRUCTIONS heading.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = build_prompt_file(tmp.path()).expect("prompt file written");
    let content = std::fs::read_to_string(&path).expect("prompt file readable");
    assert!(
        content.contains("# PM Agent -- Trusty MPM"),
        "prompt file must contain the resolved PM system prompt: {content}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn resume_command_with_id_uses_resume_flag() {
    // Why (#1744): --resume <id> restores the exact prior conversation;
    // the test pins the contract so accidental regressions are caught early.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("--resume abc-123"),
        "resume command must include --resume <id>: {cmd}"
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY"),
        "resume command must still scrub API key: {cmd}"
    );
    assert!(
        !cmd.contains("--continue"),
        "resume command must NOT contain --continue when id is set: {cmd}"
    );
}

#[test]
fn resume_command_exports_managed_session_id() {
    // #2023 component B: the resume command must ALSO export
    // TM_MANAGED_SESSION_ID before the claude invocation, for the same
    // reason as the spawn path — the pane's shell must retain the id after
    // claude exits, whichever launch path (fresh spawn or resume) started it.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    let expected_prefix = format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; ");
    // #2250: the command now starts with `cd <workdir> && { ...` — the
    // export is the first statement INSIDE that brace group, not literally
    // the first bytes of the string.
    assert!(
        cmd.contains(&format!("{{ {expected_prefix}")),
        "resume command must export the session id as the first statement \
             inside the cd-group: {cmd}"
    );
    let export_pos = cmd
        .find("export TM_MANAGED_SESSION_ID")
        .expect("export present");
    let resume_pos = cmd.find("--resume").expect("--resume flag present");
    assert!(
        export_pos < resume_pos,
        "export must precede the --resume flag: {cmd}"
    );
}

#[test]
fn resume_command_sets_claude_config_dir() {
    // DOC-34: the resume path must also carry CLAUDE_CONFIG_DIR so resumed
    // sessions read the same tm-owned roster as fresh spawns.
    let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        Some(dir),
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains(
            "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
        ),
        "resume command must export (single-quoted) CLAUDE_CONFIG_DIR after the -u option: {cmd}"
    );
    assert!(
        cmd.contains("--resume abc-123"),
        "resume command must still include --resume <id>: {cmd}"
    );
}

#[test]
fn resume_command_without_id_with_prior_conv_uses_continue() {
    // Why (#1744 / #1840): when no claude_session_id is stored but prior
    // conversation history exists, --continue resumes the most-recent
    // conversation in the workspace rather than starting fresh.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        None,
        true,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("--continue"),
        "resume command without id + prior conv must use --continue: {cmd}"
    );
    assert!(
        !cmd.contains("--resume"),
        "resume command without id must NOT contain --resume: {cmd}"
    );
}

#[test]
fn resume_command_without_id_no_prior_conv_uses_plain_spawn() {
    // Why (#1840): when no claude_session_id is stored AND no prior
    // conversation exists, --continue would error with "No conversation
    // found to continue". The plain spawn starts a fresh session instead.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        None,
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("--continue"),
        "resume command without id + no prior conv must NOT use --continue: {cmd}"
    );
    assert!(
        !cmd.contains("--resume"),
        "resume command without id must NOT use --resume: {cmd}"
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY"),
        "plain spawn command must still scrub API key: {cmd}"
    );
}

#[test]
fn has_prior_conversation_returns_false_for_fresh_workspace() {
    // Why (#1840): a fresh worktree has no Claude conversation history;
    // has_prior_conversation must return false to avoid "No conversation found".
    // Uses has_prior_conversation_in with a temp projects_dir — no HOME env
    // mutation, making this test safe for parallel execution.
    let tmp = tempfile::tempdir().expect("tempdir");
    let projects_dir = tmp.path().join("projects");
    // projects_dir does not exist → always returns false.
    assert!(
        !has_prior_conversation_in(tmp.path(), &projects_dir),
        "no projects dir → false"
    );
    // Create the projects dir but with no entry for this workspace → still false.
    std::fs::create_dir_all(&projects_dir).unwrap();
    assert!(
        !has_prior_conversation_in(tmp.path(), &projects_dir),
        "projects dir exists but no entry for this workspace → false"
    );
}

#[test]
fn has_prior_conversation_returns_true_when_jsonl_exists() {
    // Why (#1840): verify the positive path — a workspace with a .jsonl file
    // in its encoded project dir must return true.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let projects_dir = tmp.path().join("projects");
    // Encode the cwd path as Claude does: replace '/' with '-'.
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = projects_dir.join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();
    assert!(
        has_prior_conversation_in(&cwd, &projects_dir),
        "workspace with .jsonl must return true"
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_with_id_uses_resume_flag() {
    // Why (#1744, #2013): ClaudeCodeAdapter::spawn_resume must send
    // --resume <id> to the pane when the claude_session_id is known AND
    // still resolves to a real session on disk. HOME is redirected so the
    // config-dir provisioning is hermetic; a matching session jsonl file
    // is seeded under the resolved CLAUDE_CONFIG_DIR/projects/ so the
    // #2013 existence check passes.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    };
    let cwd = Path::new("/tmp");
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir()
        .expect("config dir resolves under redirected HOME");
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = config_dir.join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("my-session-id.jsonl"), "{}").unwrap();

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "tmpm-test",
            None,
            cwd,
            "task",
            Some("my-session-id"),
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        sends[0].1.contains("--resume my-session-id"),
        "spawn_resume with an existing id must use --resume: {}",
        sends[0].1
    );
    // #2157 item 1: the RESUME path must ALSO durably publish the id — a
    // fresh tmux session is created on resume, so it starts with no
    // set-environment history at all.
    let env_sets = fake.env_sets.lock().unwrap();
    assert!(
        env_sets.iter().any(|(name, key, value)| name == "tmpm-test"
            && key == "TM_MANAGED_SESSION_ID"
            && value == TEST_SESSION_ID),
        "spawn_resume must call tmux set-environment with TM_MANAGED_SESSION_ID: {env_sets:?}"
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_sends_prompt_file_when_binary_available() {
    // #2230: spawn_resume must inject --append-system-prompt-file just
    // like spawn() does — before this fix, every resume path (including
    // guided-resume and crash-recovery, which both funnel through this
    // adapter method) silently omitted the PM system prompt carrier.
    // HOME is redirected so build_prompt_file's write lands in a
    // throwaway dir, not the developer's real ~/.trusty-tools.
    let _home = HomeGuard::set();
    let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return;
    };
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "tmpm-test",
            None,
            Path::new("/tmp/does-not-exist-2230"),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        sends[0].1.contains("--append-system-prompt-file"),
        "spawn_resume must inject the PM system prompt file: {}",
        sends[0].1
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_sends_oauth_token_when_available() {
    // #2246: spawn_resume must inject CLAUDE_CODE_OAUTH_TOKEN when one is
    // resolvable, mirroring spawn()'s carrier — every resumed session
    // funnels through here, so this is the exact path that was silently
    // exposed to the login loop before this fix. HOME is redirected so
    // resolve_oauth_token()'s stored-file check is hermetic; the env var
    // itself is set/cleared around the call.
    let _home = HomeGuard::set();
    let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return;
    };
    unsafe {
        std::env::set_var(
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR,
            "sk-ant-oat01-fake-token",
        );
    }
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    let result = adapter.spawn_resume(
        "tmpm-test",
        None,
        Path::new("/tmp/does-not-exist-2246"),
        "task",
        None,
        TEST_SESSION_ID,
        &[],
    );
    unsafe {
        std::env::remove_var(crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR);
    }
    result.expect("spawn_resume");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        sends[0]
            .1
            .contains("CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-fake-token'"),
        "spawn_resume must inject the resolved oauth token: {}",
        sends[0].1
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_with_missing_id_falls_back_gracefully() {
    // Why (#2013): a stale/unknown claude_session_id must NOT be passed to
    // `--resume` (which would fail hard) — it must fall back the same way
    // a `None` id does. HOME is redirected so the config dir is hermetic
    // and no session jsonl is seeded, so the id is guaranteed missing.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    };
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "tmpm-test",
            None,
            Path::new("/tmp/does-not-exist-2013"),
            "task",
            Some("stale-session-id"),
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume must not hard-fail on a missing id");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        !sends[0].1.contains("--resume"),
        "a missing id must NOT be passed to --resume: {}",
        sends[0].1
    );
    assert!(
        !sends[0].1.contains("--continue"),
        "a missing id with no prior conversation history must fall back to a \
             plain spawn, not --continue: {}",
        sends[0].1
    );
    assert!(
        sends[0].1.contains("env -u ANTHROPIC_API_KEY"),
        "fallback command must still scrub the API key: {}",
        sends[0].1
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_targets_stored_pane_id_when_known() {
    // Sibling-window hijack fix, follow-up to #2456: when the caller
    // supplies `record.pane_id`, spawn_resume MUST target that SPECIFIC
    // pane (via `send_line_to_pane`), never the bare session name (via
    // `send_line`, which tmux resolves to whichever pane/window happens
    // to be active — e.g. a sibling window opened after the original
    // pane). This is the exact regression that let a resume/restart
    // respawn `claude` into an unrelated active pane while the original
    // pane sat at a bare shell.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    };
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "tmpm-test",
            Some("%6015"),
            Path::new("/tmp/does-not-exist-pane-target"),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume with known pane_id");

    let pane_sends = fake.pane_sends.lock().unwrap();
    assert_eq!(
        pane_sends.len(),
        1,
        "spawn_resume with a known pane_id must call send_line_to_pane exactly once: \
             {pane_sends:?}"
    );
    assert_eq!(
        pane_sends[0].0, "tmpm-test",
        "pane-scoped send must target the record's tmux session name"
    );
    assert_eq!(
        pane_sends[0].1, "%6015",
        "pane-scoped send must target the record's STORED pane_id, not whatever pane \
             tmux considers active"
    );

    let sends = fake.sends.lock().unwrap();
    assert!(
        sends.is_empty(),
        "spawn_resume with a known pane_id must NEVER fall back to the session-scoped \
             send_line (which tmux resolves to the active pane): {sends:?}"
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_falls_back_to_session_target_when_pane_id_unknown() {
    // The inverse of the above: a legacy record that predates pane-id
    // capture (`pane_id: None`) has no stronger signal to target than the
    // session name — spawn_resume must fall back to the pre-existing
    // session-scoped `send_line`, not silently drop the command.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    };
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "tmpm-test",
            None,
            Path::new("/tmp/does-not-exist-no-pane"),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume with no pane_id");

    let sends = fake.sends.lock().unwrap();
    assert_eq!(
        sends.len(),
        1,
        "spawn_resume with no pane_id must fall back to session-scoped send_line: {sends:?}"
    );
    let pane_sends = fake.pane_sends.lock().unwrap();
    assert!(
        pane_sends.is_empty(),
        "spawn_resume with no pane_id must never call send_line_to_pane: {pane_sends:?}"
    );
}

#[test]
fn session_id_exists_true_for_real_jsonl_file() {
    // Why (#2013): the positive path — a session file present under the
    // encoded project dir must resolve as existing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let projects_dir = tmp.path().join("projects");
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = projects_dir.join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("abc-123.jsonl"), "{}").unwrap();
    assert!(
        session_id_exists_in(&cwd, &projects_dir, "abc-123"),
        "existing session file must resolve as present"
    );
}

#[test]
fn session_id_exists_false_for_missing_id() {
    // Why (#2013): a stale id — no matching .jsonl for this id, even
    // though the workspace has OTHER conversation history — must resolve
    // as absent so the caller falls back instead of hard-failing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let projects_dir = tmp.path().join("projects");
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = projects_dir.join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("other-session.jsonl"), "{}").unwrap();
    assert!(
        !session_id_exists_in(&cwd, &projects_dir, "stale-id-not-present"),
        "id with no matching file must resolve as absent"
    );
}

#[test]
fn session_id_exists_false_when_projects_dir_absent() {
    // Why (#2013): a fresh workspace / unresolved config dir must never
    // panic or hard-fail — best-effort false is the safe default.
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing_projects_dir = tmp.path().join("does-not-exist");
    assert!(
        !session_id_exists_in(tmp.path(), &missing_projects_dir, "any-id"),
        "missing projects dir must resolve as absent, not panic"
    );
    assert!(
        !session_id_exists(tmp.path(), None, "any-id"),
        "session_id_exists with no config dir falls back to home resolution \
             and must never panic even if HOME is unusual in the test env"
    );
}

#[test]
fn encode_project_dir_replaces_slashes() {
    // Why (#2013 cleanup): pins the shared encoding helper's contract
    // directly, independent of either call site.
    assert_eq!(
        encode_project_dir(Path::new("/private/tmp/foo")),
        "-private-tmp-foo"
    );
}

#[test]
fn session_id_exists_finds_hardcoded_dir_name_for_known_cwd() {
    // Why (#2013 cleanup, MEDIUM): the other session_id_exists tests build
    // their expected path via the SAME formula the implementation uses
    // (`cwd.to_string_lossy().replace('/', "-")` / `encode_project_dir`),
    // so they cannot catch a future drift in the encoding scheme — the
    // test and the code would drift together. This test instead types the
    // expected directory name BY HAND as a literal, so if the encoding
    // scheme ever changes (e.g. Claude Code starts hashing paths instead
    // of dash-joining them), this assertion breaks independently of the
    // implementation.
    let tmp = tempfile::tempdir().expect("tempdir");
    let projects_dir = tmp.path().join("projects");
    // Hand-typed literal for cwd "/tmp/my-workspace" — NOT derived by
    // calling encode_project_dir/replace('/', "-") here.
    let project_dir = projects_dir.join("-tmp-my-workspace");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("known-id.jsonl"), "{}").unwrap();
    assert!(
        session_id_exists_in(Path::new("/tmp/my-workspace"), &projects_dir, "known-id"),
        "session_id_exists_in must find the seeded session under the \
             hand-typed expected dir name '-tmp-my-workspace'"
    );
}

#[test]
fn projects_dir_for_prefers_config_dir_when_present() {
    // Why (#2013): CLAUDE_CONFIG_DIR relocates the entire config home,
    // including session storage — the projects dir must be resolved
    // UNDER it, not always under ~/.claude, or the existence check would
    // never find managed sessions.
    let config_dir = Path::new("/tmp/some-managed-config-dir");
    assert_eq!(
        projects_dir_for(Some(config_dir)),
        Some(config_dir.join("projects")),
        "projects_dir_for must nest under the given config dir"
    );
}

#[serial_test::serial]
#[test]
fn spawn_resume_without_id_no_prior_conv_sends_plain_spawn() {
    // Why (#1840, #1845 item 3): when no claude_session_id is available AND the
    // workspace has no prior conversation, spawn_resume must send a plain spawn
    // (no --continue, no --resume) to avoid "No conversation found to continue".
    //
    // The command construction is tested via resume_command() directly with a fake
    // binary so assertions ALWAYS run even when the `claude` binary is absent in CI.
    // The adapter merely calls resume_command() with the same arguments; testing the
    // function directly proves the selection logic without CI depending on claude.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "__fake_claude__",
        None,
        None,
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        !cmd.contains("--continue"),
        "plain-spawn path must NOT use --continue: {cmd}"
    );
    assert!(
        !cmd.contains("--resume"),
        "plain-spawn path must NOT use --resume: {cmd}"
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY"),
        "plain-spawn path must still scrub API key: {cmd}"
    );

    // Additionally verify the adapter-level plumbing when the binary is present.
    // HOME is redirected so the config-dir provisioning is hermetic.
    let _home = HomeGuard::set();
    let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return; // core assertions above already ran; adapter path requires binary
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());
    adapter
        .spawn_resume(
            "test-tmux-session",
            None,
            tmp.path(),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume without id");
    let sends = fake.sends.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert!(
        !sends[0].1.contains("--continue"),
        "adapter plain-spawn must NOT use --continue: {}",
        sends[0].1
    );
    assert!(
        !sends[0].1.contains("--resume"),
        "adapter plain-spawn must NOT use --resume: {}",
        sends[0].1
    );
}

// ── #2250: cd-prefix belt-and-suspenders ────────────────────────────────

#[test]
fn spawn_command_prefixes_cd_to_workdir() {
    // #2250: the emitted command must be correct regardless of the pane
    // shell's actual starting directory — an explicit `cd` must lead the
    // whole line, and the exported session id / claude invocation must be
    // wrapped in a brace GROUP (not a subshell) so the env survives.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.starts_with("cd '/tmp/ws' && { "),
        "spawn command must start with an explicit cd into the workdir: {cmd}"
    );
    assert!(
        cmd.trim_end().ends_with('}'),
        "spawn command must close the brace group it opened: {cmd}"
    );
}

#[test]
fn resume_command_prefixes_cd_to_workdir() {
    // #2250: same cd-prefix guarantee on the resume path — the one most
    // directly implicated by a stale/removed workspace_path silently
    // rooting a recreated pane at $HOME.
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.starts_with("cd '/tmp/ws' && { "),
        "resume command must start with an explicit cd into the workdir: {cmd}"
    );
    assert!(
        cmd.trim_end().ends_with('}'),
        "resume command must close the brace group it opened: {cmd}"
    );
}

#[test]
fn cd_and_group_quotes_workdir_with_space() {
    // A workdir with a space must not word-split the pane command — same
    // invariant env_bin_prefix already enforces for CLAUDE_CONFIG_DIR.
    let cmd = cd_and_group(Path::new("/Users/John Doe/work"), "echo hi");
    assert_eq!(cmd, "cd '/Users/John Doe/work' && { echo hi; }");
}

// ── #2023 component D: on-exit relaunch hint ────────────────────────────

#[test]
fn spawn_command_prints_relaunch_hint_after_claude_exits() {
    // The hint must appear AFTER the claude invocation, separated by `;` so
    // it only runs once claude exits and control returns to the pane shell.
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("; echo 'tm: run `tm` to relaunch this session'"),
        "spawn command must print the relaunch hint after claude exits: {cmd}"
    );
    let claude_pos = cmd.find(" claude ").expect("claude invocation present");
    let hint_pos = cmd.find("; echo").expect("relaunch hint present");
    assert!(
        claude_pos < hint_pos,
        "relaunch hint must come AFTER the claude invocation: {cmd}"
    );
}

#[test]
fn resume_command_prints_relaunch_hint_after_claude_exits() {
    // Same invariant for the resume path (--resume branch here; the
    // --continue/plain-spawn branches share the same trailing suffix).
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(
        cmd.contains("; echo 'tm: run `tm` to relaunch this session'"),
        "resume command must print the relaunch hint after claude exits: {cmd}"
    );
    let resume_pos = cmd.find("--resume").expect("--resume flag present");
    let hint_pos = cmd.find("; echo").expect("relaunch hint present");
    assert!(
        resume_pos < hint_pos,
        "relaunch hint must come AFTER --resume: {cmd}"
    );
}

#[test]
fn resume_command_with_prompt_file_contains_flag() {
    // #2230: the resume path must carry the same --append-system-prompt-file
    // carrier as spawn_command — before this fix, resumed / guided-resume /
    // crash-recovery sessions had no way to inject the PM system prompt at all.
    let path = Path::new("/tmp/trusty-mpm-system-prompt-resume-test.txt");
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        Some("abc-123"),
        false,
        TEST_SESSION_ID,
        Some(path),
        None,
        None,
    );
    assert!(
        cmd.contains("--append-system-prompt-file '/tmp/trusty-mpm-system-prompt-resume-test.txt'"),
        "resume command must pass the prompt file via --append-system-prompt-file: {cmd}"
    );
    assert!(
        cmd.contains("--resume abc-123"),
        "--resume must still be present alongside the prompt file: {cmd}"
    );
}

// ── #2023 component C: in-place relaunch command builder ───────────────

#[test]
fn compose_inplace_args_uses_resume_for_existing_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = tmp.path().join("config");
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = config_dir.join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("existing-id.jsonl"), "{}").unwrap();

    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("existing-id"));
    assert!(
        args.windows(2).any(|w| w == ["--resume", "existing-id"]),
        "must select --resume <id> for an id that exists on disk: {args:?}"
    );
    assert!(
        !args.contains(&"--continue".to_owned()),
        "must not ALSO pass --continue when --resume is used: {args:?}"
    );
}

#[test]
fn compose_inplace_args_falls_back_for_missing_id() {
    // #2013 parity: a stale id (no matching .jsonl) with no prior
    // conversation history falls back to a fresh spawn — neither
    // --resume nor --continue.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("workspace-missing");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = tmp.path().join("config");

    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("stale-id"));
    assert!(
        !args.contains(&"--resume".to_owned()),
        "a stale id must not be passed to --resume: {args:?}"
    );
    assert!(
        !args.contains(&"--continue".to_owned()),
        "no prior conversation history: must not fall back to --continue: {args:?}"
    );
    assert!(
        args.contains(&"--setting-sources".to_owned()),
        "isolation flags must still be present: {args:?}"
    );
}

#[serial_test::serial]
#[test]
fn compose_inplace_args_uses_continue_when_no_id_but_prior_conv() {
    // has_prior_conversation (unlike session_id_exists) always looks under
    // `~/.claude/projects` regardless of `config_dir` — mirroring exactly
    // what `resume_command`'s own `has_prior_conv` computation does — so
    // this test redirects $HOME rather than seeding under a config dir.
    let _home = HomeGuard::set();
    let home = dirs::home_dir().expect("home resolves under redirected HOME");
    let cwd = std::path::PathBuf::from("/tmp/inplace-continue-test");
    // Seed prior conversation history (a .jsonl under the encoded cwd dir)
    // WITHOUT seeding the specific stale id, so has_prior_conversation is
    // true but session_id_exists for "stale-id" is false.
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = home.join(".claude").join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("some-other-session.jsonl"), "{}").unwrap();

    let args = compose_inplace_args(&cwd, None, Some("stale-id"));
    assert!(
        !args.contains(&"--resume".to_owned()),
        "a stale id must not be passed to --resume: {args:?}"
    );
    assert!(
        args.contains(&"--continue".to_owned()),
        "prior conversation history exists: must fall back to --continue: {args:?}"
    );
}

#[serial_test::serial]
#[test]
fn build_inplace_resume_command_resolves_claude_binary() {
    // Integration-style check that build_inplace_resume_command wires
    // resolve_claude + prepare_managed_config + compose_inplace_args
    // together; the pure selection logic is covered exhaustively above
    // without needing a real claude binary.
    let _home = HomeGuard::set();
    let Some(claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let result = build_inplace_resume_command(tmp.path(), Some("some-id")).expect("build succeeds");
    assert_eq!(result.claude_bin, claude_bin);
    assert!(
        result.args.contains(&"--setting-sources".to_owned()),
        "isolation flags must be present: {:?}",
        result.args
    );
}

#[serial_test::serial]
#[test]
fn build_inplace_resume_command_carries_oauth_token_when_available() {
    // #2246: the in-place relaunch (bare-`tm` inside a managed pane) must
    // carry the same resolved oauth token as the tmux-pane spawn/resume
    // paths, so the caller (guided_inplace.rs) can set the env var on the
    // relaunched process the same way it already does for CLAUDE_CONFIG_DIR.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    }
    unsafe {
        std::env::set_var(
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR,
            "sk-ant-oat01-fake-token",
        );
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let result = build_inplace_resume_command(tmp.path(), None);
    unsafe {
        std::env::remove_var(crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR);
    }
    let result = result.expect("build succeeds");
    assert_eq!(
        result.oauth_token.as_deref(),
        Some("sk-ant-oat01-fake-token")
    );
}
