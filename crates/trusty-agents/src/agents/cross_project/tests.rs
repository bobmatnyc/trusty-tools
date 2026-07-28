//! Boundary tests for the L0-only cross-project scope (#4172, epic #4167).
//!
//! Why: These are privilege-boundary tests, so they must exercise the REAL
//! resolution paths rather than hand-built helper arguments:
//! - the tier always comes from a real `agent.toml` parsed by the real
//!   loader (`AgentConfig::by_name_in`), so #4200's fail-closed
//!   `AgentInfo::tier` is the thing under test — never a locally
//!   constructed `AgentTier`;
//! - the project allow-set always comes from a real `projects.json` read by
//!   the real registry loader (`ProjectRegistry::list_active`), via
//!   `CrossProjectScope::from_registry_path`;
//! - containment is always asserted through the public entry points
//!   (`resolve_repo_root` / `permits` / `effective_search_indexes`) that the
//!   git tools and `vector_search` actually call.
//!
//! What: Four groups — L1 scoping is unchanged; L0's widening is bounded to
//! the registry allow-set and cannot escape it; a declared-but-unrecognized
//! tier fails closed to L1 for either role; and an UNDECLARED tier follows
//! ADR-0024 decision 3's kind derivation (PR #4296) — assistant-kind resolves
//! L0, every sub-agent kind stays L1 and single-tenant.
//! Test: this module.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::CrossProjectScope;
use crate::agents::{AgentConfig, AgentTier};

/// A scratch directory the project registry will accept as a real project.
///
/// Why: `ProjectEntry::is_real_project` — condition (b) of the allow-set —
/// deliberately rejects `/tmp/…`, `/private/tmp/…`, `/var/folders/…`,
/// `/private/var/…` and any path whose basename starts with `.tmp`. That is
/// EVERY default `tempfile::TempDir` on both macOS (`/var/folders/…`) and
/// Linux (`/tmp/…`), and `TempDir`'s own default prefix is `.tmp`. Testing
/// the real predicate therefore requires scratch space outside those
/// patterns, so these tests root their temp dirs in the workspace `target/`
/// directory (gitignored, always present during a test run) with a
/// non-`.tmp` prefix.
/// What: A `TempDir` under `<workspace>/target/t4172/x4172-*`.
/// Test: used by every case in this module.
fn scratch() -> TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("t4172");
    std::fs::create_dir_all(&base).expect("mkdir scratch base");
    tempfile::Builder::new()
        .prefix("x4172-")
        .tempdir_in(&base)
        .expect("tempdir")
}

/// Write a package-layout `agent.toml` and load it through the REAL loader.
///
/// Why: `AgentConfig::by_name_in` is the production resolution path (it is
/// what `by_name` delegates to), so a tier read out of a config built this
/// way is the tier production would see — including the fail-closed arms for
/// an absent or unrecognized `tier` value.
/// What: Creates `<dir>/<name>/agent.toml` with the given `tier` line (empty
/// string = omit the key entirely) and returns the loaded config.
/// Test: used by every case in this module.
fn load_agent_with_tier(dir: &Path, name: &str, tier_line: &str) -> AgentConfig {
    load_agent_with_tier_and_role(dir, name, "assistant", tier_line)
}

/// As above, but with the persona's `role` under test too.
///
/// Why: ADR-0024 decision 3 (PR #4296) made an undeclared `tier` a function of
/// the agent's KIND, so a fixture that hardcodes `role = "assistant"` can no
/// longer express the "derives L1" case at all.
fn load_agent_with_tier_and_role(
    dir: &Path,
    name: &str,
    role: &str,
    tier_line: &str,
) -> AgentConfig {
    let agent_dir = dir.join(name);
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent dir");
    let toml = format!(
        r#"[agent]
name = "{name}"
role = "{role}"
description = "test persona for #4172"
model = "claude-sonnet-4-5"
{tier_line}

[llm]
temperature = 0.2
max_tokens = 4096
"#
    );
    std::fs::write(agent_dir.join("agent.toml"), toml).expect("write agent.toml");
    // The package layout requires a sibling `persona.md`; the loader reads it
    // as the persona prose.
    std::fs::write(agent_dir.join("persona.md"), "Test persona prose.\n")
        .expect("write persona.md");
    AgentConfig::by_name_in(&[dir.to_path_buf()], name).expect("load agent config")
}

