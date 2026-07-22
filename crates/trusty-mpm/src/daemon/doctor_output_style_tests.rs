//! Unit tests for `daemon::doctor::doctor_output_style` — split out of
//! `doctor_output_style.rs` (test-file budget: 1500 SLOC; the inline module
//! pushed the production file over its 500-SLOC cap once issue #3453 added
//! the `.local.json` precedence-chain layers and the `legacy_ids` probe).
//! What: exercises `check_output_style`, `check_output_style_staleness`, and
//! `check_output_style_legacy_ids` across the full four-layer precedence
//! chain (`project-local` / `project` / `user-local` / `user`).
//! Test: this module IS the test suite for `super`.

use super::*;

/// Write `<dir>/.claude/settings.json` with the given raw JSON text.
fn write_settings(dir: &Path, json: &str) {
    let claude_dir = dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"), json).unwrap();
}

/// Write `<dir>/.claude/settings.local.json` with the given raw JSON text
/// (issue #3453 — the layer `check_output_style` previously never read).
fn write_local_settings(dir: &Path, json: &str) {
    let claude_dir = dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.local.json"), json).unwrap();
}

/// Deploy the default trusty-mpm style file under `<home>/.claude/output-styles/`.
fn deploy_default_style(home: &Path) {
    let styles_dir = home.join(".claude").join("output-styles");
    std::fs::create_dir_all(&styles_dir).unwrap();
    let default = OUTPUT_STYLES[0];
    std::fs::write(styles_dir.join(default.file_name), default.content).unwrap();
}

/// Deploy every bundled style, byte-exact, under `<home>/.claude/output-styles/`.
fn deploy_all_styles(home: &Path) {
    let styles_dir = home.join(".claude").join("output-styles");
    std::fs::create_dir_all(&styles_dir).unwrap();
    for style in OUTPUT_STYLES {
        std::fs::write(styles_dir.join(style.file_name), style.content).unwrap();
    }
}

#[test]
fn output_style_ok_when_style_resolves() {
    let home = tempfile::tempdir().unwrap();
    deploy_default_style(home.path());
    write_settings(home.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn output_style_fail_when_id_unknown() {
    // This is the exact incident condition: a stale/foreign style id.
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("claude_mpm"));
    assert!(
        check.message.contains("trusty-mpm"),
        "valid id list must be present"
    );
}

#[test]
fn output_style_warn_when_key_absent() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), r#"{}"#);
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn output_style_warn_when_settings_missing() {
    let home = tempfile::tempdir().unwrap();
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn output_style_fail_when_file_missing() {
    // A known id whose deployed file was never written (or was deleted).
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Fail);
}

#[test]
fn output_style_fail_on_malformed_json() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), "{not valid json");
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("not valid JSON"));
}

#[test]
fn output_style_prefers_project_over_global() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    deploy_default_style(home.path());
    // Global settings carry the bad incident value...
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
    // ...but the project-local settings (written by `prepare_session`)
    // carry the correct one and must take precedence.
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn output_style_falls_back_to_global_when_project_silent() {
    // Issue #1863: the exact incident condition. A project settings file
    // EXISTS (so the old `effective_settings_path` would stop looking
    // there) but does not set `outputStyle` — Claude Code itself falls
    // back to the global value in that case, and so must this check. The
    // global value here is the bad incident id, so this must Fail, not
    // silently report Warn/Ok from the project scope.
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
    write_settings(project.path(), r#"{"theme": "dark"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("claude_mpm"));
    assert!(
        check.message.contains("user"),
        "message must name the resolved scope: {}",
        check.message
    );
}

#[test]
fn output_style_falls_back_to_global_when_project_missing() {
    // No project settings file at all (e.g. `tm doctor` run before the
    // project has ever launched a session) must still catch a bad global
    // value.
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.message.contains("claude_mpm"));
}

#[test]
fn output_style_reports_project_scope_on_success() {
    // The `Ok` message must name which scope ("project") supplied the
    // resolved value, so operators can tell the two apart.
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    deploy_default_style(home.path());
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check.message.contains("project"),
        "message must name the resolved scope: {}",
        check.message
    );
}

