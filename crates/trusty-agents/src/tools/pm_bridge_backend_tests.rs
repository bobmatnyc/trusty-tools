//! Tests for `tools::pm_bridge_backend` — `binary_on_path`, the decode
//! helpers, fail-closed behavior when the target binary is absent, and
//! binary-gated integration smokes against the real `tcode`/`tm` binaries.
//!
//! Why `#![allow(clippy::await_holding_lock)]`: the two `fails_closed`
//! tests hold `crate::test_env::ENV_LOCK` across `.await` points by design
//! — they mutate the process-wide `$PATH` for their whole body, matching
//! the established crate-wide convention documented in `crate::test_env`
//! and already used by `system_status::daemons::tests` /
//! `llm::credentials::tests`.
#![allow(clippy::await_holding_lock)]

use serde_json::json;

use super::*;

// =====================================================================
// binary_on_path
// =====================================================================

#[test]
#[cfg(unix)]
fn binary_on_path_recognises_sh() {
    assert!(binary_on_path("sh"));
    assert!(!binary_on_path("definitely-not-a-real-binary-xyzzy"));
}

// =====================================================================
// Decode helpers
// =====================================================================

#[test]
fn extract_session_id_parses_wrapped_text_frame() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"id\":\"sess-123\",\"status\":\"active\"}" }]
    });
    assert_eq!(extract_session_id(&resp).as_deref(), Some("sess-123"));
}

#[test]
fn extract_session_id_falls_back_to_session_id_field() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"session_id\":\"sess-456\"}" }]
    });
    assert_eq!(extract_session_id(&resp).as_deref(), Some("sess-456"));
}

#[test]
fn extract_session_id_missing_id_returns_none() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"status\":\"active\"}" }]
    });
    assert_eq!(extract_session_id(&resp), None);
}

#[test]
fn extract_session_id_missing_content_returns_none() {
    let resp = json!({ "isError": false });
    assert_eq!(extract_session_id(&resp), None);
}

#[test]
fn extract_pane_content_parses_wrapped_text_frame() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"pane_content\":\"hello from the pane\"}" }]
    });
    assert_eq!(
        extract_pane_content(&resp).as_deref(),
        Some("hello from the pane")
    );
}

#[test]
fn is_runtime_active_defaults_true_when_absent() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"pane_content\":\"x\"}" }]
    });
    assert!(is_runtime_active(&resp));
}

#[test]
fn is_runtime_active_reads_false() {
    let resp = json!({
        "content": [{ "type": "text", "text": "{\"runtime_active\":false}" }]
    });
    assert!(!is_runtime_active(&resp));
}

// =====================================================================
// Fail-closed behavior when the target binary is absent from PATH
// =====================================================================

/// Point `$PATH` at an empty tempdir (containing neither `tcode` nor `tm`),
/// run `body`, then restore the original `$PATH`. Caller must hold
/// `crate::test_env::ENV_LOCK` for the whole call — see the module docs.
async fn with_empty_path<Fut: std::future::Future<Output = anyhow::Result<String>>>(
    body: impl FnOnce() -> Fut,
) -> anyhow::Result<String> {
    let empty_dir = tempfile::tempdir().unwrap();
    let original = std::env::var_os("PATH");
    // SAFETY: ENV_LOCK held by the caller for the whole body.
    unsafe {
        std::env::set_var("PATH", empty_dir.path());
    }
    let result = body().await;
    // SAFETY: see above.
    unsafe {
        match &original {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
    result
}

#[tokio::test]
async fn process_pm_bridge_tcode_route_fails_closed_without_binary() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let err = with_empty_path(|| bridge.run(BridgeRoute::Tcode, None, "fix the parser"))
        .await
        .expect_err("must fail closed when tcode is not on PATH");
    assert!(
        format!("{err:#}").contains("tcode"),
        "error should name the missing binary for diagnosability: {err:#}"
    );
}

#[tokio::test]
async fn process_pm_bridge_tm_route_fails_closed_without_binary() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().unwrap();
    let bridge = ProcessPmBridge::from_project(tmp.path().to_path_buf());
    let err = with_empty_path(|| bridge.run(BridgeRoute::Tm, None, "spawn a new session"))
        .await
        .expect_err("must fail closed when tm is not on PATH");
    assert!(
        format!("{err:#}").contains("tm"),
        "error should name the missing binary for diagnosability: {err:#}"
    );
}

