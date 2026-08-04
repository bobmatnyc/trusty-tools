//! Tests for monorepo workspace-member discovery (#4760).
//!
//! Why: this module reads three declaration formats and expands globs against a
//! real filesystem, and every one of its bounds is a fail-closed path that
//! silently returns "nothing found". Silent-nothing is exactly the failure this
//! module exists to remove, so each bound is pinned explicitly rather than
//! trusted.
//! What: declaration parsing per format, glob expansion and its limits, the
//! path-escape rejection, and each of the three bounds.
//! Test: this file.

use super::*;
use std::fs;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn mkdir(dir: &Path, rel: &str) {
    fs::create_dir_all(dir.join(rel)).unwrap();
}

/// Member paths relative to the root, for readable assertions.
fn members(root: &Path) -> Vec<String> {
    let budget = ProbeBudget::new();
    probe_roots(root, &budget)
        .into_iter()
        .skip(1) // the root itself is always first
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn single_package_project_probes_only_root() {
    // Zero regression for the common case: no workspace declaration means the
    // probe set is exactly what it was before this module existed.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies":{"react":"^18"}}"#,
    );
    let budget = ProbeBudget::new();
    let roots = probe_roots(tmp.path(), &budget);
    assert_eq!(roots, vec![tmp.path().to_path_buf()]);
}

#[test]
fn npm_workspaces_array_members() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/*","apps/*"]}"#,
    );
    mkdir(tmp.path(), "packages/ui");
    mkdir(tmp.path(), "packages/core");
    mkdir(tmp.path(), "apps/web");
    assert_eq!(
        members(tmp.path()),
        vec!["packages/core", "packages/ui", "apps/web"]
    );
}

#[test]
fn npm_workspaces_object_form() {
    // yarn classic's `{ "workspaces": { "packages": [...] } }` spelling.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":{"packages":["libs/*"],"nohoist":["**/x"]}}"#,
    );
    mkdir(tmp.path(), "libs/a");
    assert_eq!(members(tmp.path()), vec!["libs/a"]);
}

#[test]
fn declared_globs_beat_conventional_paths() {
    // The declaration is honoured, not `packages/*` assumed: a workspace using
    // `services/*` is covered, and an UNDECLARED `packages/` is NOT probed.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["services/*"]}"#,
    );
    mkdir(tmp.path(), "services/api");
    mkdir(tmp.path(), "packages/ignored");
    assert_eq!(members(tmp.path()), vec!["services/api"]);
}

#[test]
fn pnpm_workspace_members() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - 'packages/*'\n  - \"apps/*\"\n  - tools/cli\n",
    );
    mkdir(tmp.path(), "packages/ui");
    mkdir(tmp.path(), "apps/web");
    mkdir(tmp.path(), "tools/cli");
    let m = members(tmp.path());
    assert!(m.contains(&"packages/ui".to_string()));
    assert!(m.contains(&"apps/web".to_string()));
    assert!(m.contains(&"tools/cli".to_string()), "literal path member");
}

#[test]
fn pnpm_workspace_ignores_other_keys() {
    // A list under a different key must not be read as a member glob.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "pnpm-workspace.yaml",
        "catalog:\n  - 'should/not/count'\npackages:\n  - 'apps/*'\n",
    );
    mkdir(tmp.path(), "apps/web");
    mkdir(tmp.path(), "should/not/count");
    assert_eq!(members(tmp.path()), vec!["apps/web"]);
}

#[test]
fn elixir_umbrella_members() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "mix.exs",
        "defmodule Umbrella.MixProject do\n  def project do\n    [apps_path: \"apps\"]\n  end\nend\n",
    );
    mkdir(tmp.path(), "apps/my_app");
    mkdir(tmp.path(), "apps/my_app_web");
    assert_eq!(members(tmp.path()), vec!["apps/my_app", "apps/my_app_web"]);
}

#[test]
fn plain_mix_project_declares_no_members() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "mix.exs",
        "defp deps, do: [{:jason, \"~> 1.4\"}]\n",
    );
    mkdir(tmp.path(), "apps/not_an_umbrella");
    assert!(members(tmp.path()).is_empty());
}

#[test]
fn malformed_package_json_declares_no_members() {
    // Fail-closed: unparseable JSON yields no members rather than an error.
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "package.json", "{ not json");
    mkdir(tmp.path(), "packages/a");
    assert!(members(tmp.path()).is_empty());
}

#[test]
fn unknown_root_declares_no_members() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "README.md", "# hi\n");
    assert!(members(tmp.path()).is_empty());
}

