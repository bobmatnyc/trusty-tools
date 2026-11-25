//! Unit tests for the Python tool plugin (config parsing + execution wiring).
//!
//! Why: Config validation and the argument/execution plumbing are testable
//! without a live Python interpreter for the pure paths.
//! What: Config + execution tests.
//! Test: This module is itself the test coverage.

use super::*;

/// Build a plugin that runs an inline `python3 -c "<body>"` invocation.
///
/// Why: Tests need a self-contained Python script with no external file
/// dependencies. Using the `-c:` script-path prefix routes execution to
/// `python3 -c <body>` while reusing all the spawn / IPC plumbing.
/// What: Construct a `PythonToolPlugin` with the given inline body and a
/// 5-second timeout (override the timeout for the timeout-path test).
fn inline_plugin(name: &str, body: &str, timeout_secs: u64) -> PythonToolPlugin {
    PythonToolPlugin {
        name: name.to_string(),
        description: "test plugin".to_string(),
        script: PathBuf::from(format!("-c:{body}")),
        schema: json!({"type": "object", "properties": {}}),
        timeout: Duration::from_secs(timeout_secs),
        restricted_tiers: Vec::new(),
        service_tier_guard: Vec::new(),
    }
}

/// Check whether `python3` is on PATH; if not, integration-style tests are
/// skipped (CI may not have python installed for a Rust crate).
fn python_available() -> bool {
    let python = crate::env_compat::env_var("TAGENT_PYTHON", "OPEN_MPM_PYTHON")
        .unwrap_or_else(|_| "python3".to_string());
    std::process::Command::new(&python)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Why: TOML round-trip for the `[[plugins.python]]` form with inline schema.
/// What: Parse a TOML doc containing one plugin entry; assert all fields
/// land in `PythonPluginConfig`.
#[test]
fn python_plugin_config_parses_inline_schema() {
    #[derive(Deserialize)]
    struct Wrapper {
        plugins: Plugins,
    }
    #[derive(Deserialize)]
    struct Plugins {
        python: Vec<PythonPluginConfig>,
    }

    let toml_src = r#"
[[plugins.python]]
name = "echo"
description = "Echo the query back"
script = "scripts/echo.py"
timeout_secs = 5
restricted_tiers = ["analytics"]

[plugins.python.schema]
type = "object"

[plugins.python.schema.properties]
query = { type = "string" }
"#;
    let parsed: Wrapper = toml::from_str(toml_src).expect("toml parses");
    assert_eq!(parsed.plugins.python.len(), 1);
    let p = &parsed.plugins.python[0];
    assert_eq!(p.name, "echo");
    assert_eq!(p.description, "Echo the query back");
    assert_eq!(p.script, PathBuf::from("scripts/echo.py"));
    assert_eq!(p.timeout_secs, Some(5));
    assert_eq!(p.restricted_tiers, vec!["analytics".to_string()]);
    assert!(p.schema.is_some());
    assert!(p.schema_file.is_none());
}

/// Why: TOML round-trip for the `schema_file` form (preferred when a
/// schema lives next to the script and is shared with other tooling).
/// What: Assert `schema_file` parses and `schema` is None.
#[test]
fn python_plugin_config_parses_schema_file() {
    #[derive(Deserialize)]
    struct Wrapper {
        plugins: Plugins,
    }
    #[derive(Deserialize)]
    struct Plugins {
        python: Vec<PythonPluginConfig>,
    }

    let toml_src = r#"
[[plugins.python]]
name = "gfa_report"
description = "GFA report"
script = "../cto/app/gfa.py"
schema_file = "../cto/app/gfa_schema.json"
"#;
    let parsed: Wrapper = toml::from_str(toml_src).expect("toml parses");
    let p = &parsed.plugins.python[0];
    assert_eq!(
        p.schema_file,
        Some(PathBuf::from("../cto/app/gfa_schema.json"))
    );
    assert!(p.schema.is_none());
    // Defaults
    assert!(p.timeout_secs.is_none());
    assert!(p.restricted_tiers.is_empty());
}

/// Why: Relative `script` paths in TOML must be resolved against the agent
/// TOML directory, not the process CWD (which is unpredictable).
/// What: Build a config with a relative path, hand it a base_dir, and
/// confirm the runtime `script` is `<base_dir>/<rel>`.
#[test]
fn python_tool_plugin_from_config_resolves_relative_paths() {
    let cfg = PythonPluginConfig {
        name: "x".into(),
        description: "y".into(),
        script: PathBuf::from("tools/x.py"),
        schema: Some(json!({"type": "object"})),
        schema_file: None,
        timeout_secs: None,
        restricted_tiers: vec![],
    };
    let base = Path::new("/agents/foo");
    let plugin = PythonToolPlugin::from_config(cfg, base).expect("from_config");
    assert_eq!(plugin.script, PathBuf::from("/agents/foo/tools/x.py"));
    assert_eq!(plugin.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
}

/// Why: `schema_file` must be read from disk at load time, parsed as JSON,
/// and stored as the runtime `schema`.
/// What: Write a temp JSON file, build a config pointing at it, assert the
/// runtime schema matches.
#[test]
fn python_tool_plugin_from_config_loads_schema_file() {
    let tmp = std::env::temp_dir().join(format!("om-plugin-schema-{}.json", Uuid::new_v4()));
    std::fs::write(
        &tmp,
        r#"{"type":"object","properties":{"q":{"type":"string"}}}"#,
    )
    .unwrap();

    let cfg = PythonPluginConfig {
        name: "x".into(),
        description: "y".into(),
        script: PathBuf::from("s.py"),
        schema: None,
        schema_file: Some(tmp.clone()),
        timeout_secs: None,
        restricted_tiers: vec![],
    };
    let plugin = PythonToolPlugin::from_config(cfg, Path::new("/anywhere")).unwrap();
    assert_eq!(plugin.schema["type"], "object");
    assert_eq!(plugin.schema["properties"]["q"]["type"], "string");

    let _ = std::fs::remove_file(&tmp);
}

/// Why: `to_tool_spec` is the wire shape the LLM sees; pin the envelope so
/// a structural change in `schema()` doesn't silently break tool calling.
/// What: Assert the standard OpenAI envelope keys are present.
#[test]
fn python_tool_plugin_to_tool_spec_shape() {
    let plugin = inline_plugin("echo", "pass", 30);
    let spec = plugin.to_tool_spec();
    assert_eq!(spec["type"], "function");
    assert_eq!(spec["function"]["name"], "echo");
    assert_eq!(spec["function"]["description"], "test plugin");
    assert_eq!(spec["function"]["parameters"]["type"], "object");
}

/// Why: RBAC guard returns true only when caller tier appears in the
/// plugin's `restricted_tiers` list.
/// What: Construct a plugin with `restricted_tiers = ["analytics"]`, assert
/// guard fires for "analytics" but not for "all".
#[test]
fn python_tool_plugin_rbac_guard() {
    let mut plugin = inline_plugin("p", "pass", 30);
    plugin.restricted_tiers = vec!["analytics".into(), "read_only".into()];
    assert!(plugin.is_restricted_for_tier("analytics"));
    assert!(plugin.is_restricted_for_tier("read_only"));
    assert!(!plugin.is_restricted_for_tier("all"));
    assert!(!plugin.is_restricted_for_tier(""));
}

/// Why: Happy-path execution — spawn an inline python script that reads
/// the tool_call line and echoes a tool_result. Proves the full IPC loop.
/// What: Inline python reads one line, parses it, emits a tool_result with
/// content "hello".
#[tokio::test]
async fn execute_happy_path_inline_python() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import sys, json
line = sys.stdin.readline()
req = json.loads(line)
out = {"type": "tool_result", "id": req["id"], "content": "hello", "status": "success"}
print(json.dumps(out), flush=True)
"#;
    let plugin = inline_plugin("echo", body, 10);
    let result = plugin.execute(json!({"q": "ignored"})).await;
    assert!(
        !result.is_error(),
        "expected success, got: {}",
        result.content()
    );
    assert_eq!(result.content(), "hello");
}

/// Why: A misbehaving plugin that sleeps past its timeout must be killed
/// and produce a structured timeout error rather than hanging the agent.
/// What: Inline python sleeps 10s; plugin timeout is 1s; expect error.
#[tokio::test]
async fn execute_returns_timeout_error() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import time, sys
time.sleep(10)
sys.stdout.write('never\n')
"#;
    let plugin = inline_plugin("slow", body, 1);
    let result = plugin.execute(json!({})).await;
    assert!(result.is_error(), "expected timeout error");
    assert!(
        result.content().contains("timed out"),
        "expected timeout message, got: {}",
        result.content()
    );
}

