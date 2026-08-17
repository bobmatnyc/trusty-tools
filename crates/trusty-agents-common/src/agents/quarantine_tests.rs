//! Tests for the #4448 quarantine sweep.
//!
//! Every fixture is a `TempDir`. NOTHING here points the sweep at a real
//! deployed directory — the live tiers on a developer's machine hold their own
//! agents, and a test that swept one would be the very defect this module
//! exists to prevent.

use super::*;
use crate::agents::manifest::{MANIFEST_FILE, ManifestEntry, Origin, checksum, manifest_lock_path};
use crate::agents::quarantine_receipt::shell_quote;
use std::collections::HashMap;
use tempfile::TempDir;

/// A composed trusty-mpm agent — what an older binary actually wrote into a
/// project tier, and the only shape gate 4 accepts. The body opens with the
/// base preamble `compose_agent` concatenates in front of every agent; that
/// line is the POSITIVE half of gate 4, so a fixture without it is not composer
/// output and (correctly) does not move.
fn tm_agent(name: &str) -> String {
    format!(
        "---\nname: {name}\nrole: qa\ndescription: 'Composed by tm.'\nmodel: sonnet\n\
         skills: [systematic-debugging]\ninitialPrompt: \"Begin.\"\n---\n\n\
         # BASE-AGENT — Foundation for all trusty-mpm agents\n\n\
         Root-level instructions composed into every deployed agent.\n\n# {name}\n\nBody.\n"
    )
}

/// A claude-mpm agent on the SAME name — a different live project's file.
fn claude_mpm_agent(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: \"Use this agent when…\"\nmodel: sonnet\n\
         effort: balanced\nagent_type: qa\nversion: \"1.0.0\"\nskills:\n- code-review-standards\n\
         initialPrompt: \"Begin.\"\n---\n# {name}\n\n**Inherits from**: BASE_AGENT.md\n"
    )
}

fn roster(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// A staged project: a git repository with a flat agent tier and a separate
/// backup root, exactly as the trusty-mpm call sites wire them.
struct Fixture {
    _tmp: TempDir,
    project: PathBuf,
    tier: PathBuf,
    backups: PathBuf,
}

impl Fixture {
    /// Stage a project WITH a git repository — the common case.
    fn new() -> Self {
        let f = Self::without_git();
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.project)
            .args(["init", "-q"])
            .output()
            .expect("git must be available to run the quarantine tests");
        assert!(out.status.success(), "git init failed");
        f
    }

    /// Stage a project with NO repository — a project that does not use git
    /// must still be sweepable.
    fn without_git() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let project = tmp.path().to_path_buf();
        let tier = project.join(".claude").join("agents");
        std::fs::create_dir_all(&tier).expect("create tier");
        let backups = project.join(".trusty-mpm").join("agent-quarantine");
        Self {
            _tmp: tmp,
            project,
            tier,
            backups,
        }
    }

    fn write(&self, file_name: &str, content: &str) -> PathBuf {
        let path = self.tier.join(file_name);
        std::fs::write(&path, content).expect("write agent");
        path
    }

    fn git_add(&self, rel: &str) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.project)
            .args(["add", "--force", rel])
            .output()
            .expect("spawn git add");
        assert!(out.status.success(), "git add {rel} failed");
    }

    /// Record `file_name` in this tier's ownership ledger under `origin`.
    fn ledger(&self, file_name: &str, content: &str, origin: Origin) {
        let mut managed = HashMap::new();
        managed.insert(
            file_name.to_owned(),
            ManifestEntry {
                source_chain: vec![file_name.trim_end_matches(".md").to_owned()],
                checksum: checksum(content),
                deployed_at: "2026-08-03T00:00:00Z".to_owned(),
                origin,
            },
        );
        AgentManifest {
            managed,
            ..AgentManifest::default()
        }
        .save(&self.tier)
        .expect("save ledger");
    }

    fn sweep(&self, bundled: &BTreeSet<String>) -> Result<QuarantineReport, QuarantineError> {
        quarantine_shadowing_agents(&self.tier, &self.backups, bundled, "run-1")
    }
}

