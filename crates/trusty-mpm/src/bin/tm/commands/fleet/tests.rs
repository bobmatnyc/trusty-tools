//! Tests for `tm fleet init|status` (#8436). Every test runs under a temp
//! home and a temp project, none launches a session (`launch = false`), and
//! every `init`/`status`/poller call reads tmux through a stub [`Probe`]
//! ([`NO_TMUX`] unless the test plants a state), so nothing reaches the
//! operator's `~/.trusty-mpm` or any tmux server.

use std::path::{Path, PathBuf};

use clap::Parser;

use super::config::{self, Edit};
use super::launch::PaneState;
use super::session_name::SessionNames;
use super::*;
use crate::cli::{Cli, Command};
use trusty_mpm::core::content_source::Fetch;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;
use trusty_mpm::core::project_skill_tier::architect_skills::SHIPPED;

/// A tmux with no session and no stamp; the preflight tests use it too.
pub(super) const NO_TMUX: Probe = Probe {
    pane: |_| PaneState::Absent,
    stamp: |_| None,
    claude: |_| None,
};

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
        init(&self.dir(), self.home(), false, NO_TMUX)
    }

    fn status(&self) -> super::status::StatusReport {
        status(&self.dir(), self.home(), NO_TMUX, None)
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
            action:
                FleetAction::Init {
                    dir,
                    no_launch,
                    session,
                },
        } => {
            assert_eq!(dir.as_deref(), Some("/x/a"));
            assert!(no_launch);
            assert_eq!(session, None);
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
                json: true,
                session: None,
            }
        }
    ));
}

#[test]
fn cli_parses_fleet_relaunch() {
    // #9092: `relaunch` takes `status`'s `--dir` and `--session`.
    let cli = Cli::try_parse_from(["tm", "fleet", "relaunch", "--session", "tm-sup"]).unwrap();
    assert!(matches!(
        cli.command.unwrap(),
        Command::Fleet {
            action: FleetAction::Relaunch { dir: None, session: Some(s) }
        } if s == "tm-sup"
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
    assert!(trusty_mpm::core::session_profile::resolve(&dir, &cfg).is_architect());
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
        cfg.architect_grant().projects,
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
            cfg.architect_grant()
                .projects
                .contains(&PathBuf::from(&entry)),
            "{raw:?}\n{text}"
        );
        assert_eq!(text.matches(&entry).count(), 1, "{text}");
    }
}

/// #9755 (Q7): with `[architect]` present the grant lands there, the table
/// the reader uses; the legacy `[supervisor]` is left byte-identical.
#[test]
fn an_existing_architect_table_gains_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = Path::new("/h/.trusty-mpm/config.toml");
    let entry = canonical(dir.path());
    let raw = "[architect]\nprojects = []\n\n[supervisor]\nprojects = [\"/old\"]\n";
    let Edit::Changed(text) = config::add_allowlist_entry(raw, Path::new(&entry), path).unwrap()
    else {
        panic!("the [architect] table should gain the entry");
    };
    let cfg: trusty_mpm::core::config::MpmConfig = toml::from_str(&text).unwrap();
    assert_eq!(cfg.architect_grant().projects, [PathBuf::from(&entry)]);
    assert!(
        text.contains("[supervisor]\nprojects = [\"/old\"]\n"),
        "{text}"
    );
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
    init(&other, fx.home(), false, NO_TMUX).unwrap();
    assert!(other.join(".trusty-mpm.toml").exists());
    let default = fx.home().join(DEFAULT_DIR);
    assert!(!default.exists(), "the default directory was created");
    assert!(fx.config().contains(&canonical(&other)));
}

/// #8995: with no `--dir`, status checks the Architect `init` recorded
/// elsewhere, and its hint names that directory.
#[test]
fn status_without_dir_checks_the_recorded_architect() {
    let fx = Fixture::new();
    let other = fx.home().join("elsewhere/supervisor");
    init(&other, fx.home(), false, NO_TMUX).unwrap();

    let resolved = resolve_dir(None, fx.home()).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(&other).unwrap());
    let report = status(&resolved, fx.home(), NO_TMUX, None);
    let ok: Vec<_> = report.checks.iter().map(|c| (c.name, c.ok)).collect();
    assert_eq!(
        ok[..2],
        [("allowlist", true), ("profile", true)],
        "{}",
        report.render()
    );
    let hint = format!("tm fleet init --dir {}", resolved.display());
    assert!(report.render().contains(&hint), "{}", report.render());
}

/// #8995: two recorded Architects are never resolved by guessing.
#[test]
fn two_recorded_architects_ask_for_dir() {
    let fx = Fixture::new();
    let (a, b) = (fx.home().join("a"), fx.home().join("b"));
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".trusty-mpm.toml"), "profile = \"supervisor\"\n").unwrap();
    }
    fx.write_config(&format!(
        "[supervisor]\nprojects = [{:?}, {:?}]\n",
        a.display().to_string(),
        b.display().to_string()
    ));
    let err = resolve_dir(None, fx.home()).expect_err("two Architects");
    assert!(format!("{err:#}").contains("pass --dir"), "{err:#}");
}

#[test]
fn a_second_architect_elsewhere_is_refused() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let config_before = fx.config();
    let other = fx.home().join("second");
    let err = init(&other, fx.home(), false, NO_TMUX).expect_err("a second Architect");
    assert!(format!("{err:#}").contains("one per user"), "{err:#}");
    assert!(!other.exists());
    assert_eq!(fx.config(), config_before);
}