/// Why: A plugin that crashes (non-zero exit) without emitting a result
/// must not be reported as success. The error message should carry the
/// exit status for debugging.
/// What: Inline python calls `sys.exit(2)` without writing to stdout.
#[tokio::test]
async fn execute_returns_error_on_nonzero_exit() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import sys
sys.exit(2)
"#;
    let plugin = inline_plugin("crash", body, 5);
    let result = plugin.execute(json!({})).await;
    assert!(result.is_error(), "expected error on non-zero exit");
    // Either "exited with status" or "produced no stdout" is acceptable
    // (the order of stdout-close vs. exit observation is timing-dependent),
    // but neither is a success.
    let msg = result.content();
    assert!(
        msg.contains("exited with status") || msg.contains("produced no stdout"),
        "unexpected error message: {}",
        msg
    );
}

/// Why: A plugin that prints garbage on stdout must be surfaced as a parse
/// error, not silently swallowed (which would look like an empty success).
/// What: Inline python prints `not json` on stdout.
#[tokio::test]
async fn execute_returns_error_on_malformed_json() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import sys
sys.stdout.write('not json at all\n')
sys.stdout.flush()
"#;
    let plugin = inline_plugin("garbage", body, 5);
    let result = plugin.execute(json!({})).await;
    assert!(result.is_error(), "expected malformed-json error");
    assert!(
        result.content().contains("malformed JSON"),
        "unexpected error message: {}",
        result.content()
    );
}

