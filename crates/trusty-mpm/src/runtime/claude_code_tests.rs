//! Unit tests for the Claude Code runtime adapter (#3070).
//!
//! Why: split out of `claude_code.rs` to bring that file back toward its
//! pre-#3040 SLOC budget.
//!
//! #8233 rescoped this file. It used to hold ~50 tests asserting substrings of
//! ONE composed shell line (`env -u … CLAUDE_CONFIG_DIR='…' <claude> --flags…`).
//! That line no longer exists: the launch travels as a
//! [`super::launch_spec::LaunchSpec`] and the pane is typed a fixed-shape
//! reference to it. Those assertions moved to `managed_launch_tests.rs`, where
//! they are made against the resolved cwd/env/argv rather than against quoting —
//! a strictly sharper form of the same coverage.
//!
//! What remains here: the `ClaudeCodeAdapter` trait methods driven against a
//! `FakeTmux` (reading back the spec the adapter actually wrote), the
//! project-dir encoder, the `--resume` staleness check, the in-place exec argv,
//! the prompt-file writer, and the managed-config provisioning.
//! Test: this file IS the test suite; see individual `#[test]` doc comments.

use super::super::launch_spec::LaunchSpec;
use super::super::test_helpers::FakeTmux;
use super::*;

/// The `LaunchSpec` an adapter call typed a reference to, read back off disk.
///
/// Why (#8233): the adapter's observable output is no longer a string to match —
/// it is a short line naming a spec file. Reading that file back is how an
/// adapter test asserts on the launch it actually produced, and it doubles as
/// proof the spec the pane is about to consume is really there and really
/// decodable.
/// What: takes the single line the fake recorded (session- or pane-scoped),
/// pulls the single-quoted path after `--launch-spec`, and consumes it.
/// Panics with the line's text when it is not the expected shape — a test that
/// silently found no spec would assert nothing.
fn sent_spec(line: &str) -> LaunchSpec {
    let after = line
        .split("--launch-spec '")
        .nth(1)
        .unwrap_or_else(|| panic!("typed line must name a launch spec: {line}"));
    let path = after
        .split('\'')
        .next()
        .unwrap_or_else(|| panic!("launch-spec path must be quoted: {line}"));
    LaunchSpec::consume(std::path::Path::new(path)).expect("the typed spec must be readable")
}

/// The one line a `FakeTmux` recorded, session- or pane-scoped.
fn only_line(fake: &FakeTmux) -> String {
    let sends = fake.sends.lock().expect("send log");
    let pane_sends = fake.pane_sends.lock().expect("pane send log");
    match (sends.len(), pane_sends.len()) {
        (1, 0) => sends[0].1.clone(),
        (0, 1) => pane_sends[0].2.clone(),
        _ => panic!("expected exactly one send, got {sends:?} / {pane_sends:?}"),
    }
}

/// Whether `spec.args` contains `flag` immediately followed by `value`.
fn has_flag_pair(spec: &LaunchSpec, flag: &str, value: &str) -> bool {
    spec.args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

/// Fixed managed-session UUID string reused across command-builder tests
/// (#2023 component B) — a representative id, not a real session.
const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

// #8233: `TEST_CWD` went with the string-builder tests. Every adapter test here
// drives the REAL `spawn`/`spawn_resume`, which provisions a config dir and
// writes a launch spec, so each uses the redirected `$HOME` as its workspace
// rather than a path that does not exist.

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
    tmp: tempfile::TempDir,
}
impl HomeGuard {
    fn set() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var("HOME").ok();
        // SAFETY: callers are #[serial], so no other test thread reads HOME
        // concurrently; Drop restores the prior value even on panic.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        Self { prev, tmp }
    }

    /// The decoy home this guard installed.
    ///
    /// Why (#7568): a test that asserts a producer wrote under the root it was
    /// GIVEN has to be able to name the root it must NOT have written under.
    fn home(&self) -> &std::path::Path {
        self.tmp.path()
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
    let adapter = ClaudeCodeAdapter::new(fake, None);
    assert_eq!(adapter.identify(), "claude-code");
}

// ── issue #2246: CLAUDE_CODE_OAUTH_TOKEN injection ──────────────────────

#[test]
fn claude_code_adapter_binary_check_returns_option() {
    // resolve_claude returns Some(path) or None without panicking; when it
    // resolves, the path must be a non-empty absolute-ish string.
    if let Some(p) = ClaudeCodeAdapter::resolve_claude() {
        assert!(!p.is_empty(), "resolved claude path must be non-empty");
    }
}

#[test]
fn publish_session_env_sets_id_and_config_dir() {
    // #2157 item 1: exercises publish_session_env directly (no HOME
    // redirection or real `claude` binary needed) so this call-shape
    // assertion runs unconditionally in CI, unlike the full-spawn tests
    // below which are gated on a real `claude` binary being present.
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), None);
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
    let adapter = ClaudeCodeAdapter::new(fake.clone(), None);
    adapter.publish_session_env("tmpm-test", TEST_SESSION_ID, None);
    let env_sets = fake.env_sets.lock().unwrap();
    assert_eq!(
        env_sets.len(),
        1,
        "expected only the session-id set: {env_sets:?}"
    );
    assert_eq!(env_sets[0].1, "TM_MANAGED_SESSION_ID");
}

