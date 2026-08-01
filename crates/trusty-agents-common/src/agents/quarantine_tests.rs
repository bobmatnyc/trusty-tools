//! Tests for the shadowing-agent quarantine (#4448).
//!
//! Why: this code MOVES operator files, so the tests that matter most are the
//! REFUSALS, not the happy path. Each guard below is written so that removing
//! the guard makes the test fail — a guard whose test still passes without it
//! is not verified. The mutation evidence is recorded in the PR body.
//! What: the one movable case (untracked + shadowing), every case that must be
//! left alone (user-owned, custom, stranded-framework-owned, bundled-stem-but-
//! custom-name), every indeterminate state that must refuse wholesale (empty
//! roster, corrupt ledger), and the non-destructive properties (idempotence,
//! occupied destinations, ledger untouched, receipt appended).
//! Test: this file.

use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::agents::manifest::{AgentManifest, MANIFEST_FILE, ManifestEntry, Origin, checksum};

/// Build a bundled-name roster from string literals.
fn roster(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

/// A minimal agent document declaring `name`.
fn doc(name: &str) -> String {
    format!("---\nname: {name}\nrole: engineer\n---\n\nBody.\n")
}

/// Stage an agent tier directory with `files` (filename, contents).
fn tier(files: &[(&str, String)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (name, body) in files {
        std::fs::write(tmp.path().join(name), body).unwrap();
    }
    tmp
}

/// Write a ledger into `dir` claiming `filename` with `origin`.
fn track(dir: &Path, filename: &str, origin: Origin) {
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        filename.to_owned(),
        ManifestEntry {
            source_chain: vec![],
            checksum: checksum("whatever"),
            deployed_at: "2026-07-31T00:00:00Z".to_owned(),
            origin,
        },
    );
    manifest.save(dir).unwrap();
}

/// Whether `dir` still holds `name` under its original filename.
fn still_present(dir: &Path, name: &str) -> bool {
    dir.join(name).is_file()
}

// ---------------------------------------------------------------------------
// The one case that moves.
// ---------------------------------------------------------------------------

#[test]
fn quarantine_renames_an_untracked_shadowing_file() {
    // THE issue: a claude-mpm-era `qa.md` no ledger names, occupying a bundled
    // stem, invisible to retraction and outranking the canonical tier.
    let tmp = tier(&[("qa.md", "legacy claude-mpm agent\n".to_owned())]);
    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1, "the shadowing file must be moved");
    assert_eq!(out.quarantined[0].name, "qa");
    assert!(out.refused.is_empty());
    assert!(out.changed());
    assert!(
        !still_present(tmp.path(), "qa.md"),
        "the shadow must no longer resolve as an agent"
    );
    // A RENAME, never a delete: the bytes survive under the new name.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("qa.md.disabled")).unwrap(),
        "legacy claude-mpm agent\n"
    );
}

#[test]
fn quarantine_follows_the_frontmatter_name_not_the_stem() {
    // Invariant 1 of the shared classifier, in the direction that would be
    // MISSED by a stem-keyed check: `helper.md` declaring `name: qa` IS the
    // agent the harness resolves for `qa`, so it shadows.
    let tmp = tier(&[("helper.md", doc("qa"))]);
    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(out.quarantined[0].name, "qa");
    assert!(!still_present(tmp.path(), "helper.md"));
    assert!(tmp.path().join("helper.md.disabled").is_file());
}

// ---------------------------------------------------------------------------
// REFUSALS — per-file.
// ---------------------------------------------------------------------------

#[test]
fn quarantine_never_moves_a_user_owned_file_on_a_bundled_name() {
    // The operator's OWN `qa.md`, proven theirs by a user-owned ledger entry.
    // A name collision must never outrank that proof.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    track(tmp.path(), "qa.md", Origin::User);

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(out.quarantined.is_empty(), "a user-owned file must survive");
    assert!(!out.changed());
    assert!(still_present(tmp.path(), "qa.md"));
    assert!(!tmp.path().join("qa.md.disabled").exists());
}