/// Create a directory that looks like a git repository root.
///
/// Why: Condition (d) of the allow-set is "carries a `.git` entry"; the
/// tests must produce that condition the same way the predicate reads it.
/// What: `mkdir -p <parent>/<name>/.git` and returns the CANONICAL root.
/// Test: used by the L0 admission cases.
fn make_repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join(".git")).expect("mkdir repo/.git");
    std::fs::canonicalize(&root).expect("canonicalize repo root")
}

/// Write a real `projects.json` in the registry's on-disk shape.
///
/// Why: So `ProjectRegistry::load` / `list_active` — the production loader,
/// including its `status == Active` filter — is what produces the entry list
/// the scope is built from.
/// What: A JSON map keyed by canonical path string, matching
/// `HashMap<String, ProjectEntry>`. Each tuple is `(path, status)`.
/// Test: used by the L0 admission cases.
fn write_registry(path: &Path, entries: &[(&Path, &str)]) {
    let mut map = serde_json::Map::new();
    for (p, status) in entries {
        map.insert(
            p.display().to_string(),
            json!({
                "path": p.display().to_string(),
                "name": p.file_name().and_then(|s| s.to_str()).unwrap_or("x"),
                "last_run": null,
                "status": status,
            }),
        );
    }
    let body = serde_json::to_string(&Value::Object(map)).expect("serialize registry");
    std::fs::write(path, body).expect("write projects.json");
}

// ---------------------------------------------------------------------------
// L1: scoping unchanged (single-tenant)
// ---------------------------------------------------------------------------

#[test]
fn single_tenant_scope_is_not_cross_project() {
    let tmp = scratch();
    let scope = CrossProjectScope::single_tenant(tmp.path().to_path_buf());
    assert_eq!(scope.tier(), AgentTier::L1Standard);
    assert!(!scope.is_cross_project());
    assert_eq!(scope.allowed_roots().len(), 1);
    assert!(scope.cross_project_indexes().is_empty());
    // The default target must be the value the caller passed, verbatim —
    // pre-#4172 call sites depend on it.
    assert_eq!(scope.home_root(), tmp.path());
}

#[tokio::test]
async fn l1_scope_allows_only_its_home_root() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let other = make_repo(tmp.path(), "other-project");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(other.as_path(), "active")]);

    let cfg_dir = tmp.path().join("agents");
    // `tier = "l1"` — an EXPLICIT standard-tier persona.
    let cfg = load_agent_with_tier(&cfg_dir, "l1-explicit", "tier = \"l1\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    assert_eq!(scope.tier(), AgentTier::L1Standard);
    assert!(!scope.is_cross_project());
    assert_eq!(scope.allowed_roots(), std::slice::from_ref(&home));
    assert!(scope.cross_project_indexes().is_empty());
}

#[tokio::test]
async fn l1_resolve_repo_root_refuses_another_project() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let other = make_repo(tmp.path(), "other-project");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(other.as_path(), "active")]);

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l1-refuses", "tier = \"standard\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    let err = scope
        .resolve_repo_root(Some(other.to_str().unwrap()))
        .expect_err("L1 must not reach a second project");
    assert!(
        err.contains("outside this agent's project allow-set"),
        "{err}"
    );
    // The refusal names the allow-set it applied (auditability).
    assert!(err.contains("tier=L1Standard"), "{err}");
}

#[tokio::test]
async fn l1_effective_search_indexes_are_declared_list_verbatim() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let other = make_repo(tmp.path(), "other-project");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(other.as_path(), "active")]);

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l1-indexes", "tier = \"l1\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    let declared = vec!["cto-assistant".to_string(), "apex".to_string()];
    assert_eq!(
        scope.effective_search_indexes(declared.clone()),
        declared,
        "L1's tier-2 index list must be byte-identical to what it declared"
    );
}

// ---------------------------------------------------------------------------
// L0: widened reach, bounded to the registry allow-set
// ---------------------------------------------------------------------------

/// Build an L0 scope over a temp world.
///
/// Why: Every L0 case needs the same four-step setup (repos, registry file,
/// `agent.toml`, resolve); duplicating it obscures what each case asserts.
/// What: Returns `(tmp, home_root, scope)`. `extra` names additional repos to
/// create AND register as active.
/// Test: used by the L0 cases below.
async fn l0_world(extra: &[&str]) -> (TempDir, PathBuf, CrossProjectScope) {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let mut created: Vec<PathBuf> = Vec::new();
    for name in extra {
        created.push(make_repo(tmp.path(), name));
    }
    let rows: Vec<(&Path, &str)> = created.iter().map(|p| (p.as_path(), "active")).collect();
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &rows);

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l0-orchestrator", "tier = \"l0\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;
    (tmp, home, scope)
}