#[test]
#[serial_test::serial]
fn build_prompt_file_writes_resolved_prompt_for_project() {
    // #2125 item 3: build_prompt_file must reuse the SAME
    // build_system_prompt_for_with_style_and_native seam the CLI/client
    // launch paths use, so the daemon adapter's injected prompt is never a
    // divergent copy — proven here by asserting the written file carries
    // the bundled PM_INSTRUCTIONS heading.
    //
    // #4752 added a compiled-prompt refresh resolved from `$HOME`, so this test
    // now needs the HomeGuard + `#[serial]` pairing to keep that write off the
    // developer's real `~/.trusty-mpm` (the #2459/#2460/#2461 hazard class).
    let _home = HomeGuard::set();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = build_prompt_file(tmp.path(), Some("sess-1")).expect("prompt file written");
    let content = std::fs::read_to_string(&path).expect("prompt file readable");
    assert!(
        content.contains("# PM Agent -- Trusty MPM"),
        "prompt file must contain the resolved PM system prompt: {content}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
#[serial_test::serial]
fn build_prompt_file_refreshes_the_compiled_prompt() {
    // Why (#4752 review, HIGH 3): `resume_managed` never calls `prepare_session*`
    // on its healthy path — it goes `resume_self_heal` → `ensure_status_line` →
    // `ensure_deployment_complete` → `spawn_resume`, and `spawn_resume` builds
    // its prompt here. Before this fix, every resume, guided-resume and
    // crash-recovery launch ran a prompt that never reached the compiled path.
    //
    // This is the seam ALL THREE spawn paths share, so covering it covers
    // resume. FIXTURE: the compiled file is pre-seeded with stale sentinel
    // content, so "the file exists" cannot pass this — only an actual refresh
    // can. Equality with the prompt file is what makes the artifact the same
    // text the session runs with.
    let _home = HomeGuard::set();
    let tmp = tempfile::tempdir().expect("tempdir");
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(tmp.path(), "sess-1");
    std::fs::create_dir_all(compiled.parent().unwrap()).expect("create framework dir");
    const STALE: &str = "STALE-FROM-A-PREVIOUS-LAUNCH";
    std::fs::write(&compiled, STALE).expect("seed stale compiled prompt");

    let path = build_prompt_file(tmp.path(), Some("sess-1")).expect("prompt file written");

    let on_disk = std::fs::read_to_string(&compiled).expect("compiled prompt readable");
    assert_ne!(
        on_disk, STALE,
        "the spawn seam must refresh a stale compiled prompt — resume paths \
         depend on this, since they never run prepare_session"
    );
    let launch_prompt = std::fs::read_to_string(&path).expect("prompt file readable");
    assert_eq!(
        on_disk, launch_prompt,
        "the compiled prompt must be byte-identical to the file passed to \
         --append-system-prompt-file"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
#[serial_test::serial]
fn build_prompt_file_compiled_write_failure_does_not_block_the_spawn() {
    // Why (#4752 review, HIGH 1's lesson applied here): the compiled file is an
    // INSPECTION artifact. A failure writing it must never cost the session its
    // actual system prompt — that would trade a stale debugging aid for a
    // broken launch, the same inverted priority the review flagged in
    // `prepare_session_inner`.
    //
    // FIXTURE: a directory is planted at the compiled path so only that write
    // fails; the prompt file itself must still be produced and still carry the
    // real PM prompt.
    let _home = HomeGuard::set();
    let tmp = tempfile::tempdir().expect("tempdir");
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(tmp.path(), "sess-1");
    std::fs::create_dir_all(&compiled).expect("plant a directory at the compiled path");

    let path = build_prompt_file(tmp.path(), Some("sess-1"))
        .expect("spawn must still get its prompt file (non-fatal)");
    let content = std::fs::read_to_string(&path).expect("prompt file readable");
    assert!(
        content.contains("# PM Agent -- Trusty MPM"),
        "the session must still receive the real PM prompt: {content}"
    );
    std::fs::remove_file(&path).ok();
}

/// Everything a fold records lives under `<root>/usage/`.
///
/// What: the savings ledger, the staged rows and the `no-fold-warned` markers
/// all sit there, so its existence is the single "this root was written to"
/// probe both tests below need — whichever branch the fold took.
fn usage_dir_of(framework_root: &std::path::Path) -> std::path::PathBuf {
    framework_root.join("usage")
}

// #7568: the named-root seam has no production caller yet — `build_prompt_file`
// is what every spawn path uses — so it is imported here rather than into
// `claude_code`, where it would read as an unused import.
use crate::runtime::prompt_file::build_prompt_file_in;

/// The savings producer writes under the root it was GIVEN.
///
/// Why (#7568): `build_prompt_file` was the last producer resolving its ledger
/// from the process home, so `cargo test -p trusty-mpm` kept adding files to the
/// operator's own `~/.trusty-mpm/usage/` — 64 `no-fold-warned` markers in the
/// run that failed this issue's live verification. #7514 gave
/// `refresh_compiled_prompt` and `record_compress_savings` a named-root seam;
/// this is the third. The assertion that matters is the NEGATIVE one: the
/// process's home must come away untouched.
/// What: a decoy `$HOME` standing in for the operator's, a separate named root,
/// and one call through the seam. Asserts the fold landed under the named root
/// and that the decoy grew no framework root at all.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn build_prompt_file_records_under_the_named_framework_root() {
    let home = HomeGuard::set();
    let root = tempfile::tempdir().expect("named framework root");
    let project = tempfile::tempdir().expect("project");

    let path = build_prompt_file_in(root.path(), project.path(), Some("sess-1"))
        .expect("prompt file written");
    std::fs::remove_file(&path).ok();

    assert!(
        usage_dir_of(root.path()).exists(),
        "the fold must be recorded under the named root: {}",
        root.path().display()
    );
    assert!(
        !home.home().join(".trusty-mpm").join("usage").exists(),
        "the producer wrote under the PROCESS home ({}) instead of the root it \
         was given — this is #7568's defect, and on a real machine that home is \
         the operator's own",
        home.home().display()
    );
}

/// The hazard, pinned: the ambient form follows the process's `$HOME`.
///
/// Why: without this, a `usage/` directory that stopped appearing for an
/// unrelated reason — a producer that declines, a pricing lookup that fails —
/// would make the negative assertion above pass while proving nothing. This is
/// the pre-fix shape, and it is also the PRODUCTION contract: a real spawn's
/// ledger is the operator's, which is the whole reason the seam had to be
/// added rather than the resolution changed.
/// What: the ambient entry point under a decoy `$HOME`, asserting the decoy
/// gained the framework root the named-root test above forbids.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn an_ambient_build_prompt_file_records_under_whatever_home_it_inherits() {
    let home = HomeGuard::set();
    let project = tempfile::tempdir().expect("project");

    let path = build_prompt_file(project.path(), Some("sess-1")).expect("prompt file written");
    std::fs::remove_file(&path).ok();

    assert!(
        usage_dir_of(&home.home().join(".trusty-mpm")).exists(),
        "the ambient form must record under the process's home — if this stops \
         holding, the guard above is asserting on something nothing produces \
         any more and must be re-pointed (decoy home {})",
        home.home().display()
    );
}

// #6765: `has_prior_conversation_returns_false_for_fresh_workspace` and
// `has_prior_conversation_returns_true_when_jsonl_exists` were deleted with the
// functions they covered. The `--continue` branch they gated is gone from both
// relaunch paths, so there is no eligibility question left to answer; what
// replaced them is `session_id_exists_*` (which reads the session's OWN store)
// plus the two `#6765` never-a-bare-continue tests at the end of this file.

#[test]
fn session_id_exists_true_for_real_jsonl_file() {
    // Why (#2013): the positive path — a session file present under the
    // encoded project dir must resolve as existing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("my-workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let projects_dir = tmp.path().join("projects");
    // #6777: build the fixture through the ONE encoder — a hand-rolled
    // `replace('/', "-")` here would not fold the `.` in a tempdir's name.
    let encoded = encode_project_dir(&cwd);
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
    // #6777: build the fixture through the ONE encoder — a hand-rolled
    // `replace('/', "-")` here would not fold the `.` in a tempdir's name.
    let encoded = encode_project_dir(&cwd);
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
fn encode_project_dir_folds_dot_in_worktrees_path() {
    // Why (#6777): every tm-provisioned workspace sits under `.worktrees/` or
    // `.claude/worktrees/`, and Claude Code folds the `.` to `-` exactly as it
    // folds `/`. Encoding only `/` produced `…-.worktrees-<id>` while the real
    // directory is `…--worktrees-<id>`, so `session_id_exists` answered false
    // for every managed session and `--resume` was never passed (#6765).
    //
    // Both expected names below are transcribed from a LIVE probe: `claude`
    // was launched in each of these two directories against a throwaway
    // CLAUDE_CONFIG_DIR, and these are the project directories it created.
    assert_eq!(
        encode_project_dir(Path::new("/private/tmp/tm6777/proj/.worktrees/tm-proj-01")),
        "-private-tmp-tm6777-proj--worktrees-tm-proj-01"
    );
    assert_eq!(
        encode_project_dir(Path::new(
            "/private/tmp/tm6777/tool/.claude/worktrees/agent-0123456789abcdef0"
        )),
        "-private-tmp-tm6777-tool--claude-worktrees-agent-0123456789abcdef0"
    );
}

#[test]
fn encode_project_dir_folds_every_non_alphanumeric() {
    // Why (#6777): the rule is `[^a-zA-Z0-9]` → `-`, not `/` → `-`, so every
    // punctuation class a real path can carry must fold. Established by a live
    // probe: `claude` launched in a directory literally named
    // `a_b c@d+e.f~g'h(i)` created a project dir ending `a-b-c-d-e-f-g-h-i-`.
    // Case is NOT folded — `/Users`, `/Volumes` and `Projects` all survive
    // capitalised in the live store.
    for (raw, expected) in [
        ("/a.b", "-a-b"),
        ("/a_b", "-a-b"),
        ("/a b", "-a-b"),
        ("/a@b", "-a-b"),
        ("/a+b", "-a-b"),
        ("/a~b", "-a-b"),
        ("/a'b", "-a-b"),
        ("/a(b)", "-a-b-"),
        ("/a:b", "-a-b"),
        ("/a#b", "-a-b"),
        ("/a%b", "-a-b"),
        ("/a=b", "-a-b"),
        ("/a,b", "-a-b"),
        // Already-legal characters pass through untouched.
        ("/a-b", "-a-b"),
        ("/AbZ9", "-AbZ9"),
        // One BMP non-ASCII char is one UTF-16 unit, so one dash.
        ("/café", "-caf-"),
        ("/日本", "---"),
    ] {
        assert_eq!(
            encode_project_dir(Path::new(raw)),
            expected,
            "encoding {raw} must fold to {expected}"
        );
    }
}

#[test]
fn encode_project_dir_folds_astral_char_to_two_dashes() {
    // Why (#6777): Claude Code runs the fold with a JavaScript regex, which
    // walks UTF-16 code units. A non-BMP character is one Rust `char` but two
    // UTF-16 units, so it must contribute TWO dashes, not one. Live probe: a
    // directory named `emo-🚀x` produced a project dir ending `emo---x`.
    assert_eq!(encode_project_dir(Path::new("/emo-🚀x")), "-emo---x");
}

#[test]
fn encode_project_dir_truncates_and_hashes_a_long_path() {
    // Why (#6777): past 200 characters Claude Code keeps the first 200 and
    // appends `-<base36 of abs(int32 path hash)>`. tm's own managed worktree
    // paths already reach 188 characters in the live store, so this branch is
    // reachable. Both the cwd and the expected name below are transcribed from
    // a live probe run against a throwaway CLAUDE_CONFIG_DIR — the encoder is
    // never called to build the expectation.
    let cwd = "/private/tmp/tm6777/deep/seg00/seg01/seg02/seg03/seg04/seg05/seg06/seg07/seg08/\
seg09/seg10/seg11/seg12/seg13/seg14/seg15/seg16/seg17/seg18/seg19/seg20/seg21/seg22/seg23/seg24/\
seg25/seg26/seg27/seg28/seg29/seg30/seg31/seg32/seg33/seg34/seg35/seg36/seg37/seg38/seg39";
    let expected = "-private-tmp-tm6777-deep-seg00-seg01-seg02-seg03-seg04-seg05-seg06-seg07-\
seg08-seg09-seg10-seg11-seg12-seg13-seg14-seg15-seg16-seg17-seg18-seg19-seg20-seg21-seg22-seg23-\
seg24-seg25-seg26-seg27-seg28-s-fy7046";
    let encoded = encode_project_dir(Path::new(cwd));
    assert_eq!(
        encoded, expected,
        "an over-length cwd must truncate to 200 chars plus the base36 path hash"
    );
    assert_eq!(
        encoded.len(),
        207,
        "200 kept characters, one separator, and a 6-character hash"
    );
}

#[test]
fn session_id_exists_finds_hardcoded_dir_name_for_dotted_cwd() {
    // Why (#6777): the pre-existing hand-typed guard used a DOTLESS cwd, so it
    // stayed green while every real tm worktree lookup missed. This one seeds
    // the fixture under the hand-typed name a dotted cwd really produces; the
    // pre-fix encoder yields `-repo-.worktrees-w1` and finds nothing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let projects_dir = tmp.path().join("projects");
    let project_dir = projects_dir.join("-repo--worktrees-w1");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("dotted-id.jsonl"), "{}").unwrap();
    assert!(
        session_id_exists_in(Path::new("/repo/.worktrees/w1"), &projects_dir, "dotted-id"),
        "a cwd under .worktrees/ must resolve to the '--worktrees-' directory \
             Claude Code really creates"
    );
}

#[test]
fn session_id_exists_finds_hardcoded_dir_name_for_known_cwd() {
    // Why (#2013 cleanup, MEDIUM): the other session_id_exists tests build
    // their expected path via the SAME `encode_project_dir` the
    // implementation uses, so they cannot catch a future drift in the
    // encoding scheme — the test and the code would drift together. This
    // test instead types the expected directory name BY HAND as a literal,
    // so if the encoding scheme ever changes this assertion breaks
    // independently of the implementation.
    //
    // #6777: this cwd is DOTLESS, which is why it stayed green through the
    // dot-folding defect. `session_id_exists_finds_hardcoded_dir_name_for_\
    // dotted_cwd` is the companion that covers a real worktree path.
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

// ── #2250: cd-prefix belt-and-suspenders ────────────────────────────────

// ── #2023 component D / #6766: what the pane reports when claude exits ──

// ── #2023 component C: in-place relaunch command builder ───────────────

#[test]
fn compose_inplace_args_uses_resume_for_existing_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = tmp.path().join("config");
    // #6777: build the fixture through the ONE encoder — a hand-rolled
    // `replace('/', "-")` here would not fold the `.` in a tempdir's name.
    let encoded = encode_project_dir(&cwd);
    let project_dir = config_dir.join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("existing-id.jsonl"), "{}").unwrap();

    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("existing-id"), None);
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

    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("stale-id"), None);
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

