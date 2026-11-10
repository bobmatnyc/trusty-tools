//! Tests for `tm fleet project` (#10895). Every test builds its Architect
//! directory, registry and tmux session list in a temp dir and passes them in;
//! nothing reads the operator's home, managed root or tmux server.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::Parser;
use trusty_mpm::core::project_aliases::ProjectAliasEntry;
use trusty_mpm::project::Project;

use super::registry::Registry;
use super::{add, list, list as list_mod, migrate, record, remove, watch};
use crate::cli::{Cli, Command, FleetAction, FleetProjectAction};

/// A registry row; `repo_url` an absolute path, so no home-derived clone path.
fn project(name: &str, repo_url: &str) -> Project {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "repo_url": repo_url,
        "default_branch": "main",
    }))
    .unwrap()
}

/// An Architect directory plus two registered checkouts.
///
/// `alpha` has an alias of the same name; `beta`'s alias is `beta-fork`,
/// found by path, so its key and alias differ.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("arch/local")).unwrap();
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(tmp.path().join(name)).unwrap();
        }
        Self { tmp }
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("arch")
    }

    fn checkout(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn registry(&self) -> Registry {
        let url = |n: &str| self.checkout(n).display().to_string();
        Registry::new(
            vec![
                project("alpha", &url("alpha")),
                project("beta", &url("beta")),
            ],
            vec![
                ProjectAliasEntry {
                    alias: "alpha".into(),
                    path: self.checkout("alpha"),
                },
                ProjectAliasEntry {
                    alias: "beta-fork".into(),
                    path: self.checkout("beta"),
                },
            ],
            self.tmp.path().join("root/project-registry/projects.json"),
        )
    }

    fn list_path(&self) -> PathBuf {
        watch::path(&self.dir())
    }

    fn keys(&self) -> Vec<String> {
        watch::read(&self.list_path())
            .unwrap()
            .unwrap()
            .projects
            .into_iter()
            .map(|e| e.key)
            .collect()
    }

    fn write_list(&self, keys: &[&str]) {
        let file = watch::WatchFile {
            version: watch::VERSION,
            projects: keys
                .iter()
                .map(|k| watch::Entry { key: (*k).into() })
                .collect(),
        };
        std::fs::write(self.list_path(), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    fn write_claude_md(&self, lines: &[&str]) {
        let text = format!(
            "# The Architect\n\n## Watch set\n\nThe projects.\n\n{}\n\n## Cadence\n\n- `tm-x`: `/elsewhere`\n",
            lines.join("\n")
        );
        std::fs::write(self.dir().join("CLAUDE.md"), text).unwrap();
    }
}

/// Restores a directory's mode when dropped, so a failed test still cleans up.
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn new(path: &Path) -> Self {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
        Self(path.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn cli_parses_fleet_project() {
    let cli =
        Cli::try_parse_from(["tm", "fleet", "project", "add", "alpha", "--dir", "/a"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Fleet { action: FleetAction::Project {
            action: FleetProjectAction::Add { project, dir: Some(d) }
        } } if project == "alpha" && d == "/a"
    ));
    let cli = Cli::try_parse_from(["tm", "fleet", "project", "list", "--json"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Fleet {
            action: FleetAction::Project {
                action: FleetProjectAction::List {
                    json: true,
                    dir: None
                }
            }
        }
    ));
    let cli = Cli::try_parse_from(["tm", "fleet", "project", "remove", "beta"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Fleet { action: FleetAction::Project {
            action: FleetProjectAction::Remove { key, dir: None }
        } } if key == "beta"
    ));
}

#[test]
fn registry_join_matches_alias_exact_then_case_then_path() {
    let fx = Fixture::new();
    let mut aliases = vec![
        ProjectAliasEntry {
            alias: "ALPHA".into(),
            path: fx.checkout("x"),
        },
        ProjectAliasEntry {
            alias: "alpha".into(),
            path: fx.checkout("alpha"),
        },
    ];
    let reg = Registry::new(
        vec![project("alpha", "/nowhere/alpha")],
        aliases.clone(),
        PathBuf::new(),
    );
    let alpha = reg.get("alpha").unwrap();
    assert_eq!(reg.alias_for(alpha).unwrap().alias, "alpha", "exact wins");
    aliases.remove(1);
    let reg = Registry::new(
        vec![project("alpha", "/nowhere/alpha")],
        aliases,
        PathBuf::new(),
    );
    let alpha = reg.get("alpha").unwrap();
    assert_eq!(reg.alias_for(alpha).unwrap().alias, "ALPHA", "then case");
    let reg = fx.registry();
    let beta = reg.get("beta").unwrap();
    assert_eq!(reg.alias_for(beta).unwrap().alias, "beta-fork", "then path");
    assert_eq!(reg.path_for(beta), Some(fx.checkout("beta")));
}

#[test]
fn registry_resolves_a_checkout_directory_to_its_key() {
    let fx = Fixture::new();
    let reg = fx.registry();
    assert_eq!(reg.resolve("beta"), Some("beta"));
    let dir = fx.checkout("beta").display().to_string();
    assert_eq!(reg.resolve(&dir), Some("beta"));
    assert_eq!(reg.resolve(&format!("{dir}/")), Some("beta"));
    assert_eq!(reg.resolve("gamma"), None);
}

#[tokio::test]
async fn registry_load_reads_both_stores_under_the_root() {
    let fx = Fixture::new();
    let root = fx.tmp.path().join("root");
    std::fs::create_dir_all(root.join("project-registry")).unwrap();
    let alpha = project("alpha", &fx.checkout("alpha").display().to_string());
    std::fs::write(
        root.join("project-registry/projects.json"),
        serde_json::to_vec(&serde_json::json!({ "projects": { "alpha": alpha } })).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("project-paths.json"),
        serde_json::to_vec(
            &serde_json::json!([{ "alias": "alpha-fork", "path": fx.checkout("alpha") }]),
        )
        .unwrap(),
    )
    .unwrap();
    let reg = Registry::load(&root).await.unwrap();
    let alpha = reg.get("alpha").unwrap();
    assert_eq!(reg.alias_for(alpha).unwrap().alias, "alpha-fork");
    // An empty root is an empty registry, not an error.
    let empty = Registry::load(&fx.tmp.path().join("empty")).await.unwrap();
    assert_eq!(empty.resolve("alpha"), None);
}

#[test]
fn add_of_an_unregistered_project_writes_nothing() {
    // AC1, absent list: CLAUDE.md lists nothing, and the list stays absent.
    let fx = Fixture::new();
    let err = add(&fx.dir(), &fx.registry(), "gamma")
        .unwrap_err()
        .to_string();
    assert!(err.contains("gamma is not a registered project"), "{err}");
    assert!(err.contains("tm projects register"), "{err}");
    assert!(!fx.list_path().exists());
    assert!(!fx.dir().join("local/projects").exists());
    // AC1, existing list: byte-identical.
    fx.write_list(&["alpha"]);
    let before = std::fs::read(fx.list_path()).unwrap();
    assert!(add(&fx.dir(), &fx.registry(), "/no/such/dir").is_err());
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), before);
}

#[test]
fn add_twice_leaves_one_entry_and_reports_unchanged() {
    // AC2.
    let fx = Fixture::new();
    let first = add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    assert!(first.message.contains("added alpha"), "{}", first.message);
    assert!(first.message.contains("wrote"), "{}", first.message);
    let before = std::fs::read(fx.list_path()).unwrap();
    let second = add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    assert_eq!(second.message, "unchanged: alpha is already watched\n");
    assert_eq!(fx.keys(), ["alpha"]);
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), before);
}

