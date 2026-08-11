use super::search_index::register_project_index;
use super::settings::{
    clean_global_trusty_memory_hooks, deploy_output_style, is_stale_bare_statusline_command,
    is_stale_statusline_command, preseed_workspace_trust, resolve_palace_slug,
    resolve_statusline_binary_with, write_output_style, write_project_hooks, write_status_line,
};
use super::*;
use tempfile::tempdir;

/// Why: env-mutating tests previously restored the var by hand at the end of the
/// test body, so a panic between set and restore leaked process-global state
/// into sibling `#[serial]` tests. This guard restores the prior value (or
/// removes it) in `Drop`, making cleanup panic-safe.
/// What: on construction it snapshots the current value and sets the new one;
/// on drop it restores the snapshot (or removes the var if it was unset).
/// Test: used by `register_project_index_returns_derived_id`; correctness is
/// observable via that serial test passing without leaking the override env var.
///
/// `pub(super)` (issue #2914 split): also used by the sibling
/// `tests_search_index` module, which lives outside `tests`' own module
/// boundary (a private item is visible to descendants of its defining module,
/// not to siblings of that module) — see `session_launch/mod.rs`'s
/// `tests_search_index` declaration.
pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: env-mutating tests using this guard are tagged `#[serial]`, so
        // no other thread races the set/restore. Restore happens in `Drop`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Set a string-valued env var for the duration of a serial test (#1605).
    ///
    /// Why: the palace-pinning tests must set `TRUSTY_MEMORY_PALACE` to a plain
    /// string override; the path-based `set` would round-trip through a
    /// `PathBuf` unnecessarily.
    /// What: snapshots the prior value and sets `key=value`; restored in `Drop`.
    /// Test: used by `inject_trusty_memory_mcp_override_env_wins`.
    // #4255: `pub(super)` so the sibling `tests_search_index` module can opt
    // into real daemon writes via `TRUSTY_ALLOW_PRODUCTION_STATE`.
    pub(super) fn set_str(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized by `#[serial]`; restored in `Drop`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Clear an env var for the duration of a serial test, restoring it on drop.
    ///
    /// Why: the palace-derivation tests must run with no ambient
    /// `TRUSTY_MEMORY_PALACE` override (which would otherwise win over the
    /// derived slug and make the assertions non-hermetic).
    /// What: snapshots the prior value and removes the var; restored in `Drop`.
    /// Test: used by `inject_trusty_memory_mcp_pins_palace_from_repo_url`.
    fn clear(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized by `#[serial]`; restored in `Drop`.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `set` — serialized by `#[serial]`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[test]
fn build_system_prompt_includes_trusty_block() {
    // Why: `build_system_prompt` must always yield a prompt — generating
    // `INSTRUCTIONS.md` from the bundled assets on first run — and that
    // prompt must include the trusty tool-priority block so a launched
    // session knows to prefer `memory_recall` and `search`.
    let prompt = build_system_prompt().expect("trusty block is always present");
    assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
    assert!(prompt.contains("mcp__trusty-memory__memory_recall"));
    assert!(prompt.contains("mcp__trusty-search__search"));
    // The bundled PM instructions are also part of the assembled prompt.
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
}

#[test]
fn build_system_prompt_for_applies_project_override() {
    // Why: the live launch prompt must reflect the project's customization
    // (issue #381 — an advertised override that no code reads), while still
    // appending the non-overridable floor.
    //
    // #4286: the customization surface is a CLAUDE.md named section. The
    // `.trusty-mpm/INSTRUCTIONS.md` file this used to write is retired, and the
    // second half of this test now asserts it has no effect.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
         PROJECT_OVERRIDE_MARKER\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n",
    )
    .unwrap();
    // Present, and required to make no difference.
    let override_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(override_dir.join("INSTRUCTIONS.md"), "RETIRED_MARKER\n").unwrap();

    let prompt = build_system_prompt_for(project);
    assert!(prompt.contains("PROJECT_OVERRIDE_MARKER"));
    assert!(
        !prompt.contains("RETIRED_MARKER"),
        "the retired .trusty-mpm/INSTRUCTIONS.md must not reach the launch prompt"
    );
    assert!(prompt.contains("# Framework Instructions"));
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
}

#[test]
fn build_system_prompt_for_no_override_matches_bundled_sections() {
    // Why: with no override files the live prompt must still carry all
    // bundled sections and the BASE_PM floor last.
    let tmp = tempdir().unwrap();
    let prompt = build_system_prompt_for(tmp.path());
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
    assert!(prompt.contains("# Agent Delegation Routing"));
    let base = prompt.find("# Framework Instructions").expect("base");
    let deleg = prompt.find("# Agent Delegation Routing").expect("deleg");
    assert!(base > deleg, "BASE_PM floor must be last");
}

#[test]
#[serial_test::serial]
fn prepare_session_stash_reflects_override() {
    // Why: the inspectable stash (`last-instructions.md`) must reflect the
    // SAME override-resolved prompt the launch path uses, so `tm session
    // instructions` shows what was actually delivered (issue #381 / #382).
    //
    // Determinism (issue #1409): HR-4 added output-style injection at the
    // `build_system_prompt_for_with_style` seam, gated on whether `claude` is
    // installed/new enough. We therefore route through the native-pinned seams
    // (`prepare_session_with_style_and_native` /
    // `build_system_prompt_for_with_style_and_native`) and assert the invariant
    // under BOTH `native_supported = true` (no injection) AND `false`
    // (injection fires). This removes the dependence on the host's `claude` that
    // made this test pass locally but FAIL on CI (where `claude` is absent → the
    // launch prompt was injected but the stash was not, so the two diverged).
    //
    // #3965: `prepare_session_with_style_and_native` drives the real
    // `prepare_session_inner` pipeline, which seeds `$HOME/.claude.json` via
    // `preseed_workspace_trust_home` (resolved from the REAL process `$HOME`,
    // not `fw`). `#[serial]` + the per-iteration `$HOME` override below keep
    // this test from writing into the operator's real `~/.claude.json` and from
    // racing every other test in this binary that does the same.
    for native_supported in [true, false] {
        // A fresh tmp_home/project per iteration so the second run does not read
        // back a stash written by the first. Dedicated tmp_home keeps parallel
        // runs from racing on the shared ~/.claude/agents manifest.
        let tmp_home = tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", tmp_home.path());
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

        // #4286: the override arrives as a CLAUDE.md named section. The
        // retired `.trusty-mpm/WORKFLOW.md` this used to write is no longer
        // read, so it could not produce an override to observe.
        std::fs::write(
            project.join("CLAUDE.md"),
            "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
             # Custom Workflow\n\nSTASH_OVERRIDE_MARKER\n\
             <!-- TRUSTY-MPM: WORKFLOW END -->\n",
        )
        .unwrap();

        let report = prepare_session_with_style_and_native(&fw, project, None, native_supported)
            .expect("prep succeeds");
        let stash = std::fs::read_to_string(&report.stash).expect("stash readable");

        assert!(
            stash.contains("STASH_OVERRIDE_MARKER"),
            "stash must reflect the CLAUDE.md WORKFLOW override (native_supported={native_supported})"
        );
        assert!(
            !stash.contains("# PM Workflow Configuration"),
            "bundled workflow heading must be replaced in the stash (native_supported={native_supported})"
        );
        assert!(
            stash.contains("# Framework Instructions"),
            "stash must still carry the BASE_PM floor (native_supported={native_supported})"
        );
        // The CORE INVARIANT: the persisted stash must equal the exact prompt the
        // launcher would deliver under the SAME injection decision — in both
        // claude-present (native) and claude-absent (injected) environments.
        assert_eq!(
            stash,
            build_system_prompt_for_with_style_and_native(project, None, native_supported),
            "stash must equal the launch prompt (native_supported={native_supported})"
        );
        // When injection fires, the stash must actually carry the injected style
        // block; when native is supported it must NOT — proving the flag drives
        // the stash content, not the host.
        if native_supported {
            assert!(
                !stash.contains(crate::core::output_style::INJECTED_STYLE_HEADING),
                "native-capable: stash must NOT carry the injected style block"
            );
        } else {
            assert!(
                stash.contains(crate::core::output_style::INJECTED_STYLE_HEADING),
                "native-incapable: stash MUST carry the injected style block"
            );
        }
    }
}

#[test]
#[serial_test::serial]
fn prepare_session_writes_claude_md_and_stash() {
    // Why: the launch paths rely on `prepare_session` writing the project
    // CLAUDE.md and the inspectable stash before `claude` is started.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    // #3965: `prepare_session` seeds `$HOME/.claude.json` via the REAL process
    // `$HOME`, not `fw` — `#[serial]` + the override below keep this test off
    // the operator's real file and off every sibling test doing the same.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        project.join("CLAUDE.md").exists(),
        "CLAUDE.md must exist after prep"
    );
    assert!(
        report.stash.exists(),
        "merged instructions stash must be written"
    );
    assert_eq!(
        report.stash,
        project.join(".trusty-mpm").join("last-instructions.md")
    );
}

