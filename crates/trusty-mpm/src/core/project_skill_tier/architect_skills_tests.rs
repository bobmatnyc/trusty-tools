//! #9540: the Architect skill deployer against a scratch project tier.
//!
//! Why: df-D22's rules — write when absent, refresh only an untouched copy,
//! never overwrite a project-custom one, never fail the caller — each need a
//! case that a wrong deployer fails.
//! What: drives [`super::deploy_skills_into`] and [`super::health`] on temp
//! dirs; the launch-path cases live in
//! `session_launch::skills::architect_9540_tests`.
//! Test: this is the test module.

use super::*;
use crate::core::project_skill_tier::architect_seed::test_support::shipped;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;
use trusty_common::install_paths::MPM_DIR_NAME;

/// A shipped table of one skill with `contents`.
fn one(contents: &'static str) -> [ArchitectSkill; 1] {
    one_with_history(contents, &[])
}

/// [`one`] with the sha256 of earlier shipped versions.
fn one_with_history(
    contents: &'static str,
    previous: &'static [&'static str],
) -> [ArchitectSkill; 1] {
    [ArchitectSkill {
        stem: "tm-probe",
        contents: contents.to_owned(),
        previous,
    }]
}

/// A one-entry history holding the sha256 of `old_body`.
fn history_of(old_body: &str) -> &'static [&'static str] {
    let hash: &'static str = Box::leak(sha256_hex(old_body.as_bytes()).into_boxed_str());
    Box::leak(vec![hash].into_boxed_slice())
}

/// Pin `path`'s mtime to a fixed past instant and return it.
fn age(path: &Path) -> SystemTime {
    let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(then)
        .unwrap();
    then
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// Every [`SHIPPED`] stem, in deploy order.
fn shipped_stems() -> Vec<&'static str> {
    SHIPPED.iter().map(|s| s.stem).collect()
}

#[test]
fn an_absent_skill_is_written_byte_equal() {
    let project = tempdir().unwrap();
    let report = deploy_architect_skills(project.path(), &[], &shipped());
    for skill in shipped() {
        let path = skill_path(&project_tier(project.path()), skill.stem);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            skill.contents.as_bytes(),
            "{}",
            path.display()
        );
    }
    assert_eq!(report.written, shipped_stems());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn an_edited_copy_is_kept_and_reported() {
    use tracing_subscriber::layer::SubscriberExt;

    let project = tempdir().unwrap();
    deploy_architect_skills(project.path(), &[], &shipped());
    let path = skill_path(&project_tier(project.path()), FLEET_CHECK.stem);
    std::fs::write(&path, "PROJECT EDIT\n").unwrap();

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let report = tracing::subscriber::with_default(subscriber, || {
        deploy_architect_skills(project.path(), &[], &shipped())
    });

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "PROJECT EDIT\n");
    assert_eq!(report.kept_custom, vec!["tm-fleet-check"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("WARN")
            && l.contains("tm-fleet-check")
            && l.contains("project-custom")),
        "no WARN line names the kept copy: {lines:#?}"
    );
    let (ok, detail) = health(project.path());
    assert!(!ok, "{detail}");
    assert!(
        detail.contains("tm-fleet-check: project-custom"),
        "{detail}"
    );
    assert!(!detail.contains("tm-context-refresh"), "{detail}");
}

#[test]
fn an_untouched_copy_is_refreshed_when_the_shipped_bytes_move() {
    let dir = tempdir().unwrap();
    deploy_skills_into(dir.path(), &one("v1\n"));
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.written, vec!["tm-probe"]);
    assert_eq!(
        std::fs::read_to_string(skill_path(dir.path(), "tm-probe")).unwrap(),
        "v2\n"
    );
}

#[test]
fn a_seeded_copy_with_no_record_and_other_bytes_is_custom() {
    // A `tm fleet init` seed that predates the record and was edited since.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "older seed\n").unwrap();
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "older seed\n");
}