#[test]
fn add_writes_the_record_from_the_architects_own_template() {
    let fx = Fixture::new();
    let projects = fx.dir().join("local/projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join("_template.md"),
        "# <Project name>\n\n- at <path> as <tm-name>\n",
    )
    .unwrap();
    add(
        &fx.dir(),
        &fx.registry(),
        &fx.checkout("beta").display().to_string(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(projects.join("beta.md")).unwrap(),
        format!(
            "# beta\n\n- at {} as tm-beta-fork\n",
            fx.checkout("beta").display()
        )
    );
    assert_eq!(fx.keys(), ["beta"]);
}

#[test]
fn concurrent_adds_for_two_projects_keep_both_entries() {
    // AC3: each round starts both adds at one barrier on a fresh list.
    for _ in 0..16 {
        let fx = Fixture::new();
        fx.write_list(&[]);
        let reg = fx.registry();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for key in ["alpha", "beta"] {
                let (dir, reg, barrier) = (fx.dir(), &reg, &barrier);
                s.spawn(move || {
                    barrier.wait();
                    add(&dir, reg, key).unwrap();
                });
            }
        });
        let mut keys = fx.keys();
        keys.sort();
        assert_eq!(keys, ["alpha", "beta"]);
    }
}

#[test]
fn add_record_write_failure_leaves_the_list_unchanged() {
    // AC4, first arm: the record cannot be written, so the entry is not added.
    let fx = Fixture::new();
    fx.write_list(&[]);
    let projects = fx.dir().join("local/projects");
    std::fs::create_dir_all(&projects).unwrap();
    let before = std::fs::read(fx.list_path()).unwrap();
    let guard = ReadOnly::new(&projects);
    let result = add(&fx.dir(), &fx.registry(), "alpha");
    drop(guard);
    let err = format!(
        "{:#}",
        result.expect_err("a failed record write must fail add")
    );
    assert!(err.contains("cannot write the record"), "{err}");
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), before);
}