// ── #4752: INSTRUCTIONS-COMPILED.md must be current BEFORE the spawn ────────

#[test]
fn instruction_failure_is_fatal() {
    // Why (#4752 ruling 2026-08-04): exactly one preparation failure refuses a
    // launch. `is_fatal` is the single discriminator the seven spawning call
    // sites consult, so it is pinned directly.
    let err = PrepError::Instructions {
        path: PathBuf::from("/p/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md"),
        source: std::io::Error::other("boom"),
    };
    assert!(err.is_fatal());
    // The Display must be the operator-facing message, not a bare io error.
    let shown = err.to_string();
    assert!(shown.contains("/p/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md"));
    assert!(shown.contains("was NOT started"));
}

#[test]
fn deploy_and_io_failures_stay_non_fatal() {
    // Why (#4752): #2149 deliberately made preparation non-fatal so a roster or
    // skill deploy hiccup could not stop a session launching. Only the
    // instruction condition is ruled fatal — a blanket "all prep errors abort"
    // would have reversed #2149 wholesale. This pins that the other variants
    // did NOT silently inherit the new policy.
    assert!(!PrepError::Deploy("agents".into()).is_fatal());
    assert!(!PrepError::SkillDeploy("skills".into()).is_fatal());
    assert!(
        !PrepError::Io {
            path: PathBuf::from("/p/.trusty-mpm/last-instructions.md"),
            source: std::io::Error::other("boom"),
        }
        .is_fatal()
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_writes_the_compiled_prompt_before_returning() {
    // Why (#4752, owner ruling 2026-08-04): the framework-level compiled prompt
    // must reflect the prompt the session is ABOUT TO RUN WITH, not the one the
    // last `tm install` produced. Every caller spawns `claude` only after
    // `prepare_session` returns `Ok`, so "current at return" IS "current at
    // launch".
    //
    // FIXTURE NOTE — this is deliberately not an existence check. The compiled
    // path is PRE-SEEDED with stale sentinel content, so a test that only
    // asserted "the file exists" would pass against a broken implementation
    // that never writes at launch. Only an actual launch-time write of the
    // resolved prompt can turn the assertions below green.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    const STALE: &str = "STALE-FROM-A-PREVIOUS-INSTALL";
    let compiled = compiled_for(project);
    std::fs::create_dir_all(compiled.parent().unwrap()).unwrap();
    std::fs::write(&compiled, STALE).unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert_eq!(
        report.compiled_prompt, compiled,
        "the report must name the compiled path the launch actually wrote"
    );
    let on_disk = std::fs::read_to_string(&compiled).expect("compiled prompt must be readable");
    assert_ne!(
        on_disk, STALE,
        "the launch must overwrite a stale compiled prompt, not leave it in place"
    );
    // The stash is the byte-exact text handed to
    // `claude --append-system-prompt-file` (issue #1409). Equality here is what
    // makes the compiled file "the prompt this session runs with" rather than
    // merely "some compiled prompt".
    let launch_prompt = std::fs::read_to_string(&report.stash).expect("stash must be readable");
    assert_eq!(
        on_disk, launch_prompt,
        "the compiled prompt must be byte-identical to the text passed to claude"
    );
    assert!(!on_disk.is_empty(), "the compiled prompt must not be empty");
}

#[test]
#[serial_test::serial]
fn prepare_session_fails_when_the_compiled_prompt_cannot_be_written() {
    // Why (#4752): the ordering requirement is that the write BLOCKS the
    // launch — not that it happens eventually and not that it is best-effort.
    // This is the test that distinguishes those: if the write were a
    // `let _ = …`, a warn-and-continue, or deferred to a background task,
    // `prepare_session` would return `Ok` here and the assertion below fails.
    //
    // FIXTURE: a DIRECTORY is planted at the compiled prompt's exact path.
    // `create_dir_all(parent)` still succeeds, so the failure is isolated to
    // the compiled write itself — no other step of the preparation is
    // sabotaged, which is what keeps this test about the guard it names.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(compiled_for(project)).unwrap();

    let err = prepare_session(&fw, project)
        .expect_err("a failed compiled-prompt write must abort the launch preparation");
    // #4752 ruling: this is the ONE fatal preparation error — the seven
    // spawning call sites refuse to launch on it.
    assert!(
        err.is_fatal(),
        "a compiled-prompt write failure must be classified fatal, got {err:?}"
    );
    match &err {
        PrepError::Instructions { path, .. } => assert_eq!(
            *path,
            compiled_for(project),
            "the error must name the compiled prompt path"
        ),
        other => panic!("expected PrepError::Instructions, got {other:?}"),
    }
    // Ruling follow-up 1: the operator must see why, and where.
    let shown = err.to_string();
    assert!(
        shown.contains("was NOT started"),
        "must tell the operator the session did not start: {shown}"
    );
    assert!(
        shown.contains(&compiled_display(project)),
        "must name the path: {shown}"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_for_managed_writes_the_per_session_compiled_prompt() {
    // Why (#4832): a managed provisioning caller HOLDS the session id, so the
    // compiled prompt must land in that session's directory — not in the
    // unmanaged `local` bucket the id-less entry points fall back to. If it
    // did, the spawn (which knows the id) would refresh a different file and
    // leave this one stale forever.
    // FAILS BEFORE THIS CHANGE: there was one compiled prompt per project and
    // no way to scope it to a session at all.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let report =
        prepare_session_for_managed(&fw, project, None, "sess-abc").expect("prep succeeds");

    let expected = crate::core::instruction_pipeline::compiled_prompt_path(project, "sess-abc");
    assert_eq!(
        report.compiled_prompt, expected,
        "the report must name this session's compiled path"
    );
    assert!(expected.exists(), "the compiled prompt must be on disk");
    assert!(
        expected
            .to_string_lossy()
            .contains("/.trusty-mpm/sessions/sess-abc/"),
        "the layout is `.trusty-mpm/sessions/<id>/`: {}",
        expected.display()
    );
    assert_ne!(
        expected,
        crate::core::instruction_pipeline::compiled_prompt_path(project, "sess-other"),
        "a second session must not share this file"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_migrates_off_the_legacy_compiled_prompt() {
    // Why (#4832): an upgraded install carries
    // `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` from #4752.
    // Nothing refreshes it any more, and it is the file an operator opens to
    // answer "what is my session running" — a stale answer there is worse than
    // no answer.
    // FAILS BEFORE THIS CHANGE: the legacy file survived every launch.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let legacy = project
        .join(".trusty-mpm")
        .join("framework")
        .join("INSTRUCTIONS-COMPILED.md");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "OLD COMPILED PROMPT").unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !legacy.exists(),
        "the pre-#4832 per-project compiled prompt must be retired: {}",
        legacy.display()
    );
}

/// The compiled-prompt path `prepare_session` writes for `project`.
///
/// Why (#4832): the file is per-session, and the id-less `prepare_session`
/// entry points resolve their scope through
/// [`crate::core::harness_root::session_scope`] — which reads
/// `TM_MANAGED_SESSION_ID` when the test binary happens to run inside a managed
/// pane. Re-deriving it the same way keeps these assertions about the WRITE,
/// not about the developer's environment.
fn compiled_for(project: &std::path::Path) -> std::path::PathBuf {
    let scope = crate::core::harness_root::session_scope(None);
    crate::core::instruction_pipeline::compiled_prompt_path(project, &scope)
}

/// The compiled-prompt path as it appears in an operator-facing message.
fn compiled_display(project: &std::path::Path) -> String {
    compiled_for(project).display().to_string()
}

#[test]
#[serial_test::serial]
fn prepare_session_refuses_when_the_instructions_cannot_be_built() {
    // Why (#4752, owner ruling round 4): "If writing the instruction fails, we
    // shouldn't start ... we depend on those instructions." `build_instructions`
    // used to return the NON-fatal `PrepError::Instructions(PipelineError)`,
    // which every spawning caller logged and continued past — starting a session
    // whose instructions were never established. It is now the same fatal
    // condition as the compiled write, so the launch is refused.
    //
    // FIXTURE: a directory planted at `<project>/CLAUDE.md`, so the pipeline's
    // load-or-create step fails. This is the ONLY early exit upstream of the
    // compiled write; if it ever returns non-fatally again, the ordering
    // contract's promise stops being true and this test fails.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(project.join("CLAUDE.md"))
        .expect("plant a directory where CLAUDE.md goes");

    let err = prepare_session(&fw, project)
        .expect_err("a session whose instructions cannot be built must NOT start");

    assert!(
        err.is_fatal(),
        "an instruction-build failure must be classified fatal, got {err:?}"
    );
    match &err {
        PrepError::Instructions { path, .. } => assert!(
            path.starts_with(project),
            "the error must name the offending project path, got {}",
            path.display()
        ),
        other => panic!("expected PrepError::Instructions, got {other:?}"),
    }
    // Operator-facing, not a bare io error.
    let shown = err.to_string();
    assert!(
        shown.contains("was NOT started"),
        "must say the session did not start: {shown}"
    );
}

#[test]
#[serial_test::serial]
fn stash_write_failure_does_not_skip_the_fatal_instruction_write() {
    // Why (#4752 round 4): the ordering contract promises that a session which
    // starts has its instructions on disk. An unwritable `.trusty-mpm/` used to
    // return a short-circuiting non-fatal `PrepError::Io` from the
    // `last-instructions.md` stash, which sits ABOVE the fatal compiled write.
    // Because `Io` is non-fatal, every caller launched the session anyway —
    // having skipped the write that records its instructions. That is the exact
    // case the contract forbids.
    //
    // The stash is an inspection copy, so losing it is not itself grounds to
    // refuse a launch; what matters is that it cannot take the fatal write down
    // with it.
    //
    // FIXTURE: a directory planted at `.trusty-mpm/last-instructions.md`, so
    // `create_dir_all` on the parent succeeds and only the stash WRITE fails.
    // The compiled write below it is untouched and must still succeed, so
    // preparation reports Ok. This test fails if the stash write is ever
    // restored to a `?`.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(project.join(".trusty-mpm/last-instructions.md"))
        .expect("plant a directory where the stash file goes");

    let report = prepare_session(&fw, project)
        .expect("a failed stash write must NOT refuse the launch (#2149) nor abort preparation");

    // The compiled write still ran and is still the fatal one …
    assert!(
        report.compiled_prompt.exists(),
        "the compiled prompt must still be written when only the stash fails"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_deploys_project_tier_output_style() {
    // Why (#2125 item 2): the daemon managed-spawn path launches `claude`
    // with `--setting-sources project,local`, excluding the `user` tier the
    // home-dir deploy lands in. Without a project-tier copy the `outputStyle`
    // id written into `<project>/.claude/settings.json` cannot resolve.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let project_style = project.join(".claude/output-styles/trusty-mpm.md");
    assert!(
        project_style.exists(),
        "project-tier output style file must be deployed: {}",
        project_style.display()
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_self_heals_missing_skill_source() {
    // #1917: `fw.skills` (the framework skill *source* dir `skill_source_dir()`
    // falls back to) starts out completely absent here — simulating a machine
    // that never ran `tm install` under the current binary. Before the fix,
    // `deploy_skills_filtered` would silently deploy zero skills from an
    // absent source with no error surfaced anywhere; session prep must now
    // self-heal it first.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    assert!(!fw.skills.exists(), "precondition: no prior tm install ran");

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !report.skill_deploy.deployed.is_empty(),
        "session prep must self-heal the missing skill source and deploy at \
         least one skill; got {:?}",
        report.skill_deploy
    );
    assert!(fw.skills.join("tm-doctor.md").exists());
}

#[test]
#[serial_test::serial]
fn prepare_session_self_heals_renamed_skill_source() {
    // #1917: a pre-rename `~/.trusty-mpm/framework/skills/` (stale content
    // left by an old binary, no matching bundle stamp) must be pruned and
    // refreshed automatically during session prep — not left for a manual
    // `tm install --force` to notice and fix.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    std::fs::create_dir_all(&fw.skills).unwrap();
    std::fs::write(fw.skills.join("mpm-old-skill.md"), "stale\n").unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !fw.skills.join("mpm-old-skill.md").exists(),
        "the stale pre-rename file must be pruned during self-heal"
    );
    assert!(
        !report.skill_deploy.deployed.is_empty(),
        "renamed/stale skill source must self-heal and deploy current skills"
    );
}

// Issue #2149 roster-deploy-failure-continues coverage lives in the sibling
// `tests_roster.rs` file (split out to keep this file under the 1500-SLOC
// test-file cap, mirroring the `doctor_output_style.rs` / `doctor_fs_checks.rs`
// split pattern already used elsewhere in this crate).

/// Why (issue #1904 stretch goal): `prepare_session_inner` emits discrete
/// `provisioning_stage` events (DeployingAgents/DeployingSkills/
/// BuildingInstructions/ConfiguringMcp) so the daemon's SSE stream can drive
/// real step-by-step progress in the `tm` CLI, instead of one opaque wait.
/// This is testable without a live daemon/tmux/git-clone: `emit()` reads a
/// `tokio::task_local` that `provisioning_stage::scoped` installs, and
/// `prepare_session` is a plain sync function we can call from inside that
/// scope in a `#[tokio::test]`.
/// What: wraps `prepare_session` in a scope backed by a fresh broadcast
/// channel, drains every event the call emitted, and asserts the four
/// session_launch-owned stages appear, IN ORDER (other stages —
/// CloningRepo/CreatingTmuxSession/LaunchingRuntime/Complete — are emitted
/// elsewhere in the call chain, not by `prepare_session_inner`, so they are
/// correctly absent here).
/// Test: this is the test.
#[tokio::test]
#[serial_test::serial]
async fn prepare_session_emits_stage_events_in_order() {
    use crate::core::provisioning_stage::{ProvisioningStage, StageEmitter, scoped};

    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (tx, mut rx) = tokio::sync::broadcast::channel(32);
    let emitter = StageEmitter::new("test-session", "https://github.com/acme/widgets", tx);

    scoped(emitter, async {
        prepare_session(&fw, project).expect("prep succeeds");
    })
    .await;

    let mut stages = Vec::new();
    while let Ok(value) = rx.try_recv() {
        assert_eq!(value["kind"], "provisioning_stage");
        assert_eq!(value["repo_url"], "https://github.com/acme/widgets");
        stages.push(value["stage"].as_str().unwrap().to_string());
    }

    assert_eq!(
        stages,
        vec![
            ProvisioningStage::DeployingAgents.wire_name(),
            ProvisioningStage::DeployingSkills.wire_name(),
            ProvisioningStage::BuildingInstructions.wire_name(),
            ProvisioningStage::ConfiguringMcp.wire_name(),
        ],
        "prepare_session must emit exactly these four stages, in order"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_sets_output_style() {
    // Why: a launched session must show `style:trusty-mpm`, which Claude
    // Code reads from `<project>/.claude/settings.json`.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let settings_path = project.join(".claude").join("settings.json");
    assert!(settings_path.exists(), ".claude/settings.json must exist");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
#[serial_test::serial]
fn prepare_session_writes_configured_style() {
    // Why: HR-4 — when `[style] active` is set in the framework config, the
    // launched session's settings.json must carry that id.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    // Seed `<root>/config.toml` with a teaching-mode selection.
    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-teacher")
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_explicit_style_overrides_config() {
    // Why: HR-4 — an explicit `--style` override beats the config `[style] active`
    // key for that launch.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();

    crate::core::session_launch::prepare_session_with_style(
        &fw,
        project,
        Some("trusty-mpm-research"),
    )
    .expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_unknown_style_falls_back_to_default() {
    // Why: DOC-17 — an unknown configured style must not fail the launch; it
    // falls back to the professional default.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(fw.config_toml(), "[style]\nactive = \"does-not-exist\"\n").unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
fn write_output_style_preserves_existing_keys() {
    // Why: merging the style must not clobber an operator's other settings.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"theme":"dark","outputStyle":"old"}"#,
    )
    .unwrap();

    write_output_style(project, None).expect("write succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
    assert_eq!(value["theme"], serde_json::json!("dark"));
}

#[test]
fn write_output_style_sets_active_style() {
    // Why: HR-4 — an explicitly resolved active style id must be written into
    // settings.json so a native-capable Claude Code applies it.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, Some("trusty-mpm-research")).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
fn write_output_style_empty_falls_back_to_default() {
    // Why: a blank/whitespace id must not blank the outputStyle key; it falls
    // back to the professional default.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, Some("   ")).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
fn write_output_style_sets_spinner_tips() {
    // Why: trusty-mpm sessions must override the operator's generic
    // claude-mpm spinner tips with project-specific ones; the settings.json
    // merge must enable tips and write a non-empty tips array.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, None).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["spinnerTipsEnabled"], serde_json::json!(true));
    let tips = value["spinnerTipsOverride"]["tips"]
        .as_array()
        .expect("spinnerTipsOverride.tips must be an array");
    assert!(!tips.is_empty(), "spinner tips must be non-empty");
    assert!(tips.iter().all(|tip| tip.is_string()));
}

#[test]
fn write_project_hooks_writes_all_event_types() {
    // Why (#1270): the trusty-memory hooks must be scoped to the project and use
    // the canonical, real CLI surface — `UserPromptSubmit` → prompt-context and
    // `SessionStart` → inbox-check. The old `PostToolUse`/`Stop` events invoked
    // the nonexistent `hooks fire` subcommand and are gone.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project, true).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let hooks = value["hooks"].as_object().expect("hooks must be an object");
    for event in ["UserPromptSubmit", "SessionStart"] {
        let groups = hooks[event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} must be an array"));
        assert!(!groups.is_empty(), "{event} must have a handler group");
        let cmd = groups[0]["hooks"][0]["command"]
            .as_str()
            .expect("command must be a string");
        assert!(
            cmd.starts_with("trusty-memory "),
            "{event} command must invoke trusty-memory: {cmd}"
        );
    }
}

#[test]
fn write_project_hooks_uses_canonical_commands() {
    // Why (#1270): the hook commands MUST match the real trusty-memory CLI
    // (`prompt-context`, `inbox-check`) — never the bogus `hooks fire` form that
    // hard-blocked prompts with "unrecognized subcommand 'hooks'".
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project, true).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let raw = serde_json::to_string(&value).unwrap();
    assert!(
        !raw.contains("hooks fire"),
        "the broken `hooks fire` command must never be written: {raw}"
    );
    assert_eq!(
        value["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory prompt-context")
    );
    assert_eq!(
        value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory inbox-check")
    );
}

#[test]
fn write_project_hooks_omits_post_tool_use_and_stop() {
    // Why (#1270, superseded by #2003): trusty-memory itself has no
    // PostToolUse/Stop CLI hook surface (memory writes flow through MCP
    // tools), so PostToolUse/Stop never carry a `trusty-memory` command.
    // BUT issue #2003 folds the lifecycle triad into this same write, so
    // PostToolUse/Stop ARE now registered — with the triad's `... hook`
    // command, never a `trusty-memory ...` one. See
    // `project_hooks_tests::write_project_hooks_writes_lifecycle_triad` for
    // the full six-event triad assertion.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project, true).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    for event in ["PostToolUse", "Stop"] {
        let groups = value["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} must be registered by the lifecycle triad"));
        for group in groups {
            let cmd = group["hooks"][0]["command"].as_str().unwrap();
            assert!(
                !cmd.starts_with("trusty-memory"),
                "{event} must never carry a trusty-memory command: {cmd}"
            );
        }
    }
}

#[test]
fn write_project_hooks_registers_pm_guard() {
    // Why (#1977): managed PM sessions must register the PreToolUse enforcement
    // guard so the PM is blocked from editing code directly. The command must be
    // an absolute path (PATH-robust, per #1914) ending in `hook --pm-guard`.
    // NOTE (#2003): PreToolUse now also carries the lifecycle-triad group
    // alongside the guard — two groups, not one.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project, true).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let groups = value["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse must be an array");
    assert_eq!(
        groups.len(),
        2,
        "PreToolUse must carry the PM-guard group plus the lifecycle-triad group"
    );
    let guard = groups
        .iter()
        .find(|g| g["matcher"] == serde_json::json!(""))
        .expect("the guard group (matcher \"\") must be present");
    let cmd = guard["hooks"][0]["command"]
        .as_str()
        .expect("command must be a string");
    assert!(
        cmd.ends_with(" hook --pm-guard"),
        "guard command must end with ' hook --pm-guard', got: {cmd}"
    );
    assert!(
        !cmd.starts_with("trusty-memory"),
        "the guard group must be the tm guard, not a trusty-memory hook: {cmd}"
    );
}

#[test]
fn write_project_hooks_replaces_existing() {
    // Why: re-running prep must replace OUR OWN prior groups, not append to
    // them, so handler arrays never duplicate and cause double-firing.
    // NOTE (#2003): SessionStart now carries 2 stable groups (trusty-memory +
    // lifecycle-triad) rather than 1 — the assertion is "stays at 2 across
    // repeats", not "stays at 1".
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project, true).expect("first write succeeds");
    write_project_hooks(project, true).expect("second write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let ss = value["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart must be an array");
    assert_eq!(
        ss.len(),
        2,
        "re-running must not duplicate our own handler groups"
    );
    // Unrelated keys must survive the replace.
    write_project_hooks(project, true).expect("third write succeeds");
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Issue #1605 — trusty-memory palace-slug pinning in managed-session injection.
// ---------------------------------------------------------------------------

/// Initialise a git repo at `dir` with `origin` pointing at `remote_url`.
///
/// Why: the palace-slug git-fallback path shells out to
/// `git -C <dir> config --get remote.origin.url`; exercising it needs a real
/// repo with a configured origin remote, not a network clone.
/// What: runs `git init`, `git remote add origin <url>` in `dir`. Skips
/// (returns `false`) when git is unavailable so the test degrades gracefully on
/// a git-less host rather than failing spuriously.
/// Test: used by `inject_trusty_memory_mcp_pins_palace_from_git_remote` and
/// `resolve_palace_slug_falls_back_to_git_remote` (it is the helper).
fn init_git_repo_with_origin(dir: &std::path::Path, remote_url: &str) -> bool {
    let init = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir)
        .status();
    match init {
        Ok(status) if status.success() => {}
        _ => return false,
    }
    let remote = std::process::Command::new("git")
        .args(["remote", "add", "origin", remote_url])
        .current_dir(dir)
        .status();
    matches!(remote, Ok(status) if status.success())
}

#[test]
fn remove_global_hooks_removes_trusty_memory_entries() {
    // Why: the global trusty-memory hook entries must be cleaned out so
    // they no longer fire for unrelated Claude Code sessions; non-trusty
    // entries and empty-becoming events must be handled correctly.
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{
          "theme": "dark",
          "hooks": {
            "PostToolUse": [
              { "matcher": "*", "hooks": [ { "type": "command", "command": "bash track.sh" } ] },
              { "matcher": "Write|Edit|Bash", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.post-tool-use" } ] }
            ],
            "Stop": [
              { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.stop" } ] }
            ],
            "UserPromptSubmit": [
              { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.user-prompt" } ] }
            ]
          }
        }"#,
    )
    .unwrap();

    clean_global_trusty_memory_hooks(&settings_path).expect("clean succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    // Unrelated keys survive.
    assert_eq!(value["theme"], serde_json::json!("dark"));
    // Non-trusty PostToolUse entry survives; trusty one is gone.
    let post = value["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 1);
    assert!(
        post[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("track.sh")
    );
    // Stop and UserPromptSubmit only had trusty entries, so the keys are gone.
    assert!(
        value["hooks"].get("Stop").is_none(),
        "empty Stop event must be removed"
    );
    assert!(
        value["hooks"].get("UserPromptSubmit").is_none(),
        "empty UserPromptSubmit event must be removed"
    );
}

#[test]
fn remove_global_hooks_tolerates_missing_file() {
    // Why: cleanup is non-fatal and idempotent — a missing settings file
    // (operator never created one) must be a no-op success.
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("nope.json");
    clean_global_trusty_memory_hooks(&missing).expect("missing file is a no-op");
}

// ──────────────────────────────────────────────
// trusty-search MCP injection (#1270 / step 4)
// ──────────────────────────────────────────────

// ──────────────────────────────────────────────
// trusty-search index pin (#1373)
// ──────────────────────────────────────────────

/// Nothing registered means nothing to pin (#5091; was
/// `register_project_index_returns_derived_id`).
///
/// Why: this test used to assert the opposite — that with no daemon the id came
/// back anyway "so the stub can be pinned". That is the defect #5091 reports:
/// `.mcp.json` then pins `trusty-search serve --index <id>` for an index nothing
/// created, and every `search` in that session answers `404 unknown index` while
/// the `search` health check stays green. The launch caller already handles
/// `None` by writing the unpinned stub, so the honest outcome costs nothing.
/// What: points the data dir at an empty temp dir so no `http_addr` file exists
/// and no POST is issued, then asserts `register_project_index` returns `None`
/// while the shared reporting entry point still derives the git-root basename
/// from a nested directory — the derivation this test has always covered.
/// `#[serial]` because the override env var is process-global.
/// Test: this test.
#[test]
#[serial_test::serial]
fn register_project_index_withholds_id_when_registration_is_unconfirmed() {
    let data_dir = tempdir().unwrap();
    // Panic-safe restore: the guard restores/removes the override env var in its
    // `Drop`, so a panic in the assertions below never leaks it to sibling
    // serial tests.
    let _env = EnvVarGuard::set(
        trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV,
        data_dir.path(),
    );

    // A git-rooted project: id == the git-root basename, even from a nested dir.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let nested = project.path().join("crates/inner");
    std::fs::create_dir_all(&nested).unwrap();

    let report = trusty_common::search_index::ensure_project_indexed_reporting(
        &nested,
        trusty_common::search_index::IndexOptions::default(),
    );
    let expected = trusty_common::derive_index_id(project.path());
    assert_eq!(
        report.index_id,
        Some(expected),
        "id is the git-root basename"
    );
    assert_eq!(
        register_project_index(&nested),
        None,
        "no registration was confirmed, so the stub must not be pinned (#5091)"
    );
}

// Issue #2914 regression (`register_project_index_never_bypasses_sensitive_path_denylist`)
// split into `tests_search_index.rs` to keep this file under the 1500-SLOC
// test-file cap — mirrors the `tests_roster.rs` / `tests_scaffold_gitignore.rs`
// split pattern above.

// ── index_is_fresh (#1908) ────────────────────────────────────────────────────
// The freshness predicate was PROMOTED to trusty-common alongside the rest of
// the register+reindex logic (common entry-point rule); its unit tests now live
// in `trusty_common::search_index::tests` (run `cargo test -p trusty-common
// --features search-index`). This crate keeps only the launch-prep wrapper
// coverage below.

// ──────────────────────────────────────────────
// Workspace trust pre-seed (#1269)
// ──────────────────────────────────────────────

#[test]
fn preseed_trust_marks_directory() {
    // Why (#1269): the interactive session must not stall on the trust dialog;
    // seeding the per-dir entry in ~/.claude.json suppresses it.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = workspace.to_string_lossy().to_string();
    let entry = &value["projects"][&key];
    assert_eq!(entry["hasTrustDialogAccepted"], serde_json::json!(true));
    assert_eq!(
        entry["hasCompletedProjectOnboarding"],
        serde_json::json!(true)
    );
    assert!(
        entry["projectOnboardingSeenCount"].as_u64().unwrap() >= 1,
        "onboarding counter must be >= 1"
    );
}

#[test]
fn preseed_trust_preserves_other_keys() {
    // Why: the file holds OAuth/login data; seeding trust must not drop it.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    std::fs::write(
        &claude_json,
        r#"{"oauthAccount":{"emailAddress":"r@1mc.io"},"projects":{"/other":{"hasTrustDialogAccepted":true}}}"#,
    )
    .unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    // OAuth survives untouched (issue #1269: OAuth must be preserved).
    assert_eq!(
        value["oauthAccount"]["emailAddress"],
        serde_json::json!("r@1mc.io")
    );
    // Pre-existing project survives.
    assert_eq!(
        value["projects"]["/other"]["hasTrustDialogAccepted"],
        serde_json::json!(true)
    );
    // New workspace is trusted.
    let key = workspace.to_string_lossy().to_string();
    assert_eq!(
        value["projects"][&key]["hasTrustDialogAccepted"],
        serde_json::json!(true)
    );
}

#[test]
fn preseed_trust_is_idempotent() {
    // Why: prep may run repeatedly; a second seed must not change the file.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");

    preseed_workspace_trust(&claude_json, &workspace).expect("first seed");
    let first = std::fs::read_to_string(&claude_json).unwrap();
    preseed_workspace_trust(&claude_json, &workspace).expect("second seed");
    let second = std::fs::read_to_string(&claude_json).unwrap();

    assert_eq!(first, second, "re-seeding must leave the file unchanged");
}

#[test]
fn preseed_trust_leaves_malformed_file() {
    // Why (#1269): a malformed ~/.claude.json likely still holds OAuth state;
    // clobbering it would force a re-login. Seeding must bail out untouched.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    let garbage = "{ this is not valid json ";
    std::fs::write(&claude_json, garbage).unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("soft-fails to Ok");

    let after = std::fs::read_to_string(&claude_json).unwrap();
    assert_eq!(after, garbage, "malformed file must be left untouched");
}

// The project-scope-exclusion regression (issue #2739's defense-in-depth,
// preserved by the #3926 fix) is now covered end-to-end, through the REAL
// `prepare_session_inner` pipeline, by
// `prepare_session_excludes_trusted_project_scope_custom_from_trust_preseed`
// in `tests_mcp_trust_seed_e2e.rs` — a stronger guarantee than unit-testing
// `preseed_workspace_trust`'s pass-through in isolation, since the exclusion
// set is now computed by the caller, not this function.

#[test]
fn deploy_output_style_writes_file() {
    // Why: Claude Code resolves the `trusty-mpm` output style only when a
    // matching file exists in `~/.claude/output-styles/`; deployment must
    // create that file (and its parent dir) with the bundled content.
    let home = tempdir().unwrap();
    let path = deploy_output_style(home.path()).expect("deploy succeeds");

    assert_eq!(
        path,
        home.path()
            .join(".claude")
            .join("output-styles")
            .join("trusty-mpm.md")
    );
    let written = std::fs::read_to_string(&path).expect("style file readable");
    assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
    assert!(written.contains("name: trusty-mpm"));
}

#[test]
fn deploy_output_style_overwrites() {
    // Why: framework upgrades to the style must propagate on the next
    // launch, so deployment always overwrites any existing file.
    let home = tempdir().unwrap();
    let first = deploy_output_style(home.path()).expect("first deploy succeeds");
    std::fs::write(&first, "stale operator content").unwrap();

    let second = deploy_output_style(home.path()).expect("second deploy succeeds");
    assert_eq!(first, second);
    let written = std::fs::read_to_string(&second).unwrap();
    assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
}

#[test]
fn deploy_output_style_writes_all_styles() {
    // Why: HR-4 — the operator may select any of the three bundled styles, so
    // ALL of them must land in ~/.claude/output-styles/ for the selection to
    // resolve in Claude Code.
    let home = tempdir().unwrap();
    deploy_output_style(home.path()).expect("deploy succeeds");

    let dir = home.path().join(".claude").join("output-styles");
    for style in crate::core::bundle::OUTPUT_STYLES {
        let path = dir.join(style.file_name);
        assert!(path.exists(), "{} must be deployed", style.file_name);
        let written = std::fs::read_to_string(&path).expect("style file readable");
        assert_eq!(written, style.content, "{} content matches", style.id);
    }
    // Sanity: exactly the three bundled styles are written.
    assert_eq!(crate::core::bundle::OUTPUT_STYLES.len(), 3);
}

#[test]
#[serial_test::serial]
fn prepare_session_reports_output_style() {
    // Why: callers report the deployed style path; `prepare_session` must
    // populate `PrepReport.output_style` with the file it deployed.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let report = prepare_session(&fw, project).expect("prep succeeds");

    let style = report
        .output_style
        .expect("output style deployed when home is resolvable");
    assert!(style.ends_with("trusty-mpm.md"));
    assert!(style.exists());
    // Issue #1860: the deploy must target the scoped `tmp_home`, never the
    // real `$HOME` — this is the regression the leaking test exposed.
    assert!(
        style.starts_with(tmp_home.path()),
        "output style must deploy under the injected FrameworkPaths base, got {}",
        style.display()
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_reports_skill_deploy() {
    // Why: `prepare_session` must run the skill deploy step so launched
    // sessions see trusty-mpm skills; the report must carry its stats.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let report = prepare_session(&fw, project).expect("prep succeeds");

    // The stats are present (a fresh install with no skill source is an
    // empty-but-valid result; this asserts the field is populated, not
    // that any specific skill deployed).
    let _ = &report.skill_deploy;
}

#[test]
#[serial_test::serial]
fn prepare_session_is_idempotent() {
    // Why: `/connect` and `tm session start` may run repeatedly on the same
    // project; a second prep must not fail and must not recreate CLAUDE.md.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let first = prepare_session(&fw, project).expect("first prep succeeds");
    assert!(first.instructions.claude_md_created);

    let second = prepare_session(&fw, project).expect("second prep succeeds");
    assert!(
        !second.instructions.claude_md_created,
        "CLAUDE.md already exists on the second run"
    );
}

// ──────────────────────────────────────────────
// HR-2 — Manifest-driven harness provisioning
// ──────────────────────────────────────────────

/// Seed two bundled agent source files (a base + a leaf) under `fw.agents` so
/// the deploy step has deterministic content to filter.
///
/// Why: the manifest integration tests must assert WHICH agents deploy; seeding
/// a known two-agent set makes the include/exclude assertions deterministic
/// regardless of whether the host has the optional `agents/` submodule.
/// What: writes `base-engineer.md` and `rust-engineer.md` (the latter extends
/// the former) into the framework's bundled agent source dir.
/// Test: used by `prepare_session_manifest_*`.
fn seed_bundled_agents(fw: &crate::core::paths::FrameworkPaths) {
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::write(
        fw.agents.join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE.\n",
    )
    .unwrap();
    std::fs::write(
        fw.agents.join("rust-engineer.md"),
        "---\nname: rust-engineer\nrole: engineer\nextends: base-engineer\n---\n\n# Rust\n\nLEAF.\n",
    )
    .unwrap();
}

#[test]
#[serial_test::serial]
fn prepare_session_default_deploys_all_seeded_agents() {
    // Why: HR-2 must be regression-safe — with NO manifest present, the
    // compiled-in default reproduces today's behavior, deploying every bundled
    // agent. This proves "absent manifest = unchanged provisioning".
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    // Force the bundled source so the test does not depend on an `agents/`
    // submodule resolved from the running binary's location.
    let mut fw = fw;
    fw.trusty_mpm_root = None;
    seed_bundled_agents(&fw);

    let report = prepare_session(&fw, project).expect("prep succeeds");

    // Both seeded agents deploy (default manifest selects all).
    assert!(
        report
            .deploy
            .deployed
            .contains(&"base-engineer.md".to_string())
    );
    assert!(
        report
            .deploy
            .deployed
            .contains(&"rust-engineer.md".to_string())
    );
    assert!(fw.agent_deploy_dir().join("rust-engineer.md").exists());
}

#[test]
#[serial_test::serial]
fn prepare_session_never_retracts_the_operator_home_agents_tier() {
    // Issue #4409, code-critic CRITICAL: the #4409 retraction must be a
    // WORKSPACE operation. `prepare_session` is called with a HOME-TIER
    // `FrameworkPaths::default()` on two production paths — non-git
    // `tm session start` (`commands/session/start.rs`) and the TUI `/connect`
    // (`client/http_client/session_connect.rs`) — where `fw.claude_agents_dir()`
    // IS the operator's `~/.claude/agents`. Aiming retraction at that field
    // deleted the roster, the ownership manifest, and the directory itself out
    // of a Claude Code install trusty-mpm does not own. Binding the retraction
    // to `project_dir` instead makes that structurally impossible; this pins it.
    //
    // `run_prepare_session_never_writes_real_home_claude_dirs` does NOT cover
    // this: the standalone driver passes a MANAGED `fw`, so it never exercises
    // the home-tier shape.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;
    seed_bundled_agents(&fw);

    // Stand in for a pre-#4409 `tm install`: a fully deployed, manifest-tracked
    // bundled roster sitting in the operator's own `~/.claude/agents`.
    let home_agents = fw.claude_agents_dir();
    crate::core::agent_deployer::deploy_agents(&fw.agents, &home_agents).unwrap();
    let before: Vec<String> = std::fs::read_dir(&home_agents)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        before.iter().any(|n| n == "rust-engineer.md"),
        "fixture must genuinely deploy into the home tier, else this test is vacuous"
    );
    let body_before = std::fs::read_to_string(home_agents.join("rust-engineer.md")).unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        home_agents.is_dir(),
        "prepare_session must not remove the operator's ~/.claude/agents directory"
    );
    let mut after: Vec<String> = std::fs::read_dir(&home_agents)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let mut before_sorted = before;
    before_sorted.sort();
    after.sort();
    assert_eq!(
        after, before_sorted,
        "prepare_session must leave the operator's ~/.claude/agents untouched"
    );
    assert_eq!(
        std::fs::read_to_string(home_agents.join("rust-engineer.md")).unwrap(),
        body_before,
        "every file in the operator's home tier must survive byte-identical"
    );
    assert!(
        home_agents
            .join(crate::core::agent_manifest::MANIFEST_FILE)
            .is_file(),
        "the home tier's ownership manifest must survive too"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_manifest_filters_agent_set() {
    // Why: HR-2 — a project manifest's `[agents] include` must restrict WHICH
    // agents the harness deploys.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;
    seed_bundled_agents(&fw);

    // Project override manifest: only deploy rust-engineer.
    // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
    let manifest_dir = project.join(".trusty-mpm").join("framework");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[agents]\ninclude = [\"rust-engineer\"]\n",
    )
    .unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        report
            .deploy
            .deployed
            .contains(&"rust-engineer.md".to_string()),
        "included agent must deploy"
    );
    assert!(
        !report
            .deploy
            .deployed
            .contains(&"base-engineer.md".to_string()),
        "excluded-by-omission agent must NOT deploy"
    );
    assert!(!fw.agent_deploy_dir().join("base-engineer.md").exists());
}

#[test]
#[serial_test::serial]
fn prepare_session_manifest_sets_default_style() {
    // Why: HR-2 — a manifest `[style] active` sets the default output style when
    // no `--style` flag and no `[style] active` config key override it.
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;

    // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
    let manifest_dir = project.join(".trusty-mpm").join("framework");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[style]\nactive = \"trusty-mpm-research\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_config_style_overrides_manifest() {
    // Why: HR-2 precedence — the `[style] active` CONFIG key must win over the
    // manifest's `[style] active` (config > manifest > default).
    // #3965: `#[serial]` + `$HOME` override — see
    // `prepare_session_writes_claude_md_and_stash` for why.
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;

    // Config selects teacher; manifest selects research. Config must win.
    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();
    // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
    let manifest_dir = project.join(".trusty-mpm").join("framework");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[style]\nactive = \"trusty-mpm-research\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-teacher"),
        "config [style] active must override the manifest's style"
    );
}

// ── write_status_line tests ───────────────────────────────────────────────────

/// Assert `cmd` is `"<absolute-path> statusline"` where the path is exactly
/// the current test binary's `current_exe()` (#1914: `write_status_line`
/// prefers `current_exe()` over a bare command).
///
/// Why the `canonicalize()` call: `resolve_statusline_binary` canonicalizes
/// `current_exe()` best-effort (symlink resolution, #1914 review finding 1)
/// before returning it, so the expected value here must apply the identical
/// transform — otherwise this assertion would be flaky on any platform where
/// the test binary's path traverses a symlink (e.g. macOS `/tmp` ->
/// `/private/tmp`).
fn assert_resolved_statusline_command(cmd: &str) {
    // #2229: the resolved statusline command must invoke the `statusline`
    // subcommand and must NOT bake an ephemeral build/worktree path (the test
    // process's own `current_exe()` is `target/debug/deps/...`, exactly the path
    // that must be rejected). The binary is either a stable PATH-resolved
    // install (`~/.cargo/bin/tm`) or the bare `tm`/`trusty-mpm` fallback — never
    // the transient artifact — so we assert the invariant, not an exact path
    // (which would depend on whether `tm` is installed in the test env).
    let binary = cmd
        .strip_suffix(" statusline")
        .unwrap_or_else(|| panic!("command must end with ' statusline', got {cmd}"));
    assert!(
        !trusty_common::bin_resolve::is_ephemeral_build_path(std::path::Path::new(binary)),
        "resolved statusline binary must not be an ephemeral build path, got {binary}"
    );
}

#[test]
fn write_status_line_injects_when_absent() {
    // When no settings.json exists, write_status_line creates it with statusLine
    // resolved to the absolute current-exe path (#1914), not a bare command.
    let tmp = tempdir().unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(tmp.path().join(".claude").join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["statusLine"]["type"], "command", "type must be command");
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
    assert_eq!(v["statusLine"]["padding"], 0, "padding must be 0");
}

#[test]
fn write_status_line_skips_when_already_set() {
    // When statusLine already exists (a genuine user customization), write_status_line
    // must not overwrite it.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing =
        serde_json::json!({"statusLine": {"type": "command", "command": "my custom cmd"}});
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds without modifying");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v["statusLine"]["command"], "my custom cmd",
        "existing statusLine must not be overwritten"
    );
}

#[test]
fn write_status_line_preserves_user_config() {
    // #1914 review finding 3: this test must seed a GENUINELY custom
    // statusLine.command (not one of the bare defaults write_status_line
    // itself would have written) so it actually exercises "leave user
    // customizations alone", rather than the absent-key injection path
    // covered separately by `write_status_line_injects_when_absent` or the
    // stale-default heal path covered by `write_status_line_heals_stale_*`.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({
        "outputStyle": "trusty-mpm-research",
        "someKey": true,
        "statusLine": {"type": "command", "command": "my-custom-statusline", "padding": 2}
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v["outputStyle"], "trusty-mpm-research",
        "outputStyle must be preserved"
    );
    assert_eq!(v["someKey"], true, "arbitrary keys must be preserved");
    assert_eq!(
        v["statusLine"]["command"], "my-custom-statusline",
        "a genuinely custom statusLine.command must never be overwritten"
    );
    assert_eq!(
        v["statusLine"]["padding"], 2,
        "the rest of a custom statusLine entry must also survive untouched"
    );
}

#[test]
fn write_status_line_heals_stale_tm_default() {
    // #1914 self-heal: a pre-#1914 bare "tm statusline" default on disk (the
    // literal fingerprint this module used to write) is upgraded IN PLACE to
    // the resolved absolute path, so `ensure_status_line`'s resume self-heal
    // (#1913) also fixes the PATH-resolution risk without a separate hook.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({"statusLine": {"type": "command", "command": "tm statusline", "padding": 0}});
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
    assert_eq!(
        v["statusLine"]["padding"], 0,
        "padding must survive the in-place upgrade"
    );
}

#[test]
fn write_status_line_heals_stale_trusty_mpm_default() {
    // Same self-heal, exercised against the `trusty-mpm` binary-name fingerprint
    // (the second `[[bin]]` this crate produces).
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({
        "statusLine": {"type": "command", "command": "trusty-mpm statusline", "padding": 0}
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
}

// ── is_stale_bare_statusline_command tests ────────────────────────────────────

#[test]
fn is_stale_bare_statusline_command_matches_known_defaults() {
    assert!(is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline"})
    ));
    assert!(is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "trusty-mpm statusline"})
    ));
}

#[test]
fn is_stale_bare_statusline_command_ignores_custom_command() {
    // A user's own custom command must never be flagged for in-place upgrade,
    // even if it happens to invoke `tm` with extra arguments.
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline --compact"})
    ));
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "my custom cmd"})
    ));
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "/opt/homebrew/bin/tm statusline"})
    ));
}

