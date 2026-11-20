//! G0b: the IL packet parser, checker and apply against a `datg` store.
//!
//! Each test copies the scrubbed `pm-small` store into a temp directory as
//! `.trusty-mpm/datg/pm-small/` and applies a block with a pinned clock.
//! `il_apply_matches_datg_bytes` compares the result with the golden
//! `fixtures/ledger/il/regen_il.py` wrote by running `datg.py`.

use std::fs;
use std::path::{Path, PathBuf};

use trusty_architect::FakeClock;
use trusty_architect::ledger::il::{self, IlError};
use trusty_architect::ledger::store::StorePaths;
use trusty_architect::ledger::writer::Writer;

type R = Result<(), Box<dyn std::error::Error>>;

/// 2026-10-21T10:00:00Z, `regen_il.py`'s `START`.
const START: f64 = 1_792_576_800.0;

const BLOCK: &str = "done.\n\n```il\ntask: xa-T01\ndone: wired the parser\npct: 40\n\
close: xa-A01\nnext: run the gates\n\
act: merged the parser | ev: PR #1 | refs: crates/trusty-architect\n\
evidence: sha abc1234\n```\n";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ledger")
}

fn no_secrets(s: &str) -> Option<String> {
    s.contains("ghp_").then(|| "github token".to_owned())
}

fn seed(root: &Path) -> std::io::Result<StorePaths> {
    let dir = root.join(".trusty-mpm/datg/pm-small");
    fs::create_dir_all(&dir)?;
    for entry in fs::read_dir(fixtures().join("pm-small"))? {
        let entry = entry?;
        fs::copy(entry.path(), dir.join(entry.file_name()))?;
    }
    Ok(StorePaths::at(dir))
}

fn apply(paths: &StorePaths, text: &str) -> Result<il::Applied, IlError> {
    let clock = FakeClock::new(START);
    let scan = no_secrets;
    il::apply(
        &Writer::new(&clock, &scan),
        paths,
        "pm-small",
        "pm-small",
        text,
    )
}

fn snapshot(paths: &StorePaths) -> (String, String, String) {
    let read = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    (read(&paths.state), read(&paths.log), read(&paths.archive))
}

#[test]
fn il_apply_matches_datg_bytes() -> R {
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let (_, log0, arch0) = snapshot(&paths);
    let done = apply(&paths, BLOCK)?;
    assert!(!done.replayed);
    assert_eq!(done.acts, 1);
    assert_eq!(done.skipped, ["pct"]);
    let (state, log, arch) = snapshot(&paths);
    let gold = fixtures().join("il");
    assert_eq!(
        state,
        fs::read_to_string(gold.join("state.json"))?,
        "state.json"
    );
    assert_eq!(
        &arch[arch0.len()..],
        fs::read_to_string(gold.join("archive.tail"))?
    );
    // The packet id's `request` line is the one line datg does not write.
    let tail: String = log[log0.len()..]
        .lines()
        .filter(|l| !l.contains("\"kind\":\"request\""))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        tail,
        fs::read_to_string(gold.join("log.tail"))?,
        "log.jsonl"
    );
    assert!(state.contains("\"written_epoch\":1792576800"));
    Ok(())
}

#[test]
fn il_replayed_packet_is_a_no_op() -> R {
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let first = apply(&paths, BLOCK)?;
    let before = snapshot(&paths);
    let again = apply(&paths, &format!("relayed:\n{BLOCK}"))?;
    assert!(again.replayed);
    assert_eq!(again.packet, first.packet);
    assert_eq!(snapshot(&paths), before, "a replay writes nothing");
    Ok(())
}

fn block(rows: &str) -> String {
    format!("```il\n{rows}\n```")
}