#[test]
fn quarantine_spares_a_bundled_stem_declaring_a_custom_name() {
    // Invariant 1 in the OTHER direction, the false-positive one: the file is
    // literally called `qa.md`, but it declares `name: acme-custom`, so the
    // harness never resolves it as `qa` and it shadows nothing.
    let tmp = tier(&[("qa.md", doc("acme-custom"))]);
    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(out.quarantined.is_empty());
    assert!(still_present(tmp.path(), "qa.md"));
}

#[test]
fn quarantine_never_moves_an_untracked_custom_agent() {
    // Untracked is a necessary condition, never a sufficient one. A project's
    // hand-placed agent on a name tm does not ship is nobody's business but the
    // project's.
    let tmp = tier(&[("acme-deploy.md", doc("acme-deploy"))]);
    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa", "engineer"])).unwrap();

    assert!(out.quarantined.is_empty());
    assert!(still_present(tmp.path(), "acme-deploy.md"));
}

#[test]
fn quarantine_never_moves_a_stranded_framework_file() {
    // StrandedFrameworkOwned is OUT OF SCOPE by decision: it is ledger-tracked
    // and framework-owned, which is exactly what `retract_framework_agents`
    // deletes at the same call sites. Moving it here would duplicate that
    // repair and leave the ledger naming a file that no longer exists.
    let tmp = tier(&[("retired-agent.md", doc("retired-agent"))]);
    track(tmp.path(), "retired-agent.md", Origin::Bundled);

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(out.quarantined.is_empty());
    assert!(still_present(tmp.path(), "retired-agent.md"));
}

#[test]
fn quarantine_never_moves_a_tracked_framework_file_on_a_bundled_name() {
    // ShadowsBundled is necessary but NOT sufficient: this file classifies as
    // ShadowsBundled (the roster wins over the framework-owned ledger entry),
    // yet it is retraction's to delete, not ours to rename — and renaming it
    // would desync the ledger that still names it.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    track(tmp.path(), "qa.md", Origin::Bundled);

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(out.quarantined.is_empty());
    assert!(still_present(tmp.path(), "qa.md"));
}

// ---------------------------------------------------------------------------
// REFUSALS — whole-run, on an indeterminate state.
// ---------------------------------------------------------------------------

#[test]
fn quarantine_refuses_on_empty_roster() {
    // An empty roster means the roster could not be BUILT. Classifying against
    // it would silently find nothing today and is one inverted condition away
    // from sweeping everything.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    let err = quarantine_shadowing_agents(tmp.path(), &BTreeSet::new()).unwrap_err();

    assert!(matches!(err, QuarantineError::EmptyRoster));
    assert!(still_present(tmp.path(), "qa.md"), "nothing may be moved");
}

#[test]
fn quarantine_refuses_on_corrupt_manifest() {
    // The load-bearing refusal. The read-only probe degrades a corrupt ledger
    // to empty; doing that here would reclassify every user-owned file as
    // untracked and sweep the operator's own agents.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    std::fs::write(tmp.path().join(MANIFEST_FILE), "{ this is not json").unwrap();

    let err = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap_err();

    assert!(matches!(err, QuarantineError::CorruptLedger(_)));
    assert!(
        still_present(tmp.path(), "qa.md"),
        "an unreadable ledger must move NOTHING, not move everything"
    );
    assert!(!tmp.path().join("qa.md.disabled").exists());
}

#[test]
fn quarantine_refuses_the_whole_run_not_just_the_unreadable_file() {
    // Fail CLOSED, not per-file: a corrupt ledger invalidates the ownership
    // verdict for every file in the directory, including ones that would
    // otherwise look cleanly movable.
    let tmp = tier(&[
        ("qa.md", doc("qa")),
        ("engineer.md", doc("engineer")),
        ("acme.md", doc("acme")),
    ]);
    std::fs::write(tmp.path().join(MANIFEST_FILE), "truncated").unwrap();

    let err = quarantine_shadowing_agents(tmp.path(), &roster(&["qa", "engineer"])).unwrap_err();

    assert!(matches!(err, QuarantineError::CorruptLedger(_)));
    for f in ["qa.md", "engineer.md", "acme.md"] {
        assert!(still_present(tmp.path(), f), "{f} must be untouched");
    }
}