#[test]
fn status_reports_incomplete_setup() {
    let fx = Fixture::new();
    let fresh = fx.status();
    assert!(!fresh.complete);
    assert!(fresh.checks.iter().all(|c| !c.ok), "{}", fresh.render());
    assert!(fresh.render().contains("incomplete"), "{}", fresh.render());

    fx.init().unwrap();
    let set_up = fx.status();
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

/// #8878 PR-I: `tm fleet status` names the failed identity check in the
/// hook's words, and never folds it into `complete`.
#[test]
fn fleet_status_names_why_this_session_is_not_bound() {
    use crate::commands::pm_guard_architect_reason::NotArchitect;
    use crate::commands::pm_guard_trust_anchor::HookEnv;
    use crate::commands::pm_guard_trust_anchor::tests as anchor;
    use trusty_mpm::core::architect_launch::LaunchRefusal;

    let fx = anchor::fixture();
    let granted = || anchor::allowlist(&fx);
    // The bound Architect, run from a Bash call with no `CLAUDE_PROJECT_DIR`.
    let bash_call = HookEnv {
        project_dir: None,
        ..anchor::architect_env(&fx)
    };
    let bound = status::this_session_check(&fx.project, bash_call, granted);
    assert!(bound.ok, "{}", bound.detail);
    // No process table: the hook's own reason, and no PID.
    let unbound = status::this_session_check(&fx.project, anchor::spoof_env(&fx), granted);
    let why = NotArchitect::Launch(LaunchRefusal::ProcessLookup).to_string();
    assert!(!unbound.ok);
    assert!(unbound.detail.ends_with(&why), "{}", unbound.detail);
    assert!(!unbound.detail.chars().any(|c| c.is_ascii_digit()));
    // Not the supervisor profile at all.
    let pm = status::this_session_check(&fx.project, HookEnv::default(), granted);
    assert!(
        pm.detail
            .ends_with(&NotArchitect::NoArchitectStamp.to_string())
    );

    let home = Fixture::new();
    let mut report = home.status();
    report.this_session = Some(unbound);
    assert!(report.render().contains(&format!(
        "UNBOUND  this_session {}",
        report.this_session.as_ref().unwrap().detail
    )));
    assert!(!report.complete);
}

/// #8938: the status walk from `tm fleet status` (PID 100) over `rows`, in the
/// shape of the hook's `anchor::table`: a PID's start time is `pid * 10`, and
/// a PID absent from `rows` is a table read error.
fn status_table(
    rows: Vec<crate::commands::pm_guard_trust_anchor::tests::Row>,
) -> crate::commands::pm_guard_trust_anchor::ClaudeLookup {
    use trusty_mpm::core::twin_arming;
    crate::commands::pm_guard_trust_anchor::ClaudeLookup::new(move || {
        let find = |pid: u32| {
            rows.iter()
                .find(|row| row.0 == pid)
                .copied()
                .ok_or_else(|| format!("no pid {pid}"))
        };
        twin_arming::nearest_claude_for_status_in(
            100,
            |pid| {
                find(pid).map(|row| twin_arming::ProcessFacts {
                    parent: row.1,
                    start_time: u64::from(pid) * 10,
                })
            },
            |pid| find(pid).map(|row| row.2),
        )
    })
}

/// #8938: from the Architect's Bash tool, `tm launcher → claude → zsh → tm
/// fleet status`, the bound Architect reads as bound. The hook's one-hop
/// lookup over the same table does not reach it, which is the reported bug.
#[test]
fn this_session_reaches_claude_through_the_bash_shell() {
    use crate::commands::pm_guard_architect_reason::NotArchitect;
    use crate::commands::pm_guard_trust_anchor::HookEnv;
    use crate::commands::pm_guard_trust_anchor::tests as anchor;
    use trusty_mpm::core::architect_launch::LaunchRefusal;

    let fx = anchor::fixture();
    let granted = || anchor::allowlist(&fx);
    // tm fleet status 100 → zsh 90 → Architect claude 60 → tm launcher 50.
    let rows = vec![
        (100, Some(90), false),
        (90, Some(anchor::ARCHITECT.pid), false),
        (anchor::ARCHITECT.pid, Some(50), true),
        (50, Some(1), false),
    ];
    let from_bash = |claude| HookEnv {
        project_dir: None,
        claude,
        ..anchor::architect_env(&fx)
    };
    let bound =
        status::this_session_check(&fx.project, from_bash(status_table(rows.clone())), granted);
    assert!(bound.ok, "{}", bound.detail);
    let one_hop = status::this_session_check(&fx.project, from_bash(anchor::table(rows)), granted);
    let why = NotArchitect::Launch(LaunchRefusal::NoClaudeAncestor).to_string();
    assert!(!one_hop.ok);
    assert!(one_hop.detail.ends_with(&why), "{}", one_hop.detail);
}

/// #8938: the status walk stops at the FIRST `claude`, gives up after three
/// hops, and never steps past a process it cannot identify — so a session
/// nested under the Architect is never read as the Architect.
#[test]
fn a_nested_claude_under_the_architect_is_not_this_session() {
    use crate::commands::pm_guard_architect_reason::NotArchitect;
    use crate::commands::pm_guard_trust_anchor::HookEnv;
    use crate::commands::pm_guard_trust_anchor::tests as anchor;
    use trusty_mpm::core::architect_launch::LaunchRefusal;

    let fx = anchor::fixture();
    let granted = || anchor::allowlist(&fx);
    let architect = anchor::ARCHITECT.pid;
    let check = |rows| {
        let env = HookEnv {
            project_dir: None,
            claude: status_table(rows),
            ..anchor::architect_env(&fx)
        };
        status::this_session_check(&fx.project, env, granted)
    };
    let cases = [
        // 100 → zsh 95 → nested claude 80 → zsh 70 → Architect claude: the
        // nested claude is found first, and tm launched no record for it.
        (
            vec![
                (100, Some(95), false),
                (95, Some(80), false),
                (80, Some(70), true),
                (70, Some(architect), false),
                (architect, Some(1), true),
            ],
            LaunchRefusal::NoLaunchRecord,
        ),
        // A nested runtime not named claude: 100 → zsh 95 → node 80 → zsh 70
        // → Architect claude is four hops up, past the limit.
        (
            vec![
                (100, Some(95), false),
                (95, Some(80), false),
                (80, Some(70), false),
                (70, Some(architect), false),
                (architect, Some(1), true),
            ],
            LaunchRefusal::NoClaudeAncestor,
        ),
        // 100 → 90, which the table cannot identify → Architect claude.
        (
            vec![(100, Some(90), false), (architect, Some(1), true)],
            LaunchRefusal::ProcessLookup,
        ),
    ];
    for (rows, refusal) in cases {
        let got = check(rows);
        let why = NotArchitect::Launch(refusal).to_string();
        assert!(!got.ok, "{}", got.detail);
        assert!(got.detail.ends_with(&why), "{}", got.detail);
    }
}

// --- #8436 P4: the seeded fleet files and the poller start ---

/// The pre-#9997 records directory: never seeded, only warned about.
const STALE_DIR: &str = "records";

/// #9997: every template `tm fleet init` seeds under `local/`.
const LOCAL_TEMPLATES: &[&str] = &[
    "local/state.md",
    "local/actions.md",
    "local/goals.md",
    "local/user.md",
    "local/instance.md",
    "local/projects/_template.md",
    "local/owner/README.md",
    "local/owner/business.md",
    "local/owner/process.md",
    "local/owner/decision-making.md",
    "local/owner/risk-tolerance.md",
    "local/owner/detail-and-communication.md",
    "local/owner/contradictions.md",
    "local/owner/dream-log.md",
    "local/mandates/README.md",
    "local/mandates/example.toml",
    // See #10161: the instance config seed.
    "local/architect.json",
];

/// The checkout's Architect seed, read from content once (#9132).
fn seed() -> &'static ArchitectSeed {
    static SEED: std::sync::OnceLock<ArchitectSeed> = std::sync::OnceLock::new();
    SEED.get_or_init(|| {
        let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
        ArchitectSeed::resolve_in(&root, None, None, Fetch::Never).expect("the checkout seed")
    })
}

