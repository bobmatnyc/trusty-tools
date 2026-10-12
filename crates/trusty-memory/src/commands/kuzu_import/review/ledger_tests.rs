//! Tests for the review ledger file and `import kuzu review record|status|finish`
//! (#8624, PR-C).
//!
//! Every token is synthetic. The umask test runs its scenario in a child copy
//! of this test binary, because `umask(2)` is process-global.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::engine::filter::SECRET_FILTER_VERSION;

use super::super::mapping::map_memory;
use super::carve_out::{is_high_entropy, HIGH_ENTROPY_MIN_LEN, HIGH_ENTROPY_TENTHS};
use super::cli::{
    run_review, status, KuzuCommand, RecordArgs, ReviewCommand, RunArgs, VerdictKind,
};
use super::export::{classify_mapped, ledger_key, write_run, MaskedRecord, OsFs, RowVerdict};
use super::export_tests::{leaked_windows, mixed_token, provider_token, row};
use super::ledger::{
    append_entry, load_sealed, parse_ledger, seal, LedgerError, Lookup, LEDGER_FILE, SEAL_FILE,
};
use super::verdict::{verdict_digest, LedgerEntry, RedactOp, Verdict};

/// The store string every fixture row is keyed under.
pub(super) const STORE: &str = "/synthetic/proj/.kuzu-memory";
/// A run name of the shape `run_name` makes.
pub(super) const RUN: &str = "20261012T110000Z-abcdef12";

/// 24 chars, 12 distinct twice: flagged `mixed_case_alnum`, ent 3.6.
pub(super) fn low_token() -> String {
    format!("{}{}", "Yt6Uw3Rc8Pz5", "Yt6Uw3Rc8Pz5")
}

/// 24 distinct chars: ent 4.6 at length 24, high-entropy.
pub(super) fn high_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4rS6")
}

/// 24 chars, 23 distinct: ent rounds to exactly 4.5 at length 24.
pub(super) fn threshold_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4rSa")
}

/// 24 distinct chars with a `+`: flagged `base64_blob`, ent 4.6, high-entropy.
pub(super) fn high_b64_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4r+6")
}

/// 24 chars, 23 distinct, with a `+`: `base64_blob` at exactly ent 4.5.
pub(super) fn threshold_b64_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4r+a")
}

/// 23 chars, all distinct, with a `+`: `base64_blob` at ent 4.5 but too short.
pub(super) fn short_b64_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4r+")
}

/// 24 chars, 22 distinct, with a `+`: `base64_blob` at ent 4.4, long enough.
pub(super) fn low_ent_b64_token() -> String {
    format!("{}{}", "aB3dE5fG7hJ9", "kL1mN2pQ4+aB")
}

/// The mapped memory and its export record for `content` under `id`.
pub(super) fn exported(
    id: &str,
    content: &str,
) -> (super::super::mapping::MappedMemory, MaskedRecord) {
    let m = map_memory(&row(id, content), STORE).expect("maps");
    let RowVerdict::Export(r) = classify_mapped(&m) else {
        panic!("row {id} is exported");
    };
    (m, r)
}

/// A ledger parsed from `entries`, one line each.
pub(super) fn ledger_bytes(entries: &[LedgerEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend(serde_json::to_string(e).expect("json").as_bytes());
        out.push(b'\n');
    }
    out
}

/// A run directory under `root` holding the export records of `rows`.
fn make_run(root: &Path, rows: &[(&str, String)]) -> (PathBuf, Vec<MaskedRecord>) {
    let records: Vec<MaskedRecord> = rows.iter().map(|(id, c)| exported(id, c).1).collect();
    let dir = root.join(RUN);
    write_run(&OsFs, &dir, &records, &|_| Ok(())).expect("run written");
    (dir, records)
}

fn rec(key: &str, verdict: VerdictKind, ops: &[RedactOp]) -> ReviewCommand {
    ReviewCommand::Record(RecordArgs {
        run: RUN.to_string(),
        key: key.to_string(),
        verdict,
        ops: ops.to_vec(),
    })
}