// #6765: `compose_inplace_args_uses_continue_when_no_id_but_prior_conv` was
// deleted — it asserted the defect. It seeded `~/.claude/projects` (the
// OPERATOR store) and required `--continue`, while the composed argv runs under
// the MANAGED `CLAUDE_CONFIG_DIR`. Its replacement,
// `compose_inplace_args_never_continues_from_home_store`, seeds the same wrong
// store and requires a fresh launch instead.

#[test]
fn compose_inplace_args_carries_prompt_file_unquoted() {
    // #4336: the in-place relaunch execs claude directly — no shell splits
    // this argv — so the prompt path must be its OWN token and must NOT be
    // shell-quoted the way `prompt_file_flag` quotes it for the pane-string
    // paths. A path with a space is the case that would break if the quoting
    // helper were reused here.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("workspace-prompt");
    std::fs::create_dir_all(&cwd).unwrap();
    let prompt = tmp.path().join("system prompt.txt");
    std::fs::write(&prompt, "PM").unwrap();

    let args = compose_inplace_args(&cwd, None, None, Some(&prompt));

    let idx = args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .expect("prompt flag must be present");
    assert_eq!(
        args[idx + 1],
        prompt.display().to_string(),
        "the path must be a single, UNQUOTED argv token: {args:?}"
    );
    assert!(
        !args[idx + 1].contains('\''),
        "shell quoting must not leak into an execv argv token: {args:?}"
    );
    assert!(
        args.contains(&"--dangerously-skip-permissions".to_owned()),
        "the isolation flags must still follow the prompt file: {args:?}"
    );
}