#[test]
fn add_list_write_failure_after_the_record_fails_and_a_rerun_completes() {
    // AC4, second arm: `local/` read-only with the lock sidecar already there,
    // so the lock opens and the record lands, but the list cannot be published.
    let fx = Fixture::new();
    fx.write_list(&[]);
    let local = fx.dir().join("local");
    std::fs::create_dir_all(local.join("projects")).unwrap();
    std::fs::write(local.join("watch.json.lock"), "").unwrap();
    let before = std::fs::read(fx.list_path()).unwrap();
    let guard = ReadOnly::new(&local);
    let result = add(&fx.dir(), &fx.registry(), "alpha");
    drop(guard);
    let err = format!(
        "{:#}",
        result.expect_err("a failed list write must fail add")
    );
    assert!(err.contains("cannot add alpha"), "{err}");
    assert!(local.join("projects/alpha.md").exists(), "record first");
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), before);
    let rerun = add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    assert_eq!(rerun.message, "added alpha to the watch list\n");
    assert_eq!(fx.keys(), ["alpha"]);
}

#[test]
fn list_shows_record_present_false_for_a_deleted_record() {
    // AC4, last sentence.
    let fx = Fixture::new();
    add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    std::fs::remove_file(fx.dir().join("local/projects/alpha.md")).unwrap();
    let (_, rows) = list(&fx.dir(), &fx.registry(), Some(&[])).unwrap();
    assert!(!rows[0].record_present);
}

#[test]
fn remove_marks_the_record_and_drops_the_entry() {
    // AC5.
    let fx = Fixture::new();
    add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    add(&fx.dir(), &fx.registry(), "beta").unwrap();
    let out = remove(&fx.dir(), &fx.registry(), "alpha", "2026-11-10T12:00:00Z").unwrap();
    assert!(out.message.contains("removed alpha"), "{}", out.message);
    assert_eq!(fx.keys(), ["beta"]);
    let record = std::fs::read_to_string(fx.dir().join("local/projects/alpha.md")).unwrap();
    let lines: Vec<&str> = record.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == "- Watch: removed 2026-11-10T12:00:00Z")
        .expect("marked");
    assert!(
        lines[at - 1].starts_with("- trusty-memory palace:"),
        "{record}"
    );
    assert_eq!(lines[at + 1], "");
}