fn run(root: &Path, cmd: &ReviewCommand) -> Result<String, LedgerError> {
    let mut out = Vec::new();
    run_review(cmd, root, &[], &mut out)?;
    Ok(String::from_utf8(out).expect("utf8"))
}

fn rejected(r: Result<String, LedgerError>) -> String {
    match r {
        Err(LedgerError::Rejected(m)) => m,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// Why (#8624 rule 5): `--op` takes an op name and a span index, nothing else.
/// Test: itself.
#[test]
fn op_parses_name_and_index_only() {
    assert_eq!(
        "redact_span:0".parse(),
        Ok(RedactOp::RedactSpan { span: 0 })
    );
    assert_eq!(
        "delete_span:2".parse(),
        Ok(RedactOp::DeleteSpan { span: 2 })
    );
    assert_eq!("drop_line:11".parse(), Ok(RedactOp::DropLine { span: 11 }));
    for bad in [
        "redact:0",
        "redact_span",
        "redact_span:x",
        "redact_span:0:text",
        "",
    ] {
        assert!(bad.parse::<RedactOp>().is_err(), "{bad:?} parses");
    }
}

/// Why (#8624): the digest covers the key, the filter version and the whole
/// verdict, and a line carries no field beyond the closed set.
/// Test: itself.
#[test]
fn digest_binds_key_version_and_verdict() {
    let ops = vec![RedactOp::RedactSpan { span: 0 }];
    let base = verdict_digest("k", 1, &Verdict::AdmitRedacted { ops: ops.clone() });
    let others = [
        verdict_digest("k2", 1, &Verdict::AdmitRedacted { ops: ops.clone() }),
        verdict_digest("k", 2, &Verdict::AdmitRedacted { ops: ops.clone() }),
        verdict_digest("k", 1, &Verdict::Admit {}),
        verdict_digest("k", 1, &Verdict::KeepRefused {}),
        verdict_digest(
            "k",
            1,
            &Verdict::AdmitRedacted {
                ops: vec![RedactOp::DeleteSpan { span: 0 }],
            },
        ),
        verdict_digest(
            "k",
            1,
            &Verdict::AdmitRedacted {
                ops: vec![RedactOp::RedactSpan { span: 1 }],
            },
        ),
    ];
    for d in others {
        assert_ne!(d, base);
    }
    let e = LedgerEntry::new("k".into(), 1, Verdict::AdmitRedacted { ops });
    let line = serde_json::to_string(&e).expect("json");
    assert_eq!(
        serde_json::from_str::<LedgerEntry>(&line).expect("parses"),
        e
    );
    assert!(e.digest_matches());
    let extra_field = line.replacen("\"schema\"", "\"note\":\"admit\",\"schema\"", 1);
    let extra_op_field = line.replacen("\"span\":0", "\"span\":0,\"text\":\"x\"", 1);
    let extra_verdict_field =
        serde_json::to_string(&LedgerEntry::new("k".into(), 1, Verdict::Admit {}))
            .expect("json")
            .replacen(
                "{\"kind\":\"admit\"",
                "{\"kind\":\"admit\",\"why\":\"x\"",
                1,
            );
    for bad in [extra_field, extra_op_field, extra_verdict_field] {
        assert!(
            serde_json::from_str::<LedgerEntry>(&bad).is_err(),
            "{bad} parses"
        );
    }
}

/// Why (#8624 rules 3, 5 — fail-open): a truncated line must never read as
/// admit. Every cut of an admit line refuses the whole ledger, and a cut
/// line after a valid admit refuses it too, rather than being skipped (a
/// skipped `keep-refused` would have made that key a duplicate).
/// Test: itself.
#[test]
fn truncated_ledger_line_never_reads_as_admit() {
    let p = Path::new("/synthetic/ledger.jsonl");
    let admit =
        serde_json::to_string(&LedgerEntry::new("k".into(), 1, Verdict::Admit {})).expect("json");
    for cut in 1..admit.len() {
        let bytes = format!("{}\n", &admit[..cut]);
        match parse_ledger(p, bytes.as_bytes()) {
            Err(LedgerError::Corrupt { line: 1, .. }) => {}
            other => panic!("cut at {cut} read as {other:?}"),
        }
    }
    let keep = serde_json::to_string(&LedgerEntry::new("k".into(), 1, Verdict::KeepRefused {}))
        .expect("json");
    let bytes = format!("{admit}\n{}\n", &keep[..keep.len() / 2]);
    match parse_ledger(p, bytes.as_bytes()) {
        Err(e @ LedgerError::Corrupt { line: 2, .. }) => {
            assert!(e.to_string().contains("line 2"), "{e}");
        }
        other => panic!("a cut second line read as {other:?}"),
    }
    let full = parse_ledger(p, format!("{admit}\n{keep}\n").as_bytes()).expect("parses");
    assert_eq!(full.lookup("k"), Lookup::Duplicate);
}

/// Why (#8624): a last line without its newline, a blank line, or a line of
/// another schema is a cut or foreign write, and refuses the ledger.
/// Test: itself.
#[test]
fn unterminated_last_line_refuses_the_ledger() {
    let p = Path::new("/synthetic/ledger.jsonl");
    let admit =
        serde_json::to_string(&LedgerEntry::new("k".into(), 1, Verdict::Admit {})).expect("json");
    let schema2 = admit.replacen("\"schema\":1", "\"schema\":2", 1);
    for bad in [
        admit.clone(),
        format!("{admit}\n\n"),
        format!("{schema2}\n"),
    ] {
        assert!(
            matches!(
                parse_ledger(p, bad.as_bytes()),
                Err(LedgerError::Corrupt { .. })
            ),
            "{bad:?} parses"
        );
    }
    assert_eq!(parse_ledger(p, b"").expect("empty").lines(), 0);
}

/// Why (#8624): the import applies only a sealed ledger, byte for byte.
/// Test: itself.
#[test]
fn unsealed_or_edited_ledger_is_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, records) = make_run(tmp.path(), &[("m-1", format!("n {}", low_token()))]);
    let path = dir.join(LEDGER_FILE);
    let entry = LedgerEntry::new(
        records[0].key.clone(),
        SECRET_FILTER_VERSION,
        Verdict::KeepRefused {},
    );
    append_entry(&path, &entry).expect("append");
    assert!(matches!(
        load_sealed(&path),
        Err(LedgerError::Unsealed { .. })
    ));
    seal(&dir).expect("seal");
    assert_eq!(load_sealed(&path).expect("sealed").lines(), 1);
    assert!(matches!(seal(&dir), Err(LedgerError::Sealed { .. })));
    // A verdict flipped after the seal, digest recomputed, does not load.
    let flipped = ledger_bytes(&[LedgerEntry::new(
        records[0].key.clone(),
        SECRET_FILTER_VERSION,
        Verdict::Admit {},
    )]);
    std::fs::write(&path, &flipped).expect("edit");
    assert!(matches!(
        load_sealed(&path),
        Err(LedgerError::SealMismatch { .. })
    ));
    std::fs::write(&path, ledger_bytes(&[entry.clone(), entry])).expect("append");
    assert!(matches!(
        load_sealed(&path),
        Err(LedgerError::SealMismatch { .. })
    ));
}

