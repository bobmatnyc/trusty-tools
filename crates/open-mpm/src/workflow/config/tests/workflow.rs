//! Tests for the `WorkflowDef` / `PhaseDef` serde shapes, semantic config
//! validation, and the `safe_join` path guard.
//!
//! Why: Pins the JSON contract every workflow file relies on (phase fields,
//! auto-push/ticket defaults), the load-time semantic validation, and the
//! defense-in-depth `safe_join` containment guard.
//! What: A `#[cfg(test)]` submodule whose `super::super::*` resolves to the
//! parent `config` module.
//! Test: This file IS the test body.

use super::super::*;

#[test]
fn config_from_json_parses_minimal() {
    let raw = r#"
    {
        "name": "t",
        "description": "d",
        "phases": [
            {"name":"a","agent":"agent-a","context_template":"hi"},
            {"name":"b","agent":"agent-b","model_override":"m","context_template":"bye"}
        ]
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    assert_eq!(def.name, "t");
    assert_eq!(def.phases.len(), 2);
    assert_eq!(def.phases[0].name, "a");
    assert!(def.phases[0].model.is_none());
    assert_eq!(def.phases[1].model.as_deref(), Some("m"));
}

#[test]
fn parallel_subtasks_parse_from_json() {
    // #73: `parallel_subtasks` + `worktree_protection` land on PhaseDef
    // without breaking workflows that omit them.
    let raw = r#"
    {
        "name": "p",
        "phases": [
            {
                "name": "code",
                "agent": "code-agent",
                "context_template": "{{task}}",
                "parallel_subtasks": [
                    {"label": "a", "task_suffix": "do a"},
                    {"label": "b", "task_suffix": "do b"}
                ],
                "worktree_protection": true
            }
        ]
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    let subs = def.phases[0]
        .parallel_subtasks
        .as_ref()
        .expect("subtasks present");
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].label, "a");
    assert_eq!(subs[1].task_suffix, "do b");
    assert_eq!(def.phases[0].worktree_protection, Some(true));
}