/// Why: When the plugin reports `status = "error"` in its NDJSON result,
/// the harness must surface that as a `ToolResult::Error` rather than
/// treating it as success.
/// What: Inline python emits `{"type":"tool_result","status":"error",...}`.
#[tokio::test]
async fn execute_propagates_plugin_reported_error() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import sys, json
req = json.loads(sys.stdin.readline())
out = {"type": "tool_result", "id": req["id"], "status": "error", "error": "boom"}
print(json.dumps(out), flush=True)
"#;
    let plugin = inline_plugin("err", body, 5);
    let result = plugin.execute(json!({})).await;
    assert!(result.is_error());
    assert!(
        result.content().contains("boom"),
        "expected reported error to bubble up, got: {}",
        result.content()
    );
}

/// #3236: `from_config` must map `restricted_tiers` TOML strings into the
/// `ServiceTier` list the `ToolExecutor::restricted_tiers()` override
/// actually returns — proving the field is no longer decorative.
#[test]
fn python_tool_plugin_from_config_maps_restricted_tiers_to_service_tiers() {
    let cfg = PythonPluginConfig {
        name: "x".into(),
        description: "y".into(),
        script: PathBuf::from("s.py"),
        schema: Some(json!({"type": "object"})),
        schema_file: None,
        timeout_secs: None,
        restricted_tiers: vec!["analytics".into(), "read_only".into()],
    };
    let plugin = PythonToolPlugin::from_config(cfg, Path::new("/anywhere")).unwrap();
    assert_eq!(
        ToolExecutor::restricted_tiers(&plugin),
        &[ServiceTier::Analytics, ServiceTier::ReadOnly]
    );
}

/// #3236 fail-closed: an unrecognized tier string must refuse to load the
/// plugin rather than silently dropping the entry (which would widen
/// access beyond what the TOML author declared).
#[test]
fn python_tool_plugin_from_config_rejects_unknown_tier_string() {
    let cfg = PythonPluginConfig {
        name: "x".into(),
        description: "y".into(),
        script: PathBuf::from("s.py"),
        schema: Some(json!({"type": "object"})),
        schema_file: None,
        timeout_secs: None,
        restricted_tiers: vec!["not_a_real_tier".into()],
    };
    let err = PythonToolPlugin::from_config(cfg, Path::new("/anywhere"))
        .expect_err("unknown tier must be a load-time error, not a silent no-op");
    assert!(
        err.to_string().contains("not_a_real_tier"),
        "error should name the offending tier: {err}"
    );
}

