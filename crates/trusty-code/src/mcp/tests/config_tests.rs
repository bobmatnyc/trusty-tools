//! Two-tier resolution, and the Fail-Open Check (#5428).

use tempfile::tempdir;

use super::support::{global_file, one_stdio_server, project_file};
use crate::mcp::REMOTE_UNSUPPORTED;
use crate::mcp::config::{McpTier, resolve_at};

/// Why: the tier token is what a log line and a diagnostic render, so it is a
/// wire contract, not a `Debug` rendering.
#[test]
fn tier_tokens_are_stable() {
    assert_eq!(McpTier::Global.as_str(), "global");
    assert_eq!(McpTier::Project.as_str(), "project");
}

/// Why: the base case — a global file alone must produce its servers, graded
/// usable, with nothing to report.
#[test]
fn a_global_only_file_loads_every_server() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), &one_stdio_server("alpha", "/bin/echo"));

    let resolved = resolve_at(&global, project.path());

    assert_eq!(resolved.servers.len(), 1);
    assert_eq!(resolved.servers[0].name, "alpha");
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
    assert_eq!(resolved.usable().len(), 1);
    assert_eq!(resolved.statuses[0].tier, McpTier::Global);
}

/// Why: a fresh install has no `servers.toml`, and that is not a fault — it
/// must NOT produce an issue, or every clean machine renders a false alarm.
#[test]
fn an_absent_global_file_is_empty_without_an_issue() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");

    let resolved = resolve_at(&home.path().join("servers.toml"), project.path());

    assert!(resolved.servers.is_empty());
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
}

/// Why: the Fail-Open Check this slice is gated on. A `servers.toml` that
/// exists and does not parse must never be indistinguishable from an empty
/// catalog. This test fails if the error arm is ever replaced with
/// `unwrap_or_default()`: that would keep `servers` empty (the first assertion
/// still passes) while silently emptying `issues` — which the second and third
/// assertions catch.
#[test]
fn a_malformed_global_file_yields_zero_servers_and_an_issue() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), "this is not = = valid toml [[[");

    let resolved = resolve_at(&global, project.path());

    assert!(
        resolved.servers.is_empty(),
        "a broken file grants no servers"
    );
    assert!(
        !resolved.issues.is_empty(),
        "a broken file MUST be reported, never silently read as an empty catalog",
    );
    let issue = &resolved.issues[0];
    assert_eq!(issue.tier, McpTier::Global);
    assert_eq!(issue.path, global, "the issue names the file to fix");
    assert!(!issue.remedy.is_empty(), "a finding carries its remedy");
}

/// Why: the project tier is optional; its absence is not a finding.
#[test]
fn an_absent_project_file_is_no_overrides() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), &one_stdio_server("alpha", "/bin/echo"));

    let resolved = resolve_at(&global, project.path());

    assert!(resolved.overrides.is_empty());
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
}

/// Why: a project may narrow what it connects to — that can only reduce what
/// runs, so it needs no trust check and must take effect.
#[test]
fn a_project_disable_hides_a_global_server() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let mut text = one_stdio_server("alpha", "/bin/echo");
    text.push_str(&one_stdio_server("beta", "/bin/cat"));
    let global = global_file(home.path(), &text);
    project_file(project.path(), "disabled = [\"alpha\"]\n");

    let resolved = resolve_at(&global, project.path());

    let names: Vec<&str> = resolved.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["beta"]);
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
}

/// Why: the singular spelling is the one a person reaches for first, and
/// `deny_unknown_fields` would otherwise turn it into a confusing parse error.
#[test]
fn the_singular_disable_key_is_accepted_as_an_alias() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), &one_stdio_server("alpha", "/bin/echo"));
    project_file(project.path(), "disable = [\"alpha\"]\n");

    let resolved = resolve_at(&global, project.path());

    assert!(resolved.servers.is_empty());
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
}

/// Why: the security ruling. A repo-tracked file may not introduce a command
/// this harness spawns, and the refusal must be VISIBLE — a silently dropped
/// entry reads as a bug in the loader rather than as a policy.
#[test]
fn a_project_set_with_a_new_command_is_refused() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), &one_stdio_server("alpha", "/bin/echo"));
    project_file(
        project.path(),
        "[[servers]]\nname = \"alpha\"\n[servers.transport]\ntype = \"stdio\"\ncommand = \"/tmp/attacker\"\n",
    );

    let resolved = resolve_at(&global, project.path());

    assert_eq!(resolved.servers.len(), 1, "the global entry survives");
    let McpTransportProbe(command) = probe(&resolved.servers[0]);
    assert_eq!(command, "/bin/echo", "the global command is what runs");
    assert_eq!(resolved.issues.len(), 1, "{:?}", resolved.issues);
    assert_eq!(resolved.issues[0].tier, McpTier::Project);
    assert!(
        resolved.issues[0].remedy.contains("servers.toml"),
        "the remedy names the operator's own file: {}",
        resolved.issues[0].remedy,
    );
}

/// Why: a mistyped key that silently dropped a `disabled` list would be the
/// exact invisible failure the Fail-Open Check rules out.
#[test]
fn a_misspelled_key_is_refused_with_an_issue() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(home.path(), &one_stdio_server("alpha", "/bin/echo"));
    project_file(project.path(), "disabeld = [\"alpha\"]\n");

    let resolved = resolve_at(&global, project.path());

    assert_eq!(
        resolved.servers.len(),
        1,
        "the override did not take effect"
    );
    assert_eq!(resolved.issues.len(), 1, "and the run is told why");
    assert_eq!(resolved.issues[0].tier, McpTier::Project);
}

/// Why: a disabled entry must be shown as disabled, not omitted — a caller has
/// to be able to render what it is choosing not to connect to.
#[test]
fn a_disabled_server_is_reported_not_dropped() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(
        home.path(),
        "[[servers]]\nname = \"alpha\"\nenabled = false\n[servers.transport]\ntype = \"stdio\"\ncommand = \"/bin/echo\"\n",
    );

    let resolved = resolve_at(&global, project.path());

    assert_eq!(resolved.servers.len(), 1);
    assert!(!resolved.statuses[0].usable);
    assert_eq!(
        resolved.statuses[0].detail.as_deref(),
        Some("disabled in configuration")
    );
    assert!(resolved.usable().is_empty());
}

/// Why: slice 1 speaks stdio only. An `http` entry must be recognised and
/// reported, never silently dropped — dropping it reads as a missing config
/// rather than an unimplemented transport.
#[test]
fn a_remote_transport_is_reported_not_usable() {
    let home = tempdir().expect("tempdir");
    let project = tempdir().expect("tempdir");
    let global = global_file(
        home.path(),
        "[[servers]]\nname = \"remote\"\n[servers.transport]\ntype = \"http\"\nurl = \"https://example.invalid/mcp\"\n",
    );

    let resolved = resolve_at(&global, project.path());

    assert_eq!(resolved.servers.len(), 1, "it stays visible");
    assert!(!resolved.statuses[0].usable);
    assert_eq!(
        resolved.statuses[0].detail.as_deref(),
        Some(REMOTE_UNSUPPORTED)
    );
    assert!(
        resolved.issues.is_empty(),
        "an unimplemented transport is a status, not a config fault",
    );
}

/// The stdio command of a resolved server, for assertions.
struct McpTransportProbe(String);

fn probe(server: &trusty_mcp::config::McpServerConfig) -> McpTransportProbe {
    match &server.transport {
        trusty_mcp::config::McpTransport::Stdio { command, .. } => {
            McpTransportProbe(command.clone())
        }
        other => panic!("expected a stdio transport, got {other:?}"),
    }
}