// ---------------------------------------------------------------------------
// The approval path
// ---------------------------------------------------------------------------

/// The whole point: an untracked, trusty-mpm-schema copy on a bundled name is
/// moved out of the way, backed up byte-identically, and left restorable.
#[test]
fn quarantine_moves_an_untracked_trusty_mpm_shadow() {
    let f = Fixture::new();
    let content = tm_agent("qa");
    let original = f.write("qa.md", &content);

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert_eq!(report.moved.len(), 1, "report: {report:?}");
    let moved = &report.moved[0];
    assert_eq!(moved.name, "qa");
    assert_eq!(moved.from, original);
    assert_eq!(moved.to, f.tier.join("qa.md.disabled"));

    // Verified from disk, not from the return value.
    assert!(!original.exists(), "the shadowing file must be gone");
    assert!(moved.to.exists(), "the inert sibling must exist");
    assert_eq!(
        std::fs::read_to_string(&moved.to).expect("read moved"),
        content,
        "the moved file must be byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(&moved.backup).expect("read backup"),
        content,
        "the backup must be byte-identical"
    );
    assert!(moved.backup.starts_with(&f.backups));
}

/// The renamed file is inert: no loader keys on `.disabled`.
#[test]
fn a_quarantined_file_is_no_longer_an_agent_file() {
    let f = Fixture::new();
    f.write("qa.md", &tm_agent("qa"));
    f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(!is_agent_file("qa.md.disabled"));
    assert!(candidates(&f.tier).is_empty(), "nothing left to sweep");
}

/// Identity is the frontmatter `name:`, never the stem — a renamed file that
/// DECLARES a bundled name still shadows it.
#[test]
fn quarantine_follows_the_declared_name_not_the_filename() {
    let f = Fixture::new();
    f.write("helper.md", &tm_agent("rust-engineer"));
    let report = f.sweep(&roster(&["rust-engineer"])).expect("sweep");
    assert_eq!(report.moved.len(), 1, "report: {report:?}");
    assert_eq!(report.moved[0].name, "rust-engineer");
}

/// A project that does not use git at all is still sweepable — "no repository"
/// means nothing can be claiming the file.
#[test]
fn quarantine_works_without_a_repository() {
    let f = Fixture::without_git();
    f.write("qa.md", &tm_agent("qa"));
    let report = f.sweep(&roster(&["qa"])).expect("sweep");
    assert_eq!(report.moved.len(), 1, "report: {report:?}");
}

// ---------------------------------------------------------------------------
// The four gates — one refusal test each
// ---------------------------------------------------------------------------

/// GATE 3, and the defect that blocked #4526. A file the repository TRACKS is
/// the project's, whatever its name and schema say. This is the standing-in
/// replacement for the closed `Origin::Project` (#4443).
#[test]
fn quarantine_never_moves_a_git_tracked_file() {
    let f = Fixture::new();
    let content = tm_agent("qa");
    let path = f.write("qa.md", &content);
    f.git_add(".claude/agents/qa.md");

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].reason, SkipReason::GitTracked);
    assert!(
        path.exists(),
        "a tracked file must be left exactly where it is"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), content);
    assert!(
        !f.tier.join("qa.md.disabled").exists(),
        "no inert sibling may be created for a tracked file"
    );
}

/// GATE 4, and the constraint that a filename-keyed sweep violates. claude-mpm
/// ships `qa.md` too; it is another live project's file and must never move.
#[test]
fn quarantine_never_moves_a_claude_mpm_file_on_a_colliding_name() {
    let f = Fixture::new();
    let content = claude_mpm_agent("qa");
    let path = f.write("qa.md", &content);

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::ClaudeMpmSchema);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), content);
}