#[test]
fn output_style_project_local_wins_even_when_project_settings_correct() {
    // The exact live-repro shape from issue #3453: `settings.json` was
    // already correct (rewritten by a repair pass), but the co-located
    // `settings.local.json` still carries the pre-rebrand id and Claude
    // Code applies IT, not the plain file, at the project scope. Before
    // this fix, `check_output_style` never even read `.local.json` and
    // reported a false `Ok` from `settings.json` alone.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    deploy_default_style(home.path());
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#); // correct
    write_local_settings(project.path(), r#"{"outputStyle": "claude_mpm"}"#); // stale, wins
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("claude_mpm"));
    assert!(
        check.message.contains("project-local"),
        "message must name the resolved scope: {}",
        check.message
    );
}

#[test]
fn output_style_project_local_overrides_project() {
    // Precedence must hold even when BOTH layers resolve to known,
    // deployed styles — proving the winner is genuinely selected by
    // precedence, not merely by which one happens to be valid.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    deploy_all_styles(home.path());
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    write_local_settings(project.path(), r#"{"outputStyle": "trusty-mpm-teacher"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    assert!(check.message.contains("trusty-mpm-teacher"));
    assert!(check.message.contains("project-local"));
}

#[test]
fn output_style_user_local_overrides_user() {
    // Same format-precedence rule applied at the user scope (no project
    // dir at all): `settings.local.json` under `home` must win over
    // `settings.json` under the same `home`.
    let home = crate::test_support::hermetic_temp_dir();
    deploy_all_styles(home.path());
    write_settings(home.path(), r#"{"outputStyle": "trusty-mpm"}"#);
    write_local_settings(home.path(), r#"{"outputStyle": "trusty-mpm-teacher"}"#);
    let check = check_output_style(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    assert!(check.message.contains("trusty-mpm-teacher"));
    assert!(check.message.contains("user-local"));
}

#[test]
fn output_style_project_scope_still_outranks_user_local() {
    // Project (plain) settings must still beat a `.local` value at the
    // USER scope — scope (project vs. user) is the primary sort key,
    // format (`.local` vs. plain) only breaks ties WITHIN a scope.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    deploy_default_style(home.path());
    write_local_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#); // user-local, bad
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#); // project, good
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
    assert!(check.message.contains("project"));
}

#[test]
fn output_style_warn_names_all_checked_layers_when_none_configured() {
    // The no-`outputStyle`-key-anywhere case (the writing project's real
    // `settings.json` shape) must Warn, not Fail, and must name every
    // layer it checked so the operator can see the full resolution chain.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    write_settings(project.path(), r#"{"theme": "dark"}"#);
    let check = check_output_style(Some(project.path()), home.path());
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("settings.local.json"));
    assert!(check.message.contains("settings.json"));
}

#[test]
fn staleness_ok_when_in_sync() {
    // All three bundled styles deployed byte-exact must be Ok.
    let home = tempfile::tempdir().unwrap();
    deploy_all_styles(home.path());
    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn staleness_warns_on_drift() {
    // This is the exact incident condition (issue #2333): PR #2328
    // corrected the bundled content, but the deployed copy never got
    // refreshed. A byte-level mismatch must Warn and point at `tm install`.
    let home = tempfile::tempdir().unwrap();
    deploy_all_styles(home.path());
    let styles_dir = home.path().join(".claude").join("output-styles");
    let first = &OUTPUT_STYLES[0];
    std::fs::write(
        styles_dir.join(first.file_name),
        "stale pre-#2328 identity text",
    )
    .unwrap();

    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains(first.file_name));
    assert!(check.message.contains("tm install"));
}

#[test]
fn staleness_warns_on_orphan() {
    // A dormant foreign file (issue #2333's `claude-mpm.md` example) must
    // be flagged, but never deleted.
    let home = tempfile::tempdir().unwrap();
    deploy_all_styles(home.path());
    let styles_dir = home.path().join(".claude").join("output-styles");
    std::fs::write(styles_dir.join("claude-mpm.md"), "pre-rebrand lineage").unwrap();

    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("claude-mpm.md"));
    assert!(
        styles_dir.join("claude-mpm.md").exists(),
        "orphan detection must never delete the foreign file"
    );
}

#[test]
fn staleness_ok_when_dir_missing() {
    // Nothing deployed yet is not staleness — `check_output_style` already
    // reports on that state (Warn/Fail depending on configuration).
    let home = tempfile::tempdir().unwrap();
    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn staleness_ok_when_file_never_deployed() {
    // A style present in the bundle but never written to disk (a subset
    // deploy, or a deletion) must be skipped, not reported as drift —
    // that state is `check_output_style`'s Fail, not this probe's Warn.
    let home = tempfile::tempdir().unwrap();
    deploy_default_style(home.path()); // only OUTPUT_STYLES[0]
    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn staleness_orphan_exempts_configured_custom_id() {
    // A deliberately configured custom style id (unknown to OUTPUT_STYLES,
    // already `Fail`ed by `check_output_style` itself) is the operator's
    // own file, not a foreign leftover — orphan detection must not also
    // flag it.
    let home = tempfile::tempdir().unwrap();
    deploy_all_styles(home.path());
    write_settings(home.path(), r#"{"outputStyle": "my-custom-style"}"#);
    let styles_dir = home.path().join(".claude").join("output-styles");
    std::fs::write(
        styles_dir.join("my-custom-style.md"),
        "operator's own style",
    )
    .unwrap();

    let check = check_output_style_staleness(None, home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn legacy_ids_warns_on_shadowed_layer() {
    // The effective layer (project, since no project-local file exists)
    // resolves fine — `check_output_style` alone would report `Ok` — but
    // the USER-scope `settings.json` underneath carries the pre-rebrand
    // id. It never wins today, but it is a landmine the moment the
    // project file's key is ever removed; this probe must still Warn.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    deploy_default_style(home.path());
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#); // effective, fine
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#); // shadowed, stale

    let effective = check_output_style(Some(project.path()), home.path());
    assert_eq!(
        effective.status,
        CheckStatus::Ok,
        "sanity: effective layer must resolve fine, message: {}",
        effective.message
    );

    let check = check_output_style_legacy_ids(Some(project.path()), home.path());
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    assert!(check.message.contains("claude_mpm"));
    assert!(
        check.message.contains(
            &home
                .path()
                .join(".claude")
                .join("settings.json")
                .display()
                .to_string()
        ),
        "message must name the exact offending file: {}",
        check.message
    );
}

#[test]
fn legacy_ids_does_not_double_report_the_effective_layer() {
    // Both the effective (project-local) layer AND a shadowed (user)
    // layer carry the same stale id. `check_output_style` already Fails
    // on the effective one by name — this probe must report ONLY the
    // shadowed offender, not re-list the effective layer's file too.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    write_local_settings(project.path(), r#"{"outputStyle": "claude_mpm"}"#); // effective
    write_settings(home.path(), r#"{"outputStyle": "claude_mpm"}"#); // shadowed

    let check = check_output_style_legacy_ids(Some(project.path()), home.path());
    assert_eq!(
        check.status,
        CheckStatus::Warn,
        "message: {}",
        check.message
    );
    let user_path = home.path().join(".claude").join("settings.json");
    let project_local_path = project.path().join(".claude").join("settings.local.json");
    assert!(
        check.message.contains(&user_path.display().to_string()),
        "must name the shadowed offender: {}",
        check.message
    );
    assert!(
        !check
            .message
            .contains(&project_local_path.display().to_string()),
        "must NOT double-report the effective layer (already `Fail`ed by \
         check_output_style): {}",
        check.message
    );
}

#[test]
fn legacy_ids_ok_when_all_known() {
    // Both the effective AND a shadowed layer resolve to known, deployed
    // ids — no legacy id anywhere, so this must be `Ok`.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    deploy_all_styles(home.path());
    write_settings(project.path(), r#"{"outputStyle": "trusty-mpm"}"#); // effective
    write_settings(home.path(), r#"{"outputStyle": "trusty-mpm-teacher"}"#); // shadowed, valid

    let check = check_output_style_legacy_ids(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}

#[test]
fn legacy_ids_ok_when_none_configured() {
    // No `outputStyle` key anywhere — nothing to flag.
    let home = crate::test_support::hermetic_temp_dir();
    let project = crate::test_support::hermetic_temp_dir();
    let check = check_output_style_legacy_ids(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
}
