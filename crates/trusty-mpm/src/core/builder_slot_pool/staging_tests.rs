//! #9239: the bounded seed, the all-slot sweep, and the status listing.
//!
//! Every test roots the pool in a temp dir (#8311).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::super::{SEED_MARKER, SeedKind, SlotPool, SlotPoolError, marker_reads_seeded};
use super::*;
use trusty_common::github_path::GithubPath;

fn pool(root: &Path) -> SlotPool {
    let identity = GithubPath {
        owner: "acme".to_string(),
        repo: "widget".to_string(),
    };
    SlotPool::new(root.to_path_buf(), identity, 8)
}

/// A pid no process can hold: it does not fit a positive `pid_t`.
const DEAD_PID: u32 = 4_294_967_294;

/// The names directly in `dir` that start with `prefix`.
fn names_with(dir: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default()
}

/// Wait, bounded, until `dir` holds no name starting with any of `prefixes`.
fn wait_until_gone(dir: &Path, prefixes: &[&str]) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left: Vec<String> = prefixes.iter().flat_map(|p| names_with(dir, p)).collect();
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A dead-owner tree with one file in it, as a killed seed leaves one.
fn orphan(repo: &Path, name: &str) -> PathBuf {
    let tree = repo.join(name);
    std::fs::create_dir_all(tree.join("debug")).expect("an orphan tree");
    std::fs::write(tree.join("debug/libpartial.rlib"), b"partial").expect("an artifact");
    tree
}

/// #9239: slots 4-7 piled up dead-owner `.seeding` trees because the sweep only
/// looked at the index being seeded. A seed of slot 0 must sweep them all,
/// and a dead deleter's `.evicting` leftover too.
#[test]
fn a_dead_owners_staging_under_another_index_is_swept() {
    let tmp = tempfile::tempdir().expect("temp root");
    let pool = pool(tmp.path());
    let repo = pool.repo_dir();
    let seeding = orphan(&repo, &format!(".slot-5.seeding.{DEAD_PID}.1"));
    let evicting = orphan(&repo, &format!(".slot-6.evicting.{DEAD_PID}.2"));

    pool.seed(0, None).expect("slot 0 seeds");

    assert!(!seeding.exists(), "slot 5's orphan is swept: {seeding:?}");
    assert!(
        !evicting.exists(),
        "slot 6's leftover is swept: {evicting:?}"
    );
    let left = wait_until_gone(&repo, &[".slot-5.", ".slot-6."]);
    assert!(left.is_empty(), "the swept trees are deleted: {left:?}");
}

/// ADR-0045: a live owner's tree is never touched, whatever its index.
#[test]
fn a_live_owners_staging_is_never_swept() {
    let tmp = tempfile::tempdir().expect("temp root");
    let repo = tmp.path().join("acme/widget");
    let mine = orphan(&repo, &format!(".slot-3.seeding.{}.1", std::process::id()));
    // pid 1 is launchd/init: alive for the whole test.
    let init = orphan(&repo, ".slot-4.seeding.1.1");

    let report = sweep_abandoned_staging(&repo);

    assert!(
        report.discarded.is_empty() && report.failed.is_empty(),
        "{report:?}"
    );
    assert!(mine.exists() && init.exists(), "live owners' trees survive");
}

/// #9239 fail-open check: a tree the sweep cannot move is reported and left
/// where it was — and no seed of that index commits over it.
#[test]
fn a_sweep_that_cannot_move_a_tree_reports_it_and_leaves_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("temp root");
    let pool = pool(tmp.path());
    let repo = pool.repo_dir();
    let stuck = orphan(&repo, &format!(".slot-2.seeding.{DEAD_PID}.1"));
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o555))
        .expect("make every rename in the repo dir fail");

    let report = sweep_abandoned_staging(&repo);
    let seeded = pool.seed(2, None);
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o755)).expect("restore");

    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, stuck, "the failure names the tree");
    assert!(
        stuck.join("debug/libpartial.rlib").is_file(),
        "the tree is left whole"
    );
    assert!(seeded.is_err(), "no slot is granted beside it: {seeded:?}");
    assert_eq!(pool.committed(2), None, "slot 2 has no committed seed");
}

/// A cloner that makes its staging tree, writes into it, and never finishes.
fn never_finishes(_src: &Path, staging: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg("mkdir -p \"$1\" && echo partial > \"$1/sentinel\" && exec sleep 30")
        .arg("sh")
        .arg(staging);
    cmd
}