/// GATE 3, and the #4448 review CRITICAL reproduced end to end. A COMMITTED
/// file in a repository git cannot read must be REFUSED, not swept.
///
/// This is the critic's own probe. Before the fix it asserted the opposite and
/// passed: `report.moved.len() == 1`, `!original.exists()` — a tracked file
/// moved out from under the operator, because any non-zero `git rev-parse` exit
/// was read as "no repository here" instead of "git could not be asked".
#[test]
fn quarantine_refuses_a_tracked_file_when_git_cannot_be_read() {
    let f = Fixture::new();
    let content = tm_agent("qa");
    let original = f.write("qa.md", &content);
    f.git_add(".claude/agents/qa.md");

    // A healthy repo already refuses — establishes the fixture is real.
    let healthy = f.sweep(&roster(&["qa"])).expect("sweep");
    assert!(healthy.moved.is_empty(), "report: {healthy:?}");
    assert_eq!(healthy.skipped[0].reason, SkipReason::GitTracked);

    // Same repo, same committed file — now git cannot read it.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&f.project)
        .args(["config", "core.repositoryformatversion", "99"])
        .output()
        .expect("spawn git config");
    assert!(out.status.success(), "could not break the repo");

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(
        report.moved.is_empty(),
        "a COMMITTED file must never move because git failed to answer: {report:?}"
    );
    assert_eq!(report.skipped[0].reason, SkipReason::VcsUnknown);
    assert!(original.exists(), "the committed file must still be there");
    assert_eq!(std::fs::read_to_string(&original).expect("read"), content);
    assert!(!f.tier.join("qa.md.disabled").exists());
}

/// GATE 3, #4448 review ROUND 2 reproduced end to end. A COMMITTED file in a
/// repository whose `.git` is UNREADABLE must be refused.
///
/// This is the critic's own probe. Before `classify_failure` corroborated git's
/// message with a filesystem witness it asserted the opposite and passed:
/// `report.moved.len() == 1`, `!original.exists()` — a tracked file moved,
/// because git reports an unreadable `.git` with the same wording it uses for a
/// genuinely absent repository.
#[test]
#[cfg(unix)]
fn quarantine_refuses_a_tracked_file_when_the_git_dir_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;

    /// Restore the mode on drop so a panic cannot leave `TempDir` unable to
    /// clean up.
    struct ModeGuard(PathBuf);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    let f = Fixture::new();
    let content = tm_agent("qa");
    let original = f.write("qa.md", &content);
    f.git_add(".claude/agents/qa.md");

    // Healthy repo refuses — establishes the fixture is real.
    let healthy = f.sweep(&roster(&["qa"])).expect("sweep");
    assert!(healthy.moved.is_empty(), "report: {healthy:?}");
    assert_eq!(healthy.skipped[0].reason, SkipReason::GitTracked);

    let git_dir = f.project.join(".git");
    let _guard = ModeGuard(git_dir.clone());
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod .git");

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(
        report.moved.is_empty(),
        "a COMMITTED file must never move because git could not read .git: {report:?}"
    );
    assert_eq!(report.skipped[0].reason, SkipReason::VcsUnknown);
    assert!(original.exists(), "the committed file must still be there");
    assert_eq!(std::fs::read_to_string(&original).expect("read"), content);
    assert!(!f.tier.join("qa.md.disabled").exists());
}

/// GATE 4, and the #4448 review HIGH reproduced end to end. A MINIMAL
/// hand-authored agent — no exotic key, every key inside the whitelist — is not
/// tm's to move. Before the fix the key whitelist accepted it by omission.
#[test]
fn quarantine_never_moves_a_minimal_hand_authored_file() {
    let f = Fixture::new();
    let content = "---\nname: qa\nrole: qa\ndescription: Ours.\nmodel: sonnet\n---\n\n\
                   # Our QA\n\nHand-written by the operator.\n";
    let path = f.write("qa.md", content);

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::UnrecognizedSchema);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), content);
    assert!(!f.tier.join("qa.md.disabled").exists());
}

/// GATE 4 again — the "unrecognized" half. A hand-authored override on a
/// bundled name is not tm's to move either.
#[test]
fn quarantine_never_moves_a_hand_authored_file() {
    let f = Fixture::new();
    let path = f.write(
        "qa.md",
        "---\nname: qa\nrole: qa\ncolor: purple\n---\n\nOurs.\n",
    );
    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::UnrecognizedSchema);
    assert!(path.exists());
}