#[test]
fn is_stale_bare_statusline_command_ignores_non_command_type() {
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "text", "command": "tm statusline"})
    ));
}

// ── is_stale_statusline_command tests (#2229) ─────────────────────────────────

#[test]
fn is_stale_statusline_command_flags_missing_and_ephemeral() {
    // Bare pre-#1914 defaults are stale (superset of is_stale_bare).
    assert!(is_stale_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline"})
    ));
    // An ephemeral build path is stale even though it is absolute.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/repo/target/debug/deps/trusty_mpm-abc statusline"
    })));
    // A worktree path is stale.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/repo/.claude/worktrees/fix-1/target/release/tm statusline"
    })));
    // An absolute path that does not exist on disk is stale.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/no/such/dir/definitely-missing-2229 statusline"
    })));
}

#[cfg(unix)]
#[test]
fn is_stale_statusline_command_respects_existing_custom_binary() {
    // An absolute binary that EXISTS and is not ephemeral is a genuine
    // customization — never flagged. `/bin/sh` exists on every supported unix.
    assert!(!is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/bin/sh statusline"
    })));
    // A command that is not our "<binary> statusline" shape is never flagged.
    assert!(!is_stale_statusline_command(
        &serde_json::json!({"type": "command", "command": "my-custom-line"})
    ));
    // Non-command entries are ignored.
    assert!(!is_stale_statusline_command(&serde_json::json!({
        "type": "text",
        "command": "/no/such/dir/x statusline"
    })));
}

