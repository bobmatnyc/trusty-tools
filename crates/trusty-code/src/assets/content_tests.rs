//! Tests for [`super`]: tcode's own content fails closed when absent (#9013).

use std::path::Path;

use trusty_agents_common::agent_content::{AgentContentError, checkout_content};

use super::*;

/// Builds a trusted checkout at `root` holding `files` (repo-relative path,
/// body), with every dev class directory present.
fn fake_checkout(root: &Path, files: &[(String, String)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("file");
    }
}

/// A stub agent card for `file`.
fn card(file: &str) -> String {
    format!("---\nname: {}\n---\nbody\n", file.trim_end_matches(".md"))
}

/// Every file the built-in roster reads, as stubs, minus `skip`.
fn roster_files(skip: &str) -> Vec<(String, String)> {
    let shared = SHARED_TM_AGENT_FILES
        .iter()
        .map(|f| (format!("content/agents/{f}"), card(f)));
    let directs = DEFAULT_AGENTS.iter().filter_map(|a| match a {
        EmbeddedAgent::Direct { name } => Some(format!("{name}.md")),
        EmbeddedAgent::Composed { .. } => None,
    });
    let own = LOCAL_TM_AGENT_FORKS
        .iter()
        .map(|f| (*f).to_owned())
        .chain(directs)
        .map(|f| (format!("content/{}", own_agent_path(&f)), card(&f)));
    shared
        .chain(own)
        .filter(|(rel, _)| !rel.ends_with(&format!("/{skip}")))
        .collect()
}

/// Every default skill, as stubs, with `name`'s body replaced by `body`
/// (`None`: the file is absent).
fn skill_files(name: &str, body: Option<&str>) -> Vec<(String, String)> {
    DEFAULT_SKILL_NAMES
        .iter()
        .filter_map(|n| {
            let text = if *n == name {
                body?.to_owned()
            } else {
                format!("---\nname: {n}\n---\nbody\n")
            };
            Some((format!("content/{}", skill_path(n)), text))
        })
        .collect()
}

/// The full stub roster loads, so the absent-file tests below fail for the
/// file they remove and nothing else.
#[test]
fn a_complete_roster_loads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fake_checkout(tmp.path(), &roster_files("none"));
    let content = checkout_content(tmp.path()).expect("checkout");
    let sources = AgentSources::from_content(&content).expect("complete roster");
    assert_eq!(sources.catalog().len(), 31);
    assert_eq!(sources.directs().count(), 8);
    assert!(
        sources
            .direct("pm")
            .is_some_and(|md| md.contains("name: pm"))
    );
}

/// #9013: a content release without one of tcode's own agents fails the whole
/// roster, naming the file — never a roster one agent short, never a
/// compiled-in fallback.
#[test]
fn absent_own_agent_fails_closed_naming_the_file() {
    for skip in ["pm.md", "qa.md"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        fake_checkout(tmp.path(), &roster_files(skip));
        let content = checkout_content(tmp.path()).expect("checkout");
        let err = AgentSources::from_content(&content).expect_err("an absent file must fail");
        let want = own_agent_path(skip);
        assert!(
            matches!(&err, AgentContentError::Missing { path, .. } if *path == want),
            "{skip}: {err:?}"
        );
        assert!(err.to_string().contains(&want), "{err}");
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    fake_checkout(tmp.path(), &roster_files("engineer.md"));
    let content = checkout_content(tmp.path()).expect("checkout");
    assert!(direct_agent_from(&content, "engineer").is_err());
    assert!(
        direct_agent_from(&content, "rust-engineer")
            .expect("no read")
            .is_none()
    );
}

/// #9013: an absent or empty default skill fails the catalog, naming the file.
#[test]
fn absent_default_skill_fails_closed_naming_the_file() {
    let want = skill_path("xlsx");
    for (body, absent) in [(None, true), (Some("  \n"), false)] {
        let tmp = tempfile::tempdir().expect("tempdir");
        fake_checkout(tmp.path(), &skill_files("xlsx", body));
        let content = checkout_content(tmp.path()).expect("checkout");
        let err = default_skills_from(&content).expect_err("the catalog must fail");
        match &err {
            AgentContentError::Missing { path, .. } => assert!(absent && *path == want, "{err}"),
            AgentContentError::Invalid { path, .. } => assert!(!absent && *path == want, "{err}"),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(default_skill_from(&content, "xlsx").is_err());
        assert!(
            default_skill_from(&content, "tm-workflow")
                .expect("no read")
                .is_none()
        );
    }
}

/// #9013: with no checkout and no installed bundle, the error names the
/// remedy (`tm content update`, then the offline `tm content install --from`).
#[test]
fn no_content_names_the_remedy() {
    let cache = tempfile::tempdir().expect("cache");
    let err = trusty_agents_common::agent_content::resolve_content_in(
        cache.path(),
        trusty_agents_common::agent_content::DevOverride::Off,
    )
    .expect_err("nothing is installed");
    assert!(err.is_not_installed(), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("tm content update"), "{text}");
    assert!(text.contains("tm content install --from"), "{text}");
}