#[test]
fn il_invalid_blocks_are_rejected_and_write_nothing() -> R {
    let long = format!("task: xa-T01\ndone: {}\npct: 1\nnext: n", "x".repeat(240));
    let cases = [
        ("no block here", "NO-BLOCK"),
        (&block(&long), "LONG"),
        (
            &block("task: xa-T01\ndone: d\npct: 1\nnew: X \"t\"\nnext: n"),
            "KIND",
        ),
        (
            &block("task: xa-T01\ndone: d\npct: 1\nnext: n\nact: did it\n  and more"),
            "FIELD",
        ),
        (
            &block("task: xa-T01\ndone: d\npct: 1\nnext: n\nact: did | when: now"),
            "ACT",
        ),
        (
            &block(
                "task: xa-T01\ndone: d\npct: 1\nnext: n\nact: x | goal: abcdefghijklmnopqrstuvwxyz",
            ),
            "ACT",
        ),
        (&block("task: xa-T01\ndone: d\npct: 140\nnext: n"), "PCT"),
        (&block("task: xa-T01\npct: 1\nnext: n"), "MISSING"),
        (&block("done: d\ntask: xa-T01\npct: 1\nnext: n"), "ORDER"),
        (&block("task: xa-T9z\ndone: d\npct: 1\nnext: n"), "STALE-ID"),
        (
            &block("task: xa-T01\ndone: ghp_abc\npct: 1\nnext: n"),
            "SECRET",
        ),
    ];
    for (text, code) in cases {
        let tmp = tempfile::tempdir()?;
        let paths = seed(tmp.path())?;
        let before = snapshot(&paths);
        match apply(&paths, text) {
            Err(e) => assert_eq!(e.code(), code, "{text}: {e}"),
            Ok(a) => panic!("{code}: applied {a:?}"),
        }
        assert_eq!(snapshot(&paths), before, "{code}: files changed");
    }
    Ok(())
}

#[test]
fn il_last_fence_wins_and_rows_split_like_python() {
    let text = "```il\ntask: a\n```\nthen\n```il  \ntask: b\nno colon\n```";
    let rows = il::parse_block(text).expect("a block");
    assert_eq!(rows[0], ("task".to_owned(), "b".to_owned()));
    assert_eq!(rows[1], ("?".to_owned(), "no colon".to_owned()));
    assert!(il::parse_block("```ilx\ntask: a\n```").is_none());
}

#[test]
fn il_write_failure_reports_an_error_and_leaves_the_store() -> R {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let _ = fs::remove_file(&paths.lock);
    let before = snapshot(&paths);
    fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o555))?;
    let got = apply(&paths, BLOCK);
    fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o755))?;
    match got {
        Err(e) => assert_eq!(e.code(), "WRITE", "{e}"),
        Ok(a) => panic!("a read-only store applied {a:?}"),
    }
    assert_eq!(snapshot(&paths), before);
    Ok(())
}

#[test]
fn route_sends_uncovered_blocks_to_ilio_and_runs_the_new_checks() -> R {
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let scan = no_secrets;
    let fence = |b: &str| format!("```il\n{b}\n```");
    let covered = fence("task: free: x\ndone: d\npct: 5\nnext: n\nact: ran it");
    assert_eq!(il::route(&paths, &covered, &scan)?, il::Route::Rust);
    let pulse = fence("task: xa-T01\ndone: d\npct: 5\nnext: n");
    let new = fence("task: free: x\ndone: d\npct: 5\nnew: A \"call\"\nnext: n");
    for (text, want) in [(&pulse, vec!["pct"]), (&new, vec!["new"])] {
        match il::route(&paths, text, &scan)? {
            il::Route::Ilio { packet, fields } => {
                assert_eq!(fields, want);
                il::mark_applied(&paths, &packet)?;
                assert_eq!(il::route(&paths, text, &scan)?, il::Route::Replay(packet));
            }
            other => panic!("{want:?}: {other:?}"),
        }
    }
    let mut st: serde_json::Value = serde_json::from_str(&fs::read_to_string(&paths.state)?)?;
    st["tasks"][0]["pct"] = serde_json::json!(40);
    fs::write(&paths.state, st.to_string())?;
    for (body, code) in [
        ("task: xa-To1\ndone: d\npct: 5\nnext: n", "BADID"),
        (
            "task: xa-T01\ndone: d\npct: 50\nremind: xa-T01 at=2026-12-01\nnext: n",
            "REMIND",
        ),
        ("task: xa-T01\ndone: d\npct: 40\nnext: n", "PCT-UNCHANGED"),
    ] {
        let got = il::route(&paths, &fence(body), &scan);
        assert!(
            matches!(&got, Err(IlError::Reject(r)) if r.code == code),
            "{code}: {got:?}"
        );
    }
    Ok(())
}