#[test]
fn compose_inplace_args_omits_prompt_flag_when_absent() {
    // A prompt-file write failure is non-fatal: the flag is omitted rather
    // than passed with an empty path (which claude would fail to open).
    let tmp = tempfile::tempdir().expect("tempdir");
    let args = compose_inplace_args(tmp.path(), None, None, None);
    assert!(
        !args.contains(&"--append-system-prompt-file".to_owned()),
        "no prompt file → no flag: {args:?}"
    );
    assert!(
        args.contains(&"--setting-sources".to_owned()),
        "isolation flags are unconditional: {args:?}"
    );
}

#[test]
fn compose_inplace_args_loads_the_user_tier_when_config_dir_is_relocated() {
    // #4451: the in-place relaunch (bare `tm` inside a managed pane) is a
    // third spawn path with its own argv builder — it must pick the same
    // relocated-tier flag, or a relaunched session silently loses every
    // bundled specialist the pane had before.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("workspace-inplace-tier");
    std::fs::create_dir_all(&cwd).unwrap();
    let config_dir = tmp.path().join("claude-config");

    let relocated = compose_inplace_args(&cwd, Some(&config_dir), None, None);
    let idx = relocated
        .iter()
        .position(|a| a == "--setting-sources")
        .expect("setting-sources flag must be present");
    assert_eq!(
        relocated[idx + 1],
        "user,project,local",
        "relocated in-place relaunch must load the tm-owned `user` tier: {relocated:?}"
    );

    let ambient = compose_inplace_args(&cwd, None, None, None);
    let idx = ambient
        .iter()
        .position(|a| a == "--setting-sources")
        .expect("setting-sources flag must be present");
    assert_eq!(
        ambient[idx + 1],
        "project,local",
        "without a relocated config dir the #1269 exclusion stands: {ambient:?}"
    );
}