/// Why (#8624): run names come from `run_name`, and a run directory is
/// owner-only; anything else is refused before a file is read.
/// Test: itself.
#[test]
fn run_names_and_modes_are_checked() {
    let tmp = tempfile::tempdir().expect("tmp");
    make_run(tmp.path(), &[("m-1", format!("n {}", low_token()))]);
    for bad in ["", "../x", "a/b", ".", "run name"] {
        let cmd = ReviewCommand::Status(RunArgs {
            run: bad.to_string(),
        });
        assert!(
            matches!(run(tmp.path(), &cmd), Err(LedgerError::BadRun(_))),
            "{bad:?}"
        );
    }
    let status_cmd = ReviewCommand::Status(RunArgs {
        run: RUN.to_string(),
    });
    run(tmp.path(), &status_cmd).expect("status");
    set_mode(&tmp.path().join(RUN), 0o755);
    let err = run(tmp.path(), &status_cmd).expect_err("loose run dir");
    assert!(err.to_string().contains("755"), "{err}");
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Why (#8624 items 1, 2): record stores one verdict per row, status counts
/// reviewed against pending, and finish seals the run so nothing more can be
/// recorded; no token reaches the output or the ledger.
/// Test: itself.
#[test]
fn record_status_finish_flow() {
    let tmp = tempfile::tempdir().expect("tmp");
    let rows = [
        ("m-1", format!("label {}", low_token())),
        ("m-2", format!("deploy key {} rotated", high_token())),
        ("m-3", format!("note {} end", mixed_token())),
        ("m-4", format!("left {} pending", threshold_token())),
    ];
    let (dir, records) = make_run(tmp.path(), &rows);
    let key = |i: usize| records[i].key.clone();
    let s = status(&dir).expect("status");
    assert_eq!((s.exported, s.pending, s.sealed), (4, 4, false));
    let mut out = String::new();
    out += &run(tmp.path(), &rec(&key(0), VerdictKind::Admit, &[])).expect("admit");
    out += &run(
        tmp.path(),
        &rec(
            &key(1),
            VerdictKind::AdmitRedacted,
            &[RedactOp::RedactSpan { span: 0 }],
        ),
    )
    .expect("admit-redacted");
    out += &run(tmp.path(), &rec(&key(2), VerdictKind::KeepRefused, &[])).expect("keep");
    assert!(out.contains("recorded admit-redacted"), "{out}");
    let again = rejected(run(
        tmp.path(),
        &rec(&key(2), VerdictKind::KeepRefused, &[]),
    ));
    assert!(again.contains("already has a verdict"), "{again}");
    let s = status(&dir).expect("status");
    assert_eq!(
        (
            s.admit,
            s.admit_redacted,
            s.keep_refused,
            s.pending,
            s.invalid,
            s.unknown
        ),
        (1, 1, 1, 1, 0, 0)
    );
    out += &run(
        tmp.path(),
        &ReviewCommand::Status(RunArgs { run: RUN.into() }),
    )
    .expect("status");
    out += &run(
        tmp.path(),
        &ReviewCommand::Finish(RunArgs { run: RUN.into() }),
    )
    .expect("finish");
    assert!(out.contains("3 verdict(s); 1 pending"), "{out}");
    assert!(matches!(
        run(tmp.path(), &rec(&key(3), VerdictKind::KeepRefused, &[])),
        Err(LedgerError::Sealed { .. })
    ));
    assert!(status(&dir).expect("status").sealed);
    let ledger = load_sealed(&dir.join(LEDGER_FILE)).expect("sealed ledger");
    assert_eq!(ledger.lines(), 3);
    let mut seen = out;
    for f in [LEDGER_FILE, SEAL_FILE] {
        seen.push_str(&std::fs::read_to_string(dir.join(f)).expect("read"));
    }
    for (_, content) in &rows {
        for tok in [low_token(), high_token(), mixed_token(), threshold_token()] {
            if content.contains(&tok) {
                assert!(leaked_windows(&tok, &seen).is_empty(), "token leaked");
            }
        }
    }
}

/// Why (#8624 rule 5): a bad op list is refused at record time.
/// Test: itself.
#[test]
fn record_refuses_bad_ops() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (_, records) = make_run(
        tmp.path(),
        &[("m-1", format!("deploy {} key", high_b64_token()))],
    );
    let k = records[0].key.clone();
    let cases: [(VerdictKind, Vec<RedactOp>, &str); 5] = [
        (
            VerdictKind::AdmitRedacted,
            vec![RedactOp::RedactSpan { span: 1 }],
            "names span 1",
        ),
        (VerdictKind::AdmitRedacted, vec![], "at least one op"),
        (
            VerdictKind::AdmitRedacted,
            vec![
                RedactOp::RedactSpan { span: 0 },
                RedactOp::DropLine { span: 0 },
            ],
            "more than one op",
        ),
        (
            VerdictKind::KeepRefused,
            vec![RedactOp::RedactSpan { span: 0 }],
            "only for admit-redacted",
        ),
        (VerdictKind::Admit, vec![], "carve-out"),
    ];
    for (verdict, ops, want) in cases {
        let msg = rejected(run(tmp.path(), &rec(&k, verdict, &ops)));
        assert!(msg.contains(want), "{verdict:?} {ops:?}: {msg}");
    }
    let unknown = rejected(run(
        tmp.path(),
        &rec(&"0".repeat(64), VerdictKind::KeepRefused, &[]),
    ));
    assert!(unknown.contains("no exported row"), "{unknown}");
    assert!(
        !tmp.path().join(RUN).join(LEDGER_FILE).exists(),
        "nothing recorded"
    );
}

/// Why (#8624 rule 7 — fail-open): a row of an auto-refuse rule is never
/// exported, so record has no row to admit; and an export line forged to
/// carry such a span is still refused any admit verdict.
/// Test: itself.
#[test]
fn record_rejects_admit_for_an_auto_refuse_rule() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut forged = exported("m-1", &format!("note {}", high_token())).1;
    forged.spans[0].rule = "provider_prefix";
    let dir = tmp.path().join(RUN);
    write_run(&OsFs, &dir, &[forged.clone()], &|_| Ok(())).expect("run");
    for (verdict, ops) in [
        (VerdictKind::Admit, vec![]),
        (
            VerdictKind::AdmitRedacted,
            vec![RedactOp::RedactSpan { span: 0 }],
        ),
    ] {
        let msg = rejected(run(tmp.path(), &rec(&forged.key, verdict, &ops)));
        assert!(msg.contains("auto-refuse"), "{msg}");
    }
    let content = format!("ci {} env", provider_token());
    let provider_key = ledger_key(STORE, "m-2", &content);
    let msg = rejected(run(
        tmp.path(),
        &rec(&provider_key, VerdictKind::Admit, &[]),
    ));
    assert!(msg.contains("no exported row"), "{msg}");
    run(tmp.path(), &rec(&forged.key, VerdictKind::KeepRefused, &[]))
        .expect("keep-refused is fine");
}