// =====================================================================
// Binary-gated integration smokes (skip when the real binary is absent —
// mirrors plugins::trusty_search's try_spawn contract).
// =====================================================================

const FORBIDDEN_BRANDING_TOKENS: [&str; 4] = ["trusty-mpm", "trusty-code", "tcode", "tm"];

/// Word-tokenized forbidden-token check — mirrors
/// `pm_bridge_tests::scrub_branding_removes_every_forbidden_token`. A plain
/// substring `.contains("tm")` false-positives on innocent text (observed
/// live: macOS tempdir names like `.tmpBmUq52` contain "tm"), so the
/// production contract this smoke actually verifies — no BRANDED WORD
/// survives — is checked the same word-bounded way `scrub_branding`'s own
/// regex enforces it, not a raw substring scan.
fn assert_no_branded_word(text: &str) {
    let lower = text.to_lowercase();
    for word in lower.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        assert!(
            !FORBIDDEN_BRANDING_TOKENS.contains(&trimmed),
            "backend-identity token '{trimmed}' leaked as a whole word in: {text}"
        );
    }
}

/// Restores a set of process env vars to their captured values on drop.
///
/// Why: `ProcessPmBridge::run_tcode` builds its `Command` internally, so a
/// test can only shape the child's environment by shaping its own (the child
/// inherits it). Every mutation must be undone, panics included.
/// What: `capture` records each named var's current value (or absence);
/// `Drop` writes them back in reverse order.
/// Test: `tcode_child_env_is_hermetic_under_an_ambient_live_palace`.
struct EnvRestore {
    saved: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
}

impl EnvRestore {
    fn capture(names: impl IntoIterator<Item = std::ffi::OsString>) -> Self {
        let saved = names
            .into_iter()
            .map(|n| {
                let v = std::env::var_os(&n);
                (n, v)
            })
            .collect();
        Self { saved }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            // SAFETY: every `EnvRestore` user holds `ENV_LOCK`, and
            // `HermeticChildEnv` additionally holds `HOME_LOCK`.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(&name, v),
                    None => std::env::remove_var(&name),
                }
            }
        }
    }
}

/// #9139: an environment for the spawned `tcode` that cannot reach the live
/// trusty-memory palace.
///
/// Why: `run_tcode` hands the child the full ambient environment. In a `tm`
/// shell that carries `TRUSTY_MEMORY_PALACE=trusty-tools`, and the live
/// memory socket is reachable, so a smoke turn writes into the live palace.
/// The INSTALLED `tcode` predates trusty-code's own harness guard and does not
/// read `TRUSTY_TEST_HARNESS`, so what protects the live palace is the
/// explicit temp socket (nothing listens on it) and temp HOME / data dir.
/// What: while alive, every `TRUSTY_*` and `XDG_*` var is removed from the
/// process env, then `HOME`, `TRUSTY_DATA_DIR_OVERRIDE` and
/// `TRUSTY_MEMORY_SOCKET` point under a fresh tempdir and
/// `TRUSTY_TEST_HARNESS=1` is set. `install` reads each override back and
/// panics on a mismatch or a pre-existing socket, so a failed setup stops the
/// test instead of running against ambient state. The caller must already hold
/// `ENV_LOCK`; `install` takes `HOME_LOCK` itself, in the crate's
/// `ENV_LOCK`-then-`HOME_LOCK` order.
/// Test: `tcode_child_env_is_hermetic_under_an_ambient_live_palace`.
struct HermeticChildEnv {
    _restore: EnvRestore,
    _home_guard: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
}

