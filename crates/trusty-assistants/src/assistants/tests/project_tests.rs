//! Project-directory claims and project-to-instance resolution (#11649).
//!
//! Why: a session started in a project directory must reach exactly one
//! instance. Each error arm is pinned so none can fall back to a default.
//! What: the pilot-config fixture, write-time validation, resolution, the
//! duplicate-claim conflict, `..`/symlink escapes, and the no-match arm.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use crate::assistants::project::duplicate_claims;
use crate::assistants::{
    AssistantError, AssistantHome, AssistantHomeConfig, AssistantInstanceId, PROJECT_DIR_KEY,
    canonical_project_dir, inspect, instance_for_project, project_claims, set_project_dir,
};

/// A canonical temp dir: on macOS `/var` is a symlink to `/private/var`.
fn temp() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().canonicalize().unwrap();
    (tmp, path)
}

fn id(raw: &str) -> AssistantInstanceId {
    AssistantInstanceId::new(raw).unwrap()
}

/// Make `<base>/<name>` and return it.
fn mkdir(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a home's `config.toml` by hand, bypassing `set_project_dir`.
pub(super) fn claim(root: &Path, raw_id: &str, dir: &Path) {
    let home = mkdir(root, raw_id);
    std::fs::write(
        home.join("config.toml"),
        format!("id = \"{raw_id}\"\nproject_dir = \"{}\"\n", dir.display()),
    )
    .unwrap();
}

/// Why: pilot homes hold `config.toml` with `id` only; adding the key must not
/// make them malformed.
#[test]
fn a_pilot_config_with_only_an_id_still_parses() {
    let config: AssistantHomeConfig = toml::from_str("id = \"cto-assistant\"\n").unwrap();
    assert_eq!(config.id, "cto-assistant");
    assert_eq!(config.project_dir, None);
    assert_eq!(
        config,
        AssistantHomeConfig {
            id: "cto-assistant".into(),
            ..Default::default()
        }
    );

    let (_tmp, root) = temp();
    let home = AssistantHome::under(&root, id("cto-assistant"));
    home.ensure().unwrap();
    std::fs::write(home.config_path(), "id = \"cto-assistant\"\n").unwrap();
    assert!(inspect(&home).is_healthy(), "{:?}", inspect(&home).issues);
}

#[test]
fn project_dir_must_be_absolute_and_existing() {
    let (_tmp, base) = temp();
    let real = mkdir(&base, "real");
    let file = base.join("file.txt");
    std::fs::write(&file, "x").unwrap();

    for bad in [
        PathBuf::from("relative/dir"),
        base.join("absent"),
        file.clone(),
    ] {
        let err = canonical_project_dir(&bad).unwrap_err();
        assert!(
            matches!(err, AssistantError::InvalidProjectDir { ref path, .. } if path == &bad),
            "{bad:?}: {err}"
        );
    }
    assert_eq!(
        canonical_project_dir(&base.join("real/../real")).unwrap(),
        real
    );
    #[cfg(unix)]
    {
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(canonical_project_dir(&link).unwrap(), real);
    }
}

#[test]
fn set_project_dir_writes_the_canonical_path() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    let home = AssistantHome::under(&root, id("personal"));
    home.ensure().unwrap();
    let edited = "id = \"personal\"\ndisplay_name = \"Mine\"\n";
    std::fs::write(home.config_path(), edited).unwrap();

    let written = set_project_dir(&root, &home, &base.join("project/../project")).unwrap();

    assert_eq!(written, project);
    let raw = std::fs::read_to_string(home.config_path()).unwrap();
    assert!(raw.contains(PROJECT_DIR_KEY), "{raw}");
    let config: AssistantHomeConfig = toml::from_str(&raw).unwrap();
    assert_eq!(config.project_dir, Some(project));
    assert_eq!(config.display_name.as_deref(), Some("Mine"));
}

#[test]
fn set_project_dir_refuses_a_second_claim() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    claim(&root, "cto-assistant", &project);
    let home = AssistantHome::under(&root, id("personal"));
    home.ensure().unwrap();
    let before = std::fs::read_to_string(home.config_path()).unwrap();

    let err = set_project_dir(&root, &home, &project).unwrap_err();

    assert!(
        matches!(&err, AssistantError::ProjectDirConflict { first, second, .. }
            if first == "cto-assistant" && second == "personal"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(home.config_path()).unwrap(), before);
}

