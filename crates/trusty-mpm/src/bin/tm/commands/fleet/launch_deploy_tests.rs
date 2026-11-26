//! Tests for the #11368 launch deploy: atomic writes, the kept copy and its
//! restore, the once-only restarts, `--dry-run` and the inside-Claude-Code arm.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;

use trusty_mpm::core::content_source::Fetch;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::super::Step;
use super::super::launch_backup::{self, ROOT};
use super::super::seed;
use super::super::session_name::SessionNames;
use super::{Ctx, DASHBOARD_SCRIPTS, Mode, POLLER_SCRIPTS, Restarter, run};

/// The poller's entry script.
pub(crate) const POLL: &str = "scripts/fleet-poll.py";
/// The dashboard's entry script.
const DASH: &str = "scripts/dashboard-serve.py";
/// An earlier shipped `fleet-poll.py`, as the injected history lists it.
pub(crate) const OLD_POLL: &[u8] = b"print('an earlier shipped fleet-poll')\n";
/// An earlier shipped `dashboard-serve.py`.
const OLD_DASH: &[u8] = b"print('an earlier shipped dashboard-serve')\n";
/// An earlier shipped copy of the last skill.
pub(crate) const OLD_SKILL: &[u8] = b"# an earlier shipped skill\n";
/// Bytes no release shipped: an owner's edit.
const EDITED: &[u8] = b"print('the owner edited this')\n";
/// The release the tests file kept copies under.
const VERSION: &str = "9.9.9";

/// The checkout's seed, read once.
pub(crate) fn seed() -> &'static ArchitectSeed {
    static SEED: std::sync::OnceLock<ArchitectSeed> = std::sync::OnceLock::new();
    SEED.get_or_init(|| {
        let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
        ArchitectSeed::resolve_in(&root, None, None, Fetch::Never).expect("the checkout seed")
    })
}

/// The last file `seed::files` writes: a skill, after every script.
pub(crate) fn last_dest() -> String {
    seed::files(seed())
        .last()
        .expect("a seeded file")
        .dest
        .clone()
}

/// The shipped bytes of `dest`.
pub(crate) fn shipped(dest: &str) -> Vec<u8> {
    seed::files(seed())
        .into_iter()
        .find(|f| f.dest == dest)
        .expect("a seeded dest")
        .contents
        .as_bytes()
        .to_vec()
}

/// The injected shipped history: the `OLD_*` bytes of their destinations.
pub(crate) fn history(dest: &str, bytes: &[u8]) -> bool {
    (dest == POLL && bytes == OLD_POLL)
        || (dest == DASH && bytes == OLD_DASH)
        || (dest == last_dest() && bytes == OLD_SKILL)
}

/// A poller and dashboard that count their restarts.
#[derive(Default)]
pub(crate) struct FakeHost {
    /// Whether the poller session runs.
    pub(crate) running: bool,
    /// Whether the dashboard agent is loaded.
    pub(crate) loaded: bool,
    /// `restart_poller` fails.
    pub(crate) restart_fails: bool,
    /// Poller restarts so far.
    pub(crate) restarts: Cell<usize>,
    /// Dashboard kickstarts so far.
    pub(crate) kickstarts: Cell<usize>,
}

impl Restarter for FakeHost {
    fn poller_running(&self, _: &Path, _: &SessionNames) -> Result<bool, String> {
        Ok(self.running)
    }
    fn restart_poller(&self, _: &Path, _: &SessionNames) -> anyhow::Result<()> {
        self.restarts.set(self.restarts.get() + 1);
        anyhow::ensure!(!self.restart_fails, "tmux died");
        Ok(())
    }
    fn dashboard_loaded(&self) -> bool {
        self.loaded
    }
    fn kickstart_dashboard(&self) -> anyhow::Result<()> {
        self.kickstarts.set(self.kickstarts.get() + 1);
        Ok(())
    }
}

/// A directory holding the whole bundle at its shipped bytes.
pub(crate) fn seeded_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("scratch dir");
    seed::deploy_with(tmp.path(), seed(), &|_, _| false).expect("seed the bundle");
    tmp
}