#[serial_test::serial]
#[test]
fn build_inplace_resume_command_carries_prompt_file() {
    // #4336: the PM persona must reach the in-place relaunch through the same
    // `build_prompt_file` carrier `spawn`/`spawn_resume` use. Requires a real
    // claude install (resolve_claude gates the builder); skip otherwise.
    let _home = HomeGuard::set();
    if ClaudeCodeAdapter::resolve_claude().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let result = build_inplace_resume_command(tmp.path(), None).expect("build succeeds");
    let idx = result
        .args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .expect("in-place relaunch must carry the PM system prompt");
    assert!(
        std::path::Path::new(&result.args[idx + 1]).is_file(),
        "the prompt token must name a written file: {:?}",
        result.args
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

// ─── issue #4206: the trust seed must stay inside the redirected $HOME ────

/// RAII guard that prepends `dir` to `PATH` and restores it on drop.
///
/// Why it also holds `env_test_lock` (#7059): this binary had TWO disjoint
/// mutual-exclusion regimes over the process-global `PATH`. This guard relied on
/// `#[serial]` alone, while `core::gh_account_enforce` and `core::git_identity`
/// plant a fake `gh` on `PATH` under `core::trusty_tools_config::env_test_lock`
/// and take no `#[serial]`. Neither regime excludes the other, so this guard's
/// `Drop` restored the `PATH` it captured on ENTRY — erasing the fake-`gh`
/// directory a concurrent gh test had prepended after it. `Command::new("gh")`
/// then found the operator's real `gh`, and `gh auth status` either hit the
/// network past the 5 s `GH_ENFORCE_TIMEOUT` or answered "not logged in to
/// github.com account bobmatnyc". Taking BOTH the lock and `#[serial]` makes
/// `PATH` one regime, so no gh test can run while this guard is alive.
/// Test: `spawn_resume_trust_seed_stays_within_redirected_home` (this guard's
/// only caller) beside
/// `core::gh_account::enforce::tests::ensure_gh_account_in_dir_accepts_the_api_answer_over_a_stale_transcript`.
struct PathGuard {
    prev: Option<std::ffi::OsString>,
    /// Held for the guard's whole lifetime — see the type doc.
    _env: std::sync::MutexGuard<'static, ()>,
}
impl PathGuard {
    fn prepend(dir: &Path) -> Self {
        // #7059: one regime for PATH — the crate-wide env lock, not #[serial] alone.
        let env = crate::core::trusty_tools_config::env_test_lock();
        let prev = std::env::var_os("PATH");
        let mut entries = vec![dir.to_path_buf()];
        if let Some(ref p) = prev {
            entries.extend(std::env::split_paths(p));
        }
        let joined = std::env::join_paths(entries).expect("join PATH");
        // SAFETY: callers are #[serial] AND hold `env_test_lock` via `_env`.
        unsafe { std::env::set_var("PATH", joined) };
        Self { prev, _env: env }
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

/// Every `.claude.json` found anywhere beneath `root`.
///
/// Why (issue #4206): the leak assertion must catch the seeded config wherever
/// it lands under a redirected `$HOME`, not only at the one path the current
/// resolver happens to compute — a future refactor that moved the managed dir
/// would otherwise silently stop being covered.
/// What: a depth-bounded recursive walk (symlinks are not followed, so a
/// self-referential link cannot spin), returning display paths.
fn find_claude_json_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
        if depth > 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, depth + 1, out);
            } else if entry.file_name() == std::ffi::OsStr::new(".claude.json") {
                out.push(path.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

/// Plant an executable stub named `claude` in `dir` and return its directory.
///
/// Why: `ClaudeCodeAdapter::spawn_resume` calls `resolve_claude()` FIRST and
/// returns `BinaryNotFound` before ever reaching `prepare_managed_config`. The
/// pre-existing tests in this file handle that with
/// `if resolve_claude().is_none() { return; }` — a silent skip that makes them
/// vacuous on any machine without Claude Code installed (most CI runners).
/// A leak test that can silently pass by not running is worse than no test, so
/// this plants a stub and prepends its directory to `PATH`
/// (`bin_resolve::resolve_binary` honours the live `PATH` first), guaranteeing
/// the spawn path is actually entered on every platform.
#[cfg(unix)]
fn plant_fake_claude(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write fake claude");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude");
}

// Issue #4206 TEST 1 — THE ISOLATION INVARIANT, locked at the REAL call chain
// (`ClaudeCodeAdapter::spawn_resume` → `prepare_managed_config` →
// `preseed_managed_trust`): with `$HOME` redirected, trust seeding must write
// NOTHING outside that redirected `$HOME`.
//
// Why this test exists, and what the #4206 investigation actually established:
// the reported root cause was that `managed_claude_config_dir()` "takes no
// injectable base". That is NOT correct. It resolves via `dirs::home_dir()`,
// which reads `$HOME` on Unix — so `$HOME` IS the injectable base, and
// redirecting it genuinely confines the seeder (proven by running this very
// test against unpatched code: the seed landed in the redirected temp home,
// not the operator's real one). There is no production defect here; a daemon
// running with the operator's real `$HOME` correctly targets the operator's
// real config dir.
//
// The actual defect was TEST HYGIENE: several tests reach this production
// seeding path without redirecting `$HOME` at all, so they wrote straight into
// `~/.trusty-tools/trusty-mpm/claude-config/.claude.json` — which is how 2,443
// `tempfile::TempDir` entries accumulated there. Those tests are fixed in this
// same change (`daemon::managed_routes::lifecycle_tests`,
// `tests/session_manager_mvp.rs`); this test locks the invariant they were
// missing, at the layer they all funnel through, so the next test to drive
// `spawn`/`spawn_resume` has an executable statement of the rule.
//
// Note the pre-existing isolation test
// (`standalone::trust_seed_tests::test_preseed_managed_trust_no_home_write`)
// calls `preseed_managed_trust` DIRECTLY with an explicit `claude_config_dir`,
// so it can never observe what the production call chain resolves. Only a test
// that enters through `spawn_resume` covers that resolution step.
#[serial_test::serial]
#[test]
#[cfg(unix)]
fn spawn_resume_trust_seed_stays_within_redirected_home() {
    let home = tempfile::tempdir().expect("home tempdir");
    let bindir = tempfile::tempdir().expect("bin tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");

    plant_fake_claude(bindir.path());
    let _path = PathGuard::prepend(bindir.path());

    // Redirect HOME to an EMPTY dir — the isolation seam every test that
    // drives this path must use.
    let prev_home = std::env::var_os("HOME");
    // SAFETY: #[serial].
    unsafe { std::env::set_var("HOME", home.path()) };
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }
    let _home_guard = RestoreHome(prev_home);

    // The managed dir this run must resolve to, derived the same way
    // production does, AFTER the redirect above.
    let base = crate::core::trusty_tools_config::managed_claude_config_dir()
        .expect("managed config dir resolves under the redirected HOME");
    assert!(
        base.starts_with(home.path()),
        "precondition: the resolved managed dir must sit under the redirected \
         HOME, else this test is not isolated at all (resolved {})",
        base.display()
    );

    // Sanity: the binary really is resolvable, so the spawn path below is
    // genuinely entered and this test can never pass by skipping.
    assert!(
        ClaudeCodeAdapter::resolve_claude().is_some(),
        "fake claude must be resolvable on PATH — otherwise this test is vacuous"
    );

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), None);
    adapter
        .spawn_resume(
            "tmpm-4206",
            None,
            workspace.path(),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("spawn_resume must succeed against the fake tmux");

    // THE ISOLATION ASSERTION: every `.claude.json` written by this run must
    // sit under the redirected $HOME — none anywhere else.
    //
    // Scoped to `.claude.json` rather than "HOME is empty": other, unrelated
    // parts of the launch path legitimately resolve under `$HOME` (the
    // framework roster at `~/.trusty-mpm/framework`, and macOS's own
    // `~/Library` caches). Those are out of scope; asserting on them would
    // make this test fail for reasons unrelated to the config leak.
    let found = find_claude_json_files(home.path());
    assert!(
        found.iter().all(|p| Path::new(p).starts_with(home.path())),
        "every seeded .claude.json must live under the redirected HOME: {found:?}"
    );

    // Positive counterpart: the seed really did happen, at the resolved
    // managed dir. Without this the test could pass simply by never seeding
    // anything — the exact way an isolation test goes vacuous.
    let seeded = base.join(".claude.json");
    let text = std::fs::read_to_string(&seeded).unwrap_or_else(|e| {
        panic!(
            ".claude.json must be seeded at the resolved managed dir {}: {e}",
            seeded.display()
        )
    });
    let val: serde_json::Value = serde_json::from_str(&text).expect("seeded config is valid JSON");
    let key = workspace.path().to_string_lossy().to_string();
    assert_eq!(
        val["projects"][&key]["hasTrustDialogAccepted"],
        serde_json::Value::Bool(true),
        "the workspace must actually be trust-seeded under the redirected HOME: {text}"
    );
}

/// // #4181 (ADR-0042): the daemon spawn path writes NO workspace `.mcp.json`
/// and NO MCP approval.
///
/// Why: `prepare_managed_config` was the SECOND injector call site, and the
/// sharper one — `spawn_resume` and `build_inplace_resume_command` reach it with
/// no `prepare_session*` anywhere in their chain, so a deletion that only gutted
/// `prepare_session_inner` would have left every resume still injecting and
/// still approving. This is the successor to
/// `prepare_managed_config_pins_all_builtins_on_success` and
/// `prepare_managed_config_excludes_builtins_when_mcp_json_write_fails`, whose
/// subject — per-run pin evidence feeding an approval — no longer exists.
/// What: runs the real function under a redirected `$HOME` and asserts the
/// workspace gains no `.mcp.json` while the config dir's project entry carries
/// the trust keys and no `enabledMcpjsonServers`.
/// Test: itself.
#[serial_test::serial]
#[test]
fn prepare_managed_config_writes_no_mcp_json_and_no_approval() {
    let _home = HomeGuard::set();
    let cwd_root = tempfile::tempdir().expect("tempdir");
    let cwd = cwd_root.path();

    let config_dir = prepare_managed_config_with_exe(
        "test-session",
        cwd,
        Some(std::path::Path::new(crate::test_support::STABLE_HOOK_EXE)),
    )
    .expect("prepare_managed_config must resolve a config dir under the redirected HOME");

    assert!(
        !cwd.join(".mcp.json").exists(),
        "the daemon spawn path must not write a workspace .mcp.json"
    );

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config_dir.join(".claude.json"))
            .expect("prepare_managed_config must write <config_dir>/.claude.json"),
    )
    .expect("<config_dir>/.claude.json must be valid JSON");
    let key = cwd.to_string_lossy().to_string();
    let entry = &value["projects"][&key];
    assert_eq!(
        entry["hasTrustDialogAccepted"],
        serde_json::json!(true),
        "#1269's trust half still runs: {entry}"
    );
    assert!(
        entry.get("enabledMcpjsonServers").is_none(),
        "no MCP name may be pre-approved on the daemon path: {entry}"
    );

    // The four builtins stay reachable — declared once in the user-scope map
    // `seed_builtin_servers` writes (#5406), which is what the relocated spawn
    // reads under `--setting-sources user,project,local`.
    let servers = value["mcpServers"]
        .as_object()
        .expect("the user-scope mcpServers map is seeded");
    for name in [
        "trusty-mpm",
        "trusty-review",
        "trusty-memory",
        "trusty-search",
    ] {
        assert!(
            servers.contains_key(name),
            "{name} must be declared in user scope: {servers:?}"
        );
    }
}