/// Why (#8624 rule 8): record refuses a verbatim admit at the entropy
/// threshold, so the agent learns it before the import.
/// Test: itself.
#[test]
fn record_rejects_admit_above_the_carve_out_threshold() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (_, records) = make_run(
        tmp.path(),
        &[("m-1", format!("x {}", threshold_b64_token()))],
    );
    assert_eq!(
        (records[0].spans[0].ent_tenths, records[0].spans[0].len),
        (45, 24)
    );
    let msg = rejected(run(
        tmp.path(),
        &rec(&records[0].key, VerdictKind::Admit, &[]),
    ));
    assert!(msg.contains("carve-out"), "{msg}");
}

/// Why (Architect ruling 08:48Z): the threshold is 4.5 bits/char on the
/// rounded tenths, with a minimum length of 24.
/// Test: itself.
#[test]
fn carve_out_threshold_is_pinned_at_45_tenths_and_24_chars() {
    assert_eq!((HIGH_ENTROPY_TENTHS, HIGH_ENTROPY_MIN_LEN), (45, 24));
    assert!(is_high_entropy(24, 45));
    assert!(is_high_entropy(usize::MAX, u16::MAX));
    assert!(!is_high_entropy(24, 44));
    assert!(!is_high_entropy(23, 45));
}