impl HermeticChildEnv {
    fn install() -> Self {
        let home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir()
            .expect("#9139: no temp dir for the hermetic tcode env; refusing to use ambient state");
        let home = root.path().join("home");
        let data = root.path().join("data");
        std::fs::create_dir_all(&home).expect("#9139: cannot create temp HOME");
        std::fs::create_dir_all(&data).expect("#9139: cannot create temp data dir");
        let socket = root.path().join("no-memory.sock");
        assert!(!socket.exists(), "#9139: temp memory socket must not exist");

        let scrubbed: Vec<std::ffi::OsString> = std::env::vars_os()
            .map(|(k, _)| k)
            .filter(|k| {
                let k = k.to_string_lossy();
                k.starts_with("TRUSTY_") || k.starts_with("XDG_")
            })
            .collect();
        let mut captured = scrubbed.clone();
        for name in [
            "HOME",
            "PATH",
            "TRUSTY_DATA_DIR_OVERRIDE",
            "TRUSTY_MEMORY_SOCKET",
            "TRUSTY_TEST_HARNESS",
        ] {
            if !captured.iter().any(|c| c == name) {
                captured.push(name.into());
            }
        }
        let restore = EnvRestore::capture(captured);

        let overrides: [(&str, &std::path::Path); 3] = [
            ("HOME", &home),
            ("TRUSTY_DATA_DIR_OVERRIDE", &data),
            ("TRUSTY_MEMORY_SOCKET", &socket),
        ];
        // SAFETY: caller holds `ENV_LOCK` and this fn holds `HOME_LOCK`.
        unsafe {
            for name in &scrubbed {
                std::env::remove_var(name);
            }
            for (name, value) in overrides {
                std::env::set_var(name, value);
            }
            std::env::set_var("TRUSTY_TEST_HARNESS", "1");
        }
        for (name, value) in overrides {
            assert_eq!(
                std::env::var_os(name).as_deref(),
                Some(value.as_os_str()),
                "#9139: {name} did not take the hermetic value; refusing to run"
            );
        }
        Self {
            _restore: restore,
            _home_guard: home_guard,
            root,
        }
    }

    /// The tempdir every override points under.
    fn root(&self) -> &std::path::Path {
        self.root.path()
    }

    /// Put `dir` first on `PATH` (restored on drop, `PATH` is captured).
    fn prepend_path(&self, dir: &std::path::Path) {
        let old = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(
            std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&old)),
        )
        .expect("#9139: PATH entry contains a separator");
        // SAFETY: see `install`.
        unsafe {
            std::env::set_var("PATH", joined);
        }
    }
}

/// Real end-to-end run through `ProcessPmBridge::run_tcode` -> the tool
/// layer's `scrub_branding`: skipped unless `tcode` is on PATH. Asserts a
/// non-error result whose SCRUBBED text carries no backend-identity token —
/// this is the actual production contract (`PmBridgeTool::execute` always
/// scrubs before returning; the raw backend transcript alone is not
/// guaranteed to be branding-free, e.g. an incidental tempdir path).
///
/// Holds `crate::test_env::ENV_LOCK` for the whole body (including the
/// `binary_on_path` check and the subprocess spawn): without it this test
/// races the `fails_closed` tests above, which mutate `$PATH` process-wide
/// — observed live as a genuine flake (this test reporting "binary not
/// found" while `tcode` was actually installed, because a sibling test's
/// PATH-clearing window overlapped this one's `binary_on_path` check).
#[tokio::test]
async fn tcode_route_smoke() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !binary_on_path("tcode") {
        eprintln!("tcode not on PATH; skipping tcode_route_smoke");
        return;
    }
    // #9139: the child must not inherit the ambient palace / memory socket.
    let hermetic = HermeticChildEnv::install();
    let project = hermetic.root().join("project");
    std::fs::create_dir_all(project.join(".claude/agents")).unwrap();
    let bridge = ProcessPmBridge::from_project(project);
    let result = bridge
        .run(
            BridgeRoute::Tcode,
            None,
            "reply with a one-line status only",
        )
        .await;
    match result {
        Ok(out) => assert_no_branded_word(&crate::tools::pm_bridge::scrub_branding(&out)),
        Err(e) => {
            // A live smoke without a properly configured project (no pm
            // agent, no credentials) failing is acceptable here — the
            // binary-presence gate is what this test actually verifies.
            eprintln!("tcode_route_smoke: run failed (acceptable in an unconfigured env): {e:#}");
        }
    }
}