/// GATE 2. A user-owned ledger entry is positive proof the file is not tm's,
/// and it outranks a name collision — the same rule
/// `retract_framework_agents` follows.
#[test]
fn quarantine_never_moves_a_user_owned_entry() {
    let f = Fixture::new();
    let content = tm_agent("qa");
    let path = f.write("qa.md", &content);
    f.ledger("qa.md", &content, Origin::User);

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::UserOwned);
    assert!(path.exists());
}

/// GATE 1. A name tm does not ship shadows nothing.
#[test]
fn quarantine_never_moves_a_custom_agent() {
    let f = Fixture::new();
    let path = f.write("acme.md", &tm_agent("acme-custom"));
    let report = f.sweep(&roster(&["qa", "rust-engineer"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::NotShadowingBundled);
    assert!(path.exists());
}

/// GATE 1, the stranded tier. A framework-owned ledger entry on a retired name
/// belongs to `retract_framework_agents`; two writers on one file is a race,
/// not a repair.
#[test]
fn quarantine_never_moves_a_stranded_framework_file() {
    let f = Fixture::new();
    let content = tm_agent("retired-agent");
    let path = f.write("retired-agent.md", &content);
    f.ledger("retired-agent.md", &content, Origin::Bundled);

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(report.moved.is_empty(), "report: {report:?}");
    assert_eq!(report.skipped[0].reason, SkipReason::NotShadowingBundled);
    assert!(path.exists());
}

/// The conjunction, stated as a truth table over the pure predicate. Exactly
/// ONE row is movable; dropping any gate flips one of the others.
#[test]
fn movable_only_when_all_four_gates_agree() {
    let bundled = roster(&["qa"]);
    let tm = tm_agent("qa");
    let foreign = claude_mpm_agent("qa");
    let untracked = TierOwnership::Untracked;
    let free = VcsClaim::Unclaimed;

    // The ONLY movable row: all four gates agree.
    assert_eq!(refusal("qa", &tm, untracked, &bundled, free), None);

    for (label, got, want) in [
        // Gate 1 — not a name tm ships.
        (
            "gate 1 / custom name",
            refusal("acme", &tm, untracked, &bundled, free),
            SkipReason::NotShadowingBundled,
        ),
        // Gate 1 — a stranded framework entry belongs to retraction.
        (
            "gate 1 / stranded",
            refusal("acme", &tm, TierOwnership::FrameworkOwned, &bundled, free),
            SkipReason::NotShadowingBundled,
        ),
        // Gate 2 — the ledger proves the file is the operator's.
        (
            "gate 2 / user-owned",
            refusal("qa", &tm, TierOwnership::UserOwned, &bundled, free),
            SkipReason::UserOwned,
        ),
        // Gate 3 — the repository claims it.
        (
            "gate 3 / tracked",
            refusal("qa", &tm, untracked, &bundled, VcsClaim::Claimed),
            SkipReason::GitTracked,
        ),
        // Gate 3 — git could not be asked, so a claim cannot be ruled out.
        (
            "gate 3 / unknown",
            refusal("qa", &tm, untracked, &bundled, VcsClaim::Unknown),
            SkipReason::VcsUnknown,
        ),
        // Gate 4 — another live project's file on the same name.
        (
            "gate 4 / claude-mpm",
            refusal("qa", &foreign, untracked, &bundled, free),
            SkipReason::ClaudeMpmSchema,
        ),
        // Gate 4 — no recognised deploy shape at all.
        (
            "gate 4 / unrecognised",
            refusal("qa", "not frontmatter", untracked, &bundled, free),
            SkipReason::UnrecognizedSchema,
        ),
        // An empty roster can never make anything movable.
        (
            "empty roster",
            refusal("qa", &tm, untracked, &BTreeSet::new(), free),
            SkipReason::NotShadowingBundled,
        ),
    ] {
        assert_eq!(got, Some(want), "{label}");
    }
}

// ---------------------------------------------------------------------------
// Whole-sweep refusals
// ---------------------------------------------------------------------------

/// An unreadable ledger hides the operator-owned exemption, so nothing moves.
#[test]
fn quarantine_refuses_on_a_corrupt_ledger() {
    let f = Fixture::new();
    let path = f.write("qa.md", &tm_agent("qa"));
    std::fs::write(f.tier.join(MANIFEST_FILE), "{ not json").expect("corrupt the ledger");

    let err = f.sweep(&roster(&["qa"])).expect_err("must refuse");
    assert!(matches!(err, QuarantineError::CorruptLedger(_)), "{err}");
    assert!(path.exists(), "a refusal must touch nothing");
}

/// An empty roster would classify every file as custom and report a false
/// clean — refuse loudly instead.
#[test]
fn quarantine_refuses_an_empty_roster() {
    let f = Fixture::new();
    let path = f.write("qa.md", &tm_agent("qa"));

    let err = f.sweep(&BTreeSet::new()).expect_err("must refuse");
    assert!(matches!(err, QuarantineError::EmptyRoster), "{err}");
    assert!(path.exists());
}

/// A missing tier is a no-op that does NOT create the directory — the ledger
/// lock would, and materialising an empty `.claude/agents/` in every project on
/// every launch is a regression of its own.
#[test]
fn quarantine_missing_dir_is_a_noop() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    let report =
        quarantine_shadowing_agents(&tier, &tmp.path().join("bk"), &roster(&["qa"]), "run-1")
            .expect("noop");
    assert_eq!(report, QuarantineReport::default());
    assert!(!tier.exists(), "the sweep must not create the tier");
}

/// The whole read-classify-move sequence runs under the SAME lock the deploy
/// and retract paths take, so two concurrent launches serialise.
#[test]
fn quarantine_blocks_while_the_ledger_lock_is_held() {
    let f = Fixture::new();
    f.write("qa.md", &tm_agent("qa"));

    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(manifest_lock_path(&f.tier))
        .expect("open lock");
    let mut lock = fd_lock::RwLock::new(lock_file);
    let held = lock.try_write().expect("take the lock");

    let (tx, rx) = std::sync::mpsc::channel();
    let (tier, backups) = (f.tier.clone(), f.backups.clone());
    let handle = std::thread::spawn(move || {
        let out = quarantine_shadowing_agents(&tier, &backups, &roster(&["qa"]), "run-1");
        tx.send(()).ok();
        out
    });

    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "the sweep must BLOCK while the ledger lock is held"
    );
    drop(held);
    let report = handle.join().expect("join").expect("sweep");
    assert_eq!(report.moved.len(), 1, "report: {report:?}");
}