/// Every file `tm fleet init` seeds from the checkout's content.
fn seeded_files() -> Vec<super::seed::Seeded<'static>> {
    super::seed::files(seed())
}

/// The two Architect-only skills, as shipped.
fn ported_skills() -> Vec<super::seed::Seeded<'static>> {
    seeded_files()
        .into_iter()
        .filter(|f| f.dest.starts_with(".claude/skills/"))
        .collect()
}

#[test]
fn a_first_run_seeds_the_architect_project() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    let dir = fx.dir();
    for file in seeded_files() {
        let path = dir.join(&file.dest);
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
    for skill in SHIPPED {
        assert!(
            dir.join(".claude/skills")
                .join(skill.stem)
                .join("SKILL.md")
                .is_file(),
            "{}",
            skill.stem
        );
    }
    // #9997: the `local/` layout, every template non-empty, none of the old one.
    for rel in LOCAL_TEMPLATES {
        let body = std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert!(!body.trim().is_empty(), "{rel} was seeded empty");
    }
    assert!(dir.join("local/projects").is_dir());
    assert!(
        !dir.join(STALE_DIR).exists(),
        "a fresh init created the stale layout"
    );
    let claude_md = std::fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(
        !claude_md.contains("TRUSTY-MPM:"),
        "an override marker was seeded"
    );
    assert!(!claude_md.contains("TRUSTY_MPM_PM_UNRESTRICTED"));
    assert!(claude_md.contains("local/state.md"), "{claude_md}");
    for file in seeded_files() {
        assert!(
            !file.contents.contains(&format!("{STALE_DIR}/")),
            "{} still names the stale layout",
            file.dest
        );
    }
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

/// #9132: with no content to read the seed from, `tm fleet init` refuses,
/// names the remedy, and writes nothing: no project dir, no user config.
#[test]
fn a_seed_that_does_not_resolve_writes_nothing() {
    let fx = Fixture::new();
    let cache = tempfile::tempdir().unwrap();
    let err = init_with_seed(&fx.dir(), fx.home(), false, NO_TMUX, None, |dir| {
        ArchitectSeed::resolve_in(dir, None, Some(cache.path()), Fetch::Never)
    })
    .expect_err("no content serves the seed");
    let text = format!("{err:#}");
    assert!(text.contains("writes nothing"), "{text}");
    assert!(text.contains("tm content"), "{text}");
    assert!(!fx.dir().exists(), "the project directory was created");
    assert!(!fx.config_path().exists(), "the user config was written");
}

/// #8891: a sibling the poller loads by path must be seeded beside it, or the
/// deployed poller fails at import.
#[test]
fn every_script_a_seeded_script_loads_by_path_is_seeded() {
    let files = seeded_files();
    let seeded: Vec<&str> = files.iter().map(|f| f.dest.as_str()).collect();
    let mut checked = 0;
    for file in files.iter().filter(|f| f.dest.ends_with(".py")) {
        for call in file.contents.split("_load_by_path(").skip(1) {
            let args = call.split(')').next().unwrap_or_default();
            let Some(arg) = args.split(',').nth(1).map(str::trim) else {
                continue;
            };
            // The `def _load_by_path(name, filename)` line passes no literal.
            let Some(name) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) else {
                continue;
            };
            let dest = format!("scripts/{name}");
            assert!(
                seeded.contains(&dest.as_str()),
                "{} loads {dest}, which tm fleet init does not seed",
                file.dest
            );
            checked += 1;
        }
    }
    assert!(checked >= 4, "found only {checked} load-by-path calls");
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
    std::fs::remove_file(dir.join("local/actions.md")).unwrap();
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
            .contains(&Step::Changed("wrote local/actions.md".to_owned()))
    );
    assert!(!report.failed(), "{}", report.render());
    let again = fx.init().unwrap();
    assert!(!again.changed(), "{}", again.render());
}

/// Acceptance 2, and #9997 AC1: every Architect-relative path and script the
/// shipped manual and each seeded skill name exists in a freshly initialised
/// project. `inbox/` is excluded: it is the poller's runtime directory, and
/// `events.jsonl` is absent until #8392. `scripts/check_architect_paths.sh`
/// is the CI form of this check, with fenced blocks and command forms too.
#[test]
fn every_path_a_ported_skill_names_exists_after_init() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let manual = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("content/instructions/architect/architect.md");
    let mut texts: Vec<(String, String)> = ported_skills()
        .into_iter()
        .map(|s| (s.dest, s.contents.to_owned()))
        .collect();
    texts.push((
        "architect.md".to_owned(),
        std::fs::read_to_string(&manual).expect("the shipped manual"),
    ));
    let mut total = 0;
    for (name, contents) in &texts {
        let mut checked = 0;
        for raw in contents.split(|c: char| c.is_whitespace() || "`'\"(),".contains(c)) {
            let word = raw.trim_end_matches(['.', ':', ';']);
            // #9997: the Architect's records live in `local/`; its docs in `docs/`.
            let architect_path = ["scripts/", "local/", ".claude/", "docs/", "templates/"]
                .iter()
                .any(|p| word.starts_with(p))
                || word == "CLAUDE.md";
            if !architect_path {
                continue;
            }
            // `local/projects/<project>.md` and `local/projects/*.md` name a
            // directory of files; the directory must exist.
            let path = match word.find(['<', '*']) {
                Some(i) => dir.join(&word[..i]),
                None => dir.join(word),
            };
            assert!(path.exists(), "{name}: `{word}` is missing after init");
            checked += 1;
        }
        let floor = if name.contains("/tm-") || name == "architect.md" {
            3
        } else {
            0
        };
        assert!(checked >= floor, "{name}: only {checked} paths found");
        total += checked;
    }
    assert!(
        total >= 40,
        "only {total} paths found across {} texts",
        texts.len()
    );
}