#[test]
fn a_byte_equal_seed_is_adopted_then_refreshed() {
    // An unrecorded copy equal to the shipped one is adopted, not rewritten,
    // and is then refreshed like any copy tm deployed.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "v1\n").unwrap();
    let pinned = age(&path);
    let adopted = deploy_skills_into(dir.path(), &one("v1\n"));
    assert!(adopted.written.is_empty(), "{:?}", adopted.written);
    assert_eq!(mtime(&path), pinned);
    let refreshed = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(refreshed.written, vec!["tm-probe"]);
}

#[test]
fn a_second_run_writes_nothing() {
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    let first = deploy_architect_skills(project.path(), &[], &shipped());
    assert_eq!(first.written.len(), SHIPPED.len());
    let mut files: Vec<PathBuf> = SHIPPED.iter().map(|s| skill_path(&tier, s.stem)).collect();
    files.push(tier.join(RECORD_FILE));
    let pinned: Vec<SystemTime> = files.iter().map(|f| age(f)).collect();

    let second = deploy_architect_skills(project.path(), &[], &shipped());

    assert_eq!(second, DeployReport::default());
    let after: Vec<SystemTime> = files.iter().map(|f| mtime(f)).collect();
    assert_eq!(after, pinned, "a run with nothing changed rewrote a file");
}

#[test]
fn a_blocked_skill_dir_is_an_error_line() {
    // A FILE where the skill's directory must go: unwritable even as root.
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    std::fs::create_dir_all(&tier).unwrap();
    std::fs::write(tier.join(FLEET_CHECK.stem), "not a dir").unwrap();

    let report = deploy_architect_skills(project.path(), &[], &shipped());

    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("tm-fleet-check"),
        "{:?}",
        report.errors
    );
    // #9997: every other shipped skill is still written.
    let others: Vec<&str> = shipped_stems()
        .into_iter()
        .filter(|stem| *stem != FLEET_CHECK.stem)
        .collect();
    assert_eq!(report.written, others);
    let (ok, detail) = health(project.path());
    assert!(!ok, "a failed deploy reported healthy: {detail}");
    assert!(detail.contains("tm-fleet-check: unreadable"), "{detail}");
}

#[test]
fn health_names_each_skill_that_is_not_current() {
    let project = tempdir().unwrap();
    let (ok, detail) = health(project.path());
    assert!(!ok);
    assert!(detail.contains("tm-fleet-check: missing"), "{detail}");
    assert!(detail.contains("tm-context-refresh: missing"), "{detail}");

    deploy_architect_skills(project.path(), &[], &shipped());
    let (ok, detail) = health(project.path());
    assert!(ok, "{detail}");
}

/// Seed `body` as the probe skill's live copy.
fn seed_probe(dir: &Path, body: &str) -> PathBuf {
    let path = skill_path(dir, "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn a_prior_shipped_seed_with_no_record_is_refreshed_and_recorded() {
    // #9540: a `tm fleet init` seed of an older shipped version has no record,
    // but it is a tm deploy nobody edited.
    let dir = tempdir().unwrap();
    let history = history_of("v0\n");
    let path = seed_probe(dir.path(), "v0\n");

    let report = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));

    assert_eq!(report.written, vec!["tm-probe"], "{report:?}");
    assert!(report.kept_custom.is_empty(), "{:?}", report.kept_custom);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2\n");
    let again = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));
    assert_eq!(
        again,
        DeployReport::default(),
        "the refresh was not recorded"
    );
}

#[test]
fn other_bytes_with_no_record_stay_custom_even_with_a_history() {
    let dir = tempdir().unwrap();
    let history = history_of("v0\n");
    let path = seed_probe(dir.path(), "v0\nplus a project line\n");

    let report = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));

    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "v0\nplus a project line\n"
    );
}