#[test]
fn claims_list_every_declared_project_dir() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let a = mkdir(&base, "a");
    let b = mkdir(&base, "b");
    claim(&root, "personal", &a);
    claim(&root, "cto-assistant", &b);
    // A home with no claim, and a stray directory that is not an instance id.
    AssistantHome::under(&root, id("izzie")).ensure().unwrap();
    mkdir(&root, "Not An Id");

    let claims = project_claims(&root).unwrap();

    let got: Vec<(String, PathBuf)> = claims
        .into_iter()
        .map(|c| (c.id.to_string(), c.declared))
        .collect();
    assert_eq!(
        got,
        vec![("cto-assistant".into(), b), ("personal".into(), a)]
    );
    assert!(
        project_claims(&base.join("no-such-root"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_stale_claim_matches_nothing() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let gone = mkdir(&base, "gone");
    claim(&root, "personal", &gone);
    std::fs::remove_dir(&gone).unwrap();
    let claims = project_claims(&root).unwrap();
    assert_eq!(claims[0].canonical(), None);
    // A cwd that no longer exists is refused; its parent matches no claim.
    let err = instance_for_project(&root, &gone).unwrap_err();
    assert!(
        matches!(err, AssistantError::InvalidProjectDir { .. }),
        "{err}"
    );
    let err = instance_for_project(&root, &base).unwrap_err();
    assert!(
        matches!(err, AssistantError::NoInstanceForProject { .. }),
        "{err}"
    );
}

#[test]
fn resolves_a_cwd_at_or_under_a_project_dir() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let personal = mkdir(&base, "personal");
    let cto = mkdir(&base, "cto");
    let nested = mkdir(&cto, "inner");
    claim(&root, "personal", &personal);
    claim(&root, "cto-assistant", &cto);
    claim(&root, "izzie", &nested);

    let resolve = |cwd: &Path| instance_for_project(&root, cwd).unwrap().to_string();
    assert_eq!(resolve(&personal), "personal");
    assert_eq!(resolve(&mkdir(&personal, "deep/er")), "personal");
    assert_eq!(resolve(&cto), "cto-assistant");
    // The deepest claim wins over its ancestor.
    assert_eq!(resolve(&mkdir(&nested, "x")), "izzie");
}

#[test]
fn two_homes_claiming_one_dir_is_a_named_conflict() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    claim(&root, "personal", &project);
    claim(&root, "cto-assistant", &project);

    let err = instance_for_project(&root, &project).unwrap_err();

    match &err {
        AssistantError::ProjectDirConflict { dir, first, second } => {
            assert_eq!(dir, &project);
            assert_eq!(
                (first.as_str(), second.as_str()),
                ("cto-assistant", "personal")
            );
        }
        other => panic!("expected a conflict, got {other}"),
    }
    let message = err.to_string();
    assert!(message.contains("cto-assistant") && message.contains("personal"));
    assert_eq!(duplicate_claims(&project_claims(&root).unwrap()).len(), 1);
}

#[test]
fn dotdot_and_symlink_escapes_are_rejected() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    let outside = mkdir(&base, "outside");
    claim(&root, "personal", &project);

    let err = instance_for_project(&root, &project.join("../outside")).unwrap_err();
    assert!(
        matches!(&err, AssistantError::NoInstanceForProject { cwd } if cwd == &outside),
        "{err}"
    );
    let err = instance_for_project(&root, Path::new("project")).unwrap_err();
    assert!(
        matches!(err, AssistantError::InvalidProjectDir { .. }),
        "{err}"
    );
    #[cfg(unix)]
    {
        let link = project.join("escape");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let err = instance_for_project(&root, &link).unwrap_err();
        assert!(
            matches!(&err, AssistantError::NoInstanceForProject { cwd } if cwd == &outside),
            "{err}"
        );
    }
}

/// Fail-open check: no match is an error, never the first or only instance.
#[test]
fn no_match_is_an_error_not_a_default() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    let elsewhere = mkdir(&base, "elsewhere");
    claim(&root, "personal", &project);

    let err = instance_for_project(&root, &elsewhere).unwrap_err();
    assert!(
        matches!(err, AssistantError::NoInstanceForProject { .. }),
        "{err}"
    );
    let empty = mkdir(&base, "empty-root");
    let err = instance_for_project(&empty, &project).unwrap_err();
    assert!(
        matches!(err, AssistantError::NoInstanceForProject { .. }),
        "{err}"
    );
}

/// Fail-open check: a home whose config cannot be parsed might hold the claim,
/// so resolution stops rather than answer from the remaining homes.
#[test]
fn a_malformed_config_stops_resolution() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    claim(&root, "personal", &project);
    let broken = mkdir(&root, "cto-assistant");
    std::fs::write(broken.join("config.toml"), "id = [unclosed\n").unwrap();

    let err = instance_for_project(&root, &project).unwrap_err();

    assert!(
        matches!(&err, AssistantError::InvalidConfig { path, .. } if path.starts_with(&broken)),
        "{err}"
    );
}

/// #11649 review (HIGH): a claim that is relative or not a directory might be
/// the one meant for this cwd, so it stops resolution with an error naming
/// its home instead of being skipped in favour of another instance.
#[test]
fn an_invalid_claim_stops_resolution() {
    let (_tmp, base) = temp();
    let root = mkdir(&base, "homes");
    let project = mkdir(&base, "project");
    let cwd = mkdir(&project, "x");
    claim(&root, "personal", &project);
    let cto = mkdir(&root, "cto-assistant");
    let not_a_dir = base.join("file.txt");
    std::fs::write(&not_a_dir, "x").unwrap();

    for declared in ["relative/path".to_string(), not_a_dir.display().to_string()] {
        std::fs::write(
            cto.join("config.toml"),
            format!("id = \"cto-assistant\"\nproject_dir = \"{declared}\"\n"),
        )
        .unwrap();

        let err = instance_for_project(&root, &cwd).unwrap_err();

        match &err {
            AssistantError::InvalidConfig { path, reason } => {
                assert_eq!(path, &cto.join("config.toml"));
                assert!(reason.contains("cto-assistant"), "{reason}");
            }
            other => panic!("{declared}: expected InvalidConfig, got {other}"),
        }
    }
}