/// THE #7490 REGRESSION TEST at the seam every resume path shares — fails on
/// the pre-fix code.
///
/// Why: `prepare_managed_config_with_exe` is what `spawn`, `spawn_resume` and
/// the bare-`tm` in-place relaunch all reach, and before #7490 it provisioned
/// the tm-owned config dir and left the PROJECT's `.claude/settings.json`
/// exactly as it found it. A project whose `SessionStart` array predated the
/// `tm hook` group therefore never gained it, however many times it was
/// resumed — no savings row under the live Claude session id, no 💸 segment.
/// What: seeds the incident file (memory hook only under `SessionStart`),
/// runs the real function under a redirected `$HOME` with a pinned
/// installed-looking hook binary, and asserts the lifecycle entry arrived and
/// the project's own entry stayed.
/// Test: itself.
#[serial_test::serial]
#[test]
fn prepare_managed_config_merges_the_project_hook_group() {
    let _home = HomeGuard::set();
    let cwd_root = tempfile::tempdir().expect("tempdir");
    let cwd = cwd_root.path();
    let claude = cwd.join(".claude");
    std::fs::create_dir_all(&claude).expect("create .claude");
    let settings = claude.join("settings.json");
    std::fs::write(
        &settings,
        serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook --pm-guard" }] }
                ],
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "trusty-memory inbox-check" }] }
                ]
            }
        })
        .to_string(),
    )
    .expect("seed settings");

    prepare_managed_config_with_exe(
        "test-session",
        cwd,
        Some(std::path::Path::new(crate::test_support::STABLE_HOOK_EXE)),
    )
    .expect("prepare_managed_config must resolve a config dir under the redirected HOME");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read settings"))
            .expect("settings is valid JSON");
    let session_start = after["hooks"]["SessionStart"].to_string();
    assert!(
        session_start.contains(&format!("{} hook\"", crate::test_support::STABLE_HOOK_EXE)),
        "every session-composition path must merge the SessionStart tm-hook group: {session_start}"
    );
    assert!(
        session_start.contains("trusty-memory inbox-check"),
        "the project's own SessionStart entry must survive: {session_start}"
    );
}

// ── #6765: a managed relaunch never emits a bare `--continue` ───────────

#[serial_test::serial]
#[test]
fn compose_inplace_args_never_continues_from_home_store() {
    // Why (#6765): the in-place relaunch exports the MANAGED
    // `CLAUDE_CONFIG_DIR`, so `claude --continue` resolves "most recent
    // conversation" against `<config_dir>/projects`, NOT `~/.claude/projects`.
    // The old eligibility check read `~/.claude/projects` — always populated on
    // an operator machine — so `--continue` fired unconditionally and attached
    // to whatever the managed store held most recently. In the reported case
    // that was a live `claude agents` daemon, which refused the second attach
    // and exited 0, dropping the pane to a bare shell.
    //
    // With no usable id the only safe selection is a FRESH launch: never a bare
    // `--continue`, whatever `~/.claude` happens to contain.
    let _home = HomeGuard::set();
    let home = dirs::home_dir().expect("home resolves under redirected HOME");
    let cwd = std::path::PathBuf::from("/tmp/inplace-6765-test");
    // #6777: build the fixture through the ONE encoder — a hand-rolled
    // `replace('/', "-")` here would not fold the `.` in a tempdir's name.
    let encoded = encode_project_dir(&cwd);
    // Populate the OPERATOR store (the wrong one) with prior history.
    let home_project_dir = home.join(".claude").join("projects").join(&encoded);
    std::fs::create_dir_all(&home_project_dir).unwrap();
    std::fs::write(home_project_dir.join("some-other-session.jsonl"), "{}").unwrap();
    // The managed store the spawned process will actually read stays empty.
    let config_dir = home.join("managed-config-6765");
    std::fs::create_dir_all(config_dir.join("projects")).unwrap();

    // (a) a null claude_session_id starts fresh — neither flag.
    let args = compose_inplace_args(&cwd, Some(&config_dir), None, None);
    assert!(
        !args.contains(&"--continue".to_owned()),
        "a null claude_session_id must never emit a bare --continue: {args:?}"
    );
    assert!(
        !args.contains(&"--resume".to_owned()),
        "a null claude_session_id must not emit --resume either: {args:?}"
    );

    // (b) an id absent from the session's OWN store also starts fresh.
    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("stale-id-6765"), None);
    assert!(
        !args.contains(&"--continue".to_owned()),
        "a stale id must fall back to a fresh launch, not --continue: {args:?}"
    );
    assert!(
        !args.contains(&"--resume".to_owned()),
        "a stale id must not be passed to --resume: {args:?}"
    );

    // (c) an id that DOES exist in the session's own store still resumes by id.
    let managed_project_dir = config_dir.join("projects").join(&encoded);
    std::fs::create_dir_all(&managed_project_dir).unwrap();
    std::fs::write(managed_project_dir.join("live-id-6765.jsonl"), "{}").unwrap();
    let args = compose_inplace_args(&cwd, Some(&config_dir), Some("live-id-6765"), None);
    assert!(
        args.windows(2).any(|w| w == ["--resume", "live-id-6765"]),
        "an id present in the session's own store must resume by id: {args:?}"
    );
    assert!(
        !args.contains(&"--continue".to_owned()),
        "--resume must never be paired with --continue: {args:?}"
    );
}

// ── #8233: the adapter's observable output is a short line + a launch spec ──

/// Drive `spawn` against a planted `claude` under a redirected `$HOME`.
///
/// Why: `spawn` provisions the real managed config dir and now also WRITES a
/// launch spec, both resolved under `$HOME`. Redirecting it keeps every side
/// effect inside a throwaway directory.
/// What: returns the single line the fake recorded.
fn drive_spawn(fake: &std::sync::Arc<FakeTmux>, home: &HomeGuard) -> String {
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(true));
    adapter
        .spawn(
            "tm-sess",
            home.home(),
            "task",
            TEST_SESSION_ID,
            &[("GH_TOKEN".to_owned(), "gho_fake".to_owned())],
        )
        .expect("spawn must succeed with claude on PATH");
    only_line(fake)
}

/// #8233: the whole point — the pane is typed a FIXED-SHAPE line, and the
/// launch's parameters are in the spec it names.
#[test]
#[serial_test::serial]
fn spawn_sends_the_parameterized_launch_line() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let line = drive_spawn(&fake, &home);

    assert!(
        line.len() < 512,
        "the typed line must stay far under MAX_CANON; got {} for {line}",
        line.len()
    );
    assert!(
        line.contains("internal-spawn-disclaimed --launch-spec '"),
        "#2997: claude must still be spawned by the disclaim shim: {line}"
    );
    assert!(
        !line.contains("--dangerously-skip-permissions"),
        "no launch flag may appear in the typed line: {line}"
    );
    let spec = sent_spec(&line);
    assert_eq!(spec.session_id, TEST_SESSION_ID);
    assert_eq!(spec.cwd, home.home());
}