#[tokio::test]
async fn l0_scope_includes_registered_git_projects() {
    let (_tmp, home, scope) = l0_world(&["alpha", "beta"]).await;
    assert_eq!(scope.tier(), AgentTier::L0Orchestration);
    assert!(scope.is_cross_project());
    assert_eq!(
        scope.allowed_roots().len(),
        3,
        "{:?}",
        scope.allowed_roots()
    );
    assert_eq!(scope.allowed_roots()[0], home);
}

#[tokio::test]
async fn l0_cross_project_indexes_are_derived_from_allowed_roots() {
    let (_tmp, _home, scope) = l0_world(&["alpha", "beta"]).await;
    let ids = scope.cross_project_indexes();
    assert!(ids.contains(&"alpha".to_string()), "{ids:?}");
    assert!(ids.contains(&"beta".to_string()), "{ids:?}");
    // The agent's OWN root is never advertised as a cross-project index —
    // that is `[[stores]]`'s job (DOC-54 §5.1), untouched here.
    assert!(!ids.contains(&"home-project".to_string()), "{ids:?}");
}

#[tokio::test]
async fn l0_effective_search_indexes_append_cross_project_ids() {
    let (_tmp, _home, scope) = l0_world(&["alpha"]).await;
    let out = scope.effective_search_indexes(vec!["cto-assistant".to_string()]);
    assert_eq!(out, vec!["cto-assistant".to_string(), "alpha".to_string()]);
}

#[tokio::test]
async fn l0_effective_search_indexes_do_not_duplicate_declared_ids() {
    let (_tmp, _home, scope) = l0_world(&["alpha"]).await;
    let out = scope.effective_search_indexes(vec!["alpha".to_string(), "  ".to_string()]);
    assert_eq!(out, vec!["alpha".to_string()]);
}

#[tokio::test]
async fn l0_resolve_repo_root_accepts_registered_project() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    let alpha = std::fs::canonicalize(tmp.path().join("alpha")).unwrap();
    let got = scope
        .resolve_repo_root(Some(alpha.to_str().unwrap()))
        .expect("L0 may reach a registered project");
    assert_eq!(got, alpha);
}

#[tokio::test]
async fn l0_resolve_repo_root_accepts_subdirectory_of_registered_project() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    let sub = tmp.path().join("alpha").join("crates");
    std::fs::create_dir_all(&sub).unwrap();
    let sub = std::fs::canonicalize(&sub).unwrap();
    let got = scope
        .resolve_repo_root(Some(sub.to_str().unwrap()))
        .expect("a descendant of an allowed root is allowed");
    assert_eq!(got, sub);
}

#[tokio::test]
async fn l0_resolve_repo_root_refuses_unregistered_sibling() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    // A real git repo sitting right next to `alpha`, but absent from the
    // registry — the bound is the registry, not "any repo on disk".
    let rogue = make_repo(tmp.path(), "rogue");
    let err = scope
        .resolve_repo_root(Some(rogue.to_str().unwrap()))
        .expect_err("an unregistered repo must be refused even for L0");
    assert!(
        err.contains("outside this agent's project allow-set"),
        "{err}"
    );
    assert!(err.contains("tier=L0Orchestration"), "{err}");
}

#[tokio::test]
async fn l0_resolve_repo_root_refuses_parent_traversal_escape() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    // `<tmp>/alpha/..` is `<tmp>`, which is NOT an allowed root. Canonicalizing
    // before the containment test is what catches this.
    let escape = tmp.path().join("alpha").join("..");
    let err = scope
        .resolve_repo_root(Some(escape.to_str().unwrap()))
        .expect_err("`..` must not walk out of the allow-set");
    assert!(
        err.contains("outside this agent's project allow-set"),
        "{err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn l0_resolve_repo_root_refuses_symlink_escape() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    let outside = make_repo(tmp.path(), "outside-not-registered");
    let link = tmp.path().join("alpha").join("sneaky");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    // The link LIVES inside an allowed root but POINTS outside it.
    let err = scope
        .resolve_repo_root(Some(link.to_str().unwrap()))
        .expect_err("a symlink target outside the allow-set must be refused");
    assert!(
        err.contains("outside this agent's project allow-set"),
        "{err}"
    );
}