#[test]
fn remove_record_mark_failure_keeps_the_entry() {
    // #10895 review: the record exists but cannot be rewritten, so the entry
    // stays and the list is byte-identical.
    let fx = Fixture::new();
    add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    let projects = fx.dir().join("local/projects");
    let record = projects.join("alpha.md");
    let list_before = std::fs::read(fx.list_path()).unwrap();
    let record_before = std::fs::read(&record).unwrap();
    let guard = ReadOnly::new(&projects);
    let result = remove(&fx.dir(), &fx.registry(), "alpha", "t");
    drop(guard);
    let err = format!(
        "{:#}",
        result.expect_err("a failed record mark must fail remove")
    );
    assert!(err.contains("cannot mark"), "{err}");
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), list_before);
    assert_eq!(fx.keys(), ["alpha"]);
    assert_eq!(std::fs::read(&record).unwrap(), record_before);
}

#[test]
fn remove_of_an_unlisted_key_writes_nothing() {
    // AC5: with a list, and with no list (the import's plan decides).
    let fx = Fixture::new();
    assert!(remove(&fx.dir(), &fx.registry(), "alpha", "t").is_err());
    assert!(!fx.list_path().exists());
    add(&fx.dir(), &fx.registry(), "alpha").unwrap();
    let list_before = std::fs::read(fx.list_path()).unwrap();
    let record = fx.dir().join("local/projects/alpha.md");
    let record_before = std::fs::read(&record).unwrap();
    let err = remove(&fx.dir(), &fx.registry(), "beta", "t").unwrap_err();
    assert!(
        err.to_string().contains("beta is not on the watch list"),
        "{err}"
    );
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), list_before);
    assert_eq!(std::fs::read(&record).unwrap(), record_before);
}

#[test]
fn list_rows_carry_the_eight_fields() {
    // AC6.
    let fx = Fixture::new();
    fx.write_list(&["alpha"]);
    let names = vec![
        "tm-alpha".to_owned(),
        "tm-alpha-02".to_owned(),
        "tm-beta".to_owned(),
    ];
    let (_, rows) = list(&fx.dir(), &fx.registry(), Some(&names)).unwrap();
    let value = serde_json::to_value(&rows).unwrap();
    let row = value[0].as_object().unwrap();
    let mut fields: Vec<&str> = row.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "alias_mismatch",
            "key",
            "live_sessions",
            "path",
            "path_exists",
            "record_present",
            "registered",
            "session_alias"
        ]
    );
    assert_eq!(value[0]["live_sessions"], 2);
    assert_eq!(value[0]["registered"], true);
    assert_eq!(value[0]["path_exists"], true);
}

#[test]
fn list_reports_a_missing_path_and_an_alias_mismatch() {
    // AC6: a missing path is still listed; beta's alias differs from its key;
    // an unregistered key is listed with no path.
    let fx = Fixture::new();
    fx.write_list(&["alpha", "beta", "gone"]);
    std::fs::remove_dir(fx.checkout("alpha")).unwrap();
    let (out, rows) = list(&fx.dir(), &fx.registry(), None).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(!rows[0].path_exists && rows[0].path.is_some());
    assert!(!rows[0].alias_mismatch);
    assert!(rows[1].alias_mismatch);
    assert_eq!(rows[1].session_alias.as_deref(), Some("beta-fork"));
    assert!(!rows[2].registered && rows[2].path.is_none());
    assert_eq!(rows[2].live_sessions, None);
    assert!(out.message.contains("[path missing]"), "{}", out.message);
    assert!(
        out.message.contains("sessions named tm-beta-fork"),
        "{}",
        out.message
    );
    assert!(out.message.contains("[not registered]"), "{}", out.message);
}