/// #9997 AC3: the relay scripts and the voice guide the manual points at are
/// seeded, the two shell scripts executable, and the directories the skills
/// write into exist.
#[test]
fn the_relay_scripts_and_the_voice_guide_are_seeded() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    for (rel, executable) in [
        ("scripts/relay.sh", true),
        ("scripts/posted.sh", true),
        ("scripts/relay-marker.py", false),
        ("docs/architect/voice.md", false),
    ] {
        let meta = std::fs::metadata(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(meta.permissions().mode() & 0o111 != 0, executable, "{rel}");
    }
    for rel in ["local/users", "local/briefs", "local/improvements"] {
        assert!(dir.join(rel).is_dir(), "{rel} is not created");
    }
}

/// #10161: `tm fleet init` seeds the config loader beside the scripts and the
/// `local/architect.json` seed, which parses and carries no owner values.
#[test]
fn the_architect_config_loader_and_seed_are_seeded() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let loader = std::fs::read_to_string(dir.join("scripts/architect_config.py"))
        .expect("scripts/architect_config.py is seeded");
    assert!(loader.contains("def load("), "not the config loader");
    let meta = std::fs::metadata(dir.join("scripts/architect_config.py")).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o111,
        0,
        "a module, not a command"
    );
    let text = std::fs::read_to_string(dir.join("local/architect.json"))
        .expect("local/architect.json is seeded");
    let seed: serde_json::Value = serde_json::from_str(&text).expect("the seed is JSON");
    assert_eq!(seed["owner_name"], "", "{text}");
    assert_eq!(seed["memory_palace"], "", "{text}");
    assert_eq!(seed["dashboard_port"], 7890, "{text}");
    assert_eq!(seed["repos"], serde_json::json!({}), "{text}");
    assert_eq!(seed["caps"], serde_json::json!({}), "{text}");
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
    // #9997: one context bar, in tokens, set in `CLAUDE.md`.
    assert!(
        text("tm-context-refresh").contains("the bar (250k tokens by default; set in `CLAUDE.md`)")
    );
}

/// #8878 R1 critic MEDIUM: the seeded instructions derive the poller's
/// session from the Architect's own, so a `--session` Architect is told its
/// real poller name; only the scripts' env defaults may name the default.
#[test]
fn the_seeded_instructions_never_hard_code_the_poller_session() {
    let seeded = |dest: &str| {
        seeded_files()
            .into_iter()
            .find(|f| f.dest == dest)
            .unwrap_or_else(|| panic!("{dest} is not seeded"))
            .contents
    };
    for dest in ["CLAUDE.md", ".claude/skills/tm-fleet-check/SKILL.md"] {
        let text = seeded(dest);
        assert!(!text.contains("tm-architect-poll"), "{dest}");
        assert!(
            text.contains("-poll`") || text.contains("-poll\""),
            "{dest}"
        );
    }
    assert!(
        seeded(".claude/skills/tm-fleet-check/SKILL.md")
            .contains(r#"has-session -t "=$(tmux display-message -p '#S')-poll""#)
    );
}

/// The fail-closed arm: a start script that fails is an error naming its
/// exit status and output, never a pass.
#[test]
fn a_failing_start_script_is_an_error_with_its_cause() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join(super::poller::START_SCRIPT);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "echo 'no tmux here' >&2\nexit 7\n").unwrap();
    let names = SessionNames::default_names();
    let err =
        super::poller::start(dir.path(), NO_TMUX, &names).expect_err("a failing script must fail");
    let text = format!("{err:#}");
    assert!(
        text.contains("no tmux here") && text.contains('7'),
        "{text}"
    );
}