#[tokio::test]
async fn l0_scope_excludes_non_git_and_inactive_and_temp_entries() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    // (a) active + git   -> admitted
    let good = make_repo(tmp.path(), "good");
    // (b) active but NOT a git repo -> rejected by condition (d)
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let plain = std::fs::canonicalize(&plain).unwrap();
    // (c) a git repo whose registry status is `removed` -> rejected by (a)
    let stale = make_repo(tmp.path(), "stale");
    // (d) a registered path that does not exist -> rejected by (c)
    let ghost = tmp.path().join("ghost");
    // (e) a real git repo under a temp-dir pattern -> rejected by (b),
    //     `ProjectEntry::is_real_project`.
    let temp_shaped = PathBuf::from("/tmp").join("x4172-not-a-real-project");
    std::fs::create_dir_all(temp_shaped.join(".git")).expect("mkdir /tmp repo");

    let registry = tmp.path().join("projects.json");
    write_registry(
        &registry,
        &[
            (good.as_path(), "active"),
            (plain.as_path(), "active"),
            (stale.as_path(), "removed"),
            (ghost.as_path(), "active"),
            (temp_shaped.as_path(), "active"),
        ],
    );

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l0-filter", "tier = \"orchestration\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    assert_eq!(scope.allowed_roots(), &[home, good.clone()]);
    for rejected in [plain, stale, ghost, temp_shaped] {
        assert!(
            scope
                .resolve_repo_root(Some(rejected.to_str().unwrap()))
                .is_err(),
            "must not admit {}",
            rejected.display()
        );
    }
}

#[tokio::test]
async fn l0_permits_registered_project_but_not_its_parent() {
    let (tmp, _home, scope) = l0_world(&["alpha"]).await;
    assert!(scope.permits(&tmp.path().join("alpha")));
    assert!(!scope.permits(tmp.path()));
}

// ---------------------------------------------------------------------------
// Fail-closed behaviour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unrecognized_tier_declaration_resolves_to_single_tenant_scope() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let other = make_repo(tmp.path(), "other-project");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(other.as_path(), "active")]);
    let cfg_dir = tmp.path().join("agents");

    // Every DECLARED spelling #4200's resolver fails closed on — an
    // unrecognized value, including "l2", which must NOT be read as "more
    // privileged than l1". Checked against the assistant kind as well as a
    // sub-agent kind: after ADR-0024 decision 3 (PR #4296) an ABSENT tier
    // derives from the role, and a declared-but-unrecognized string must NOT
    // fall through to that derivation and quietly widen an assistant's reach.
    for role in ["assistant", "engineer"] {
        for (name, tier_line) in [
            ("bogus-tier", "tier = \"level-zero\""),
            ("l2-tier", "tier = \"l2\""),
            ("l1-tier", "tier = \"l1\""),
        ] {
            let name = format!("{role}-{name}");
            let cfg = load_agent_with_tier_and_role(&cfg_dir, &name, role, tier_line);
            let scope =
                CrossProjectScope::from_registry_path(&cfg.agent, &home, registry.clone()).await;
            assert_eq!(
                scope.tier(),
                AgentTier::L1Standard,
                "{name} must fail closed to L1"
            );
            assert!(!scope.is_cross_project(), "{name} must stay single-tenant");
            assert!(
                scope
                    .resolve_repo_root(Some(other.to_str().unwrap()))
                    .is_err(),
                "{name} must not reach a second project"
            );
            assert!(
                scope.cross_project_indexes().is_empty(),
                "{name} must have no cross-project indexes"
            );
        }
    }
}

