//! Tests for `tm fleet init|status` (#8436). Every test runs under a temp
//! home and a temp project, and none launches a session (`launch = false`),
//! so nothing reaches the operator's `~/.trusty-mpm` or tmux server.

use std::path::{Path, PathBuf};

use clap::Parser;

use super::config::{self, Edit};
use super::*;
use crate::cli::{Cli, Command};

/// A scratch home plus the default Architect directory under it.
struct Fixture {
    home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("scratch home"),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn dir(&self) -> PathBuf {
        resolve_dir(None, self.home()).unwrap()
    }

    fn config_path(&self) -> PathBuf {
        user_config_path(self.home())
    }

    fn write_config(&self, text: &str) {
        let path = self.config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.config_path()).unwrap()
    }

    fn init(&self) -> anyhow::Result<InitReport> {
        init(&self.dir(), self.home(), false)
    }
}

/// The canonical Architect path as `init` records it.
fn canonical(dir: &Path) -> String {
    std::fs::canonicalize(dir).unwrap().display().to_string()
}

#[test]
fn cli_parses_fleet_init() {
    let cli = Cli::try_parse_from(["tm", "fleet", "init", "--dir", "/x/a", "--no-launch"]).unwrap();
    match cli.command.unwrap() {
        Command::Fleet {
            action: FleetAction::Init { dir, no_launch },
        } => {
            assert_eq!(dir.as_deref(), Some("/x/a"));
            assert!(no_launch);
        }
        other => panic!("expected fleet init, got {other:?}"),
    }
}

#[test]
fn cli_parses_fleet_status() {
    let cli = Cli::try_parse_from(["tm", "fleet", "status", "--json"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Fleet {
            action: FleetAction::Status {
                dir: None,
                json: true
            }
        }
    ));
}

#[test]
fn a_first_run_writes_the_profile_the_grant_and_a_remote_less_repo() {
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    assert!(report.changed(), "{}", report.render());
    let dir = fx.dir();
    let project = std::fs::read_to_string(dir.join(".trusty-mpm.toml")).unwrap();
    assert!(project.contains("profile = \"supervisor\""), "{project}");
    assert!(fx.config().contains(&canonical(&dir)), "{}", fx.config());
    let remotes = std::process::Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "remote"])
        .output()
        .unwrap();
    assert!(remotes.status.success());
    assert!(remotes.stdout.is_empty(), "the Architect repo has a remote");
    // Ruling Q7: no twin grant.
    assert!(!fx.config().contains("twin"), "{}", fx.config());
    // With these files, a launch in the directory resolves to the supervisor.
    let cfg = trusty_mpm::core::config::MpmConfig::load(&fx.home().join(".trusty-mpm"));
    assert!(trusty_mpm::core::session_profile::resolve(&dir, &cfg).is_supervisor());
}

#[test]
fn a_second_run_changes_nothing() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let config_before = fx.config();
    let project_before = std::fs::read(fx.dir().join(".trusty-mpm.toml")).unwrap();
    let report = fx.init().unwrap();
    assert!(!report.changed(), "{}", report.render());
    assert!(
        report.render().contains("Nothing changed"),
        "{}",
        report.render()
    );
    assert_eq!(fx.config(), config_before);
    assert_eq!(
        std::fs::read(fx.dir().join(".trusty-mpm.toml")).unwrap(),
        project_before
    );
    assert_eq!(fx.config().matches(&canonical(&fx.dir())).count(), 1);
}

/// The fail-open guard: a config tm cannot parse is never rewritten or reset,
/// and nothing else is written either (both files are parsed before any write).
#[test]
fn a_malformed_config_fails_and_is_left_byte_identical() {
    for bad in [
        "# operator notes\n[supervisor\nprojects = [",
        "[supervisor]\nprojects = \"/not/an/array\"\n",
        "supervisor = 3\n",
    ] {
        let fx = Fixture::new();
        fx.write_config(bad);
        let err = fx.init().expect_err(bad);
        assert!(
            format!("{err:#}").contains("is malformed"),
            "{bad:?}: {err:#}"
        );
        assert_eq!(fx.config(), bad, "the malformed config was rewritten");
        assert!(
            !fx.dir().exists(),
            "{bad:?}: init wrote the project before failing"
        );
    }
}

#[test]
fn a_malformed_project_config_fails_and_is_left_byte_identical() {
    let fx = Fixture::new();
    let dir = fx.dir();
    std::fs::create_dir_all(&dir).unwrap();
    let bad = "profile = \"supervisor\"\nunknown_key = 1\n";
    std::fs::write(dir.join(".trusty-mpm.toml"), bad).unwrap();
    let err = fx.init().expect_err("unknown key must fail");
    assert!(format!("{err:#}").contains("is malformed"), "{err:#}");
    assert_eq!(
        std::fs::read_to_string(dir.join(".trusty-mpm.toml")).unwrap(),
        bad
    );
    assert!(
        !fx.config_path().exists(),
        "the grant was written for a refused project"
    );
}

