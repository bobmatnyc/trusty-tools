//! The two guards `palace legacy-kg` runs around its import (#8434).
//!
//! Why: the import upserts legacy rows verbatim, outside the `memory_remember`
//! write pipeline, so without a screen it would store credentials and noise a
//! live write refuses. It also rewrites `kg.redb` and the vector index, and a
//! rewrite with no copy of the prior bytes has no way back.
//! What: [`screen_drawers`] runs each candidate through the gates a live
//! `memory_remember` write runs and returns the refused ids with a reason,
//! never the content. [`backup_stores`] copies every store file the apply can
//! modify into a timestamped directory inside the palace dir — beside
//! `kg.redb.pre-compact.bak`, where `palace compact` keeps its backup — and
//! verifies each copy by size and SHA-256 before anything is written.
//! Test: `credential_row_is_rejected_in_dry_run_and_apply`,
//! `noise_row_is_rejected_in_dry_run_and_apply`,
//! `apply_with_a_failing_backup_writes_nothing`,
//! `apply_leaves_a_verified_backup_of_the_pre_apply_bytes`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use trusty_common::memory_core::filter::{check_secret, FilterReject};
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::retrieval::RememberOptions;
use uuid::Uuid;

use std::panic::AssertUnwindSafe;
use trusty_common::memory_core::store::concurrent_open::try_open_or_snapshot;
use trusty_common::memory_core::store::OpenIntent;

use super::{COPIED_SUFFIXES, INDEX_FILE, LEGACY_KG_FILE};
use crate::commands::store_snapshot::{with_store_copy, KG_FILE, SCRATCH_PREFIX};
use crate::tools::helpers::{blocklist_gate, content_gate, mcp_remember_opts};

/// Name prefix of a backup directory inside the palace dir.
pub(crate) const BACKUP_PREFIX: &str = "legacy-kg-backup-";

/// Checksum file written into every backup, in `shasum -a 256 -c` format.
pub(crate) const MANIFEST_FILE: &str = "MANIFEST.sha256";

/// The per-file copy [`backup_stores`] runs; a seam for the failure tests.
pub(crate) type CopyFn = fn(&Path, &Path) -> std::io::Result<u64>;

/// Why a legacy drawer is held back from the import. Carries no content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// `check_secret` found a credential-shaped token.
    Secret,
    /// A quality gate refused it; the label names the gate.
    Noise(&'static str),
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret => f.write_str("secret"),
            Self::Noise(kind) => write!(f, "noise ({kind})"),
        }
    }
}

/// One legacy drawer the screen refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: Uuid,
    pub reason: RejectReason,
}

/// The first gate a live `memory_remember` write would refuse `content` on.
///
/// Why: #8434 — an imported row must meet the bar a live write meets, and
/// "the same bar" holds only while it is the same code, so this calls the live
/// gates rather than restating them.
/// What: `None` when a live write stores `content`. Runs `check_secret` first,
/// so a row that is both a secret and noise is reported as a secret. Then the
/// MCP handler's `blocklist_gate` and `content_gate` (`too_few_words`, which
/// `memory_note` also applies), then `FilterConfig::apply` with the handler's
/// own options (`mcp_remember_opts`, no `force`), as `run_pipeline` does. The
/// filter runs first without its minimum-token check, so `too_short` means
/// the 8-token minimum is the only gate refusing the row. `allow_short` skips
/// that check alone, as `memory_note` does; nothing skips `check_secret`.
/// Test: `credential_row_is_rejected_in_dry_run_and_apply`,
/// `noise_row_is_rejected_in_dry_run_and_apply`,
/// `short_row_is_rejected_by_default_and_imported_with_allow_short`,
/// `short_secret_row_is_rejected_even_with_allow_short`,
/// `word_count_and_blocklist_rows_are_rejected_with_and_without_allow_short`.
fn screen_content(
    opts: &RememberOptions,
    content: &str,
    allow_short: bool,
) -> Option<RejectReason> {
    if check_secret(content.trim()).is_err() {
        return Some(RejectReason::Secret);
    }
    if blocklist_gate(content).is_some() {
        return Some(RejectReason::Noise("blocklisted"));
    }
    if content_gate(content, None, false).is_none() {
        return Some(RejectReason::Noise("too_few_words"));
    }
    // #8434: the minimum-token check last, so `too_short` is the only reason.
    let enforce = [false, opts.enforce_min_tokens && !allow_short];
    enforce
        .into_iter()
        .find_map(|min| match opts.filter.apply(content, min) {
            Ok(()) => None,
            Err(FilterReject::PotentialSecret { .. }) => Some(RejectReason::Secret),
            Err(FilterReject::TooShort { .. }) => Some(RejectReason::Noise(TOO_SHORT)),
            Err(FilterReject::NoisePattern { .. }) => Some(RejectReason::Noise("noise_pattern")),
            Err(FilterReject::NonAlphabetic { .. }) => Some(RejectReason::Noise("non_alphabetic")),
        })
}