/// The DERIVED case, after ADR-0024 decision 3 (PR #4296, on `main`): with no
/// usable `tier =` declaration the tier is a function of the agent's KIND.
///
/// Why this test exists: #4172 was written when an absent tier meant L1, so
/// the spellings below used to be single-tenant. They are now cross-project
/// for assistant-kind — not because this PR widened the bound (the allow-set
/// is still exactly "home root, plus active + real + existing + git-root
/// entries of `projects.json`"), but because `main` changed which personas are
/// L0. Pinning the derivation here means a future change to that population
/// fails loudly instead of silently redefining every persona's reach.
#[tokio::test]
async fn undeclared_tier_follows_the_kind_derivation() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let other = make_repo(tmp.path(), "other-project");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(other.as_path(), "active")]);
    let cfg_dir = tmp.path().join("agents");

    for (name, tier_line) in [
        ("no-tier", ""),
        ("blank-tier", "tier = \"\""),
        ("space-tier", "tier = \"   \""),
    ] {
        // Assistant kind derives L0 and therefore reaches the registered,
        // active sibling — but still ONLY that: the bound is unchanged.
        let a_name = format!("assistant-{name}");
        let cfg = load_agent_with_tier_and_role(&cfg_dir, &a_name, "assistant", tier_line);
        let scope =
            CrossProjectScope::from_registry_path(&cfg.agent, &home, registry.clone()).await;
        assert_eq!(
            scope.tier(),
            AgentTier::L0Orchestration,
            "{a_name} is assistant-kind and derives L0 (ADR-0024 decision 3)"
        );
        assert!(scope.is_cross_project(), "{a_name} must be cross-project");
        assert!(
            scope
                .resolve_repo_root(Some(other.to_str().unwrap()))
                .is_ok(),
            "{a_name} must reach the registered active sibling"
        );
        // The bound still holds: an unregistered path is refused.
        let unregistered = make_repo(tmp.path(), "unregistered-project");
        assert!(
            scope
                .resolve_repo_root(Some(unregistered.to_str().unwrap()))
                .is_err(),
            "{a_name} must NOT reach a project absent from the registry"
        );

        // Sub-agent kinds stay L1 and single-tenant.
        for role in ["engineer", "researcher", "planner"] {
            let s_name = format!("{role}-{name}");
            let cfg = load_agent_with_tier_and_role(&cfg_dir, &s_name, role, tier_line);
            let scope =
                CrossProjectScope::from_registry_path(&cfg.agent, &home, registry.clone()).await;
            assert_eq!(
                scope.tier(),
                AgentTier::L1Standard,
                "{s_name} is a sub-agent kind and must stay L1"
            );
            assert!(
                !scope.is_cross_project(),
                "{s_name} must stay single-tenant"
            );
            assert!(
                scope
                    .resolve_repo_root(Some(other.to_str().unwrap()))
                    .is_err(),
                "{s_name} must not reach a second project"
            );
        }
    }
}

#[tokio::test]
async fn from_registry_loads_active_entries_for_l0() {
    // Guards the loader wiring itself: the widening must come from a real
    // `projects.json` read, so a scope built over a registry file that
    // exists and lists an active repo is cross-project.
    let (_tmp, _home, scope) = l0_world(&["alpha"]).await;
    assert!(scope.is_cross_project());
}

#[tokio::test]
async fn from_registry_fails_closed_when_registry_is_unreadable() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let alpha = make_repo(tmp.path(), "alpha");
    let registry = tmp.path().join("projects.json");
    // Malformed JSON: the loader cannot produce entries, so the L0 widening
    // must collapse to the home root rather than proceed on a
    // partially-parsed allow-set.
    std::fs::write(&registry, "{ this is not json").unwrap();

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l0-broken-registry", "tier = \"l0\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    assert_eq!(scope.tier(), AgentTier::L0Orchestration);
    assert!(
        !scope.is_cross_project(),
        "unreadable registry must deny reach"
    );
    assert!(
        scope
            .resolve_repo_root(Some(alpha.to_str().unwrap()))
            .is_err(),
        "no reach may survive an unreadable registry"
    );
}

// ---------------------------------------------------------------------------
// Shared path-resolution rules
// ---------------------------------------------------------------------------

#[test]
fn resolve_repo_root_defaults_to_home_root() {
    let tmp = scratch();
    let scope = CrossProjectScope::single_tenant(tmp.path().to_path_buf());
    assert_eq!(scope.resolve_repo_root(None).unwrap(), tmp.path());
    assert_eq!(scope.resolve_repo_root(Some("")).unwrap(), tmp.path());
    assert_eq!(scope.resolve_repo_root(Some("   ")).unwrap(), tmp.path());
}

#[tokio::test]
async fn resolve_repo_root_refuses_relative_path() {
    let (_tmp, _home, scope) = l0_world(&["alpha"]).await;
    let err = scope
        .resolve_repo_root(Some("alpha"))
        .expect_err("a relative path must be refused, not joined to the cwd");
    assert!(err.contains("not absolute"), "{err}");
}