/// #8436 P4 fix: a live `tm-architect` on this test process's tmux server
/// (the #6542 relocated one, never the operator's) reaches no fixture: `init`
/// and `status` read the stubbed probe, not tmux.
#[test]
fn a_live_architect_on_the_test_server_does_not_reach_a_fixture() {
    use crate::test_support::tmux_session::ScratchTmuxSession;
    let tmux_bin = trusty_mpm::core::tmux::resolve_tmux_binary_or_bare();
    assert!(
        ScratchTmuxSession::tmux_available(&tmux_bin),
        "tmux is required"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let _live = ScratchTmuxSession::spawn_in(
        &tmux_bin,
        ARCHITECT_SESSION,
        Some(elsewhere.path()),
        "sleep 300",
    );
    let fx = Fixture::new();
    let report = fx.init().unwrap_or_else(|e| panic!("{e:#}"));
    assert!(report.changed(), "{}", report.render());
    let status = fx.status();
    let session = status.checks.iter().find(|c| c.name == "session").unwrap();
    assert!(
        session.detail.ends_with("is not running"),
        "{}",
        status.render()
    );
}

#[test]
fn a_failed_step_fails_the_report() {
    let report = InitReport {
        steps: vec![
            Step::Changed("wrote CLAUDE.md".to_owned()),
            Step::Failed("poller start: boom".to_owned()),
        ],
        unbound: false,
        registration: None,
    };
    assert!(report.failed());
    let text = report.render();
    assert!(text.contains("FAILED     poller start: boom"), "{text}");
    assert!(!text.contains("Architect set up"), "{text}");
}

/// #8436 P4 fix, item 8: a started but unbound Architect exits 0, and the
/// summary says it is not bound instead of only "Architect set up".
#[test]
fn an_unbound_architect_is_named_in_the_summary() {
    let mut report = InitReport {
        steps: vec![Step::Changed(
            "started tmux session tm-architect".to_owned(),
        )],
        unbound: true,
        registration: None,
    };
    assert!(!report.failed());
    let text = report.render();
    assert!(
        text.contains(
            "Architect set up (NOT bound: anchor writes will be denied; see the warning above)"
        ),
        "{text}"
    );
    report.unbound = false;
    assert!(!report.render().contains("NOT bound"));
}

/// The fixed Architect directory the poller stubs below report.
const STUB_DIR: &str = "/fleet-stub/architect";

/// #8436 P4 fix, item 1: a dead pane in this directory, a tmux answer that
/// cannot be read, and a poller that dies after a clean start are each a
/// FAILED step, never "running".
#[test]
fn a_dead_or_unreadable_poller_pane_is_a_failed_step() {
    let dead = Probe {
        pane: |_| PaneState::Dead(PathBuf::from(STUB_DIR)),
        ..NO_TMUX
    };
    let unknown = Probe {
        pane: |_| PaneState::Unknown("server exited".to_owned()),
        ..NO_TMUX
    };
    let live = Probe {
        pane: |_| PaneState::Live(PathBuf::from(STUB_DIR)),
        ..NO_TMUX
    };
    let dir = Path::new(STUB_DIR);
    let names = SessionNames::default_names();
    let text = |step: Step| match step {
        Step::Failed(text) => text,
        other => panic!("expected a FAILED step, got {other:?}"),
    };
    assert!(text(super::poller::step(dir, true, dead, &names)).contains("pane is dead"));
    assert!(text(super::poller::step(dir, true, unknown, &names)).contains("server exited"));
    assert!(matches!(
        super::poller::step(dir, true, live, &names),
        Step::Unchanged(_)
    ));

    // The start arm: the script exits 0, then the pane reads dead.
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join(super::poller::START_SCRIPT);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "exit 0\n").unwrap();
    for (probe, cause) in [(dead, "pane is dead"), (unknown, "server exited")] {
        let err = super::poller::start(scratch.path(), probe, &names).expect_err(cause);
        assert!(format!("{err:#}").contains(cause), "{err:#}");
    }
}

/// #8436 P4 fix, item 4: the start script never sees the caller's
/// `TMUX_SOCKET`, so it and tm's own tmux calls address one server.
#[test]
fn the_start_command_drops_tmux_socket() {
    let cmd = super::poller::start_command(Path::new(STUB_DIR), &SessionNames::default_names());
    let removed = cmd
        .get_envs()
        .any(|(key, value)| key == "TMUX_SOCKET" && value.is_none());
    assert!(removed, "TMUX_SOCKET is not removed");
}

/// #9822: both ported skills classify the input box with the Rust verb, not
/// `input-state.py`. The poller itself still runs `fleet-poll.py`.
#[test]
fn the_seeded_skills_run_tm_architect_input_state_not_python() {
    let seeded = |dest: &str| {
        seeded_files()
            .into_iter()
            .find(|f| f.dest == dest)
            .unwrap()
            .contents
    };
    for dest in [
        ".claude/skills/tm-fleet-check/SKILL.md",
        ".claude/skills/tm-context-refresh/SKILL.md",
    ] {
        let text = seeded(dest);
        assert!(!text.contains("python3"), "{dest} still runs python3");
        assert!(
            !text.contains("input-state.py"),
            "{dest} still names input-state.py"
        );
        assert!(
            text.contains("tm architect input-state '=<session>:'"),
            "{dest} does not classify the box with tm architect input-state"
        );
    }
}

/// The `#{pane_dead} #{session_path}` answers `pane_state` reads.
#[test]
fn a_pane_answer_parses_to_its_state() {
    use super::launch::parse_pane;
    assert_eq!(parse_pane(""), PaneState::Absent);
    assert_eq!(parse_pane("\n"), PaneState::Absent);
    assert_eq!(
        parse_pane("0 /fleet-stub/a b\n"),
        PaneState::Live(PathBuf::from("/fleet-stub/a b"))
    );
    assert_eq!(
        parse_pane("1 /fleet-stub/a\n"),
        PaneState::Dead(PathBuf::from("/fleet-stub/a"))
    );
    for bad in ["/fleet-stub/a", "0 ", "2 /fleet-stub/a"] {
        assert!(matches!(parse_pane(bad), PaneState::Unknown(_)), "{bad:?}");
    }
}

/// #8436 P4 fix, item 7: every Architect-only skill asset is seeded to
/// `.claude/skills/`, and every seeded skill has an asset. #9132: the assets
/// are content files under `instructions/architect/skills/`.
#[test]
fn every_architect_skill_asset_is_seeded() {
    let assets = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("content/instructions/architect/skills");
    let mut on_disk: Vec<String> = std::fs::read_dir(&assets)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut seeded: Vec<String> = ported_skills()
        .iter()
        .map(|f| {
            let name = f
                .dest
                .strip_prefix(".claude/skills/")
                .and_then(|rest| rest.strip_suffix("/SKILL.md"))
                .unwrap_or_else(|| panic!("{} is not a SKILL.md", f.dest));
            format!("{name}.md")
        })
        .collect();
    seeded.sort();
    assert_eq!(on_disk, seeded);
    for skill in ported_skills() {
        let name = skill.dest.split('/').nth(2).unwrap();
        let asset = std::fs::read_to_string(assets.join(format!("{name}.md"))).unwrap();
        assert_eq!(asset, skill.contents, "{}", skill.dest);
    }
}

/// #8981: the Architect line selects its conversation and ends in a bare
/// `--remote-control`, for a resume and for a fresh start alike.
#[test]
fn the_architect_line_resumes_and_enables_remote_control() {
    use trusty_mpm::core::architect_conversation::ConversationStart;
    let id = "3f2b8c1e-5d4a-4b6f-9e2d-1a7c0b9e8f61".to_owned();
    assert_eq!(
        launch::architect_args(&ConversationStart::Resume(id.clone())),
        ["--resume", id.as_str(), "--remote-control"]
    );
    let fresh = ConversationStart::Fresh {
        id: id.clone(),
        reason: Some("corrupt".to_owned()),
    };
    assert_eq!(
        launch::architect_args(&fresh),
        ["--session-id", id.as_str(), "--remote-control"]
    );
}