/// `run` over `dir` with `host`, the injected history and `version`.
fn deploy(dir: &Path, host: &FakeHost, mode: Mode, version: &str) -> anyhow::Result<Vec<Step>> {
    let ctx = Ctx {
        seed: seed(),
        shipped_before: &history,
        host,
        version,
    };
    run(dir, &SessionNames::default_names(), mode, &ctx)
}

/// A deploy outside Claude Code.
pub(crate) const LIVE: Mode = Mode {
    dry_run: false,
    restart: true,
};

/// Every file below `dir` and its bytes; symlinks by their target.
pub(crate) fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read dir").flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(dir).expect("below").display().to_string();
            let meta = std::fs::symlink_metadata(&path).expect("meta");
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).expect("link");
                out.insert(rel, target.display().to_string().into_bytes());
            } else if meta.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                stack.push(path);
            } else {
                out.insert(rel, std::fs::read(&path).expect("read"));
            }
        }
    }
    out
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

/// #11368 AC 1: a write that fails mid-deploy leaves every file whole — the
/// scripts written before it hold the new bytes, the failed one its old ones,
/// and no staging file is left.
#[cfg(unix)]
#[test]
fn an_interrupted_deploy_leaves_no_half_written_script_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    // The last write fails: `write_atomic` refuses a symlinked target, also as root.
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("SKILL.md");
    std::fs::write(&target, OLD_SKILL).unwrap();
    let last = dir.join(last_dest());
    std::fs::remove_file(&last).unwrap();
    std::os::unix::fs::symlink(&target, &last).unwrap();
    let host = FakeHost {
        running: true,
        ..FakeHost::default()
    };
    let err = deploy(dir, &host, LIVE, VERSION).expect_err("the symlinked write fails");
    assert!(format!("{err:#}").contains("cannot write"), "{err:#}");
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), shipped(POLL));
    assert_eq!(mode(&dir.join(POLL)), 0o755);
    assert_eq!(std::fs::read(&target).unwrap(), OLD_SKILL);
    for dest in POLLER_SCRIPTS.iter().chain(DASHBOARD_SCRIPTS) {
        assert_eq!(
            std::fs::read(dir.join(dest)).unwrap(),
            shipped(dest),
            "{dest}"
        );
    }
    let staged: Vec<String> = snapshot(dir)
        .into_keys()
        .filter(|p| p.ends_with(".tm-tmp"))
        .collect();
    assert!(staged.is_empty(), "{staged:?}");
    assert_eq!(host.restarts.get(), 0, "a failed deploy restarts nothing");
}

/// #11368 Q8: a backup that cannot be written aborts before the first write.
#[test]
fn a_failed_backup_aborts_and_writes_nothing_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    std::fs::create_dir_all(dir.join(".trusty-mpm")).unwrap();
    std::fs::write(dir.join(ROOT), b"a file where the backup root goes").unwrap();
    let before = snapshot(dir);
    let host = FakeHost {
        running: true,
        ..FakeHost::default()
    };
    let err = deploy(dir, &host, LIVE, VERSION).expect_err("the backup fails");
    assert!(format!("{err:#}").contains("backup"), "{err:#}");
    assert_eq!(snapshot(dir), before);
    assert_eq!(host.restarts.get(), 0);
}

/// #11368 AC 2: the previous release's bytes and mode come back from the kept copy.
#[cfg(unix)]
#[test]
fn restore_brings_back_the_previous_release_bytes_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    deploy(dir, &FakeHost::default(), LIVE, VERSION).expect("deploy");
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), shipped(POLL));
    let r = launch_backup::restore(dir, None).expect("restore");
    assert_eq!(
        (r.exit(), r.version.as_str()),
        (0, VERSION),
        "{:?}",
        r.steps
    );
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), OLD_POLL);
    assert_eq!(mode(&dir.join(POLL)), 0o755);
    let again = launch_backup::restore(dir, Some(VERSION)).expect("restore again");
    assert_eq!(again.exit(), 0, "{:?}", again.steps);
    assert!(
        matches!(again.steps.as_slice(), [Step::Unchanged(_)]),
        "{:?}",
        again.steps
    );
    assert!(launch_backup::restore(dir, Some("0.0.1")).is_err());
}