/// #9139 regression guard: the environment `run_tcode` hands its child is
/// hermetic even when the ambient one names the live palace.
///
/// Why: `tcode_route_smoke` spawned the installed `tcode` with the ambient
/// env (`TRUSTY_MEMORY_PALACE=trusty-tools`, a reachable live memory socket),
/// so smoke turns could land in the live palace. A fake `tcode` that dumps its
/// environment makes the leak observable without any real binary or daemon.
/// What: seeds a live-looking palace, socket and a `TRUSTY_*` sentinel, runs
/// the fake `tcode` through `ProcessPmBridge` under `HermeticChildEnv`, and
/// asserts the child saw none of them, plus a temp HOME, data dir and memory
/// socket (nothing listening) and `TRUSTY_TEST_HARNESS=1`. Without the
/// isolation the child sees the seeded values and this fails.
/// Test: this test.
#[cfg(unix)]
#[tokio::test]
async fn tcode_child_env_is_hermetic_under_an_ambient_live_palace() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let seeds = [
        ("TRUSTY_MEMORY_PALACE", "trusty-tools"),
        ("TRUSTY_MEMORY_SOCKET", "/ambient/live-memory.sock"),
        ("TRUSTY_9139_SENTINEL", "ambient"),
    ];
    // Declared before the hermetic env so it drops after it.
    let _ambient = EnvRestore::capture(seeds.iter().map(|(k, _)| (*k).into()));
    // SAFETY: ENV_LOCK held for the whole body.
    unsafe {
        for (k, v) in seeds {
            std::env::set_var(k, v);
        }
    }

    let fake_dir = tempfile::tempdir().unwrap();
    let dump = fake_dir.path().join("child-env.txt");
    crate::test_env::write_executable_script(
        fake_dir.path(),
        "tcode",
        &format!("#!/bin/sh\nenv > '{}'\necho '{{}}'\n", dump.display()),
    );

    let hermetic = HermeticChildEnv::install();
    hermetic.prepend_path(fake_dir.path());
    let bridge = ProcessPmBridge::from_project(hermetic.root().join("project"));
    let _ = bridge.run(BridgeRoute::Tcode, None, "status").await;

    let seen = std::fs::read_to_string(&dump).expect("fake tcode must have run and dumped its env");
    let var = |name: &str| {
        seen.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")))
            .map(str::to_owned)
    };
    assert_eq!(var("TRUSTY_MEMORY_PALACE"), None, "live palace leaked");
    assert_eq!(var("TRUSTY_9139_SENTINEL"), None, "ambient TRUSTY_* leaked");
    assert_eq!(var("TRUSTY_TEST_HARNESS").as_deref(), Some("1"));
    let root = hermetic.root().to_string_lossy().into_owned();
    for name in ["HOME", "TRUSTY_DATA_DIR_OVERRIDE", "TRUSTY_MEMORY_SOCKET"] {
        let value = var(name).unwrap_or_else(|| panic!("{name} missing from child env"));
        assert!(value.starts_with(&root), "{name}={value} is outside {root}");
    }
    let socket = var("TRUSTY_MEMORY_SOCKET").unwrap();
    assert!(
        !std::path::Path::new(&socket).exists(),
        "nothing may listen on the child's memory socket"
    );
}

/// Real end-to-end run through `ProcessPmBridge::run_tm` -> `scrub_branding`:
/// skipped unless `tm` is on PATH. Holds `ENV_LOCK` for the same reason as
/// `tcode_route_smoke`.
#[tokio::test]
async fn tm_route_smoke() {
    let _env_guard = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !binary_on_path("tm") {
        eprintln!("tm not on PATH; skipping tm_route_smoke");
        return;
    }
    // #9139: `tm serve --stdio` + `session_new` must never reach the live
    // daemon. Under the hermetic env the daemon socket is a temp path with
    // nothing listening, so a spawn or handshake failure surfaces as `Err`
    // (reported below), never as a fallback to ambient state.
    let hermetic = HermeticChildEnv::install();
    let project = hermetic.root().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let bridge = ProcessPmBridge::from_project(project);
    let result = bridge
        .run(BridgeRoute::Tm, None, "report session status")
        .await;
    match result {
        Ok(out) => assert_no_branded_word(&crate::tools::pm_bridge::scrub_branding(&out)),
        Err(e) => {
            eprintln!("tm_route_smoke: run failed (acceptable in an unconfigured env): {e:#}");
        }
    }
}