/// #8981 critic MEDIUM: only a proven `Absent` pane fails a resume; a pane
/// that cannot be read twice is `Unknown`, and one bad read is asked again.
#[test]
fn a_resume_fails_only_on_a_proven_absent_claude() {
    use launch::{RESUME_SETTLE, ResumeCheck, resume_check};
    use trusty_mpm::core::process::PaneClaude::{Absent, Present, Unknown};
    let bound = Ok(trusty_mpm::core::twin_identity::ArmingRecord {
        pid: 1,
        start_time: 0,
        project_dir: PathBuf::from("/arch"),
        armed_at: String::new(),
    });
    let unbound = Err("no `claude` process appeared".to_owned());
    let exited = || {
        let secs = RESUME_SETTLE.as_secs();
        ResumeCheck::Failed(format!("`claude` exited within {secs} s of starting"))
    };
    let cases = [
        (
            "unbound, gone",
            &unbound,
            vec![Absent],
            ResumeCheck::Failed("no `claude` process appeared".to_owned()),
        ),
        (
            "unbound, unreadable twice",
            &unbound,
            vec![Unknown, Unknown],
            ResumeCheck::Unknown,
        ),
        (
            "unbound, then gone",
            &unbound,
            vec![Unknown, Absent],
            ResumeCheck::Failed("no `claude` process appeared".to_owned()),
        ),
        (
            "unbound, then up",
            &unbound,
            vec![Present, Present],
            ResumeCheck::Running,
        ),
        ("bound, up", &bound, vec![Present], ResumeCheck::Running),
        ("bound, exited", &bound, vec![Absent], exited()),
        (
            "bound, unreadable twice",
            &bound,
            vec![Unknown, Unknown],
            ResumeCheck::Unknown,
        ),
        (
            "bound, then up",
            &bound,
            vec![Unknown, Present],
            ResumeCheck::Running,
        ),
    ];
    for (name, binding, answers, want) in cases {
        let mut answers = answers.into_iter();
        let mut slept = Vec::new();
        let got = resume_check(
            binding,
            || answers.next().expect("one probe too many"),
            |d| slept.push(d),
        );
        assert_eq!(got, want, "{name}");
        assert_eq!(
            answers.next(),
            None,
            "{name}: a probe answer was left unread"
        );
        let settled = !matches!(
            (binding, &got),
            (Err(_), ResumeCheck::Failed(_) | ResumeCheck::Unknown)
        );
        assert_eq!(slept.contains(&RESUME_SETTLE), settled, "{name}: {slept:?}");
    }
}

/// #9540: the `skills` row is not ok when a shipped skill is missing or kept
/// as project-custom, names that skill, and never moves `complete`.
#[test]
fn the_skills_row_names_a_missing_or_custom_skill() {
    let fx = Fixture::new();
    let fresh = fx.status();
    assert!(!fresh.skills.ok, "{}", fresh.render());
    assert!(
        fresh.skills.detail.contains("tm-fleet-check: missing"),
        "{}",
        fresh.render()
    );

    fx.init().unwrap();
    assert!(fx.status().skills.ok, "{}", fx.status().render());

    let edited = fx.dir().join(".claude/skills/tm-fleet-check/SKILL.md");
    std::fs::write(&edited, "THE ARCHITECT'S OWN EDIT\n").unwrap();
    let report = fx.status();
    assert!(!report.skills.ok, "{}", report.render());
    assert!(
        report
            .render()
            .contains("WARN     skills       tm-fleet-check: project-custom"),
        "{}",
        report.render()
    );
    assert!(
        !report.skills.detail.contains("tm-context-refresh"),
        "{}",
        report.render()
    );
}

/// #9997 (decisions 1a and 2): an Architect seeded before `local/` keeps its
/// `records/`. `tm fleet status` names the stale directory, says the refreshed
/// skills now name `local/`, and moves nothing.
#[test]
fn status_warns_about_a_stale_records_dir_and_moves_nothing() {
    let fx = Fixture::new();
    fx.init().unwrap();
    assert!(fx.status().skills.ok, "{}", fx.status().render());
    let dir = fx.dir();
    let stale = dir.join(STALE_DIR);
    std::fs::create_dir_all(stale.join("projects")).unwrap();
    std::fs::write(stale.join("state.md"), "poll_count: 41\n").unwrap();
    let kept = dir.join("local/projects/kept.md");
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    std::fs::write(&kept, "kept\n").unwrap();

    let report = fx.status();

    assert!(!report.skills.ok, "{}", report.render());
    let detail = &report.skills.detail;
    for needle in [
        format!("`{STALE_DIR}/`"),
        "now name `local/`".to_owned(),
        "moves nothing".to_owned(),
    ] {
        assert!(detail.contains(&needle), "missing `{needle}`: {detail}");
    }
    assert!(report.render().contains("WARN"), "{}", report.render());
    assert_eq!(
        std::fs::read_to_string(stale.join("state.md")).unwrap(),
        "poll_count: 41\n"
    );
    assert!(stale.join("projects").is_dir());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "kept\n");
}

/// #9997: a re-init on a records-era Architect (a `records/` directory exists)
/// writes no `local/` template row and creates no `local/` directory.
#[test]
fn a_reinit_beside_records_writes_no_local_templates() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    std::fs::remove_dir_all(dir.join("local")).unwrap();
    std::fs::create_dir_all(dir.join(STALE_DIR)).unwrap();

    let report = fx.init().unwrap();

    assert!(
        !dir.join("local").exists(),
        "local/ was written beside records/:\n{}",
        report.render()
    );
    assert!(!report.failed(), "{}", report.render());
    assert!(dir.join("scripts/fleet-poll.py").is_file());
}