/// A pre-existing ledger with a looser mode, or a symlink in its place, is
/// refused rather than appended to.
/// Test: itself.
#[cfg(unix)]
#[test]
fn record_refuses_a_loose_or_symlinked_ledger() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, records) = make_run(tmp.path(), &[("m-1", format!("n {}", low_token()))]);
    let path = dir.join(LEDGER_FILE);
    std::fs::write(&path, b"").expect("ledger");
    set_mode(&path, 0o644);
    let cmd = rec(&records[0].key, VerdictKind::KeepRefused, &[]);
    assert!(run(tmp.path(), &cmd)
        .expect_err("loose")
        .to_string()
        .contains("644"));
    std::fs::remove_file(&path).expect("rm");
    let elsewhere = tmp.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, b"").expect("target");
    std::os::unix::fs::symlink(&elsewhere, &path).expect("symlink");
    assert!(run(tmp.path(), &cmd).is_err(), "symlinked ledger");
    assert_eq!(std::fs::read(&elsewhere).expect("read"), b"");
}

/// Names the child scenario's scratch directory; set only by the parent.
const CHILD_ENV: &str = "TRUSTY_MEMORY_8624_LEDGER_UMASK_CHILD";
const CHILD_TEST: &str =
    "commands::kuzu_import::review::ledger_tests::umask_000_ledger_child_scenario";