/// #3236 end-to-end: proves the REAL production RBAC chokepoint
/// (`ToolRegistry::dispatch_for_user`) now denies a restricted-tier caller
/// for a `PythonToolPlugin`. Pre-fix, `restricted_tiers()` used the trait
/// default (`&[]`), so `user.can_access_tier(&[])` was unconditionally
/// `true` and this exact call would have gone through (no error, "denied"
/// output) — i.e. this test fails against pre-fix code, proving causality.
#[tokio::test]
async fn dispatch_for_user_denies_restricted_tier_python_plugin() {
    // Struct literal (not `from_config`) so the `-c:` inline-script marker
    // isn't corrupted by `from_config`'s base_dir join (see
    // `dispatch_for_user_allows_permitted_tier_python_plugin` below). The
    // inline body would emit a *successful* NDJSON result if actually
    // executed, so pre-fix (tier check bypassed) this test observes a
    // non-error "should have been denied" result rather than an unrelated
    // subprocess failure — a clean causality signal.
    let plugin = PythonToolPlugin {
        name: "secret_report".into(),
        description: "sensitive report".into(),
        script: PathBuf::from(
            "-c:import sys,json\nline=sys.stdin.readline()\nreq=json.loads(line)\nprint(json.dumps({'type':'tool_result','id':req['id'],'content':'should have been denied','status':'success'}))",
        ),
        schema: json!({"type": "object", "properties": {}}),
        timeout: Duration::from_secs(5),
        restricted_tiers: vec!["analytics".into()],
        service_tier_guard: parse_restricted_tiers(&["analytics".to_string()]).unwrap(),
    };

    let mut registry = crate::tools::ToolRegistry::new();
    registry.register(std::sync::Arc::new(plugin));

    let restricted_user =
        crate::rbac::UserIdentity::new("u1", "test", crate::rbac::ServiceTier::Analytics);
    let result = registry
        .dispatch_for_user("secret_report", json!({}), None, &restricted_user)
        .await;
    assert!(
        result.is_error(),
        "analytics-tier caller must be denied by the restricted_tiers wiring"
    );
    assert_eq!(
        result.content(),
        "This tool is not available for your access tier."
    );
}

/// The flip side of the previous test: a caller whose tier is NOT in
/// `restricted_tiers` still reaches real execution through the same
/// `dispatch_for_user` chokepoint — proving the fix doesn't over-restrict
/// legitimate dispatch.
#[tokio::test]
async fn dispatch_for_user_allows_permitted_tier_python_plugin() {
    if !python_available() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let body = r#"
import sys, json
line = sys.stdin.readline()
req = json.loads(line)
out = {"type": "tool_result", "id": req["id"], "content": "ok", "status": "success"}
print(json.dumps(out), flush=True)
"#;
    // Built via the same struct-literal pattern as `inline_plugin` (not
    // `from_config`) so the `-c:` inline-script prefix survives untouched —
    // `from_config` joins relative scripts onto `base_dir`, which would
    // corrupt the `-c:` marker into `/base/-c:...`. `service_tier_guard` is
    // computed with the real `parse_restricted_tiers` helper so this still
    // exercises the actual #3236 string->ServiceTier mapping.
    let plugin = PythonToolPlugin {
        name: "secret_report".into(),
        description: "sensitive report".into(),
        script: PathBuf::from(format!("-c:{body}")),
        schema: json!({"type": "object", "properties": {}}),
        timeout: Duration::from_secs(5),
        restricted_tiers: vec!["analytics".into()],
        service_tier_guard: parse_restricted_tiers(&["analytics".to_string()]).unwrap(),
    };

    let mut registry = crate::tools::ToolRegistry::new();
    registry.register(std::sync::Arc::new(plugin));

    let full_access_user =
        crate::rbac::UserIdentity::new("u2", "test", crate::rbac::ServiceTier::All);
    let result = registry
        .dispatch_for_user("secret_report", json!({}), None, &full_access_user)
        .await;
    assert!(
        !result.is_error(),
        "an unrestricted tier must still be able to dispatch: {}",
        result.content()
    );
    assert_eq!(result.content(), "ok");
}