/// #9997: the init report warns, naming the path, when `records/` and a stale
/// `local/` sit side by side; it deletes nothing. No `local/`: no warning.
#[test]
fn a_reinit_warns_about_a_stale_local_beside_records() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    std::fs::create_dir_all(dir.join(STALE_DIR)).unwrap();
    let local = dir.join("local");

    let rendered = fx.init().unwrap().render();

    let needle = format!("stale `local/` directory at {}", local.display());
    assert!(
        rendered.contains(&needle),
        "missing `{needle}`:\n{rendered}"
    );
    assert!(local.join("state.md").is_file(), "local/ was touched");

    std::fs::remove_dir_all(&local).unwrap();
    let rendered = fx.init().unwrap().render();
    assert!(!rendered.contains("stale `local/`"), "{rendered}");
}

/// #9997 Fail-Open check: a records path that cannot be checked is a warning,
/// never a silent pass; an absent one or a plain file is no warning.
#[test]
fn an_uncheckable_stale_dir_is_a_warning() {
    use super::status::stale_records_warning;
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(stale_records_warning(scratch.path()), None);
    std::fs::write(scratch.path().join(STALE_DIR), "a file\n").unwrap();
    assert_eq!(stale_records_warning(scratch.path()), None);
    // A file where the Architect directory should be: the lookup errors.
    let not_a_dir = scratch.path().join("plain-file");
    std::fs::write(&not_a_dir, "x").unwrap();
    let warning = stale_records_warning(&not_a_dir).expect("an error is a warning");
    assert!(warning.starts_with("cannot check"), "{warning}");
}

/// #9997: every template asset is seeded byte-equal, so a template added
/// under `src/assets/architect/templates/` without a `template!` row fails.
#[test]
fn every_template_asset_is_seeded() {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("crates/trusty-mpm/src/assets/architect/templates");
    let files = seeded_files();
    let mut stack = vec![root.clone()];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            // Finder metadata, skipped like the drift gate skips it.
            if path.file_name().is_some_and(|n| n == ".DS_Store") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let dest = if rel == "gitignore" {
                ".gitignore".to_owned()
            } else {
                rel
            };
            let seeded = files
                .iter()
                .find(|f| f.dest == dest)
                .unwrap_or_else(|| panic!("{dest}: a template asset tm fleet init does not seed"));
            assert_eq!(
                seeded.contents,
                std::fs::read_to_string(&path).unwrap(),
                "{dest}"
            );
            checked += 1;
        }
    }
    assert_eq!(
        checked,
        LOCAL_TEMPLATES.len() + 1,
        "templates found: {checked}"
    );
}

/// #10161: every script asset is seeded byte-equal at `scripts/<name>`, so a
/// script added under `src/assets/architect/scripts/` without an `embedded!`
/// row fails, and every seeded script row has an asset.
#[test]
fn every_script_asset_is_seeded() {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("crates/trusty-mpm/src/assets/architect/scripts");
    let files = seeded_files();
    let mut checked = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // Finder metadata and a local `__pycache__/`, skipped like the drift gate skips them.
        if path.is_dir() || name == ".DS_Store" {
            continue;
        }
        let dest = format!("scripts/{name}");
        let seeded = files
            .iter()
            .find(|f| f.dest == dest)
            .unwrap_or_else(|| panic!("{dest}: a script asset tm fleet init does not seed"));
        assert_eq!(
            seeded.contents,
            std::fs::read_to_string(&path).unwrap(),
            "{dest}"
        );
        checked += 1;
    }
    let rows = files
        .iter()
        .filter(|f| f.dest.starts_with("scripts/"))
        .count();
    assert_eq!(checked, rows, "script assets {checked}, seeded rows {rows}");
}

/// #10161: every dashboard schema asset is seeded byte-equal at
/// `schemas/dashboard/<name>`, where `dashboard_schema.py` reads it, and every
/// seeded schema row has an asset.
#[test]
fn every_schema_asset_is_seeded() {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("crates/trusty-mpm/src/assets/architect/schemas/dashboard");
    let files = seeded_files();
    let mut checked = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() || name == ".DS_Store" {
            continue;
        }
        let dest = format!("schemas/dashboard/{name}");
        let seeded = files
            .iter()
            .find(|f| f.dest == dest)
            .unwrap_or_else(|| panic!("{dest}: a schema asset tm fleet init does not seed"));
        assert!(!seeded.executable, "{dest}: a schema is not executable");
        assert_eq!(
            seeded.contents,
            std::fs::read_to_string(&path).unwrap(),
            "{dest}"
        );
        checked += 1;
    }
    let rows = files
        .iter()
        .filter(|f| f.dest.starts_with("schemas/"))
        .count();
    assert!(checked > 0, "no schema assets under {}", root.display());
    assert_eq!(checked, rows, "schema assets {checked}, seeded rows {rows}");
}

/// #10161: every dashboard page asset (the page, the Live view and the reader
/// libraries) is seeded byte-equal at `templates/dashboard/<rel>`, where
/// `dashboard-serve.py` reads it, and every seeded page row has an asset.
#[test]
fn every_dashboard_asset_is_seeded() {
    let root = trusty_common::test_harness::test_repo_root()
        .expect("resolve the checkout")
        .join("crates/trusty-mpm/src/assets/architect/dashboard");
    let files = seeded_files();
    let mut stack = vec![root.clone()];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|n| n == ".DS_Store") {
                continue;
            }
            let rel = path.strip_prefix(&root).unwrap().to_string_lossy();
            let dest = format!("templates/dashboard/{rel}");
            let seeded = files
                .iter()
                .find(|f| f.dest == dest)
                .unwrap_or_else(|| panic!("{dest}: a dashboard asset tm fleet init does not seed"));
            assert!(!seeded.executable, "{dest}: a page file is not executable");
            assert_eq!(
                seeded.contents,
                std::fs::read_to_string(&path).unwrap(),
                "{dest}"
            );
            checked += 1;
        }
    }
    let rows = files
        .iter()
        .filter(|f| f.dest.starts_with("templates/dashboard/"))
        .count();
    assert!(checked > 0, "no dashboard assets under {}", root.display());
    assert_eq!(
        checked, rows,
        "dashboard assets {checked}, seeded rows {rows}"
    );
}