// ── resolve_statusline_binary_with tests ──────────────────────────────────────

#[test]
fn resolve_statusline_binary_with_prefers_current_exe() {
    let resolved = resolve_statusline_binary_with(
        || Ok(PathBuf::from("/abs/path/to/tm")),
        |_name| Some(PathBuf::from("/should/not/be/used/tm")),
    );
    assert_eq!(resolved, "/abs/path/to/tm");
}

#[test]
fn resolve_statusline_binary_with_rejects_ephemeral_current_exe() {
    // #2229: when current_exe() is an ephemeral build/worktree path it must be
    // rejected so the PATH-lookup fallback (a stable installed binary) is used
    // instead — never the transient artifact.
    let resolved = resolve_statusline_binary_with(
        || Ok(PathBuf::from("/repo/target/debug/deps/trusty_mpm-abc123")),
        |name| {
            assert_eq!(
                name, "tm",
                "path lookup must search for the bare 'tm' name first"
            );
            Some(PathBuf::from("/opt/homebrew/bin/tm"))
        },
    );
    assert_eq!(
        resolved, "/opt/homebrew/bin/tm",
        "an ephemeral current_exe must be rejected in favour of the PATH-resolved install"
    );
}

/// Why (#4492, same root cause as #4485): `statusLine.command` is resolved by
/// this function, and the #2229 ephemeral guard it consults knew only about
/// build/worktree layouts. A `current_exe()` under the agent harness's temp
/// scratchpad therefore passed as "stable" and was persisted, leaving
/// `statusLine.command` pointing at a dead libtest harness. Only the hooks call
/// site had coverage for this, which is why the statusline path regressed
/// independently.
/// What: injects each system-temp `current_exe()` shape and asserts the
/// PATH-resolved install wins instead.
#[test]
fn resolve_statusline_binary_with_rejects_system_temp_current_exe() {
    let leaked: Vec<String> = [
        PathBuf::from("/private/tmp/claude-502/-Users-x-proj/9f1c/scratchpad/base-bins/tm"),
        PathBuf::from("/tmp/tm"),
        std::env::temp_dir().join("claude-4485/base-bins/tm"),
    ]
    .into_iter()
    .map(|exe| {
        resolve_statusline_binary_with(
            move || Ok(exe.clone()),
            |_name| Some(PathBuf::from("/opt/homebrew/bin/tm")),
        )
    })
    .filter(|resolved| resolved != "/opt/homebrew/bin/tm")
    .collect();
    assert!(
        leaked.is_empty(),
        "a system temp current_exe must be rejected in favour of the PATH-resolved install \
         (#4492), but these were persisted verbatim: {leaked:#?}"
    );
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_path_lookup() {
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |name| {
            assert_eq!(name, "tm", "path lookup must search for the bare 'tm' name");
            Some(PathBuf::from("/opt/homebrew/bin/tm"))
        },
    );
    assert_eq!(resolved, "/opt/homebrew/bin/tm");
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_trusty_mpm_name() {
    // #1914 review finding 1: a machine with ONLY `trusty-mpm` on PATH (not
    // the `tm` alias) must still resolve when current_exe() is unavailable —
    // a bare single-name "tm" PATH lookup would silently degrade to the bare
    // literal here, reproducing the exact bug this module fixes.
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |name| match name {
            "tm" => None,
            "trusty-mpm" => Some(PathBuf::from("/opt/homebrew/bin/trusty-mpm")),
            other => panic!("unexpected binary name looked up: {other}"),
        },
    );
    assert_eq!(resolved, "/opt/homebrew/bin/trusty-mpm");
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_bare_name() {
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |_name| None,
    );
    assert_eq!(
        resolved, "tm",
        "must degrade to the bare literal when both sources fail"
    );
}