// ---------------------------------------------------------------------------
// The receipt contract, including partial failure
// ---------------------------------------------------------------------------

/// THE PARTIAL-FAILURE CONTRACT. One file moves, one cannot, one is skipped —
/// and the run still accounts for all three, on disk and in the return value.
/// #4526 shipped without this and it was the CRITICAL gap the reorder carried.
#[test]
fn a_partial_failure_still_reports_every_candidate() {
    let f = Fixture::new();
    let moves = f.write("qa.md", &tm_agent("qa"));
    let blocked_content = tm_agent("ops");
    let blocked = f.write("ops.md", &blocked_content);
    let skipped = f.write("research.md", &claude_mpm_agent("research"));

    // Occupy every `.md.disabled` name `free_path` will try for `ops.md`, so
    // its rename — and ONLY its rename — has nowhere to go.
    std::fs::write(f.tier.join("ops.md.disabled"), "occupied").expect("occupy");
    for n in 1..=MAX_COLLISION_ATTEMPTS {
        std::fs::write(f.tier.join(format!("ops.md.disabled.{n}")), "occupied").expect("occupy");
    }

    let report = f.sweep(&roster(&["qa", "ops", "research"])).expect("sweep");

    assert_eq!(report.examined(), 3, "report: {report:?}");
    assert_eq!(report.moved.len(), 1);
    assert_eq!(report.moved[0].name, "qa");
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].name, "ops");
    assert_eq!(report.failed[0].stage, FailStage::Rename);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].reason, SkipReason::ClaudeMpmSchema);

    // The failure did not stop the sweep and did not damage anything.
    assert!(!moves.exists(), "the successful move still happened");
    assert!(
        blocked.exists(),
        "the failed move left the original in place"
    );
    assert_eq!(
        std::fs::read_to_string(&blocked).expect("read"),
        blocked_content
    );
    assert!(skipped.exists());

    // The receipt exists and names all three.
    let receipt_path = report.receipt.as_ref().expect("a receipt must be written");
    let receipt = std::fs::read_to_string(receipt_path).expect("read receipt");
    assert!(receipt.contains("## Moved (1)"), "{receipt}");
    assert!(receipt.contains("## Failed (1)"), "{receipt}");
    assert!(receipt.contains("## Skipped (1)"), "{receipt}");
    for name in ["qa", "ops", "research"] {
        assert!(receipt.contains(name), "receipt omits `{name}`:\n{receipt}");
    }
    assert!(receipt.contains("Examined: 3"), "{receipt}");
}