/// #9997 AC5: a seed carrying one more skill writes it with no change to
/// `seed.rs`; the table is built from the seed's skills.
#[test]
fn a_seed_with_one_more_skill_seeds_it() {
    use trusty_mpm::core::project_skill_tier::architect_skills::ArchitectSkill;
    let mut seed = seed().clone();
    seed.skills.push(ArchitectSkill {
        stem: "tm-probe-extra",
        contents: "---\nname: tm-probe-extra\n---\n".to_owned(),
        previous: &[],
    });
    let files = super::seed::files(&seed);
    let row = files
        .iter()
        .find(|f| f.dest == ".claude/skills/tm-probe-extra/SKILL.md")
        .expect("the extra skill is seeded");
    assert_eq!(row.contents, "---\nname: tm-probe-extra\n---\n");
    assert!(!row.executable);
}

/// Copy the tree at `from` to `to`, files and directories only.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// #10161 D3c: `tm fleet init` writes the five agents byte-equal into the
/// Architect project's `.claude/agents/` and into no PM agent tier, and they
/// are not plain seed rows (they follow the skills' deploy rule).
#[test]
fn the_architect_agents_reach_only_the_architect_project() {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::project_skill_tier::architect_agents::{
        AGENT_RECORD_FILE, SHIPPED_AGENTS,
    };
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    let dir = fx.dir();
    assert!(!report.failed(), "{}", report.render());
    assert_eq!(seed().agents.len(), SHIPPED_AGENTS.len());
    for agent in &seed().agents {
        let rel = format!(".claude/agents/{}.md", agent.stem);
        assert_eq!(
            std::fs::read_to_string(dir.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}")),
            agent.contents,
            "{rel}"
        );
        assert!(
            report.steps.contains(&Step::Changed(format!("wrote {rel}"))),
            "{rel} not reported:\n{}",
            report.render()
        );
        assert!(!dir.join(".claude/skills").join(agent.stem).exists());
    }
    assert!(dir.join(".claude/agents").join(AGENT_RECORD_FILE).is_file());
    let fw = FrameworkPaths::under(fx.home());
    for tier in [fw.agent_deploy_dir(), fw.claude_agents_dir()] {
        for agent in SHIPPED_AGENTS {
            let stray = tier.join(format!("{}.md", agent.name));
            assert!(!stray.exists(), "{} reached a PM tier", stray.display());
        }
    }
    assert!(
        !seeded_files()
            .iter()
            .any(|f| f.dest.starts_with(".claude/agents/")),
        "an agent became a write-when-absent seed row"
    );
}

/// #10161 D3c: a re-run keeps an owner's edit, rewrites a deleted agent,
/// leaves the rest unchanged, and a third run changes nothing.
#[test]
fn a_second_init_follows_the_shipped_history_and_keeps_an_edit() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let edited = dir.join(".claude/agents/drift-guard.md");
    std::fs::write(&edited, "---\nname: drift-guard\n---\nmine\n").unwrap();
    std::fs::remove_file(dir.join(".claude/agents/runner.md")).unwrap();
    let report = fx.init().unwrap();
    assert!(!report.failed(), "{}", report.render());
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "---\nname: drift-guard\n---\nmine\n"
    );
    assert!(
        report.steps.iter().any(|s| matches!(s, Step::Skipped(t)
            if t.starts_with(".claude/agents/drift-guard.md") && t.contains("left as is"))),
        "{}",
        report.render()
    );
    assert!(
        report
            .steps
            .contains(&Step::Changed("wrote .claude/agents/runner.md".to_owned())),
        "{}",
        report.render()
    );
    assert!(
        report
            .steps
            .contains(&Step::Unchanged(".claude/agents/improver.md".to_owned())),
        "{}",
        report.render()
    );
    let again = fx.init().unwrap();
    assert!(!again.changed(), "{}", again.render());
}

/// #10161 D3c: a content source with one blank agent fails the seed load, so
/// `tm fleet init` writes nothing at all: no project, no config, no agent.
#[test]
fn a_blank_agent_source_writes_nothing() {
    let fx = Fixture::new();
    let checkout = tempfile::tempdir().unwrap();
    let repo = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    copy_tree(&repo.join("content"), &checkout.path().join("content"));
    std::fs::create_dir_all(checkout.path().join(".git")).unwrap();
    std::fs::write(checkout.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    let blank = checkout
        .path()
        .join("content/instructions/architect/agents/drift-guard.md");
    std::fs::write(&blank, " \n\n").unwrap();
    let root = checkout.path().to_path_buf();
    let err = init_with_seed(&fx.dir(), fx.home(), false, NO_TMUX, None, |_| {
        ArchitectSeed::resolve_in(&root, None, None, Fetch::Never)
    })
    .expect_err("a blank agent is refused");
    let text = format!("{err:#}");
    assert!(text.contains("writes nothing"), "{text}");
    assert!(text.contains("agents/drift-guard.md"), "{text}");
    assert!(!fx.dir().exists(), "the project directory was created");
    assert!(!fx.config_path().exists(), "the user config was written");
}

/// #10161 D3c, AC1 for agents: every Architect-relative path and every
/// `architect-*` skill a seeded agent names exists after init.
/// `scripts/check_architect_paths.sh` is the CI form, with fenced blocks and
/// command forms too.
#[test]
fn every_path_an_architect_agent_names_exists_after_init() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let mut total = 0;
    for agent in &seed().agents {
        let name = format!(".claude/agents/{}.md", agent.stem);
        for raw in agent
            .contents
            .split(|c: char| c.is_whitespace() || "`'\"(),".contains(c))
        {
            let word = raw.trim_end_matches(['.', ':', ';']);
            let architect_path = ["scripts/", "local/", ".claude/", "docs/", "templates/"]
                .iter()
                .any(|p| word.starts_with(p))
                || word == "CLAUDE.md";
            if architect_path {
                let path = match word.find(['<', '*']) {
                    Some(i) => dir.join(&word[..i]),
                    None => dir.join(word),
                };
                assert!(path.exists(), "{name}: `{word}` is missing after init");
                total += 1;
            }
            let skill = word.trim_end_matches(['.', ':', ';', '-']);
            if skill.starts_with("architect-") && skill.len() > "architect-".len() {
                assert!(
                    dir.join(".claude/skills").join(skill).join("SKILL.md").is_file(),
                    "{name}: skill `{skill}` is missing after init"
                );
            }
        }
    }
    assert!(total >= 10, "only {total} paths found in the agents");
}
