//! Tests for the review export's row verdicts and its writer (#8624).
//!
//! Every token is synthetic and assembled from pieces at run time.

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::engine::filter::{
    find_secret_spans, mask_content, MaskError, SecretSpan, SECRET_FILTER_VERSION,
};

use super::super::bridge::KuzuMemoryRow;
use super::export::*;
use super::ReviewError;

/// A bare mixed-case alphanumeric token: an exportable rule.
pub(super) fn mixed_token() -> String {
    format!("{}{}", "Xq7Wz2Lp9Vk4", "Tn8Rm3Bj6Hd5Gf1S")
}

/// A provider-prefixed token: an auto-refuse rule.
pub(super) fn provider_token() -> String {
    format!("{}_{}{}", "ghp", "Zq8Xw3Vt7Ny2Mk5Lj9Hb4", "Gc6Fd1Sa0Pe2Rr")
}

fn b64_token() -> String {
    format!("{}+{}/{}", "dGhpcyBpcyBh", "IHRlc3Qgb2Yg", "QmFzZTY0ZW5j2")
}

fn aws_token() -> String {
    format!("{}{}", "AKIA", "Q3RZ7XK2M9PLW4NB")
}

fn jwt_token() -> String {
    format!(
        "{}.{}.{}",
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
    )
}

fn pem_block() -> String {
    format!(
        "-----BEGIN RSA {}-----\n{}{}\n-----END RSA {}-----",
        "PRIVATE KEY",
        "MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWy",
        "F8PbnGy0AHB7MhgHcTz6sE2I2yPB",
        "PRIVATE KEY"
    )
}

pub(super) fn row(id: &str, content: &str) -> KuzuMemoryRow {
    serde_json::from_value(serde_json::json!({"id": id, "content": content})).expect("row parses")
}

/// Every 4-char window of `token` that occurs in `haystack`.
pub(super) fn leaked_windows(token: &str, haystack: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    chars
        .windows(4)
        .map(|w| w.iter().collect::<String>())
        .filter(|w| haystack.contains(w.as_str()))
        .collect()
}

/// Why (#8624 rule 6): only `base64_blob` and `mixed_case_alnum` rows are
/// exported; every other rule, a secret-shaped id, and a row holding any
/// auto-refuse span are counted and never written.
/// Test: itself.
#[test]
fn exportable_rows_are_masked_and_auto_refuse_rows_are_not() {
    let exportable = [
        (mixed_token(), "mixed_case_alnum"),
        (b64_token(), "base64_blob"),
    ];
    for (tok, rule) in &exportable {
        let RowVerdict::Export(r) = classify_row(&row("m-1", &format!("note {tok} end")), "s")
        else {
            panic!("{rule} row is exported");
        };
        assert_eq!(r.spans.len(), 1);
        assert_eq!(r.spans[0].rule, *rule);
        assert!(r.masked.starts_with("note ⟦S0:") && r.masked.ends_with("⟧ end"));
        assert!(leaked_windows(tok, &serde_json::to_string(&r).expect("json")).is_empty());
    }
    let auto = [
        (provider_token(), "provider_prefix"),
        (aws_token(), "aws_key_id"),
        (jwt_token(), "jwt"),
        (pem_block(), "private_key_block"),
        // One auto-refuse span refuses the whole row, exportable spans or not.
        (format!("{} {}", mixed_token(), jwt_token()), "jwt"),
    ];
    for (content, label) in &auto {
        assert_eq!(
            classify_row(&row("m-2", content), "s"),
            RowVerdict::AutoRefuse(label),
            "{label}"
        );
    }
    assert_eq!(
        classify_row(&row(&mixed_token(), "an ordinary note"), "s"),
        RowVerdict::AutoRefuse(SECRET_SHAPED_ID)
    );
    assert_eq!(
        classify_row(&row("m-3", "an ordinary synthetic note"), "s"),
        RowVerdict::Clean
    );
    assert_eq!(classify_row(&row("m-4", "   "), "s"), RowVerdict::Clean);
}

fn no_spans(_: &str) -> Vec<SecretSpan> {
    Vec::new()
}