/// A failed BACKUP never advances to the rename — the original stays put.
#[test]
fn a_failed_backup_leaves_the_original_in_place() {
    let f = Fixture::new();
    let content = tm_agent("qa");
    let path = f.write("qa.md", &content);
    // A regular file where the backup ROOT must be a directory: every
    // `create_dir_all` under it fails.
    std::fs::create_dir_all(f.backups.parent().expect("parent")).expect("mkdir");
    std::fs::write(&f.backups, "not a directory").expect("occupy the backup root");

    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert_eq!(report.failed.len(), 1, "report: {report:?}");
    assert_eq!(report.failed[0].stage, FailStage::Backup);
    assert!(report.moved.is_empty());
    assert_eq!(std::fs::read_to_string(&path).expect("read"), content);
    assert!(!f.tier.join("qa.md.disabled").exists());
    // The receipt could not be written either — reported, never swallowed, and
    // never an `Err` that would discard the account of what happened.
    assert!(report.receipt.is_none());
    assert!(report.receipt_error.is_some(), "report: {report:?}");
}

/// Two sweeps in the same UTC second must not truncate the first run's receipt.
/// `run_id` is second-resolution, so the receipt needs the same collision
/// protection the backups already had (#4448 review LOW).
#[test]
fn a_same_second_rerun_does_not_overwrite_the_first_receipt() {
    let f = Fixture::new();
    f.write("qa.md", &tm_agent("qa"));
    let first = f.sweep(&roster(&["qa"])).expect("first sweep");
    let first_receipt = first.receipt.clone().expect("receipt");

    // Same run id — `Fixture::sweep` always passes "run-1".
    f.write("qa.md", &tm_agent("qa"));
    let second = f.sweep(&roster(&["qa"])).expect("second sweep");
    let second_receipt = second.receipt.clone().expect("receipt");

    assert_ne!(first_receipt, second_receipt, "receipts must not collide");
    assert!(first_receipt.exists(), "run 1's receipt must survive");
    let body = std::fs::read_to_string(&first_receipt).expect("read");
    assert!(
        body.contains("## Moved (1)"),
        "run 1's record intact:\n{body}"
    );
}

/// A clean tier gains no files — no backup directory, no receipt.
#[test]
fn a_clean_tier_writes_no_receipt() {
    let f = Fixture::new();
    f.write("acme.md", &tm_agent("acme-custom"));
    let report = f.sweep(&roster(&["qa"])).expect("sweep");

    assert!(!report.wrote_anything());
    assert!(report.receipt.is_none());
    assert!(
        !f.backups.exists(),
        "a clean project must gain no quarantine directory"
    );
}