// ── Issue #4203: the isolated deploy layout must land in a tier the spawn reads ──

/// Why (#4203): `tm launch`, `tm connect`, and `tm meta launch` all spawn a
/// harness carrying `--setting-sources project,local` and then deploy the agent
/// roster. If the deploy destination is not in a tier that flag names, the
/// roster is invisible to the session it was deployed for — and nothing errors,
/// because the deploy itself succeeds. This asserts the RELATIONSHIP between the
/// two, with both sides derived from production code, so it keeps biting if
/// either moves. `isolated_framework_paths` is the single layout every isolated
/// caller now shares, so this one test covers all three CLI paths at once.
#[test]
fn isolated_layout_deploys_into_a_tier_the_spawn_reads() {
    // Derive the tier list from the flag itself — never hard-coded, so the
    // check and the spawned command cannot drift apart.
    let (flag, tier_list) = crate::core::model_inject::SETTING_SOURCES_FLAG
        .split_once(' ')
        .expect("SETTING_SOURCES_FLAG must be `--setting-sources <tiers>`");
    assert_eq!(flag, "--setting-sources");
    let tiers: Vec<&str> = tier_list.split(',').map(str::trim).collect();
    assert!(
        !tiers.is_empty() && !tiers.contains(&"user"),
        "this invariant is only meaningful while the flag names tiers and excludes \
         `user` (#1269); got {tiers:?}"
    );
    assert!(
        tiers.contains(&"project"),
        "the isolated deploy targets the project tier, so the flag must name it; \
         got {tiers:?}"
    );

    // `project_dir` is the cwd the harness is spawned in: the managed worktree
    // for `launch`, the live checkout for `connect`, the project dir for
    // `meta launch`.
    let project_dir = std::path::Path::new("/work/some-checkout");
    let fw = isolated_framework_paths(project_dir);

    // `<cwd>/.claude` is exactly what the `project` (and `local`) tiers read.
    assert_eq!(
        fw.claude_home_dir(),
        project_dir,
        "the deploy base must BE the harness cwd — anything else is the `user`-tier \
         mismatch of #4203"
    );
    assert_eq!(
        fw.claude_agents_dir(),
        project_dir.join(".claude").join("agents"),
        "agents must land under the harness cwd, not $HOME"
    );
    assert_eq!(
        fw.claude_skills_dir(),
        project_dir.join(".claude").join("skills"),
        "skills must land under the harness cwd, not $HOME"
    );
}