#[test]
fn the_shipped_history_is_pinned_so_an_asset_edit_trips() {
    // Drift guard: editing an asset without moving its old hash into
    // `previous` would make a `tm fleet init` seed of the old text look custom.
    const PINNED: [(&str, &str); 17] = [
        (
            "tm-fleet-check",
            "8e41b332fcb147ddf9d81d95936315986e5768c4efcd9d91a4149f2e3e7b55fd",
        ),
        (
            "tm-context-refresh",
            "2a92a066c73c6ebce7b3f07cbf1030936e0aebabf5d4f27d3d3685dc71abce54",
        ),
        (
            "architect-tmux-ops",
            "4be6fa1b350ef09a43d49dde96870f496a74caac95d493c78fe37a44fa2b159d",
        ),
        (
            "architect-operating-rules",
            "cd4130f466da33d8b63baa6a5948d2474b2677862f4b1df4a89d62b5b91dd249",
        ),
        (
            "architect-workflow",
            "dce96874a33465b9a362c53b17e22b54466eee1a4ea3ca0f03ac226e1edb503e",
        ),
        (
            "architect-search-memory",
            "3705f84cf316939df211ec5dded2cba48e5f28cc8636898286c21450962ff1d7",
        ),
        (
            "architect-github",
            "0a839e6671261dd4999cc65c4f90ab2e782db7f44aecdeab18174ac913c68b3d",
        ),
        (
            "architect-strategy",
            "b21d4a4b479453cbec8ae3982db27e7364a4e094b014836eabb8acc483aa1e5a",
        ),
        (
            "architect-completion-standard",
            "70343273c49dd1c517cebcb64bf7efe160dfbf09904febee2d0dd26ea904475e",
        ),
        (
            "architect-owner-model",
            "f9d9b4afd2f28fd93d45947bc0b13376a7d99f44befd73e08132136fb70f4bf1",
        ),
        (
            "architect-drift-check",
            "26a86b0a4ac74bdc255e7dd5c42e5a818b0cb776ccd3f513193f30ec5a400401",
        ),
        (
            "architect-review",
            "ce86d88ac5823fb981cb71df0e503c21377030b89911de991f84673c89fba117",
        ),
        (
            "architect-project-provision",
            "cbe193f6d533f586cfbed18f7e3c672101236a53783c92e437e2ce9e8a36d599",
        ),
        (
            "architect-macos",
            "ecb65835f13999c7be19111fd93681ea23399b1aeba5c6f051ca8d7845d2a77c",
        ),
        (
            "architect-improvements",
            "349ed4105e92fae097bb5b335b96a83070b8ccf0a379a5b7d054e0be2b5ed3ee",
        ),
        (
            "architect-host-admin",
            "7362004b8be5aa0384c1b5c55853e5b427fc4bb29a168a51759f354af977f296",
        ),
        (
            "architect-dashboard",
            // #9457: `fleet init` names when it starts the dashboard on macOS.
            "908deda6a43c699d7c2d355c3cb0fbfbb64d33d12b6117915aa49e14abc4b0c7",
        ),
    ];
    // #9997: a skill on its first release has no earlier text; its first
    // edit moves it off this list and its old hash into `previous`.
    // #11896: the drift-guard and improver skills moved into their agents.
    const FIRST_RELEASE: [&str; 4] = [
        "architect-github",
        "architect-owner-model",
        "architect-drift-check",
        "architect-review",
    ];
    assert_eq!(PINNED.len(), SHIPPED.len());
    for (skill, (stem, pinned)) in shipped().iter().zip(PINNED) {
        assert_eq!(skill.stem, stem);
        assert_eq!(
            sha256_hex(skill.contents.as_bytes()),
            pinned,
            "{stem} changed: add the OLD hash (the PINNED value) to its `previous` \
             list, then update PINNED"
        );
        assert_eq!(
            skill.previous.is_empty(),
            FIRST_RELEASE.contains(&stem),
            "{stem}: only a first-release skill has no shipped history"
        );
        assert!(
            !skill.previous.contains(&pinned),
            "{stem}: the current hash is listed as previous"
        );
    }
}