#[test]
fn quarantine_missing_dir_is_a_noop() {
    // And must NOT materialise the directory just to take a lock in it.
    let tmp = tempfile::tempdir().unwrap();
    let absent = tmp.path().join("nope").join(".claude").join("agents");

    let out = quarantine_shadowing_agents(&absent, &roster(&["qa"])).unwrap();

    assert_eq!(out, QuarantineResult::default());
    assert!(!absent.exists(), "a no-op must not create the tier");
}

#[test]
fn quarantine_tolerates_an_absent_ledger() {
    // The issue's own live shape: workspaces with NO manifest at all. Absent is
    // not corrupt — it is the ordinary "everything here is untracked" case, and
    // the sweep must run.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    assert!(!tmp.path().join(MANIFEST_FILE).exists());

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1);
}

#[test]
fn quarantine_with_a_partial_roster_moves_only_what_that_roster_names() {
    // A roster missing entries must UNDER-sweep, never over-sweep.
    let tmp = tier(&[("qa.md", doc("qa")), ("engineer.md", doc("engineer"))]);

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(out.quarantined[0].name, "qa");
    assert!(
        still_present(tmp.path(), "engineer.md"),
        "a name absent from the roster is not provably bundled"
    );
}

// ---------------------------------------------------------------------------
// Non-destructive properties.
// ---------------------------------------------------------------------------

#[test]
fn quarantine_is_idempotent() {
    // The second run must find nothing: `.md.disabled` is not an agent file, so
    // it is never re-scanned and never re-quarantined.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    let first = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();
    let second = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(first.quarantined.len(), 1);
    assert!(second.quarantined.is_empty());
    assert!(!second.changed());
}

#[test]
fn quarantine_refuses_when_the_destination_is_occupied() {
    // `fs::rename` clobbers on Unix. A second, DIFFERENT `qa.md` appearing
    // beside an existing `qa.md.disabled` must not destroy the earlier one.
    let tmp = tier(&[
        ("qa.md", "second generation\n".to_owned()),
        ("qa.md.disabled", "first generation\n".to_owned()),
    ]);
    // Occupy every numbered fallback too, so the refusal branch is reached.
    for n in 1..MAX_COLLISION_ATTEMPTS {
        std::fs::write(tmp.path().join(format!("qa.md.disabled.{n}")), "taken").unwrap();
    }

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(out.quarantined.is_empty());
    assert_eq!(out.refused, vec!["qa.md".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("qa.md.disabled")).unwrap(),
        "first generation\n",
        "the earlier quarantine must survive byte-identical"
    );
    assert!(still_present(tmp.path(), "qa.md"), "refused, not deleted");
}

#[test]
fn quarantine_falls_back_to_a_numbered_name_on_collision() {
    // The ordinary collision: one destination taken, the next free.
    let tmp = tier(&[
        ("qa.md", "second generation\n".to_owned()),
        ("qa.md.disabled", "first generation\n".to_owned()),
    ]);

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("qa.md.disabled")).unwrap(),
        "first generation\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("qa.md.disabled.1")).unwrap(),
        "second generation\n"
    );
}

#[test]
fn quarantine_leaves_the_ledger_untouched() {
    // Structural invariant: only untracked files move, so no ledger entry can
    // end up naming a renamed file — and the ledger is never rewritten at all.
    let tmp = tier(&[("qa.md", doc("qa")), ("mine.md", doc("mine"))]);
    track(tmp.path(), "mine.md", Origin::User);
    let before = std::fs::read_to_string(tmp.path().join(MANIFEST_FILE)).unwrap();

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert_eq!(out.quarantined.len(), 1);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(MANIFEST_FILE)).unwrap(),
        before,
        "the quarantine must never write the ownership ledger"
    );
}