#[test]
fn an_unrelated_key_and_comment_survive_the_allowlist_write() {
    let fx = Fixture::new();
    let original = "# my settings, keep me\n[models]\ndefault = \"sonnet\" # pinned\n\n\
                    [style]\nactive = \"trusty-mpm\"\n";
    fx.write_config(original);
    fx.init().unwrap();
    let after = fx.config();
    assert!(after.starts_with(original), "the prefix changed:\n{after}");
    assert!(after.contains("[supervisor]"), "{after}");
    let cfg = trusty_mpm::core::config::MpmConfig::load(&fx.home().join(".trusty-mpm"));
    assert_eq!(
        cfg.supervisor.projects,
        vec![PathBuf::from(canonical(&fx.dir()))]
    );
}

#[test]
fn an_existing_supervisor_table_gains_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = Path::new("/h/.trusty-mpm/config.toml");
    let entry = canonical(dir.path());
    for raw in [
        "[supervisor]\nprojects = [\"/elsewhere\"] # kept\n",
        "[supervisor]\n",
        "[supervisor.twin]\nprojects = []\n",
        "supervisor = { projects = [] }\n",
    ] {
        let Edit::Changed(text) =
            config::add_allowlist_entry(raw, Path::new(&entry), path).unwrap()
        else {
            panic!("{raw:?} should gain the entry");
        };
        let cfg: trusty_mpm::core::config::MpmConfig = toml::from_str(&text).unwrap();
        assert!(
            cfg.supervisor.projects.contains(&PathBuf::from(&entry)),
            "{raw:?}\n{text}"
        );
        assert_eq!(text.matches(&entry).count(), 1, "{text}");
    }
}

#[test]
fn a_second_add_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = Path::new("/h/.trusty-mpm/config.toml");
    let entry = std::fs::canonicalize(dir.path()).unwrap();
    let Edit::Changed(once) = config::add_allowlist_entry("", &entry, path).unwrap() else {
        panic!("an empty config gains the entry");
    };
    assert_eq!(
        config::add_allowlist_entry(&once, &entry, path).unwrap(),
        Edit::Unchanged
    );
}

#[test]
fn dir_overrides_the_default() {
    let fx = Fixture::new();
    assert_eq!(fx.dir(), fx.home().join("trusty-mpm-projects/architect"));
    let other = fx.home().join("elsewhere/arch");
    assert_eq!(
        resolve_dir(Some(other.to_str().unwrap()), fx.home()).unwrap(),
        other
    );
    init(&other, fx.home(), false).unwrap();
    assert!(other.join(".trusty-mpm.toml").exists());
    assert!(!fx.dir().exists(), "the default directory was created");
    assert!(fx.config().contains(&canonical(&other)));
}

#[test]
fn a_second_architect_elsewhere_is_refused() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let config_before = fx.config();
    let other = fx.home().join("second");
    let err = init(&other, fx.home(), false).expect_err("a second Architect");
    assert!(format!("{err:#}").contains("one per user"), "{err:#}");
    assert!(!other.exists());
    assert_eq!(fx.config(), config_before);
}

#[test]
fn status_reports_incomplete_setup() {
    let fx = Fixture::new();
    let fresh = status(&fx.dir(), fx.home());
    assert!(!fresh.complete);
    assert!(fresh.checks.iter().all(|c| !c.ok), "{}", fresh.render());
    assert!(fresh.render().contains("incomplete"), "{}", fresh.render());

    fx.init().unwrap();
    let set_up = status(&fx.dir(), fx.home());
    let ok: Vec<_> = set_up.checks.iter().map(|c| (c.name, c.ok)).collect();
    assert_eq!(
        ok,
        [
            ("allowlist", true),
            ("profile", true),
            ("session", false),
            ("launch_stamp", false)
        ],
        "{}",
        set_up.render()
    );
    assert!(
        !set_up.complete,
        "no session runs, so the setup is incomplete"
    );
}

// --- #8436 P4: the seeded fleet files and the poller start ---

/// The two Architect-only skills, as shipped.
fn ported_skills() -> Vec<&'static super::seed::Seeded> {
    super::seed::FILES
        .iter()
        .filter(|f| f.dest.starts_with(".claude/skills/"))
        .collect()
}