/// The child half of `umask_000_ledger_and_seal_are_owner_only`.
///
/// Test: `umask_000_ledger_and_seal_are_owner_only`.
#[test]
fn umask_000_ledger_child_scenario() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let root = PathBuf::from(dir);
    // SAFETY: this child process runs this one test on one thread.
    unsafe { libc::umask(0) };
    let (run_dir, records) = make_run(&root, &[("m-1", format!("n {}", low_token()))]);
    run(&root, &rec(&records[0].key, VerdictKind::Admit, &[])).expect("record");
    run(&root, &ReviewCommand::Finish(RunArgs { run: RUN.into() })).expect("finish");
    let mut report = std::io::stdout();
    for f in [LEDGER_FILE, SEAL_FILE] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::symlink_metadata(run_dir.join(f))
            .expect("meta")
            .permissions()
            .mode()
            & 0o7777;
        let _ = writeln!(report, "mode {f} {mode:o}");
    }
}

/// Why (#8624 item 2): under umask 000 the ledger and the seal are still
/// created 0600, because the mode is set by the creating call.
/// Test: itself, through `umask_000_ledger_child_scenario`.
#[test]
fn umask_000_ledger_and_seal_are_owner_only() {
    let tmp = tempfile::tempdir().expect("tmp");
    let exe = std::env::current_exe().expect("test binary");
    let out = std::process::Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, tmp.path())
        .output()
        .expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "child failed:\n{stdout}");
    assert!(
        stdout.contains("1 passed"),
        "the child scenario ran:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("mode {LEDGER_FILE} 600")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("mode {SEAL_FILE} 600")),
        "{stdout}"
    );
}

/// Why (#8624 item 1): the review subcommands parse under `import kuzu`, and
/// take none of the import's flags.
/// Test: itself.
#[test]
fn review_subcommands_parse_and_take_no_import_flags() {
    use super::super::ImportSource;
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        source: ImportSource,
    }
    let parse =
        |args: &[&str]| Cli::try_parse_from(std::iter::once("t").chain(args.iter().copied()));
    let ImportSource::Kuzu(a) = parse(&[
        "kuzu",
        "review",
        "record",
        "--run",
        RUN,
        "--key",
        "abc",
        "--verdict",
        "admit-redacted",
        "--op",
        "redact_span:0",
        "--op",
        "drop_line:1",
    ])
    .expect("record parses")
    .source;
    let Some(KuzuCommand::Review(ReviewCommand::Record(r))) = a.command else {
        panic!("record subcommand");
    };
    assert_eq!(r.verdict, VerdictKind::AdmitRedacted);
    assert_eq!(
        r.ops,
        vec![
            RedactOp::RedactSpan { span: 0 },
            RedactOp::DropLine { span: 1 }
        ]
    );
    assert!(parse(&["kuzu", "review", "status", "--run", RUN]).is_ok());
    assert!(parse(&["kuzu", "review", "finish", "--run", RUN]).is_ok());
    assert!(parse(&[
        "kuzu",
        "review",
        "record",
        "--run",
        RUN,
        "--key",
        "k",
        "--verdict",
        "admit",
        "--op",
        "replace:0"
    ])
    .is_err());
    assert!(parse(&["kuzu", "--dry-run", "review", "status", "--run", RUN]).is_err());
    let ImportSource::Kuzu(a) = parse(&["kuzu", "--from", "/s", "--review-ledger", "/l.jsonl"])
        .expect("ledger flag parses")
        .source;
    assert_eq!(a.review_ledger, Some(PathBuf::from("/l.jsonl")));
}