/// #4467, requirement 9: env-scrub coverage must survive the migration. The
/// scrub is now an `env_unset` list on the spec rather than `-u` flags on a
/// shell line; this asserts the SAME invariant against the SAME marker list.
#[test]
#[serial_test::serial]
fn spawn_sends_env_scrub_when_binary_available() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let spec = sent_spec(&drive_spawn(&fake, &home));

    assert!(
        spec.env_unset.contains(&"ANTHROPIC_API_KEY".to_owned()),
        "DOC-34: the API key must be stripped: {:?}",
        spec.env_unset
    );
    for marker in crate::core::claude_env_scrub::INHERITED_SESSION_MARKERS {
        assert!(
            spec.env_unset.iter().any(|n| n == marker),
            "#4467: {marker} must be scrubbed: {:?}",
            spec.env_unset
        );
    }
    assert!(
        !spec.env_unset.iter().any(|n| n == "CLAUDE_CONFIG_DIR"),
        "#4451/#4455: the relocation must never be scrubbed: {:?}",
        spec.env_unset
    );
}

/// #3025/#6668: the pinned `gh` identity must reach `claude`'s environment, and
/// the token must appear nowhere in what is typed at the pane.
#[test]
#[serial_test::serial]
fn spawn_keeps_the_gh_token_out_of_the_typed_line() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let line = drive_spawn(&fake, &home);
    assert!(
        !line.contains("gho_fake"),
        "no secret may be typed into the pane: {line}"
    );
    let spec = sent_spec(&line);
    assert!(
        spec.env_set
            .iter()
            .any(|(k, v)| k == "GH_TOKEN" && v == "gho_fake"),
        "the pinned identity must still reach claude: {:?}",
        spec.env_set
    );
}

/// #8233 fail-closed: a send the driver refuses must ERROR, so the caller marks
/// the record errored instead of leaving it Active with no runtime.
#[test]
#[serial_test::serial]
fn spawn_errors_when_the_line_is_refused() {
    use crate::session_manager::{ManagedError, ManagedTmuxDriver};

    struct Refusing;
    impl ManagedTmuxDriver for Refusing {
        fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
            Err(ManagedError::TmuxUnavailable("refused".into()))
        }
        fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }
    }

    let home = HomeGuard::set();
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);

    let adapter = ClaudeCodeAdapter::new(std::sync::Arc::new(Refusing), Some(true));
    let err = adapter
        .spawn("tm-sess", home.home(), "task", TEST_SESSION_ID, &[])
        .expect_err("a refused send must not report a successful spawn");
    assert!(matches!(err, RuntimeError::TmuxUnavailable(_)), "{err}");
}

/// #1744/#2013: a stored id that still resolves on disk resumes by id.
#[test]
#[serial_test::serial]
fn spawn_resume_with_id_uses_resume_flag() {
    let home = HomeGuard::set();
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);

    // Seed the conversation the id names, under the managed config dir the
    // resume will resolve — otherwise the staleness check drops the flag.
    let cwd = home.home().join("ws");
    std::fs::create_dir_all(&cwd).expect("mkdir ws");
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir()
        .expect("a redirected HOME must resolve");
    let projects = config_dir.join("projects").join(encode_project_dir(&cwd));
    std::fs::create_dir_all(&projects).expect("mkdir projects");
    std::fs::write(projects.join("conv-77.jsonl"), b"{}").expect("seed conversation");

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(true));
    adapter
        .spawn_resume(
            "tm-sess",
            Some("%3"),
            &cwd,
            "task",
            Some("conv-77"),
            TEST_SESSION_ID,
            &[],
        )
        .expect("resume must succeed");

    let pane_sends = fake.pane_sends.lock().expect("pane send log").clone();
    assert_eq!(
        pane_sends.len(),
        1,
        "#2456: a known pane_id must be targeted directly: {pane_sends:?}"
    );
    assert_eq!(pane_sends[0].1, "%3");
    let spec = sent_spec(&pane_sends[0].2);
    assert!(
        has_flag_pair(&spec, "--resume", "conv-77"),
        "a live id must resume by id: {:?}",
        spec.args
    );
    assert!(
        !spec.args.iter().any(|a| a == "--continue"),
        "#6765: never a bare --continue: {:?}",
        spec.args
    );
}

/// #2013: a stale id must fall back to a FRESH launch, not a hard failure —
/// and #2456's session-scoped fallback stands when no pane id is recorded.
#[test]
#[serial_test::serial]
fn spawn_resume_falls_back_to_session_target_when_pane_id_unknown() {
    let home = HomeGuard::set();
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(true));
    adapter
        .spawn_resume(
            "tm-sess",
            None,
            home.home(),
            "task",
            Some("no-such-conversation"),
            TEST_SESSION_ID,
            &[],
        )
        .expect("a stale id must still launch");

    let sends = fake.sends.lock().expect("send log").clone();
    assert_eq!(
        sends.len(),
        1,
        "no pane id recorded → the session-scoped send: {sends:?}"
    );
    let spec = sent_spec(&sends[0].1);
    assert!(
        !spec
            .args
            .iter()
            .any(|a| a == "--resume" || a == "--continue"),
        "a stale id must launch fresh: {:?}",
        spec.args
    );
    assert!(
        spec.args
            .iter()
            .any(|a| a == crate::core::model_inject::PERMISSION_MODE_FLAG),
        "a fresh launch still carries the isolation flags: {:?}",
        spec.args
    );
}

/// #2230: the PM system prompt reaches the RESUME path too — before that fix
/// only `spawn` carried it and every resumed session ran vanilla Claude Code.
#[test]
#[serial_test::serial]
fn spawn_resume_sends_prompt_file_when_binary_available() {
    let home = HomeGuard::set();
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(true));
    adapter
        .spawn_resume(
            "tm-sess",
            None,
            home.home(),
            "task",
            None,
            TEST_SESSION_ID,
            &[],
        )
        .expect("resume must succeed");

    let spec = sent_spec(&only_line(&fake));
    let flag = spec
        .args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .expect("the resume must carry the PM prompt");
    assert!(
        !spec.args[flag + 1].contains('\''),
        "the path is an argv token, never shell-quoted: {:?}",
        spec.args
    );
}

/// #7685: the adapter must use the reachability the LAUNCH resolved rather than
/// probing trusty-memory a second time.
#[test]
#[serial_test::serial]
fn spawn_uses_the_launch_resolved_reachability() {
    let home = HomeGuard::set();
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);

    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(false));
    adapter
        .spawn("tm-sess", home.home(), "task", TEST_SESSION_ID, &[])
        .expect("spawn must succeed");

    let spec = sent_spec(&only_line(&fake));
    assert!(
        !spec
            .env_set
            .iter()
            .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_AUTO_MEMORY"),
        "an unreachable trusty-memory must leave auto memory on: {:?}",
        spec.env_set
    );
}