#[test]
fn quarantine_blocks_while_the_ledger_lock_is_held() {
    // The sweep and `retract_framework_agents` decide the fate of files in the
    // SAME directory, and retraction deletes while this renames. Running
    // unlocked lets a retraction observe a file, then find it renamed out from
    // under itself mid-loop. Without the lock this sweep finishes in ~1ms.
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp = tier(&[("qa.md", doc("qa"))]);
    let dir = tmp.path().to_path_buf();
    let (tx, rx) = mpsc::channel();

    let handle = with_agent_manifest_lock(&dir, || {
        let d = dir.clone();
        let handle = std::thread::spawn(move || {
            let out = quarantine_shadowing_agents(&d, &roster(&["qa"])).unwrap();
            let _ = tx.send(out.quarantined.len());
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the quarantine completed while another holder had the ledger lock"
        );
        Ok::<_, crate::agents::manifest::ManifestError>(handle)
    })
    .unwrap();

    // Lock released: the sweep proceeds.
    handle.join().unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Discoverability — the receipt, not a log line.
// ---------------------------------------------------------------------------

#[test]
fn quarantine_writes_a_recovery_receipt() {
    let tmp = tier(&[("qa.md", doc("qa"))]);
    quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    let receipt = std::fs::read_to_string(tmp.path().join(RECEIPT_FILE)).unwrap();
    assert!(receipt.contains("qa.md.disabled"), "names the moved file");
    assert!(
        receipt.contains("qa.md'"),
        "names the restore target, quoted"
    );
    assert!(receipt.contains("mv "), "gives a copy-pasteable undo");
    assert!(receipt.contains("4448"), "points at the explanation");
}

// ---------------------------------------------------------------------------
// The undo command is executable INPUT to a shell, so it is an injection
// surface. Filenames are attacker-influenced: they are whatever landed in the
// directory.
// ---------------------------------------------------------------------------

#[test]
fn shell_quote_neutralises_expansion() {
    // `{:?}` emits DOUBLE quotes, inside which all three of these still fire.
    assert_eq!(shell_quote("a$(id)b"), "'a$(id)b'");
    assert_eq!(shell_quote("a`id`b"), "'a`id`b'");
    assert_eq!(shell_quote("a${HOME}b"), "'a${HOME}b'");
}

#[test]
fn shell_quote_escapes_a_quote() {
    // The one character single quotes cannot contain: close, escape, reopen.
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
}

#[cfg(unix)]
#[test]
fn receipt_undo_command_survives_a_hostile_filename() {
    // The real proof: RUN the printed command. A filename carrying a command
    // substitution and an embedded single quote must restore the file and
    // execute nothing. Under the old `{:?}` formatting the payload fired.
    let tmp = tempfile::tempdir().unwrap();
    let evil = "it's evil$(touch PWNED)`touch PWNED2`.md";
    // The FILE name carries the payload; the frontmatter `name:` is what makes
    // it classify as a shadow.
    std::fs::write(tmp.path().join(evil), doc("qa")).unwrap();

    let out = quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();
    assert_eq!(out.quarantined.len(), 1);
    assert!(!tmp.path().join(evil).exists(), "precondition: it moved");

    let receipt = std::fs::read_to_string(tmp.path().join(RECEIPT_FILE)).unwrap();
    let mv = receipt
        .lines()
        .find(|l| l.trim_start().starts_with("mv "))
        .expect("receipt must carry an mv line");

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(mv)
        .current_dir(tmp.path())
        .status()
        .unwrap();

    assert!(status.success(), "the undo must actually run: {mv}");
    assert!(
        tmp.path().join(evil).is_file(),
        "the undo must restore the file under its original name"
    );
    assert!(
        !tmp.path().join("PWNED").exists() && !tmp.path().join("PWNED2").exists(),
        "the filename's payload must never execute: {mv}"
    );
}

#[test]
fn no_receipt_when_nothing_moved() {
    // A receipt for an empty sweep is noise that trains operators to ignore it.
    let tmp = tier(&[("acme.md", doc("acme"))]);
    quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();

    assert!(!tmp.path().join(RECEIPT_FILE).exists());
}

#[test]
fn receipt_appends_across_runs() {
    // Truncating would erase the record of the FIRST sweep — the one whose
    // files the operator is most likely hunting for.
    let tmp = tier(&[("qa.md", doc("qa"))]);
    quarantine_shadowing_agents(tmp.path(), &roster(&["qa"])).unwrap();
    std::fs::write(tmp.path().join("engineer.md"), doc("engineer")).unwrap();
    quarantine_shadowing_agents(tmp.path(), &roster(&["qa", "engineer"])).unwrap();

    let receipt = std::fs::read_to_string(tmp.path().join(RECEIPT_FILE)).unwrap();
    assert!(receipt.contains("qa.md.disabled"));
    assert!(receipt.contains("engineer.md.disabled"));
}

#[test]
fn receipt_is_not_an_agent_file() {
    // It lives in an agent tier; it must never be resolved as an agent, nor
    // classified by the shared auditor.
    assert!(!crate::agents::deployer::is_agent_file(RECEIPT_FILE));
}

#[test]
fn quarantined_name_is_not_an_agent_file() {
    // The suffix is what makes the quarantine effective AND idempotent.
    assert!(!crate::agents::deployer::is_agent_file(&format!(
        "qa.md{QUARANTINE_SUFFIX}"
    )));
}

// ---------------------------------------------------------------------------
// `is_free` — the "may I rename onto this?" probe. Fail-closed in both
// directions a plain `Path::exists()` gets wrong.
// ---------------------------------------------------------------------------

#[test]
fn free_only_when_provably_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(is_free(&tmp.path().join("nothing-here")));

    std::fs::write(tmp.path().join("taken"), "x").unwrap();
    assert!(!is_free(&tmp.path().join("taken")));

    std::fs::create_dir(tmp.path().join("a-dir")).unwrap();
    assert!(!is_free(&tmp.path().join("a-dir")));
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_not_free() {
    // `Path::exists()` FOLLOWS the link and reports a dangling one as absent —
    // then `rename` clobbers the link itself. `symlink_metadata` is what makes
    // this a refusal instead of a silent overwrite.
    let tmp = tempfile::tempdir().unwrap();
    let link = tmp.path().join("qa.md.disabled");
    std::os::unix::fs::symlink(tmp.path().join("gone"), &link).unwrap();

    assert!(!link.exists(), "precondition: the link dangles");
    assert!(
        !is_free(&link),
        "a dangling symlink must never read as free"
    );
}

#[test]
fn an_unstattable_path_is_not_free() {
    // Any error that is NOT NotFound means we could not establish absence, and
    // an unreadable destination is exactly when a blind rename is least safe.
    // Reaching through a regular file yields ENOTDIR deterministically, on
    // every platform and regardless of the running user.
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let through_a_file = file.join("child.md.disabled");

    let err = std::fs::symlink_metadata(&through_a_file).unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound, "got {err:?}");
    assert!(
        !is_free(&through_a_file),
        "only a proven NotFound may count as free"
    );
}

// ---------------------------------------------------------------------------
// The movable predicate, exhaustively.
// ---------------------------------------------------------------------------

#[test]
fn movable_truth_table_is_exhaustive() {
    use crate::agents::tier_audit::{TierOwnership::*, TierResidentClass::*};

    // Exactly ONE of the six combinations may move. Enumerated rather than
    // staged on disk because two of them are unreachable through the current
    // classifier — which is precisely why a filesystem test cannot pin the
    // class half of the filter, and why an ownership-only check would look
    // correct today and start sweeping the moment `tier_audit` grows a variant.
    let table = [
        ((ShadowsBundled, Untracked), true),
        ((ShadowsBundled, FrameworkOwned), false),
        ((ShadowsBundled, UserOwned), false),
        ((StrandedFrameworkOwned, Untracked), false),
        ((StrandedFrameworkOwned, FrameworkOwned), false),
        ((StrandedFrameworkOwned, UserOwned), false),
        ((Custom, Untracked), false),
        ((Custom, FrameworkOwned), false),
        ((Custom, UserOwned), false),
    ];
    for ((class, ownership), expected) in table {
        assert_eq!(
            is_movable(class, ownership),
            expected,
            "({class:?}, {ownership:?}) must be {}movable",
            if expected { "" } else { "un" }
        );
    }
}

#[test]
fn empty_result_reports_no_change() {
    let mut result = QuarantineResult::default();
    assert!(!result.changed());
    result.refused.push("qa.md".to_owned());
    assert!(!result.changed(), "a refusal moved nothing");
}
