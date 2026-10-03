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
            .ends_with(&NotArchitect::NoSupervisorStamp.to_string())
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

/// #8891: a sibling the poller loads by path must be seeded beside it, or the
/// deployed poller fails at import.
#[test]
fn every_script_a_seeded_script_loads_by_path_is_seeded() {
    let seeded: Vec<&str> = super::seed::FILES.iter().map(|f| f.dest).collect();
    let mut checked = 0;
    for file in super::seed::FILES
        .iter()
        .filter(|f| f.dest.ends_with(".py"))
    {
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

/// #8878 R1 critic MEDIUM: the seeded instructions derive the poller's
/// session from the Architect's own, so a `--session` Architect is told its
/// real poller name; only the scripts' env defaults may name the default.
#[test]
fn the_seeded_instructions_never_hard_code_the_poller_session() {
    let seeded = |dest: &str| {
        super::seed::FILES
            .iter()
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
/// `.claude/skills/`, and every seeded skill has an asset.
#[test]
fn every_architect_skill_asset_is_seeded() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/architect/skills");
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