/// The label of the 8-token minimum, the one gate `--allow-short` skips.
pub(crate) const TOO_SHORT: &str = "too_short";

/// Split `drawers` into `(passed, rejected)` by [`screen_content`].
///
/// Why: see [`screen_content`]; the dry run and the apply both call this, so
/// the counts the owner reads are the ones the apply acts on.
/// What: order-preserving; a rejected drawer keeps only its id and reason.
/// Test: `credential_row_is_rejected_in_dry_run_and_apply`.
pub(crate) fn screen_drawers(
    drawers: impl IntoIterator<Item = Drawer>,
    allow_short: bool,
) -> (Vec<Drawer>, Vec<Rejected>) {
    let opts = mcp_remember_opts(false, false, false);
    let mut passed = Vec::new();
    let mut rejected = Vec::new();
    for d in drawers {
        match screen_content(&opts, d.content(), allow_short) {
            None => passed.push(d),
            Some(reason) => rejected.push(Rejected { id: d.id, reason }),
        }
    }
    (passed, rejected)
}

/// Refuse to import when a `Writer` open would recreate a store (#8434).
///
/// Why: `OpenIntent::Writer` renames an incompatible-format `kg.redb` or
/// vector index aside and creates it empty (`concurrent_open.rs`, #702). An
/// import must not be the step that does that.
/// What: opens private copies of both files the way the dry run does and
/// returns the first failure; an absent file passes.
/// Test: `apply_refuses_a_store_the_writer_open_would_rename_aside`.
pub(crate) fn probe_stores(data_dir: &Path) -> Result<()> {
    with_store_copy(data_dir, &std::env::temp_dir(), |s| s.load_drawer_ids())
        .context("kg.redb failed a read-only probe; refusing to open it for writing")?;
    let live = data_dir.join(INDEX_FILE);
    if !live
        .try_exists()
        .with_context(|| format!("cannot stat {}", live.display()))?
    {
        return Ok(());
    }
    let scratch = tempfile::TempDir::with_prefix_in(SCRATCH_PREFIX, std::env::temp_dir())
        .context("create scratch dir for the vector index probe")?;
    let copy = scratch.path().join(INDEX_FILE);
    std::fs::copy(&live, &copy).with_context(|| format!("copy {}", live.display()))?;
    // redb can panic on a torn file; see `with_store_copy`.
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        try_open_or_snapshot(&copy, OpenIntent::ReadOnlyClient).map(drop)
    })) {
        Ok(opened) => opened.with_context(|| {
            format!(
                "{} failed a read-only probe; refusing to open it for writing",
                live.display()
            )
        }),
        Err(_) => bail!("panic while probing a copy of {}", live.display()),
    }
}

/// One store file copied into the backup, with the digest both sides matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackedUpFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

/// A verified backup: its directory and every file in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub dir: PathBuf,
    pub files: Vec<BackedUpFile>,
}

/// Every palace file an apply can modify, by name.
fn store_file_names() -> Vec<String> {
    let mut names = vec![KG_FILE.to_string(), INDEX_FILE.to_string()];
    names.extend(
        COPIED_SUFFIXES
            .iter()
            .map(|s| format!("{LEGACY_KG_FILE}{s}")),
    );
    names
}

/// Copy and verify every store file the apply can modify, before it writes.
///
/// Why: #8434 — the apply rewrites `kg.redb` and the vector index of palaces
/// whose legacy rows exist nowhere else. A backup that failed silently would
/// look like one that succeeded, so any failure must stop the apply.
/// What: creates `<parent>/legacy-kg-backup-<UTC timestamp>-<8 hex>/` (never reusing
/// an existing dir) and copies each present file of [`store_file_names`] with
/// `copy`, then fsyncs it and compares its size and SHA-256 with a re-read of
/// the original. Writes `MANIFEST.sha256`, then fsyncs it and the directory.
/// Any error removes the partial directory — or names it when it cannot — and
/// is returned, so the caller writes nothing.
/// Test: `apply_with_a_failing_backup_writes_nothing`,
/// `failed_backup_cleanup_names_the_partial_backup`,
/// `apply_leaves_a_verified_backup_of_the_pre_apply_bytes`.
pub(crate) fn backup_stores(data_dir: &Path, parent: &Path, copy: CopyFn) -> Result<Backup> {
    // The random tail keeps two runs in one millisecond from colliding.
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let tail = &Uuid::new_v4().simple().to_string()[..8];
    let dir = parent.join(format!("{BACKUP_PREFIX}{stamp}-{tail}"));
    std::fs::create_dir(&dir)
        .with_context(|| format!("create backup dir {}; nothing was written", dir.display()))?;
    match copy_and_verify(data_dir, &dir, copy) {
        Ok(files) => Ok(Backup { dir, files }),
        Err(e) => {
            // #8434: an unverified backup must not be mistaken for a good one.
            let e = match std::fs::remove_dir_all(&dir) {
                Ok(()) => e,
                Err(rm) => e.context(format!("partial backup left at {} ({rm})", dir.display())),
            };
            Err(e.context(format!(
                "backup to {} failed; nothing was written",
                dir.display()
            )))
        }
    }
}