/// Why (#1931, restated for #4203): only the deploy DESTINATION moves
/// workspace-local. If the framework SOURCE paths moved with it, the isolated
/// seam would deploy from an empty `<workspace>/.trusty-mpm` and every session
/// would come up with a zero-agent roster — a worse failure than the one #4203
/// fixes, and equally silent.
///
/// Why serial: reads `FrameworkPaths::default()`, which resolves
/// `dirs::home_dir()`; serialized against the other `HOME`-redirecting tests in
/// this binary (the #2461 sweep).
#[test]
#[serial_test::serial]
fn isolated_layout_keeps_framework_source_at_the_install_root() {
    let project_dir = std::path::Path::new("/work/some-checkout");
    let fw = isolated_framework_paths(project_dir);

    assert_eq!(
        fw.root,
        FrameworkPaths::default().root,
        "the framework install root must NOT move workspace-local"
    );
    assert!(
        !fw.agent_source_dir().starts_with(project_dir),
        "agent SOURCE must never resolve inside the deploy destination; got {}",
        fw.agent_source_dir().display()
    );
    assert!(
        !fw.skill_source_dir().starts_with(project_dir),
        "skill SOURCE must never resolve inside the deploy destination; got {}",
        fw.skill_source_dir().display()
    );
}

/// // #4181: `resolve_palace_slug` outlives the memory injector that used to be
/// its only caller — it now derives the `TRUSTY_MEMORY_PALACE` the spawn
/// exports, so its git-remote fallback needs coverage of its own.
///
/// Why: a local-path session has no `repo_url`, so the slug must come from the
/// workspace's own `origin` remote. Without this the exported palace would be
/// the throwaway directory basename, which is #1605's original bug.
/// What: creates a temp repo with an `origin` remote and asserts the derived
/// slug is the `owner-repo` identity rather than the directory name.
/// Test: this is the test.
#[test]
#[serial_test::serial]
fn resolve_palace_slug_falls_back_to_git_remote() {
    let _env = EnvVarGuard::clear("TRUSTY_MEMORY_PALACE");
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("some-throwaway-dir");
    std::fs::create_dir_all(&project).unwrap();
    if !init_git_repo_with_origin(&project, "git@github.com:bobmatnyc/trusty-tools.git") {
        eprintln!("skipping: git unavailable");
        return;
    }

    assert_eq!(
        resolve_palace_slug(&project, None).as_deref(),
        Some("bobmatnyc-trusty-tools"),
        "the slug must come from the origin remote, not the directory basename"
    );
}