#[tokio::test]
async fn resolve_repo_root_refuses_prefix_lookalike_sibling() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let alpha = make_repo(tmp.path(), "alpha");
    // `alpha-extra` shares a STRING prefix with the allowed `alpha` but is a
    // different directory; component-wise containment must reject it.
    let lookalike = make_repo(tmp.path(), "alpha-extra");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(alpha.as_path(), "active")]);

    let cfg_dir = tmp.path().join("agents");
    let cfg = load_agent_with_tier(&cfg_dir, "l0-lookalike", "tier = \"l0\"");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    assert!(
        scope
            .resolve_repo_root(Some(alpha.to_str().unwrap()))
            .is_ok()
    );
    assert!(
        scope
            .resolve_repo_root(Some(lookalike.to_str().unwrap()))
            .is_err(),
        "a string-prefix sibling must not be treated as contained"
    );
}

#[test]
fn permits_accepts_home_root_and_descendants() {
    let tmp = scratch();
    let home = tmp.path().join("home");
    let sub = home.join("crates").join("inner");
    std::fs::create_dir_all(&sub).unwrap();
    let scope = CrossProjectScope::single_tenant(home.clone());
    assert!(scope.permits(&home));
    assert!(scope.permits(&sub));
    assert!(!scope.permits(tmp.path()));
}

#[test]
fn audit_summary_names_tier_and_allowed_roots() {
    let tmp = scratch();
    let scope = CrossProjectScope::single_tenant(tmp.path().to_path_buf());
    let line = scope.audit_summary();
    assert!(line.contains("tier=L1Standard"), "{line}");
    assert!(line.contains("cross_project=false"), "{line}");
    assert!(
        line.contains(
            &std::fs::canonicalize(tmp.path())
                .unwrap()
                .display()
                .to_string()
        ),
        "{line}"
    );
}

/// The SHIPPED-PERSONA consequence of this PR landing after ADR-0024
/// decision 3 (PR #4296) — recorded as a reviewed fact, not left implicit.
///
/// Why: ADR-0024's "Capability grants: zero observable delta" analysis was
/// written while #4170/#4172/#4173 were still open, and it holds for the other
/// two — their tools are gated by NAME, and no bundled assistant's
/// `[tools].allow` matches one. It does NOT hold for this PR. `git_log` and
/// `git_status` ARE in the shipped `izzie` / `personal-assistant` allow-lists,
/// so those personas — assistant-kind, therefore L0 by derivation — go from a
/// single-tenant git surface to a cross-project one bounded by the operator's
/// own `projects.json`. That is a real, if bounded, widening of what an
/// untrusted-content-ingesting persona can read, and it should fail this test
/// (and be re-reviewed) rather than change silently again.
///
/// What: an assistant-kind persona with no declared tier, over a registry
/// holding one active sibling repo, resolves a CROSS-PROJECT scope that
/// reaches that sibling — and still refuses anything the registry does not
/// list. Read-only vs write is a `[tools].allow` question, not a scope
/// question, and is covered by `git_write_tools_refuse_repo_outside_allow_set`.
/// Test: this function IS the test.
#[tokio::test]
async fn shipped_assistant_kind_gains_bounded_cross_project_git_reach() {
    let tmp = scratch();
    let home = make_repo(tmp.path(), "home-project");
    let sibling = make_repo(tmp.path(), "registered-sibling");
    let unregistered = make_repo(tmp.path(), "unregistered-sibling");
    let registry = tmp.path().join("projects.json");
    write_registry(&registry, &[(sibling.as_path(), "active")]);
    let cfg_dir = tmp.path().join("agents");

    // No `tier =` line — exactly how every bundled assistant ships, since
    // decision 3 forbids hand-declaring it.
    let cfg = load_agent_with_tier_and_role(&cfg_dir, "izzie-shaped", "assistant", "");
    let scope = CrossProjectScope::from_registry_path(&cfg.agent, &home, registry).await;

    assert_eq!(scope.tier(), AgentTier::L0Orchestration);
    assert!(
        scope.is_cross_project(),
        "a shipped-shaped assistant persona now resolves a cross-project scope — \
         this is the #4172 + #4296 delta and is deliberate"
    );
    assert!(
        scope
            .resolve_repo_root(Some(sibling.to_str().unwrap()))
            .is_ok(),
        "it reaches the registered active sibling"
    );
    assert!(
        scope
            .resolve_repo_root(Some(unregistered.to_str().unwrap()))
            .is_err(),
        "and NOTHING beyond the registry — the bound is what makes the delta acceptable"
    );
}