#[test]
fn list_counts_sessions_by_the_alias_leaf() {
    let names: Vec<String> = [
        "tm-beta-fork",
        "tm-beta-fork-03",
        "tm-beta",
        "tm-beta-fork-x",
        "tm-beta-fork-123",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(list_mod::count_sessions(&names, "beta-fork"), 2);
    assert_eq!(list_mod::count_sessions(&names, "beta"), 1);
}

#[test]
fn migration_parses_lines_with_and_without_a_goal() {
    assert_eq!(
        migrate::parse_line("- `tm-a`: `/x/a`"),
        Some(("tm-a", "/x/a"))
    );
    assert_eq!(
        migrate::parse_line("- `tm-a`: `/x/a` — ship the release"),
        Some(("tm-a", "/x/a"))
    );
    assert_eq!(migrate::parse_line("- tm-a: /x/a"), None);
    // #10895 review: any bullet character, at any indentation.
    assert_eq!(
        migrate::parse_line("  * `tm-a`: `/x/a`"),
        Some(("tm-a", "/x/a"))
    );
    assert_eq!(
        migrate::parse_line("\t+\t`tm-a`: `/x/a`"),
        Some(("tm-a", "/x/a"))
    );
    assert_eq!(migrate::parse_line("-`tm-a`: `/x/a`"), None);
    let text =
        "# T\n\n## Watch set\n\nProse.\n\n- `tm-a`: `/a`\n- note\n\n## Next\n\n- `tm-b`: `/b`\n";
    assert_eq!(
        migrate::section_lines(text),
        Some(vec!["- `tm-a`: `/a`", "- note"])
    );
    assert_eq!(migrate::section_lines("# T\n\n- `tm-a`: `/a`\n"), None);
}

#[test]
fn migration_notes_every_bullet_shape_and_a_missing_section() {
    // #10895 review: every bullet in the section imports or is named in a
    // skip note; the heading tolerates case and whitespace.
    let fx = Fixture::new();
    let text = "# The Architect\n\n## watch SET  \n\nThe projects.\n\n\
                - `tm-<project>`: `<absolute project path>`\n\
                \x20 - `tm-alpha`: `/moved/alpha`\n\
                * `tm-beta`: `/moved/beta` — a goal\n\
                + a plus note\n\
                \t- a tab-indented note\n\
                \x20  * an indented star note\n\
                -\n\n## Cadence\n\n- `tm-x`: `/elsewhere`\n";
    std::fs::write(fx.dir().join("CLAUDE.md"), text).unwrap();
    let (out, _) = list(&fx.dir(), &fx.registry(), Some(&[])).unwrap();
    assert_eq!(fx.keys(), ["alpha", "beta"]);
    assert_eq!(out.notes.len(), 6, "{:?}", out.notes);
    for line in [
        "  - `tm-alpha`: `/moved/alpha`",
        "* `tm-beta`: `/moved/beta` — a goal",
    ] {
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("added") && n.contains(&format!("{line:?}"))),
            "{line}: {:?}",
            out.notes
        );
    }
    for line in [
        "+ a plus note",
        "\t- a tab-indented note",
        "   * an indented star note",
        "-",
    ] {
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("skipped") && n.contains(&format!("{line:?}"))),
            "{line:?}: {:?}",
            out.notes
        );
    }
    assert!(
        !out.notes
            .iter()
            .any(|n| n.contains("<absolute project path>")),
        "placeholder is silent: {:?}",
        out.notes
    );
    assert_eq!(
        std::fs::read_to_string(fx.dir().join("CLAUDE.md")).unwrap(),
        text
    );

    // No watch-set section: one note, and an empty list.
    let fx = Fixture::new();
    let text = "# The Architect\n\n## Watched\n\n- `tm-alpha`: `/x`\n";
    std::fs::write(fx.dir().join("CLAUDE.md"), text).unwrap();
    let (out, rows) = list(&fx.dir(), &fx.registry(), Some(&[])).unwrap();
    assert_eq!(out.notes.len(), 1, "{:?}", out.notes);
    assert!(
        out.notes[0].contains("has no \"## Watch set\" section"),
        "{:?}",
        out.notes
    );
    assert!(rows.is_empty() && fx.keys().is_empty());
    assert_eq!(
        std::fs::read_to_string(fx.dir().join("CLAUDE.md")).unwrap(),
        text
    );
}