/// The receipt's restore command is POSIX-quoted, so a filename carrying `$`
/// or backticks cannot execute when pasted. #4526 emitted `{:?}` (Rust
/// `Debug`), which is DOUBLE quotes — inside which both still fire.
#[test]
#[cfg(unix)]
fn restore_command_survives_a_hostile_filename() {
    let f = Fixture::new();
    let hostile = "it's $(touch PWNED-SUB) `touch PWNED-TICK`.md";
    f.write(hostile, &tm_agent("qa"));

    let report = f.sweep(&roster(&["qa"])).expect("sweep");
    assert_eq!(report.moved.len(), 1, "report: {report:?}");
    let moved = &report.moved[0];

    let command = format!("mv {} {}", shell_quote(&moved.to), shell_quote(&moved.from));
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&f.tier)
        .output()
        .expect("run the printed restore command");
    assert!(
        out.status.success(),
        "restore failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(moved.from.exists(), "the file must be back where it was");
    assert!(
        !f.tier.join("PWNED-SUB").exists(),
        "command substitution ran"
    );
    assert!(!f.tier.join("PWNED-TICK").exists(), "a backtick ran");
    // The receipt prints exactly what was executed.
    let receipt = std::fs::read_to_string(report.receipt.expect("receipt")).expect("read");
    assert!(receipt.contains(&command), "receipt:\n{receipt}");
}

// ---------------------------------------------------------------------------
// Collision and symlink handling — never overwrite, never follow
// ---------------------------------------------------------------------------

/// A second sweep must not overwrite the first run's backup or its inert
/// sibling; both are an operator's only copies.
#[test]
fn a_second_run_does_not_overwrite_the_first_backup() {
    let f = Fixture::new();
    f.write("qa.md", &tm_agent("qa"));
    let first = f.sweep(&roster(&["qa"])).expect("first sweep");
    assert_eq!(first.moved.len(), 1);

    // A fresh stale copy lands, and the same run id is reused.
    f.write(
        "qa.md",
        "---\nname: qa\nrole: qa\n---\n\n\
         # BASE-AGENT — Foundation for all trusty-mpm agents\n\nSecond copy.\n",
    );
    let second = f.sweep(&roster(&["qa"])).expect("second sweep");

    assert_eq!(second.moved.len(), 1, "report: {second:?}");
    assert_ne!(second.moved[0].backup, first.moved[0].backup);
    assert_ne!(second.moved[0].to, first.moved[0].to);
    assert!(first.moved[0].backup.exists(), "run 1's backup survives");
    assert!(first.moved[0].to.exists(), "run 1's inert sibling survives");
    assert!(
        std::fs::read_to_string(&first.moved[0].backup)
            .expect("read")
            .contains("Composed by tm"),
        "run 1's backup content must be unchanged"
    );
}

/// Freeness is `symlink_metadata`, so a DANGLING symlink counts as occupied.
/// `metadata()` would follow it, report the path free, and the rename would
/// then write through the link to wherever it points.
#[test]
#[cfg(unix)]
fn a_dangling_symlink_is_not_free() {
    let tmp = TempDir::new().expect("tempdir");
    let link = tmp.path().join("qa.md.disabled");
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).expect("symlink");

    assert!(std::fs::metadata(&link).is_err(), "the link dangles");
    let chosen = free_path(&link).expect("a free name");
    assert_ne!(chosen, link, "a dangling symlink must count as occupied");
}

/// A symlink in the tier is never a candidate — following one could aim the
/// sweep at the canonical deploy directory.
#[test]
#[cfg(unix)]
fn a_symlink_is_never_a_candidate() {
    let f = Fixture::new();
    let real = f.project.join("elsewhere.md");
    std::fs::write(&real, tm_agent("qa")).expect("write");
    std::os::unix::fs::symlink(&real, f.tier.join("qa.md")).expect("symlink");

    let report = f.sweep(&roster(&["qa"])).expect("sweep");
    assert_eq!(report.examined(), 0, "report: {report:?}");
    assert!(real.exists(), "the symlink target must be untouched");
}

// ---------------------------------------------------------------------------
// The non-negotiable: this module cannot delete
// ---------------------------------------------------------------------------