fn copy_and_verify(data_dir: &Path, dir: &Path, copy: CopyFn) -> Result<Vec<BackedUpFile>> {
    let mut files = Vec::new();
    for name in store_file_names() {
        let src = data_dir.join(&name);
        if !src
            .try_exists()
            .with_context(|| format!("cannot stat {}", src.display()))?
        {
            continue;
        }
        let dst = dir.join(&name);
        copy(&src, &dst).with_context(|| format!("copy {}", src.display()))?;
        std::fs::File::open(&dst)
            .and_then(|f| f.sync_all())
            .with_context(|| format!("sync {}", dst.display()))?;
        let original = digest(&src)?;
        let copied = digest(&dst)?;
        if original != copied {
            bail!(
                "backup of {} does not match the original ({} bytes sha256={} vs {} bytes \
                 sha256={})",
                src.display(),
                original.0,
                original.1,
                copied.0,
                copied.1
            );
        }
        files.push(BackedUpFile {
            name,
            bytes: copied.0,
            sha256: copied.1,
        });
    }
    let manifest: String = files
        .iter()
        .map(|f| format!("{}  {}\n", f.sha256, f.name))
        .collect();
    let path = dir.join(MANIFEST_FILE);
    std::fs::write(&path, manifest).with_context(|| format!("write {}", path.display()))?;
    // #8434: the manifest and the directory entries must be durable too.
    for synced in [path.as_path(), dir] {
        std::fs::File::open(synced)
            .and_then(|f| f.sync_all())
            .with_context(|| format!("sync {}", synced.display()))?;
    }
    Ok(files)
}

/// `(len, lowercase hex SHA-256)` of `path`, streamed.
fn digest(path: &Path) -> Result<(u64, String)> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let len = std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("read {}", path.display()))?;
    Ok((len, format!("{:x}", hasher.finalize())))
}

/// The report lines for the screen and the backup.
///
/// Why: kept here so the report text for both guards sits beside them.
/// What: counts by reason (`too_short` broken out of the noise count), what
/// `--allow-short` would change, then one line per rejected id — never
/// content — then the backup dir and each verified file, or why there is none.
/// Test: `credential_row_is_rejected_in_dry_run_and_apply`,
/// `short_row_is_rejected_by_default_and_imported_with_allow_short`,
/// `apply_leaves_a_verified_backup_of_the_pre_apply_bytes`.
pub(crate) fn render_guards(
    dry_run: bool,
    allow_short: bool,
    rejected: &[Rejected],
    backup: Option<&Backup>,
) -> String {
    let count = |want: RejectReason| rejected.iter().filter(|r| r.reason == want).count();
    let secret = count(RejectReason::Secret);
    let short = count(RejectReason::Noise(TOO_SHORT));
    let fate = if dry_run {
        "--apply will not import them"
    } else {
        "not imported"
    };
    let mut out = format!(
        "  screen: rejected_secret={secret} rejected_noise={} (too_short={short}) (the \
         memory_remember write gates; {fate}; content not shown)\n",
        rejected.len() - secret
    );
    if allow_short {
        out.push_str(
            "    --allow-short: the 8-token minimum was skipped; the secret, blocklist, \
             word-count and noise-pattern gates still applied\n",
        );
    } else if short > 0 {
        out.push_str(&format!(
            "    --allow-short would import these {short} too_short drawer(s) (fewer than 8 \
             tokens; memory_note's rule), except any content duplicate, which \
             --include-content-duplicates governs\n"
        ));
    }
    for r in rejected {
        out.push_str(&format!("    rejected {}: {}\n", r.id, r.reason));
    }
    match backup {
        Some(b) => {
            out.push_str(&format!("  backup: {}\n", b.dir.display()));
            for f in &b.files {
                out.push_str(&format!(
                    "    verified {} bytes={} sha256={}\n",
                    f.name, f.bytes, f.sha256
                ));
            }
        }
        None if dry_run => out.push_str(
            "  backup: none (a dry run writes nothing; --apply backs up and verifies the \
             store files first)\n",
        ),
        None => {}
    }
    out
}