#[tokio::test]
async fn migration_refuses_without_a_registry_file_and_writes_nothing() {
    // #10895 review: an absent registry must not use up the import.
    let fx = Fixture::new();
    let root = fx.tmp.path().join("wrong-root");
    let reg = Registry::load(&root).await.unwrap();
    assert!(reg.is_absent());
    fx.write_claude_md(&[
        "- `tm-<project>`: `<absolute project path>`",
        "- `tm-alpha`: `/x/alpha`",
    ]);
    let claude_before = std::fs::read(fx.dir().join("CLAUDE.md")).unwrap();
    let registry_path = root.join("project-registry/projects.json");
    for result in [
        list(&fx.dir(), &reg, Some(&[])).map(|_| ()),
        remove(&fx.dir(), &reg, "alpha", "t").map(|_| ()),
    ] {
        let err = format!("{:#}", result.expect_err("an absent registry refuses"));
        assert!(err.contains(&registry_path.display().to_string()), "{err}");
        assert!(err.contains("TRUSTY_MPM_ROOT"), "{err}");
    }
    assert!(!fx.list_path().exists());
    assert!(!fx.dir().join("local/watch.json.lock").exists());
    assert_eq!(
        std::fs::read(fx.dir().join("CLAUDE.md")).unwrap(),
        claude_before
    );

    // Only the placeholder: the empty list is created, as before.
    fx.write_claude_md(&["- `tm-<project>`: `<absolute project path>`"]);
    let (out, rows) = list(&fx.dir(), &reg, Some(&[])).unwrap();
    assert!(out.notes.is_empty() && rows.is_empty(), "{:?}", out.notes);
    assert!(fx.keys().is_empty());
}

#[test]
fn migration_imports_registered_lines_and_skips_the_rest() {
    // AC7: two registered (one by path, one by name) and one unregistered.
    let fx = Fixture::new();
    fx.write_claude_md(&[
        "- `tm-<project>`: `<absolute project path>`",
        &format!(
            "- `tm-beta-fork`: `{}` — a goal",
            fx.checkout("beta").display()
        ),
        "- `tm-alpha`: `/moved/away`",
        "- `tm-gamma`: `/x/gamma`",
    ]);
    let claude_before = std::fs::read(fx.dir().join("CLAUDE.md")).unwrap();
    let (out, rows) = list(&fx.dir(), &fx.registry(), Some(&[])).unwrap();
    assert_eq!(fx.keys(), ["beta", "alpha"]);
    assert_eq!(rows.len(), 2);
    let skipped: Vec<&String> = out.notes.iter().filter(|n| n.contains("skipped")).collect();
    assert_eq!(skipped.len(), 1, "{:?}", out.notes);
    assert!(skipped[0].contains("tm-gamma"), "{skipped:?}");
    assert!(
        !out.notes.iter().any(|n| n.contains("<project>")),
        "placeholder is silent"
    );
    assert_eq!(
        std::fs::read(fx.dir().join("CLAUDE.md")).unwrap(),
        claude_before
    );
}

#[test]
fn migration_runs_once_and_an_emptied_list_stays_empty() {
    // AC7: a second list imports nothing; removing every entry leaves an empty
    // list, and the next command imports nothing.
    let fx = Fixture::new();
    fx.write_claude_md(&["- `tm-alpha`: `/x`"]);
    list(&fx.dir(), &fx.registry(), None).unwrap();
    assert_eq!(fx.keys(), ["alpha"]);
    let (again, _) = list(&fx.dir(), &fx.registry(), None).unwrap();
    assert!(again.notes.is_empty(), "{:?}", again.notes);
    remove(&fx.dir(), &fx.registry(), "alpha", "t").unwrap();
    assert!(fx.keys().is_empty());
    let (after, rows) = list(&fx.dir(), &fx.registry(), None).unwrap();
    assert!(after.notes.is_empty() && rows.is_empty());
    assert!(fx.keys().is_empty());
}