/// The `records/`-era text of each shipped skill, as `tm fleet init` seeded
/// it before #9997.
const RECORDS_ERA: [(&str, &str); 2] = [
    (
        "tm-fleet-check",
        include_str!("testdata/9997/tm-fleet-check.md"),
    ),
    (
        "tm-context-refresh",
        include_str!("testdata/9997/tm-context-refresh.md"),
    ),
];

/// #9997 upgrade path: an untouched `records/`-era seed with no deploy record
/// is a `previous` hit and is refreshed to the `local/` text; the same seed
/// with one hand-added line stays project-custom.
#[test]
fn a_records_era_seed_is_refreshed_and_an_edited_one_stays_custom() {
    // #9997 U1b: only the two skills that shipped in the records era have a
    // records-era text; the later skills are not part of this upgrade path.
    let skills: Vec<_> = shipped()
        .into_iter()
        .filter(|s| RECORDS_ERA.iter().any(|(stem, _)| *stem == s.stem))
        .collect();
    let era_stems: Vec<&str> = RECORDS_ERA.iter().map(|(stem, _)| *stem).collect();
    assert_eq!(skills.iter().map(|s| s.stem).collect::<Vec<_>>(), era_stems);
    let untouched = tempdir().unwrap();
    for (stem, text) in RECORDS_ERA {
        let path = skill_path(untouched.path(), stem);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    let report = deploy_skills_into(untouched.path(), &skills);
    assert_eq!(report.written, era_stems, "{report:?}");
    assert!(report.kept_custom.is_empty(), "{report:?}");
    for skill in &skills {
        let live = std::fs::read_to_string(skill_path(untouched.path(), skill.stem)).unwrap();
        assert_eq!(live, skill.contents, "{}", skill.stem);
        assert!(live.contains("local/state.md"), "{}", skill.stem);
    }

    let edited = tempdir().unwrap();
    for (stem, text) in RECORDS_ERA {
        let path = skill_path(edited.path(), stem);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{text}\nA project line.\n")).unwrap();
    }
    let report = deploy_skills_into(edited.path(), &skills);
    assert_eq!(report.kept_custom, era_stems, "{report:?}");
    assert!(report.written.is_empty(), "{report:?}");
    for (stem, text) in RECORDS_ERA {
        assert_eq!(
            std::fs::read_to_string(skill_path(edited.path(), stem)).unwrap(),
            format!("{text}\nA project line.\n")
        );
    }
}

#[test]
fn a_malformed_record_keeps_a_differing_copy_and_names_the_record() {
    let dir = tempdir().unwrap();
    let path = seed_probe(dir.path(), "project edit\n");
    std::fs::write(dir.path().join(RECORD_FILE), "{ not json").unwrap();

    let report = deploy_skills_into(dir.path(), &one("v2\n"));

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "project edit\n");
    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("malformed") && e.contains(RECORD_FILE)),
        "{:?}",
        report.errors
    );
}

#[test]
fn the_skills_row_names_a_malformed_record() {
    let project = tempdir().unwrap();
    deploy_architect_skills(project.path(), &[], &shipped());
    std::fs::write(project_tier(project.path()).join(RECORD_FILE), "{ not json").unwrap();

    let (ok, detail) = health(project.path());

    assert!(!ok, "{detail}");
    assert!(
        detail.contains("malformed") && detail.contains(RECORD_FILE),
        "{detail}"
    );
}

#[test]
fn a_record_write_failure_is_an_error_and_the_row_does_not_say_current() {
    // A directory where the record file goes: the write cannot succeed.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project_tier(project.path()).join(RECORD_FILE)).unwrap();

    let report = deploy_architect_skills(project.path(), &[], &shipped());

    assert!(
        report.errors.iter().any(|e| e.contains(RECORD_FILE)),
        "{:?}",
        report.errors
    );
    let (ok, detail) = health(project.path());
    assert!(!ok, "an unrecorded deploy read as current: {detail}");
    assert!(detail.contains(RECORD_FILE), "{detail}");
}