#[test]
fn il_secret_rules_match_ilio_on_the_rust_path() -> R {
    // ilio `SECRET`, beyond the injected scan (which only knows `ghp_`).
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let before = snapshot(&paths);
    let bad = [
        "act: rotated, password: hunter2hunter2",
        "act: set API-KEY = abcdefgh1234",
        "act: used sk-abcdefghij0123456789",
        "act: pasted -----begin rsa private key",
    ];
    for row in bad {
        let text = block(&format!("task: xa-T01\ndone: d\npct: 41\nnext: n\n{row}"));
        match apply(&paths, &text) {
            Err(e) => assert_eq!(e.code(), "SECRET", "{row}: {e}"),
            Ok(a) => panic!("{row}: applied {a:?}"),
        }
    }
    assert_eq!(snapshot(&paths), before);
    let ok = block("task: xa-T01\ndone: d\npct: 41\nnext: n\nact: password: short");
    apply(&paths, &ok)?;
    Ok(())
}

/// Fails the commit once, at `step`.
struct FailOnce(
    trusty_architect::ledger::writer::CommitStep,
    std::cell::Cell<bool>,
);

impl trusty_architect::ledger::writer::Faults for FailOnce {
    fn at(
        &self,
        step: trusty_architect::ledger::writer::CommitStep,
        _: &StorePaths,
    ) -> std::io::Result<()> {
        if step == self.0 && !self.1.replace(true) {
            return Err(std::io::Error::other("injected act failure"));
        }
        Ok(())
    }
}

#[test]
fn il_failed_act_append_is_retried_and_lands_once() -> R {
    use trusty_architect::ledger::writer::CommitStep;
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let clock = FakeClock::new(START);
    let scan = no_secrets;
    let fault = FailOnce(CommitStep::Acts, std::cell::Cell::new(false));
    let w = Writer {
        faults: &fault,
        ..Writer::new(&clock, &scan)
    };
    let first = il::apply(&w, &paths, "pm-small", "pm-small", BLOCK);
    assert_eq!(
        first.as_ref().map_err(IlError::code).err(),
        Some("WRITE-ACT")
    );
    let again = il::apply(&w, &paths, "pm-small", "pm-small", BLOCK)?;
    assert!(!again.replayed && again.acts == 1, "{again:?}");
    let third = il::apply(&w, &paths, "pm-small", "pm-small", BLOCK)?;
    assert!(third.replayed, "{third:?}");
    let log = fs::read_to_string(&paths.log)?;
    assert_eq!(log.matches("merged the parser").count(), 1, "{log}");
    Ok(())
}

#[test]
fn il_pending_marker_reads_unknown_until_applied() -> R {
    let tmp = tempfile::tempdir()?;
    let paths = seed(tmp.path())?;
    let held = il::hold(&paths, std::time::Duration::from_secs(1))?;
    // A second holder waits out its bound and is refused.
    assert!(il::hold(&paths, std::time::Duration::from_millis(50)).is_err());
    drop(held);
    assert!(!il::pending_unknown(&paths, "il-x")?);
    il::mark_pending(&paths, "il-x")?;
    assert!(il::pending_unknown(&paths, "il-x")?);
    il::mark_applied(&paths, "il-x")?;
    assert!(!il::pending_unknown(&paths, "il-x")?);
    il::clear_pending(&paths, "il-x")?;
    il::clear_pending(&paths, "il-x")?;
    Ok(())
}