#[test]
fn a_future_version_is_refused_and_left_unchanged() {
    let fx = Fixture::new();
    std::fs::write(fx.list_path(), r#"{"version": 2, "projects": []}"#).unwrap();
    let before = std::fs::read(fx.list_path()).unwrap();
    let err = format!("{:#}", add(&fx.dir(), &fx.registry(), "alpha").unwrap_err());
    assert!(err.contains("version 2"), "{err}");
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), before);
}

#[test]
fn record_render_fills_known_placeholders_and_keeps_the_rest() {
    let template = record::template(Path::new("/nonexistent")).unwrap();
    let fields = record::Fields {
        key: "alpha",
        repo: Some("acme/alpha".into()),
        path: Some(Path::new("/src/alpha")),
        session: "tm-alpha".into(),
    };
    let text = record::render(&template, &fields);
    assert!(
        text.starts_with("# alpha\n\n- Repo: acme/alpha\n"),
        "{text}"
    );
    assert!(text.contains("- Local path: /src/alpha\n"), "{text}");
    assert!(text.contains("- tmux session: tm-alpha "), "{text}");
    assert!(text.contains("<palace name>"), "unknown placeholders stay");
    assert!(!text.contains("Copy this file"), "{text}");
}

#[test]
fn record_create_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("p/alpha.md");
    assert!(record::create(&path, "one").unwrap());
    assert!(!record::create(&path, "two").unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("p")).unwrap().collect();
    assert_eq!(left.len(), 1, "no temp file left behind");
    assert!(record::path(tmp.path(), "../x").is_err());
    assert!(record::path(tmp.path(), ".hidden").is_err());
}

#[test]
fn record_mark_removed_appends_after_the_first_bullet_block() {
    let stamp = "2026-11-10T00:00:00Z";
    let marked = record::mark_removed("# A\n\n- one\n- two\n  more\n\n## B\n\n- x\n", stamp);
    assert_eq!(
        marked,
        format!("# A\n\n- one\n- two\n  more\n- Watch: removed {stamp}\n\n## B\n\n- x\n")
    );
    // No bullets after the heading: directly after it.
    assert_eq!(
        record::mark_removed("# A\n\nProse.\n", stamp),
        format!("# A\n- Watch: removed {stamp}\n\nProse.\n")
    );
    // A file ending in the bullet block, with no final newline.
    assert_eq!(
        record::mark_removed("# A\n- one", stamp),
        format!("# A\n- one\n- Watch: removed {stamp}\n")
    );
}

#[test]
fn fleet_init_seeds_the_watch_set_pointer_and_no_watch_list() {
    // AC8: `tm fleet init`, seeded from this checkout's `content/` tree.
    use trusty_mpm::core::content_source::Fetch;
    use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("arch");
    let checkout = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    super::super::init_with_seed(
        &dir,
        home.path(),
        false,
        super::super::tests::NO_TMUX,
        None,
        |_| ArchitectSeed::resolve_in(&checkout, None, None, Fetch::Never),
    )
    .unwrap();
    let claude = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    let section: String = claude
        .lines()
        .skip_while(|l| *l != "## Watch set")
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(section.contains("`tm fleet project list`"), "{section}");
    assert!(!section.contains("- `tm-<project>`"), "{section}");
    assert_eq!(migrate::section_lines(&claude), Some(Vec::new()));
    let state = std::fs::read_to_string(dir.join("local/state.md")).unwrap();
    assert!(state.contains("`local/watch.json`"), "{state}");
    let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(
        gitignore.lines().any(|l| l == "local/watch.json.lock"),
        "{gitignore}"
    );
    assert!(!watch::path(&dir).exists());
}