/// NO CODE PATH INVOKES DELETION. Asserted against the source text, because
/// the property is "there is no such call", which no behavioural test can
/// establish — a behavioural test only proves the paths it happened to walk.
///
/// A quarantine that deletes is a worse defect than the shadowing it fixes
/// (2026-07-21: an unreviewed cleanup destroyed a bare clone). If a future
/// change genuinely needs a removal, this test failing is the review gate that
/// forces the conversation.
#[test]
fn never_deletes_on_any_path() {
    for (label, source) in [
        ("quarantine.rs", include_str!("quarantine.rs")),
        (
            "quarantine_receipt.rs",
            include_str!("quarantine_receipt.rs"),
        ),
        ("vcs_claim.rs", include_str!("vcs_claim.rs")),
        ("agent_schema.rs", include_str!("agent_schema.rs")),
    ] {
        // Split the needles so this test does not match itself.
        for needle in [
            concat!("remove_", "file"),
            concat!("remove_", "dir"),
            concat!("remove_", "dir_all"),
            concat!("set_", "len"),
            concat!("File::", "create"),
        ] {
            // Comments explaining the ban are allowed; calls are not.
            let calls: Vec<&str> = source
                .lines()
                .filter(|line| {
                    let t = line.trim_start();
                    !t.starts_with("//") && !t.starts_with("///") && line.contains(needle)
                })
                .collect();
            assert!(
                calls.is_empty(),
                "{label} must never call `{needle}` — quarantine moves, it does not destroy. \
                 Offending lines: {calls:?}"
            );
        }
    }
}

/// The inverse evidence: after a real sweep, every byte that was in the tier is
/// still on disk somewhere. Nothing is destroyed, only relocated.
#[test]
fn a_sweep_destroys_no_content() {
    let f = Fixture::new();
    let staged: Vec<(&str, String)> = vec![
        ("qa.md", tm_agent("qa")),
        ("ops.md", claude_mpm_agent("ops")),
        ("acme.md", tm_agent("acme-custom")),
    ];
    for (file_name, content) in &staged {
        f.write(file_name, content);
    }

    let report = f.sweep(&roster(&["qa", "ops"])).expect("sweep");
    assert_eq!(report.moved.len(), 1, "report: {report:?}");

    for (_, content) in &staged {
        let found = walk(&f.project).into_iter().any(|p| {
            std::fs::read_to_string(&p)
                .map(|c| &c == content)
                .unwrap_or(false)
        });
        assert!(found, "content vanished from the project:\n{content}");
    }
}

/// #5626 (ADR-0045). A ledger the sweep could not READ must reach the same
/// refusal a corrupt one does.
///
/// `CorruptLedger` exists so nothing moves "on the strength of an unreadable
/// ledger, because the operator-owned exemption lives in it". Before the fix
/// only a parse failure reached it: an EACCES took `load_checked`'s catch-all
/// `Err(_)` arm to `Ok(default)`, the operator's user-owned entry read as
/// untracked, and this sweep renamed their file to `qa.md.disabled`.
#[test]
#[cfg(unix)]
fn quarantine_refuses_when_the_ledger_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;

    /// Restore the mode on drop so a panic cannot leave `TempDir` unable to
    /// clean up.
    struct ModeGuard(PathBuf);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o644));
        }
    }

    let f = Fixture::new();
    let content = tm_agent("qa");
    let original = f.write("qa.md", &content);
    // The operator owns this file, and only the ledger says so.
    f.ledger("qa.md", &content, Origin::User);

    // A readable ledger already preserves it — establishes the fixture is real.
    let healthy = f.sweep(&roster(&["qa"])).expect("sweep");
    assert!(healthy.moved.is_empty(), "report: {healthy:?}");

    let ledger = f.tier.join(MANIFEST_FILE);
    let _guard = ModeGuard(ledger.clone());
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o000)).expect("chmod ledger");

    let err = f
        .sweep(&roster(&["qa"]))
        .expect_err("an unreadable ledger must refuse the whole sweep");
    assert!(
        matches!(err, QuarantineError::CorruptLedger(_)),
        "expected CorruptLedger, got {err:?}"
    );
    assert!(original.exists(), "the operator's file must still be there");
    assert!(!f.tier.join("qa.md.disabled").exists());
}

/// Every regular file under `root`, recursively. Test-only.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