/// #11368 Q1: restore never overwrites a file edited after the deploy.
#[test]
fn restore_never_overwrites_an_edited_file_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    deploy(dir, &FakeHost::default(), LIVE, VERSION).expect("deploy");
    std::fs::write(dir.join(POLL), EDITED).unwrap();
    let r = launch_backup::restore(dir, None).expect("restore");
    assert_eq!(r.exit(), 1, "{:?}", r.steps);
    assert!(
        matches!(r.steps.as_slice(), [Step::Skipped(_)]),
        "{:?}",
        r.steps
    );
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), EDITED);
}

/// #11368 Q1: an edited file is kept, never kept-as-backup, and its shipped
/// copy goes to `<file>.shipped` (#11245).
#[test]
fn an_edited_file_is_never_backed_up_or_overwritten_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), EDITED).unwrap();
    let host = FakeHost {
        running: true,
        ..FakeHost::default()
    };
    deploy(dir, &host, LIVE, VERSION).expect("deploy");
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), EDITED);
    let sibling = dir.join(format!("{POLL}{}", seed::SHIPPED_SUFFIX));
    assert_eq!(std::fs::read(sibling).unwrap(), shipped(POLL));
    assert!(!dir.join(ROOT).exists(), "nothing replaced, nothing kept");
    assert_eq!(host.restarts.get(), 0, "the poller's script did not change");
}

/// #11368 Q4: a deploy that changes no hashed script restarts nothing.
#[test]
fn an_unchanged_deploy_does_not_restart_the_poller_11368() {
    let tmp = seeded_dir();
    let host = FakeHost {
        running: true,
        loaded: true,
        ..FakeHost::default()
    };
    deploy(tmp.path(), &host, LIVE, VERSION).expect("deploy");
    assert_eq!((host.restarts.get(), host.kickstarts.get()), (0, 0));
    assert!(!tmp.path().join(ROOT).exists());
}

/// #11368 AC 3: two changed poller scripts restart the running poller once;
/// a second deploy changes nothing and restarts nothing.
#[test]
fn a_changed_poller_script_restarts_it_exactly_once_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    std::fs::remove_file(dir.join("scripts/fleet_poll_io.py")).unwrap();
    let host = FakeHost {
        running: true,
        loaded: true,
        ..FakeHost::default()
    };
    let steps = deploy(dir, &host, LIVE, VERSION).expect("deploy");
    assert_eq!(
        (host.restarts.get(), host.kickstarts.get()),
        (1, 0),
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|s| matches!(s, Step::Changed(t) if t.contains("once"))),
        "{steps:?}"
    );
    deploy(dir, &host, LIVE, VERSION).expect("deploy again");
    assert_eq!(host.restarts.get(), 1);
}

/// #11368 Q4: a changed dashboard script kickstarts a loaded agent once.
#[test]
fn a_changed_dashboard_script_kickstarts_a_loaded_agent_once_11368() {
    let tmp = seeded_dir();
    std::fs::write(tmp.path().join(DASH), OLD_DASH).unwrap();
    let host = FakeHost {
        running: true,
        loaded: true,
        ..FakeHost::default()
    };
    deploy(tmp.path(), &host, LIVE, VERSION).expect("deploy");
    assert_eq!((host.restarts.get(), host.kickstarts.get()), (0, 1));
}

/// #11368 Q4: a stopped poller or an unloaded dashboard is not started.
#[test]
fn a_stopped_poller_is_not_started_by_a_restart_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    std::fs::write(dir.join(DASH), OLD_DASH).unwrap();
    let host = FakeHost::default();
    let steps = deploy(dir, &host, LIVE, VERSION).expect("deploy");
    assert_eq!((host.restarts.get(), host.kickstarts.get()), (0, 0));
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), shipped(POLL));
    assert!(
        steps
            .iter()
            .any(|s| matches!(s, Step::Unchanged(t) if t.contains("not running"))),
        "{steps:?}"
    );
}