fn mask_fails(_: &str, _: &[SecretSpan]) -> Result<String, MaskError> {
    Err(MaskError::Empty {
        index: 0,
        range: 0..0,
    })
}

/// Why (#8624 rule 7): a row the screen refuses but no span locates cannot
/// be masked, so it is auto-refused rather than written whole.
/// Test: itself.
#[test]
fn unlocated_refusal_is_auto_refused_never_exported() {
    let r = row("m-1", &format!("note {}", mixed_token()));
    assert_eq!(
        classify_row_with(&r, "s", no_spans, mask_content),
        RowVerdict::AutoRefuse(UNLOCATED)
    );
}

/// Why (#8624 rule 7): a masking error refuses the row; no fallback writes
/// it unmasked.
/// Test: itself.
#[test]
fn mask_error_refuses_the_row() {
    let r = row("m-1", &format!("note {}", mixed_token()));
    assert_eq!(
        classify_row_with(&r, "s", find_secret_spans, mask_fails),
        RowVerdict::AutoRefuse(MASK_ERROR)
    );
}

/// Why (#8624 rule 8): PR-C keys a verdict on the record, so it carries the
/// ledger key over the content the import would write, the filter version
/// and each span's shape, and never the content.
/// Test: itself.
#[test]
fn record_carries_key_version_and_shapes_but_no_content() {
    let tok = mixed_token();
    let content = format!("  keep {tok} here \n");
    let RowVerdict::Export(r) = classify_row(&row("m-9", &content), "/p/.kuzu-memory") else {
        panic!("exported");
    };
    assert_eq!(r.id, "m-9");
    assert_eq!(r.store, "/p/.kuzu-memory");
    assert_eq!(r.filter_version, SECRET_FILTER_VERSION);
    assert_eq!(r.key, ledger_key("/p/.kuzu-memory", "m-9", &content));
    assert_eq!(r.key.len(), 64);
    let shape = &r.spans[0];
    assert_eq!((shape.index, shape.len, shape.cls), (0, tok.len(), "uld"));
    assert!(shape.ent_tenths > 0);
    let v: serde_json::Value = serde_json::to_value(&r).expect("json");
    let mut keys: Vec<&str> = v
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "filter_version",
            "id",
            "key",
            "masked",
            "schema",
            "spans",
            "store"
        ]
    );
}

/// Why (#8624 rule 8): length prefixes keep field boundaries in the key.
/// Test: itself.
#[test]
fn ledger_key_separates_its_fields() {
    assert_ne!(ledger_key("ab", "c", "x"), ledger_key("a", "bc", "x"));
    assert_ne!(ledger_key("s", "i", "x"), ledger_key("s", "i", "y"));
    assert_eq!(ledger_key("s", "i", "x"), ledger_key("s", "i", "x"));
}

// ── the writer ──────────────────────────────────────────────────────────

/// The step a [`FaultFs`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The `n`th directory create (0 is the first missing ancestor).
    CreateDir(usize),
    CreateFile,
    /// The `n`th mode read.
    Mode(usize),
    /// The file is handed back read-only, so the write fails.
    Write,
    Rename,
}

/// [`OsFs`] with one step made to fail.
struct FaultFs {
    fail: Step,
    dirs: Cell<usize>,
    modes: Cell<usize>,
}

impl FaultFs {
    fn new(fail: Step) -> Self {
        Self {
            fail,
            dirs: Cell::new(0),
            modes: Cell::new(0),
        }
    }
    fn hit(&self, step: Step) -> io::Result<()> {
        match step == self.fail {
            true => Err(io::Error::other("injected")),
            false => Ok(()),
        }
    }
}

impl ReviewFs for FaultFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let n = self.dirs.replace(self.dirs.get() + 1);
        self.hit(Step::CreateDir(n))?;
        OsFs.create_dir(path)
    }
    fn create_file(&self, path: &Path) -> io::Result<File> {
        self.hit(Step::CreateFile)?;
        let f = OsFs.create_file(path)?;
        if self.fail == Step::Write {
            drop(f);
            return File::open(path);
        }
        Ok(f)
    }
    fn mode(&self, path: &Path) -> io::Result<u32> {
        let n = self.modes.replace(self.modes.get() + 1);
        self.hit(Step::Mode(n))?;
        OsFs.mode(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.hit(Step::Rename)?;
        OsFs.rename(from, to)
    }
}