#[test]
fn a_first_run_seeds_the_architect_project() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    let dir = fx.dir();
    for file in super::seed::FILES {
        let path = dir.join(file.dest);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            file.contents.as_bytes(),
            "{}",
            file.dest
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o111 != 0,
            file.executable,
            "{}: mode {mode:o}",
            file.dest
        );
    }
    for rel in super::seed::DIRS {
        assert!(dir.join(rel).is_dir(), "{rel}");
    }
    for skill in ["tm-fleet-check", "tm-context-refresh"] {
        assert!(
            dir.join(".claude/skills")
                .join(skill)
                .join("SKILL.md")
                .is_file(),
            "{skill}"
        );
    }
    let claude_md = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(
        !claude_md.contains("TRUSTY-MPM:"),
        "an override marker was seeded"
    );
    assert!(!claude_md.contains("TRUSTY_MPM_PM_UNRESTRICTED"));
    assert!(claude_md.contains("records/"), "{claude_md}");
    let rendered = report.render();
    assert!(
        rendered.contains("poller start (--no-launch)"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("lands in"),
        "a stale P4 note survived: {rendered}"
    );
}

#[test]
fn an_edited_seed_or_script_is_never_overwritten() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let edited = [
        ("CLAUDE.md", "# my fleet\n"),
        ("scripts/fleet-poll.py", "print('mine')\n"),
    ];
    for (rel, text) in edited {
        std::fs::write(dir.join(rel), text).unwrap();
    }
    std::fs::remove_file(dir.join("records/actions.md")).unwrap();
    let report = fx.init().unwrap();
    for (rel, text) in edited {
        assert_eq!(
            std::fs::read_to_string(dir.join(rel)).unwrap(),
            text,
            "{rel} was overwritten"
        );
        assert!(
            report.steps.iter().any(
                |s| matches!(s, Step::Skipped(t) if t.starts_with(rel) && t.contains("left as is"))
            ),
            "{rel} not reported skipped:\n{}",
            report.render()
        );
    }
    assert!(
        report
            .steps
            .contains(&Step::Changed("wrote records/actions.md".to_owned()))
    );
    assert!(!report.failed(), "{}", report.render());
    let again = fx.init().unwrap();
    assert!(!again.changed(), "{}", again.render());
}

/// Acceptance 2: every Architect-relative path and script a ported skill
/// names exists in a freshly initialised project. `inbox/` is excluded: it is
/// the poller's runtime directory, and `events.jsonl` is absent until #8392.
#[test]
fn every_path_a_ported_skill_names_exists_after_init() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    for skill in ported_skills() {
        let mut checked = 0;
        for raw in skill
            .contents
            .split(|c: char| c.is_whitespace() || "`'\"(),".contains(c))
        {
            let word = raw.trim_end_matches(['.', ':', ';']);
            let architect_path = ["scripts/", "records/", ".claude/"]
                .iter()
                .any(|p| word.starts_with(p))
                || word == "CLAUDE.md";
            if !architect_path {
                continue;
            }
            // `records/projects/<project>.md` names a directory of per-project files.
            let path = match word.find('<') {
                Some(i) => dir.join(&word[..i]),
                None => dir.join(word),
            };
            assert!(
                path.exists(),
                "{}: `{word}` is missing after init",
                skill.dest
            );
            checked += 1;
        }
        assert!(checked >= 3, "{}: only {checked} paths found", skill.dest);
    }
}

/// Acceptance 3: with no `events.jsonl` the pass is a full poll, and the
/// every-4th-pass and empty-inbox fallbacks stay.
#[test]
fn the_fleet_check_skill_runs_a_full_poll_without_events() {
    let text = |name: &str| {
        ported_skills()
            .into_iter()
            .find(|s| s.dest.contains(name))
            .unwrap()
            .contents
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let check = text("tm-fleet-check");
    for needle in [
        "Run a full poll when `events.jsonl` is absent",
        "has no new lines this pass (an empty inbox)",
        "every 4th pass (`poll_count % 4 == 0`)",
    ] {
        assert!(check.contains(needle), "missing: {needle}");
    }
    assert!(text("tm-context-refresh").contains("Threshold: 50% context by default"));
}

/// The fail-closed arm: a start script that fails is an error naming its
/// exit status and output, never a pass.
#[test]
fn a_failing_start_script_is_an_error_with_its_cause() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join(super::poller::START_SCRIPT);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "echo 'no tmux here' >&2\nexit 7\n").unwrap();
    let err = super::poller::start(dir.path()).expect_err("a failing script must fail");
    let text = format!("{err:#}");
    assert!(
        text.contains("no tmux here") && text.contains('7'),
        "{text}"
    );
}

#[test]
fn a_failed_step_fails_the_report() {
    let report = InitReport {
        steps: vec![
            Step::Changed("wrote CLAUDE.md".to_owned()),
            Step::Failed("poller start: boom".to_owned()),
        ],
    };
    assert!(report.failed());
    let text = report.render();
    assert!(text.contains("FAILED     poller start: boom"), "{text}");
    assert!(!text.contains("Architect set up"), "{text}");
}