#[test]
fn glob_expands_one_level() {
    // `packages/*` reaches direct children and NOT grandchildren — the depth
    // bound, asserted rather than assumed.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/*"]}"#,
    );
    mkdir(tmp.path(), "packages/a/nested/deep");
    assert_eq!(members(tmp.path()), vec!["packages/a"]);
}

#[test]
fn double_star_is_one_level() {
    // `**` carries no recursive meaning here; it behaves as a single `*`. The
    // limitation is real and this pins it so it cannot be mistaken for support.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/**"]}"#,
    );
    mkdir(tmp.path(), "packages/a/deep");
    assert_eq!(members(tmp.path()), vec!["packages/a"]);
}

#[test]
fn prefix_glob_matches_named_members() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "package.json", r#"{"workspaces":["pkg-*"]}"#);
    mkdir(tmp.path(), "pkg-ui");
    mkdir(tmp.path(), "other");
    assert_eq!(members(tmp.path()), vec!["pkg-ui"]);
}

#[test]
fn literal_member_path() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "package.json", r#"{"workspaces":["apps/web"]}"#);
    mkdir(tmp.path(), "apps/web");
    assert_eq!(members(tmp.path()), vec!["apps/web"]);
}

#[test]
fn files_are_never_members() {
    // Only directories can be member roots.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/*"]}"#,
    );
    write(tmp.path(), "packages/README.md", "x");
    mkdir(tmp.path(), "packages/real");
    assert_eq!(members(tmp.path()), vec!["packages/real"]);
}

#[test]
fn pattern_with_parent_escape_is_rejected() {
    // A declaration must not be able to walk outside the project.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["../escape/*","packages/*"]}"#,
    );
    mkdir(tmp.path(), "packages/ok");
    assert_eq!(
        members(tmp.path()),
        vec!["packages/ok"],
        "the `..` pattern contributes nothing"
    );
}

#[test]
fn member_count_is_capped() {
    // The enumeration bound, exercised past its limit.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/*"]}"#,
    );
    for i in 0..(MAX_WORKSPACE_MEMBERS + 20) {
        mkdir(tmp.path(), &format!("packages/p{i:04}"));
    }
    let budget = ProbeBudget::new();
    let roots = probe_roots(tmp.path(), &budget);
    assert_eq!(
        roots.len(),
        MAX_WORKSPACE_MEMBERS + 1,
        "root plus at most MAX_WORKSPACE_MEMBERS members"
    );
}

#[test]
fn oversized_manifest_is_not_read() {
    // The per-file cap wins even though the declaration is present and valid.
    let tmp = TempDir::new().unwrap();
    let mut body = String::from(r#"{"workspaces":["packages/*"],"pad":""#);
    body.push_str(&"x".repeat(MAX_MANIFEST_BYTES as usize + 1));
    body.push_str("\"}");
    write(tmp.path(), "package.json", &body);
    mkdir(tmp.path(), "packages/a");
    assert!(members(tmp.path()).is_empty());
}

#[test]
fn budget_exhaustion_stops_probing() {
    // With no budget left, a readable in-cap file still reads as absent —
    // fail-closed, never an error.
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"workspaces":["packages/*"]}"#,
    );
    mkdir(tmp.path(), "packages/a");

    let generous = ProbeBudget::new();
    assert_eq!(probe_roots(tmp.path(), &generous).len(), 2);

    let starved = ProbeBudget::with_bytes(0);
    assert_eq!(
        probe_roots(tmp.path(), &starved).len(),
        1,
        "no budget -> root only"
    );
}

#[test]
fn budget_is_shared_across_members() {
    // One allowance covers the whole call, so `members x per-file-cap` cannot be
    // spent in aggregate. A budget sized to a single small read is exhausted by
    // the root manifest and leaves nothing for members.
    let tmp = TempDir::new().unwrap();
    let decl = r#"{"workspaces":["packages/*"]}"#;
    write(tmp.path(), "package.json", decl);
    mkdir(tmp.path(), "packages/a");
    write(tmp.path(), "packages/a/package.json", r#"{"x":1}"#);

    let budget = ProbeBudget::with_bytes(decl.len() as u64);
    let roots = probe_roots(tmp.path(), &budget);
    assert_eq!(roots.len(), 2, "the declaration itself was affordable");
    assert!(
        !budget.take(1),
        "the budget is spent, so any further read fails closed"
    );
}

#[test]
fn read_bounded_rejects_a_directory() {
    let tmp = TempDir::new().unwrap();
    mkdir(tmp.path(), "adir");
    let budget = ProbeBudget::new();
    assert!(read_bounded(&tmp.path().join("adir"), &budget).is_none());
    assert!(read_bounded(&tmp.path().join("missing"), &budget).is_none());
}