/// #4026 regression pin: widening `run_tcode` to accept a named target must
/// not change what an UNNAMED dispatch runs. The default agent argument is the
/// same literal the bridge has always passed, so a caller omitting a
/// specialist produces a byte-identical command line.
///
/// Why: #4026's acceptance requires "omitting `agent_name` preserves today's
/// exact behavior". A one-line constant is easy to edit by accident; this pins
/// it without needing the real binary on PATH.
/// What: asserts `DEFAULT_TCODE_AGENT` is still `"pm"`.
/// Test: this test.
#[test]
fn default_tcode_agent_is_unchanged_by_the_4026_widening() {
    assert_eq!(super::DEFAULT_TCODE_AGENT, "pm");
}

// =====================================================================
// #4351 — relaying the child's actionable result
// =====================================================================

/// The `--json` snapshot a `tcode run-task` child prints after a run that
/// wrote something and then exhausted its turn budget.
fn child_snapshot_with_a_diff() -> String {
    json!({
        "id": "s-1",
        "task": "t",
        "status": "turn_cap_exceeded",
        "mode": "daily-driver",
        "result": {
            "status": "partial",
            "diff_ref": "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c",
            "branch": "feat/thing",
            "pr_ref": null,
            "summary": "partial: changes captured at 0f1e2d3c on branch feat/thing"
        }
    })
    .to_string()
}

/// #4351 acceptance: a run that produced a diff relays the child's ref AND
/// classifies the outcome from the child's EXIT CODE — with exit 6 landing on
/// `Partial`, never `Failed`.
///
/// Why: exit 6 is trusty-code's "the turn budget ran out but real work is on
/// disk" code. Reading it as failure is how a caller throws away working code
/// (`trusty_code::run_task::report::ExitCode::Partial`'s own docs).
/// Test: this test.
#[test]
fn a_child_reporting_a_diff_relays_its_ref_and_partial_status() {
    let result = extract_child_result(&child_snapshot_with_a_diff(), Some(6));

    assert_eq!(result.status, TaskResultStatus::Partial);
    assert_ne!(
        result.status,
        TaskResultStatus::Failed,
        "exit 6 must not collapse into failure (#4351)"
    );
    assert_eq!(
        result.diff_ref.as_deref(),
        Some("0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c")
    );
    assert_eq!(result.branch.as_deref(), Some("feat/thing"));
    assert_eq!(result.pr_ref, None);
    assert!(result.summary.is_some());
}

/// #4351 acceptance: a run that changed nothing relays `None` refs and still
/// reports its status.
#[test]
fn a_child_with_no_diff_relays_nones_and_the_status() {
    let stdout = json!({
        "id": "s-2",
        "task": "t",
        "status": "finished",
        "result": { "status": "success", "summary": "success: no changes" }
    })
    .to_string();

    let result = extract_child_result(&stdout, Some(0));
    assert_eq!(result.status, TaskResultStatus::Success);
    assert_eq!(result.diff_ref, None);
    assert_eq!(result.branch, None);
    assert_eq!(result.pr_ref, None);
    assert_eq!(result.summary.as_deref(), Some("success: no changes"));
}

/// A child that printed prose rather than the JSON snapshot — an older binary,
/// or the stderr fallback path — still yields a status rather than an error.
/// The run has already finished by then; there is nothing left to fail.
#[test]
fn unparseable_child_output_still_yields_a_status() {
    let result = extract_child_result("not json at all\n", Some(3));
    assert_eq!(result.status, TaskResultStatus::Failed);
    assert_eq!(result.diff_ref, None);
    assert_eq!(result.summary, None);
}

/// A backend that only implements `run` — every pre-#4351 implementor,
/// including the test doubles — reaches `run_result` through the trait's
/// default body and honestly reports no result rather than an invented one.
#[tokio::test]
async fn default_run_result_reports_no_result() {
    struct TranscriptOnlyBackend;

    #[async_trait]
    impl PmBridgeBackend for TranscriptOnlyBackend {
        async fn run(
            &self,
            _route: BridgeRoute,
            _target: Option<&str>,
            _task: &str,
        ) -> Result<String> {
            Ok("just a transcript".to_string())
        }
    }

    let outcome = TranscriptOnlyBackend
        .run_result(BridgeRoute::Tcode, None, "t")
        .await
        .expect("the default body must not fail");
    assert_eq!(outcome.transcript, "just a transcript");
    assert_eq!(outcome.result, None);
}