/// #11368 Q7: inside Claude Code the deploy writes files and restarts nothing.
#[test]
fn inside_claude_code_the_deploy_restarts_nothing_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    let host = FakeHost {
        running: true,
        loaded: true,
        ..FakeHost::default()
    };
    let inside = Mode {
        dry_run: false,
        restart: false,
    };
    let steps = deploy(dir, &host, inside, VERSION).expect("deploy");
    assert_eq!((host.restarts.get(), host.kickstarts.get()), (0, 0));
    assert_eq!(std::fs::read(dir.join(POLL)).unwrap(), shipped(POLL));
    assert!(
        steps.iter().any(|s| matches!(s, Step::Skipped(_))),
        "{steps:?}"
    );
}

/// Fail-Open Check: a poller restart that fails is a `Failed` step, never a note.
#[test]
fn a_failed_poller_restart_is_a_failed_step_11368() {
    let tmp = seeded_dir();
    std::fs::write(tmp.path().join(POLL), OLD_POLL).unwrap();
    let host = FakeHost {
        running: true,
        restart_fails: true,
        ..FakeHost::default()
    };
    let steps = deploy(tmp.path(), &host, LIVE, VERSION).expect("deploy");
    assert!(
        steps.iter().any(|s| matches!(s, Step::Failed(_))),
        "{steps:?}"
    );
}

/// #11368 Q7: `--dry-run` names the planned writes and restarts and writes nothing.
#[test]
fn dry_run_writes_nothing_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    std::fs::remove_file(dir.join("scripts/fleet_poll_io.py")).unwrap();
    std::fs::write(dir.join("scripts/datg.py"), EDITED).unwrap();
    let before = snapshot(dir);
    let host = FakeHost {
        running: true,
        loaded: true,
        ..FakeHost::default()
    };
    let dry = Mode {
        dry_run: true,
        restart: true,
    };
    let steps = deploy(dir, &host, dry, VERSION).expect("plan");
    let text: Vec<String> = steps.iter().map(|s| s.render_line()).collect();
    for want in [
        "would replace scripts/fleet-poll.py",
        "would write scripts/fleet_poll_io.py",
        "would keep the edited scripts/datg.py",
        "would restart poller session",
    ] {
        assert!(text.iter().any(|l| l.contains(want)), "{want}: {text:?}");
    }
    assert_eq!(snapshot(dir), before);
    assert_eq!((host.restarts.get(), host.kickstarts.get()), (0, 0));
}

/// #11368 Q3: one release is kept; a relaunch that replaces nothing keeps it,
/// and the next release's backup prunes it.
#[test]
fn a_new_release_prunes_the_older_kept_copy_11368() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    let host = FakeHost::default();
    std::fs::write(dir.join(POLL), OLD_POLL).unwrap();
    deploy(dir, &host, LIVE, "1.0.0").expect("deploy 1.0.0");
    deploy(dir, &host, LIVE, "1.0.0").expect("relaunch 1.0.0");
    assert!(dir.join(ROOT).join("1.0.0/manifest.tsv").is_file());
    std::fs::write(dir.join(DASH), OLD_DASH).unwrap();
    deploy(dir, &host, LIVE, "2.0.0").expect("deploy 2.0.0");
    assert!(dir.join(ROOT).join("2.0.0/manifest.tsv").is_file());
    assert!(
        !dir.join(ROOT).join("1.0.0").exists(),
        "the older release is pruned"
    );
}

/// #11368 Q5: every name in the restart tables is a script the bundle seeds.
#[test]
fn the_script_tables_name_seeded_scripts_11368() {
    let dests: Vec<String> = seed::files(seed()).into_iter().map(|f| f.dest).collect();
    for dest in POLLER_SCRIPTS.iter().chain(DASHBOARD_SCRIPTS) {
        assert!(dests.iter().any(|d| d == dest), "{dest} is not seeded");
    }
}