pub(super) fn sample_record() -> MaskedRecord {
    let RowVerdict::Export(r) = classify_row(&row("m-1", &format!("a {}", mixed_token())), "s")
    else {
        panic!("exported");
    };
    r
}

fn admit(_: &Path) -> Result<(), ReviewError> {
    Ok(())
}

/// Every path under `root`, files and directories.
pub(super) fn tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&p) {
            for e in rd.flatten() {
                stack.push(e.path());
                out.push(e.path());
            }
        }
    }
    out
}

/// Why (#8624 rule 7): a failed directory create, file create, mode read,
/// write, rename or guard re-check aborts the export, and nothing it made —
/// no directory, no staging file, no partial `masked.jsonl` — is left.
/// Test: itself.
#[test]
fn each_failed_step_aborts_with_nothing_left_behind() {
    let records = vec![sample_record(), sample_record()];
    let steps = [
        // Three missing ancestors, then the run directory itself.
        Step::CreateDir(0),
        Step::CreateDir(2),
        Step::CreateDir(3),
        Step::CreateFile,
        Step::Mode(0),
        Step::Mode(3),
        // The staging file's mode.
        Step::Mode(4),
        Step::Write,
        Step::Rename,
    ];
    for step in steps {
        let tmp = tempfile::tempdir().expect("tmp");
        let run_dir = tmp.path().join("home/.trusty-tools/kuzu-review/run-1");
        let err = write_run(&FaultFs::new(step), &run_dir, &records, &admit);
        assert!(err.is_err(), "{step:?} aborts");
        assert_eq!(tree(tmp.path()), Vec::<PathBuf>::new(), "{step:?}");
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let run_dir = tmp.path().join("kuzu-review/run-1");
    let refuse = |p: &Path| -> Result<(), ReviewError> {
        Err(ReviewError::Guard {
            path: p.to_path_buf(),
            reason: "test".into(),
        })
    };
    let err = write_run(&OsFs, &run_dir, &records, &refuse).expect_err("guard refuses");
    assert!(matches!(err, ReviewError::Guard { .. }), "{err}");
    assert!(tree(tmp.path()).is_empty(), "a guard error leaves nothing");
}

/// [`OsFs`] whose directories are created with a world-readable mode.
struct LooseDirFs;

impl ReviewFs for LooseDirFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o755).create(path)
    }
    fn create_file(&self, path: &Path) -> io::Result<File> {
        OsFs.create_file(path)
    }
    fn mode(&self, path: &Path) -> io::Result<u32> {
        OsFs.mode(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        OsFs.rename(from, to)
    }
}

/// Why (#8624 rule 3): a directory that comes out looser than 0700 aborts
/// the export; nothing falls back to writing into it.
/// Test: itself.
#[test]
fn a_looser_mode_at_creation_aborts_the_export() {
    let tmp = tempfile::tempdir().expect("tmp");
    let run_dir = tmp.path().join("kuzu-review/run-1");
    let err = write_run(&LooseDirFs, &run_dir, &[sample_record()], &admit)
        .expect_err("a 0755 directory is refused");
    assert!(
        matches!(err, ReviewError::Mode { want: 0o700, .. }),
        "{err}"
    );
    assert!(tree(tmp.path()).is_empty());
}

/// Why (#8624): a run directory that already exists is never written into.
/// Test: itself.
#[test]
fn an_existing_run_directory_is_refused_and_kept() {
    let tmp = tempfile::tempdir().expect("tmp");
    let run_dir = tmp.path().join("run-1");
    std::fs::create_dir(&run_dir).expect("mkdir");
    std::fs::write(run_dir.join("keep"), b"x").expect("write");
    assert!(write_run(&OsFs, &run_dir, &[sample_record()], &admit).is_err());
    assert_eq!(tree(tmp.path()).len(), 2, "the existing tree is untouched");
}