#[test]
fn auto_push_config_defaults() {
    // #76: `auto_push` at workflow level parses with defaults.
    let raw = r#"
    {
        "name": "p",
        "phases": [{"name":"a","agent":"x","context_template":"t"}],
        "auto_push": {"enabled": true}
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    let ap = def.auto_push.expect("auto_push present");
    assert!(ap.enabled);
    assert_eq!(ap.version_bump, "patch");
    assert_eq!(ap.push_remote, "origin");
    assert_eq!(ap.push_branch, "main");
    assert!(ap.commit_message_template.contains("{{workflow}}"));
}

#[test]
fn phase_ast_native_parses_from_json() {
    // Per-phase ast_native override deserializes correctly.
    // Why: Hybrid mode (research+plan AST-native, code+qa traditional)
    // requires the field to round-trip through serde_json.
    // What: A JSON doc with `"ast_native": true` must produce
    // `Some(true)` on the PhaseDef.
    let raw = r#"{"name":"research","agent":"r","context_template":"t","ast_native":true}"#;
    let phase: PhaseDef = serde_json::from_str(raw).unwrap();
    assert_eq!(phase.ast_native, Some(true));

    let raw_false = r#"{"name":"qa","agent":"q","context_template":"t","ast_native":false}"#;
    let phase: PhaseDef = serde_json::from_str(raw_false).unwrap();
    assert_eq!(phase.ast_native, Some(false));
}

#[test]
fn phase_ast_native_defaults_to_none() {
    // Phases without `ast_native` inherit the global --ast-native flag.
    // Why: Backward compatibility — existing workflow JSON files must
    // continue to deserialize without the new field.
    // What: A PhaseDef parsed from JSON omitting `ast_native` must have
    // `ast_native == None`.
    let raw = r#"{"name":"code","agent":"e","context_template":"t"}"#;
    let phase: PhaseDef = serde_json::from_str(raw).unwrap();
    assert!(phase.ast_native.is_none());
}

#[test]
fn phase_skip_parses_and_defaults_to_none() {
    // #82: `skip: true` on a phase parses, and a phase without `skip` at
    // all leaves the field as `None` (treated as false by the engine).
    let raw = r#"
    {
        "name": "w",
        "phases": [
            {"name": "a", "agent": "x", "context_template": "t"},
            {"name": "b", "agent": "y", "context_template": "t", "skip": true}
        ]
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    assert_eq!(def.phases[0].skip, None);
    assert_eq!(def.phases[1].skip, Some(true));
}

#[test]
fn ticket_management_config_defaults() {
    // #84: A workflow JSON that includes a minimal `ticket_management`
    // block must parse with the boolean defaults (`auto_relate`,
    // `phase_comments`, `close_on_success` all true) and `enabled=false`
    // when omitted, so adding the field does not accidentally enable
    // ticket creation for workflows that never opted in.
    let raw = r#"
    {
        "name": "w",
        "phases": [{"name":"a","agent":"x","context_template":"t"}],
        "ticket_management": {"enabled": true, "repo": "owner/repo"}
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    let tm = def.ticket_management.expect("ticket_management present");
    assert!(tm.enabled);
    assert_eq!(tm.repo, "owner/repo");
    assert!(tm.auto_relate);
    assert!(tm.phase_comments);
    assert!(tm.close_on_success);

    // Omitting the block entirely leaves it as `None`.
    let raw_absent = r#"
    {
        "name": "w",
        "phases": [{"name":"a","agent":"x","context_template":"t"}]
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw_absent).unwrap();
    assert!(def.ticket_management.is_none());
}

#[test]
fn validate_rejects_enabled_ticket_management_without_repo() {
    // #102: `ticket_management.enabled = true` + empty `repo` must fail
    // validation with a clear message rather than silently producing a
    // 404 at runtime.
    let raw = r#"
    {
        "name": "w",
        "phases": [{"name":"a","agent":"x","context_template":"t"}],
        "ticket_management": {"enabled": true, "repo": ""}
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    let err = def.validate().expect_err("empty repo must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("repo"),
        "error should mention the empty repo field, got: {msg}"
    );
}

#[test]
fn validate_accepts_disabled_ticket_management_without_repo() {
    // #102: When disabled, an empty repo is irrelevant — validation must
    // still pass so operators can keep the block around for reference.
    let raw = r#"
    {
        "name": "w",
        "phases": [{"name":"a","agent":"x","context_template":"t"}],
        "ticket_management": {"enabled": false, "repo": ""}
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    assert!(def.validate().is_ok());
}

#[test]
fn validate_accepts_enabled_ticket_management_with_repo() {
    let raw = r#"
    {
        "name": "w",
        "phases": [{"name":"a","agent":"x","context_template":"t"}],
        "ticket_management": {"enabled": true, "repo": "owner/name"}
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    assert!(def.validate().is_ok());
}

#[test]
fn auto_push_absent_by_default() {
    let raw = r#"
    {
        "name": "p",
        "phases": [{"name":"a","agent":"x","context_template":"t"}]
    }
    "#;
    let def: WorkflowDef = serde_json::from_str(raw).unwrap();
    assert!(def.auto_push.is_none());
}

// -------- safe_join (#114) defense-in-depth path-traversal guard --------

#[test]
fn safe_join_rejects_parent_traversal() {
    // Why: ../../etc/passwd must NOT escape the out_dir, even when the
    // candidate would lexically resolve outside the base.
    // Test: assert safe_join returns None for parent-traversal attempts.
    let tmp = tempfile::tempdir().unwrap();
    assert!(safe_join(tmp.path(), "../../etc/passwd").is_none());
    assert!(safe_join(tmp.path(), "../escape.txt").is_none());
    assert!(safe_join(tmp.path(), "src/../../escape.txt").is_none());
}

#[test]
fn safe_join_rejects_absolute_path() {
    // Why: An absolute path like /tmp/evil must be rejected — we strip the
    // leading slash and treat it as relative; any subsequent escape via
    // `..` should still be caught.
    // Test: absolute paths are anchored to base; pure absolute paths that
    // don't traverse simply land inside base.
    let tmp = tempfile::tempdir().unwrap();
    // Pure traversal absolute paths must be rejected.
    assert!(safe_join(tmp.path(), "/etc/../../etc/passwd").is_none());
    // But a normal absolute path becomes relative-to-base (defensive — we
    // never want a write to actually land at /tmp/evil regardless).
    let resolved = safe_join(tmp.path(), "/tmp/evil").expect("anchored");
    assert!(resolved.starts_with(tmp.path().canonicalize().unwrap()));
}

#[test]
fn safe_join_allows_normal_path() {
    // Why: Legitimate relative paths like src/main.py must succeed and
    // resolve inside the base.
    let tmp = tempfile::tempdir().unwrap();
    let resolved = safe_join(tmp.path(), "src/main.py").expect("legit path");
    assert!(resolved.starts_with(tmp.path().canonicalize().unwrap()));
    assert!(resolved.ends_with("src/main.py"));
}

#[test]
fn safe_join_allows_nested_path() {
    // Why: Deep nested legitimate paths (pkg/sub/module.py) must succeed.
    let tmp = tempfile::tempdir().unwrap();
    let resolved = safe_join(tmp.path(), "pkg/sub/module.py").expect("nested path");
    assert!(resolved.starts_with(tmp.path().canonicalize().unwrap()));
    assert!(resolved.ends_with("pkg/sub/module.py"));
}

#[test]
fn safe_join_resolves_internal_dotdot_safely() {
    // Why: `pkg/../pkg2/file.py` is a legitimate (if odd) construct — the
    // parent is consumed by `pkg`. It should resolve inside base.
    let tmp = tempfile::tempdir().unwrap();
    let resolved = safe_join(tmp.path(), "pkg/../pkg2/file.py").expect("dotdot ok");
    assert!(resolved.starts_with(tmp.path().canonicalize().unwrap()));
    assert!(resolved.ends_with("pkg2/file.py"));
}