/// #2157 item 1: the durable `tmux set-environment` publish is belt-and-braces
/// alongside the pane-shell export the launch line still carries.
#[test]
#[serial_test::serial]
fn spawn_publishes_session_id_via_set_environment() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let line = drive_spawn(&fake, &home);
    assert!(
        line.starts_with(&format!(
            "export {}='{TEST_SESSION_ID}'; ",
            crate::core::harness_root::MANAGED_SESSION_ID_ENV
        )),
        "#2023 B: the export must land in the PANE's own shell: {line}"
    );
    let env_sets = fake.env_sets.lock().expect("env-set log").clone();
    assert!(
        env_sets
            .iter()
            .any(|(_, k, v)| k == "TM_MANAGED_SESSION_ID" && v == TEST_SESSION_ID),
        "the durable publish must still happen: {env_sets:?}"
    );
}

/// Drive `spawn_resume` under a redirected `$HOME` with a planted `claude`.
fn drive_resume(
    fake: &std::sync::Arc<FakeTmux>,
    home: &HomeGuard,
    cwd: &std::path::Path,
    pane_id: Option<&str>,
    claude_session_id: Option<&str>,
) {
    let bin_dir = home.home().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");
    plant_fake_claude(&bin_dir);
    let _path = PathGuard::prepend(&bin_dir);
    let adapter = ClaudeCodeAdapter::new(fake.clone(), Some(true));
    adapter
        .spawn_resume(
            "tm-sess",
            pane_id,
            cwd,
            "task",
            claude_session_id,
            TEST_SESSION_ID,
            &[],
        )
        .expect("resume must succeed with claude on PATH");
}

/// #2456: the record's own pane must be targeted, not tmux's "active" pane.
/// `spawn_resume_with_id_uses_resume_flag` asserts the FLAG; this asserts the
/// TARGET, so a regression in either names the invariant it broke.
#[test]
#[serial_test::serial]
fn spawn_resume_targets_stored_pane_id_when_known() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let cwd = home.home().to_path_buf();
    drive_resume(&fake, &home, &cwd, Some("%9"), None);

    assert!(
        fake.sends.lock().expect("send log").is_empty(),
        "a known pane id must never take the session-scoped path"
    );
    let pane_sends = fake.pane_sends.lock().expect("pane send log").clone();
    assert_eq!(pane_sends.len(), 1);
    assert_eq!(pane_sends[0].1, "%9");
}

/// #6765: with no usable id the resume launches FRESH — never a bare
/// `--continue`, which would resolve "most recent" against a managed store this
/// process did not choose.
#[test]
#[serial_test::serial]
fn spawn_resume_never_sends_bare_continue() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let cwd = home.home().to_path_buf();
    drive_resume(&fake, &home, &cwd, None, None);
    let spec = sent_spec(&only_line(&fake));
    assert!(
        !spec.args.iter().any(|a| a == "--continue"),
        "never a bare --continue: {:?}",
        spec.args
    );
}

/// #6765: no stored id and no prior conversation is a PLAIN spawn — the same
/// argv a fresh session gets, with no resume selection appended.
#[test]
#[serial_test::serial]
fn spawn_resume_without_id_no_prior_conv_sends_plain_spawn() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let cwd = home.home().to_path_buf();
    drive_resume(&fake, &home, &cwd, None, None);
    let spec = sent_spec(&only_line(&fake));
    assert!(
        !spec.args.iter().any(|a| a == "--resume" || a == "attach"),
        "a plain spawn carries no resume selection: {:?}",
        spec.args
    );
    assert!(
        spec.args
            .iter()
            .any(|a| a == crate::core::model_inject::PERMISSION_MODE_FLAG),
        "and it still carries the isolation flags: {:?}",
        spec.args
    );
}

/// #2013, restored by the #8233 review (MEDIUM): a stored `claude_session_id`
/// goes stale when its conversation is pruned or the workspace moves, and
/// `claude --resume <missing>` fails hard with no recovery. The daemon resume
/// path must therefore existence-check the id and launch FRESH when it does not
/// resolve. The deleted `spawn_resume_with_missing_id_falls_back_gracefully`
/// asserted this against the old shell string; this asserts it against the spec
/// the pane will actually consume, which is strictly sharper.
#[test]
#[serial_test::serial]
fn spawn_resume_drops_a_stale_stored_id() {
    let home = HomeGuard::set();
    let fake = FakeTmux::new();
    let cwd = home.home().to_path_buf();
    // No transcript is seeded for this id anywhere under the redirected home.
    drive_resume(&fake, &home, &cwd, None, Some("pruned-conversation"));

    let spec = sent_spec(&only_line(&fake));
    assert!(
        !spec.args.iter().any(|a| a == "--resume"),
        "a stale id must not reach `claude --resume`, which fails hard: {:?}",
        spec.args
    );
    assert!(
        !spec.args.iter().any(|a| a == "pruned-conversation"),
        "and the id itself must appear nowhere in the argv: {:?}",
        spec.args
    );
}

/// #2246: every resumed / guided-resume / crash-recovery session must carry the
/// OAuth token too, or exactly those sessions keep the login loop `spawn` was
/// fixed against. #8233: it reaches `claude` through the spec, and must appear
/// nowhere in the typed text.
#[test]
#[serial_test::serial]
fn spawn_resume_sends_oauth_token_when_available() {
    let home = HomeGuard::set();
    // The resolver reads this env var first (see `core::oauth_token`).
    // SAFETY: #[serial] and the PathGuard's env lock serialise env mutation.
    unsafe { std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-test") };
    let fake = FakeTmux::new();
    let cwd = home.home().to_path_buf();
    drive_resume(&fake, &home, &cwd, None, None);
    let line = only_line(&fake);
    unsafe { std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN") };

    assert!(
        !line.contains("sk-ant-oat01-test"),
        "no token may be typed into the pane: {line}"
    );
    let spec = sent_spec(&line);
    assert!(
        spec.env_set.iter().any(|(k, v)| {
            k == crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR && v == "sk-ant-oat01-test"
        }),
        "the token must reach claude's environment: {:?}",
        spec.env_set
    );
}

/// #6777 regression guard: a worktree cwd contains a dot segment
/// (`.claude/worktrees/…`), and an encoder that folded only `/` produced a
/// directory name one character short — so the stored id looked stale and the
/// resume silently launched fresh. Driving the REAL adapter against a seeded
/// transcript is what makes this non-circular: it fails if `encode_project_dir`
/// and Claude Code's own `sanitizePath` ever disagree again.
#[test]
#[serial_test::serial]
fn spawn_resume_uses_resume_flag_for_a_worktree_cwd() {
    let home = HomeGuard::set();
    let cwd = home.home().join("repo/.claude/worktrees/agent-1");
    std::fs::create_dir_all(&cwd).expect("mkdir worktree");
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir()
        .expect("a redirected HOME must resolve");
    let projects = config_dir.join("projects").join(encode_project_dir(&cwd));
    std::fs::create_dir_all(&projects).expect("mkdir projects");
    std::fs::write(projects.join("conv-wt.jsonl"), b"{}").expect("seed conversation");

    let fake = FakeTmux::new();
    drive_resume(&fake, &home, &cwd, None, Some("conv-wt"));
    let spec = sent_spec(&only_line(&fake));
    assert!(
        has_flag_pair(&spec, "--resume", "conv-wt"),
        "a dotted worktree cwd must still resolve its own transcript: {:?}",
        spec.args
    );
}