/// #9239: a release agent waited 2h44m on a seed. A clone past its bound is
/// killed, its staging tree discarded, and the slot committed cold — the
/// builder proceeds within the bound and never builds in the staging tree.
#[test]
fn a_clone_past_its_deadline_is_killed_and_discarded() {
    if !cfg!(target_os = "macos") {
        // No clone runs off macOS, so there is no clone to bound.
        return;
    }
    let tmp = tempfile::tempdir().expect("temp root");
    let shared = tmp.path().join("shared");
    std::fs::create_dir_all(&shared).expect("a shared dir");
    std::fs::write(shared.join("sentinel"), b"warm").expect("an artifact");
    let pool = pool(&tmp.path().join("pool")).with_cloner(never_finishes);

    let started = Instant::now();
    let (path, seed) = pool
        .seed_within(0, Some(&shared), Duration::from_millis(300))
        .expect("a timed-out clone still yields a committed slot");
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(10),
        "bounded, not 30 s: {took:?}"
    );
    assert!(
        matches!(&seed, SeedKind::ColdDirectory(d) if d.contains("did not finish within")),
        "{seed:?}"
    );
    assert_eq!(pool.committed(0), Some(path.clone()), "the slot committed");
    assert!(
        !path.join("sentinel").exists(),
        "no partial clone in the slot"
    );
    let left = wait_until_gone(&pool.repo_dir(), &[".slot-0.seeding.", ".slot-0.evicting."]);
    assert!(left.is_empty(), "the staging tree is discarded: {left:?}");
}

/// #9239: a marker that does not read as seeded and cannot be retired used
/// to win the `hard_link` election as `AlreadySeeded`, granting a slot whose
/// seed never committed.
#[test]
fn a_marker_that_cannot_be_retired_is_never_committed() {
    let tmp = tempfile::tempdir().expect("temp root");
    let pool = pool(tmp.path());
    let slot = pool.slot_path(0);
    std::fs::create_dir_all(&slot).expect("a slot");
    std::fs::write(
        slot.join(SEED_MARKER),
        "#8261 builder slot pool\nseed: ColdDirectory(\"cp -c exited 1\")\n",
    )
    .expect("a failure-text marker");
    // A directory where the retired marker would go: the retire rename fails.
    std::fs::create_dir_all(pool.repo_dir().join(".slot-0.seed-failed/x")).expect("a blocker");

    match pool.seed(0, None) {
        Ok((path, kind)) => assert!(
            marker_reads_seeded(&path),
            "granted {path:?} ({kind:?}) though its marker does not read as seeded"
        ),
        Err(err) => assert!(matches!(err, SlotPoolError::SeedFailed { .. }), "{err:?}"),
    }
    assert_eq!(pool.committed(0), None, "slot 0 never reads as committed");
}

#[test]
fn a_staging_name_parses_into_slot_pid_and_age() {
    let started = SystemTime::now() - Duration::from_secs(90);
    let nanos = started
        .duration_since(UNIX_EPOCH)
        .expect("after epoch")
        .as_nanos();
    let path = PathBuf::from(format!("/p/.slot-4.seeding.{}.{nanos}", std::process::id()));

    let entry = StagingEntry::parse(path.clone()).expect("a staging name");

    assert_eq!(
        (entry.slot, entry.kind, entry.alive),
        (4, StagingKind::Seeding, true)
    );
    assert!(
        entry.age().is_some_and(|a| a >= Duration::from_secs(89)),
        "{entry:?}"
    );
    assert!(
        entry.render().starts_with("seeding slot 4: pid "),
        "{}",
        entry.render()
    );
    for other in [
        "/p/slot-4",
        "/p/.slot-4.seed-failed",
        "/p/.slot-x.seeding.1.1",
    ] {
        assert_eq!(StagingEntry::parse(PathBuf::from(other)), None, "{other}");
    }
}

#[test]
fn pool_status_lists_a_seed_in_progress_and_slot_states() {
    let tmp = tempfile::tempdir().expect("temp root");
    let pool = pool(tmp.path());
    pool.seed(1, None).expect("slot 1 seeds");
    std::fs::create_dir_all(pool.slot_path(0)).expect("an unseeded slot 0");
    let live = orphan(
        &pool.repo_dir(),
        &format!(".slot-3.seeding.{}.1", std::process::id()),
    );

    let repos = pool_status(tmp.path());

    assert_eq!(repos.len(), 1, "{repos:?}");
    assert_eq!(repos[0].slots, vec![(0, false), (1, true)]);
    assert_eq!(repos[0].staging.len(), 1, "{repos:?}");
    assert_eq!(repos[0].staging[0].path, live);
    assert!(live.exists(), "status deletes nothing");
}