#[cfg(unix)]
#[test]
fn a_tier_resolving_onto_a_reserved_dir_is_refused() {
    use crate::core::paths::FrameworkPaths;

    let home = tempdir().unwrap();
    let root = home.path().join(MPM_DIR_NAME);
    let fw = FrameworkPaths::from_root(&root);
    for reserved in [fw.claude_skills_dir(), fw.skill_deploy_dir()] {
        let project = tempdir().unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&reserved).unwrap();
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.path().display().to_string()
            ),
        )
        .unwrap();
        std::fs::write(
            project.path().join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join(".claude")).unwrap();
        std::os::unix::fs::symlink(&reserved, project_tier(project.path())).unwrap();

        let report = deploy_for_launch(&root, project.path()).expect("an Architect launch");

        assert!(report.written.is_empty(), "{:?}", report.written);
        assert!(
            !report.errors.is_empty(),
            "no error line for {}",
            reserved.display()
        );
        for skill in SHIPPED {
            let landed = skill_path(&reserved, skill.stem);
            assert!(
                !landed.exists(),
                "wrote onto the reserved tier: {}",
                landed.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn the_skills_row_names_a_refused_reserved_tier() {
    let project = tempdir().unwrap();
    let reserved = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(reserved.path(), project_tier(project.path())).unwrap();

    let (ok, detail) = health_with(project.path(), &[reserved.path().to_path_buf()], &shipped());
    assert!(!ok, "{detail}");
    assert!(detail.contains("deploy refused"), "{detail}");
    let report =
        deploy_architect_skills(project.path(), &[reserved.path().to_path_buf()], &shipped());
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        std::fs::read_dir(reserved.path()).unwrap().next().is_none(),
        "the reserved dir was written"
    );
}

#[test]
fn architect_skills_never_join_the_bundled_pm_roster() {
    // Drift guard: a bundled skill deploys to every PM's managed tier.
    let bundled = crate::core::skill_retire::bundled_skill_stems(
        crate::core::content_source::test_support::rc(),
    );
    assert!(!bundled.is_empty(), "the bundled roster read as empty");
    for skill in SHIPPED {
        assert!(
            !bundled.contains(skill.stem),
            "{} is in the bundled PM skill roster",
            skill.stem
        );
    }
}

/// Deploy `skills` the way a launch and `sync-assets` do.
fn refresh(project: &Path, skills: &[ArchitectSkill]) -> DeployReport {
    deploy_architect_skills(project, &[], skills)
}

/// #10596 criterion 2: status after a content update.
///
/// Why: `tm content update` moves the shipped skills; `tm fleet status` must
/// read them stale until the refresh deploys, then current. The update's hint
/// names the refresh command, so the status it promises has to be true.
/// What: deploys the shipped skills (old pin), moves the shipped text (new
/// pin), reads `health_with`, runs the deploy `deploy_for_launch` and
/// `sync-assets` share, reads it again. Scratch project tier only: no HOME,
/// no daemon.
#[test]
fn a_content_update_leaves_skills_stale_until_the_refresh_deploys() {
    let project = tempdir().unwrap();
    let old = shipped();
    // The earlier launch that deployed the old pin.
    deploy_architect_skills(project.path(), &[], &old);
    let (ok, detail) = health_with(project.path(), &[], &old);
    assert!(ok, "{detail}");

    // The content update: every shipped skill's text moves.
    let new: Vec<ArchitectSkill> = shipped()
        .into_iter()
        .map(|mut skill| {
            skill
                .contents
                .push_str("\n<!-- pinned by a content update -->\n");
            skill
        })
        .collect();
    let (ok, detail) = health_with(project.path(), &[], &new);
    assert!(!ok, "{detail}");
    for stem in shipped_stems() {
        assert!(
            detail.contains(&format!(
                "{stem}: stale (the next Architect launch refreshes it)"
            )),
            "{detail}"
        );
    }

    let report = refresh(project.path(), &new);
    assert_eq!(report.written.len(), new.len(), "{report:?}");
    let (ok, detail) = health_with(project.path(), &[], &new);
    assert!(ok, "{detail}");
    assert_eq!(detail, format!("{} Architect skills current", new.len()));
}