/// An explicit clone URL outranks the workspace's own remote (#1605).
///
/// Why: a repo_url-cloned managed session lives under a session-id directory
/// and may have no usable remote of its own, so `LaunchParams.repo_url` is the
/// authoritative identity.
/// What: passes an explicit remote and asserts it wins.
/// Test: this is the test.
#[test]
#[serial_test::serial]
fn resolve_palace_slug_prefers_the_explicit_remote() {
    let _env = EnvVarGuard::clear("TRUSTY_MEMORY_PALACE");
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("01H-session-id");
    std::fs::create_dir_all(&project).unwrap();

    assert_eq!(
        resolve_palace_slug(&project, Some("git@github.com:acme/widget.git")).as_deref(),
        Some("acme-widget")
    );
}

/// The operator's `TRUSTY_MEMORY_PALACE` override wins over both (#1605).
#[test]
#[serial_test::serial]
fn resolve_palace_slug_override_env_wins() {
    let _env = EnvVarGuard::set(
        "TRUSTY_MEMORY_PALACE",
        std::path::Path::new("operator-choice"),
    );
    let tmp = tempfile::tempdir().unwrap();

    assert_eq!(
        resolve_palace_slug(tmp.path(), Some("git@github.com:acme/widget.git")).as_deref(),
        Some("operator-choice")
    );
}
