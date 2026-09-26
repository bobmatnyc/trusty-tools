//! `trusty-memory audit secrets --count-only` — count stored drawers the
//! current secret filter would refuse.
//!
//! Why: #8645. `check_secret` runs at write time only, so a drawer stored
//! before a filter fix is never re-screened. trusty-common 0.52.3 (#8589)
//! started screening the VALUE half of a `KEY=value` token; drawers written
//! earlier may hold values the filter now refuses. This command measures how
//! many, without printing any of them — remediation is a separate,
//! owner-gated step.
//! What: enumerates palaces the way `kg-rebuild` does
//! (`PalaceRegistry::list_palaces`), reads each drawer table through
//! [`with_store_copy`] (a private copy of `kg.redb`, never the live file), runs
//! `check_secret` on each drawer's content in memory, and reports counts only.
//! The `PotentialSecret { token }` preview is matched away at the call site
//! and never stored, logged or printed.
//!
//! Daemon safety: the live store is only ever `std::fs::copy`-read. No redb
//! open, lock, WAL or rename touches it, so a running daemon neither blocks the
//! scan nor sees it. `ReadOnlyRedb` was rejected: its live variant takes a
//! shared lock, and a daemon reopening an idle-evicted palace during the scan
//! would get `DatabaseAlreadyOpen` and fail its writes loud. The cost of the
//! copy is a point-in-time read: a copy torn by a concurrent commit fails for
//! that palace and is reported as an error. Anything the scan could not see —
//! an unreadable store, an undecodable drawer row — fails the run.
//! Test: `counts_refusals_per_palace_and_variant`,
//! `output_and_tracing_carry_no_drawer_content`,
//! `undecodable_drawer_row_is_counted_and_fails_the_run`,
//! `scan_leaves_palace_files_byte_identical_under_a_live_writer`.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use trusty_common::memory_core::filter::{check_secret, FilterReject};
use trusty_common::memory_core::PalaceRegistry;

use super::store_snapshot::{sweep_stale_copies, with_store_copy, SCRATCH_PREFIX, STALE_COPY_AGE};

/// Arguments of `trusty-memory audit` (#8645).
///
/// Why: lives beside its handler, as `PalaceAction` does, so `main.rs` stays
/// under the SLOC cap — `main.rs` holds only the newtype variant.
/// What: wraps the required [`AuditAction`] subcommand.
/// Test: `count_only_flag_is_required`.
#[derive(Debug, Args)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub action: AuditAction,
}

/// Actions under `trusty-memory audit` (#8645).
///
/// What: `Secrets` is the only action and is read-only.
/// Test: `count_only_flag_is_required`.
#[derive(Debug, Subcommand)]
pub enum AuditAction {
    /// Count stored drawers the current secret filter would refuse.
    ///
    /// READ-ONLY and COUNT ONLY. Each palace's store is copied to a private
    /// temp dir and read from the copy, so this is safe with the daemon
    /// running and writes nothing to any palace. No drawer text, id or token
    /// preview is printed — only counts per palace and per refusal kind.
    ///
    ///   trusty-memory audit secrets --count-only
    ///   trusty-memory audit secrets --count-only --palace trusty-tools --json
    Secrets {
        /// Required. Names the only mode: counts, never content.
        #[arg(long = "count-only", required = true)]
        count_only: bool,
        /// Restrict the scan to one palace id.
        #[arg(long, value_name = "ID")]
        palace: Option<String>,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

/// Run one [`AuditAction`].
///
/// What: one arm per variant; `count_only` is enforced by clap.
/// Test: `count_only_flag_is_required` (parsing); the handler is covered
/// through [`scan_palaces`] and [`render`].
pub async fn dispatch(args: AuditArgs) -> Result<()> {
    match args.action {
        AuditAction::Secrets { palace, json, .. } => {
            handle_audit_secrets(AuditSecretsOptions { palace, json }).await
        }
    }
}

/// What one `audit secrets` invocation was asked to do.
#[derive(Debug, Clone, Default)]
pub struct AuditSecretsOptions {
    /// Restrict the scan to one palace id. `None` scans every palace.
    pub palace: Option<String>,
    /// Emit JSON instead of text.
    pub json: bool,
}

/// Refusals broken down by [`FilterReject`] variant.
///
/// Why: the issue asks for a per-variant breakdown. `check_secret` only
/// returns `PotentialSecret` today; the exhaustive match in [`tally_reject`]
/// makes a new variant a compile error here rather than an uncounted refusal.
/// What: one counter per variant. The quality gates (`TooShort`,
/// `NoisePattern`, `NonAlphabetic`) are not run by this audit, so they stay 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RejectCounts {
    pub potential_secret: usize,
    pub too_short: usize,
    pub noise_pattern: usize,
    pub non_alphabetic: usize,
}

/// What happened to a palace's store during the scan.
///
/// Why: #8645 — a palace with no `kg.redb` and a palace whose store was read
/// and found clean must never render alike.
/// What: `Read` — the copy was opened and its drawer table read; `Absent` — the
/// palace has no store file; `Error` — the store could not be read (see
/// [`PalaceSecretCounts::error`]).
/// Test: `unstattable_palace_dir_is_an_error_row_not_an_absent_store`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreState {
    #[default]
    Read,
    Absent,
    Error,
}

impl StoreState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Absent => "absent",
            Self::Error => "error",
        }
    }
}

/// One palace's counts. Carries no drawer text, id or token preview.
///
/// Why: the output contract is counts only (#8645).
/// What: `drawers_unreadable` counts drawer rows present in the table that
/// could not be decoded, so were never screened; any non-zero value fails the
/// run (see [`scan_verdict`]). `drawers_refused` is the number of drawers
/// `check_secret` refuses. `key_value_first` counts refusals whose refusing
/// token — the first flagged token, the one `check_secret` names — is
/// `KEY=value`-shaped. `key_value_only` counts refusals where every flagged token is
/// `KEY=value`-shaped: the best cheap estimate of drawers only the #8589 fix
/// catches, since a drawer that also holds a bare flagged token was refused
/// before it too. Both are shape classifications, not a replay of the old
/// filter. `error` holds only the outermost error message, which this module
/// writes itself, so a store error can never echo stored bytes.
/// Test: `counts_refusals_per_palace_and_variant`,
/// `undecodable_drawer_row_is_counted_and_fails_the_run`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PalaceSecretCounts {
    pub palace: String,
    pub store: StoreState,
    pub drawers_scanned: usize,
    pub drawers_unreadable: usize,
    pub drawers_refused: usize,
    pub by_variant: RejectCounts,
    pub key_value_first: usize,
    pub key_value_only: usize,
    pub error: Option<String>,
}

/// Count one refusal against its variant, dropping any payload unread.
///
/// Why: the `PotentialSecret` preview is a partial secret; it must be
/// discarded where it is produced.
/// What: increments the matching counter. Every arm binds with `{ .. }`.
/// Test: `counts_refusals_per_palace_and_variant`.
fn tally_reject(counts: &mut RejectCounts, reject: FilterReject) {
    match reject {
        FilterReject::PotentialSecret { .. } => counts.potential_secret += 1,
        FilterReject::TooShort { .. } => counts.too_short += 1,
        FilterReject::NoisePattern { .. } => counts.noise_pattern += 1,
        FilterReject::NonAlphabetic { .. } => counts.non_alphabetic += 1,
    }
}

/// Split `content` into the tokens `find_secret_token` classifies.
///
/// Why: telling the `KEY=value` path apart needs the flagged tokens, and
/// trusty-common returns only a redacted preview of the first.
/// What: mirrors `find_secret_token`'s tokenizer in
/// `trusty-common/src/memory_core/filter/secret.rs` — split on whitespace or a
/// backtick, then trim everything but ASCII alphanumerics, `-` and `_` from
/// both ends. `check_secret` on one such token is exactly the per-token test
/// `find_secret_token` applies, because the split and trim are idempotent.
/// Test: `every_refused_drawer_has_a_flagged_token`.
fn secret_tokens(content: &str) -> impl Iterator<Item = &str> {
    content
        .split(|c: char| c.is_whitespace() || c == '`')
        .map(|raw| {
            raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
        })
        .filter(|tok| check_secret(tok).is_err())
}

/// True when `token` has the `KEY=value` shape #8589 started screening.
///
/// What: a non-empty identifier-like key (`[A-Za-z0-9_.-]+`), one `=`, and a
/// value that is not pure `=` padding (so a padded base64 blob is not a key).
/// Test: `key_value_shape_boundaries`.
fn is_key_value_shaped(token: &str) -> bool {
    let Some((key, value)) = token.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !value.is_empty()
        && !value.bytes().all(|b| b == b'=')
}

/// Screen one drawer's content and add it to `counts`.
///
/// What: counts the drawer as scanned; on a refusal, tallies the variant and
/// classifies the flagged tokens by shape. Nothing derived from `content`
/// outlives this call.
/// Test: `counts_refusals_per_palace_and_variant`.
fn screen_drawer(counts: &mut PalaceSecretCounts, content: &str) {
    counts.drawers_scanned += 1;
    let Err(reject) = check_secret(content) else {
        return;
    };
    counts.drawers_refused += 1;
    tally_reject(&mut counts.by_variant, reject);
    let mut flagged = secret_tokens(content);
    let Some(first) = flagged.next() else {
        return;
    };
    if is_key_value_shaped(first) {
        counts.key_value_first += 1;
        if flagged.all(is_key_value_shaped) {
            counts.key_value_only += 1;
        }
    }
}

/// Scan every palace (or one) under `registry_dir`.
///
/// Why: the testable core — the CLI handler only resolves the data root and
/// renders. `scratch_parent` is where the per-palace store copies live and die,
/// so a test can prove nothing is left behind.
/// What: lists palaces from disk, skips all but `palace_filter` when set, and
/// screens each palace's drawers from a private copy of its store. A palace
/// that cannot be read is recorded with an error and the scan continues; one
/// with no store is recorded as `Absent`; undecodable drawer rows are counted
/// in `drawers_unreadable`, never dropped. A `palace_filter` naming no palace
/// is an error, not an empty report.
/// Test: `counts_refusals_per_palace_and_variant`,
/// `palace_filter_scans_one_and_rejects_an_unknown_name`,
/// `scan_leaves_palace_files_byte_identical_under_a_live_writer`,
/// `undecodable_drawer_row_is_counted_and_fails_the_run`,
/// `truncated_store_copy_is_an_error_row`.
pub fn scan_palaces(
    registry_dir: &Path,
    palace_filter: Option<&str>,
    scratch_parent: &Path,
) -> Result<Vec<PalaceSecretCounts>> {
    let palaces = PalaceRegistry::list_palaces(registry_dir)
        .with_context(|| format!("list palaces under {}", registry_dir.display()))?;
    let mut out = Vec::new();
    for palace in palaces {
        let id = palace.id.0.clone();
        if palace_filter.is_some_and(|f| f != id) {
            continue;
        }
        let mut counts = PalaceSecretCounts {
            palace: id,
            ..Default::default()
        };
        let read = with_store_copy(&palace.data_dir, scratch_parent, |store| {
            let (drawers, unreadable) =
                store.load_drawers_with_skipped().context("load drawers")?;
            // #8645: a skipped row was never screened; count it, never drop it.
            counts.drawers_unreadable = unreadable;
            for drawer in drawers {
                screen_drawer(&mut counts, drawer.content());
            }
            Ok(())
        });
        match read {
            Ok(Some(())) => {}
            Ok(None) => counts.store = StoreState::Absent,
            Err(e) => {
                // #8645: outermost message only — the chain below it comes
                // from the store and could quote stored bytes.
                counts = PalaceSecretCounts {
                    palace: counts.palace,
                    store: StoreState::Error,
                    error: Some(e.to_string()),
                    ..Default::default()
                };
            }
        }
        out.push(counts);
    }
    if let Some(name) = palace_filter {
        if out.is_empty() {
            anyhow::bail!("no palace named `{name}` under {}", registry_dir.display());
        }
    }
    Ok(out)
}

/// Totals across palaces, for the last text line and the JSON `totals`.
#[derive(Debug, Default, Serialize)]
struct Totals {
    palaces: usize,
    absent: usize,
    drawers_scanned: usize,
    drawers_unreadable: usize,
    drawers_refused: usize,
    key_value_first: usize,
    key_value_only: usize,
    errors: usize,
}

fn totals(rows: &[PalaceSecretCounts]) -> Totals {
    let mut t = Totals {
        palaces: rows.len(),
        ..Default::default()
    };
    for r in rows {
        t.absent += usize::from(r.store == StoreState::Absent);
        t.drawers_scanned += r.drawers_scanned;
        t.drawers_unreadable += r.drawers_unreadable;
        t.drawers_refused += r.drawers_refused;
        t.key_value_first += r.key_value_first;
        t.key_value_only += r.key_value_only;
        t.errors += usize::from(r.error.is_some());
    }
    t
}

/// Decide whether a finished scan may exit zero.
///
/// Why: #8645 — a scan that could not see everything must never look like a
/// clean scan. A palace that errored, or one with undecodable rows, hid drawers
/// from `check_secret`.
/// What: `Ok(())` when no row carries an error or an unreadable drawer;
/// otherwise an `Err` naming both counts. An `Absent` store is not a failure:
/// the palace has no drawers, and the output says so.
/// Test: `verdict_fails_on_error_or_unreadable_rows_and_passes_a_clean_set`.
pub fn scan_verdict(rows: &[PalaceSecretCounts]) -> Result<()> {
    let t = totals(rows);
    if t.errors == 0 && t.drawers_unreadable == 0 {
        return Ok(());
    }
    let partial = rows.iter().filter(|r| r.drawers_unreadable > 0).count();
    anyhow::bail!(
        "audit secrets: incomplete scan — {} palace(s) could not be read; \
         {} unreadable drawer row(s) in {partial} palace(s) were not screened",
        t.errors,
        t.drawers_unreadable,
    )
}

/// Render the counts as text (errors to `err`) or JSON (all to `out`).
///
/// What: text mode prints one `key=value` line per palace, carrying its
/// `store=` state and `unreadable=` count, and a total line;
/// JSON mode prints `{ "palaces": [...], "totals": {...} }`. Only counts,
/// palace ids and this module's own error messages are ever written.
/// Test: `output_and_tracing_carry_no_drawer_content`,
/// `json_output_is_counts_only`.
pub fn render(
    out: &mut dyn Write,
    err: &mut dyn Write,
    rows: &[PalaceSecretCounts],
    json: bool,
) -> Result<()> {
    let t = totals(rows);
    if json {
        let doc = serde_json::json!({ "palaces": rows, "totals": t });
        writeln!(out, "{}", serde_json::to_string_pretty(&doc)?)?;
        return Ok(());
    }
    writeln!(
        out,
        "audit secrets: COUNT ONLY — read-only scan of a private copy of each palace store; \
         no drawer text or token preview is printed"
    )?;
    for r in rows {
        if let Some(e) = &r.error {
            writeln!(err, "[error] palace={} error={e}", r.palace)?;
            continue;
        }
        let v = &r.by_variant;
        writeln!(
            out,
            "palace={} store={} scanned={} unreadable={} refused={} potential_secret={} \
             too_short={} noise_pattern={} non_alphabetic={} key_value_first={} \
             key_value_only={}",
            r.palace,
            r.store.as_str(),
            r.drawers_scanned,
            r.drawers_unreadable,
            r.drawers_refused,
            v.potential_secret,
            v.too_short,
            v.noise_pattern,
            v.non_alphabetic,
            r.key_value_first,
            r.key_value_only,
        )?;
    }
    writeln!(
        out,
        "total: palaces={} absent={} scanned={} unreadable={} refused={} key_value_first={} \
         key_value_only={} errors={}",
        t.palaces,
        t.absent,
        t.drawers_scanned,
        t.drawers_unreadable,
        t.drawers_refused,
        t.key_value_first,
        t.key_value_only,
        t.errors
    )?;
    Ok(())
}

/// CLI entry point for `trusty-memory audit secrets --count-only`.
///
/// Why: a thin shim over [`scan_palaces`] and [`render`], matching
/// `backfill-report`'s shape.
/// What: resolves the data root, sweeps store copies a dead run left in the
/// system temp dir, then scans on a blocking thread with every copy under one
/// run-level scratch dir. Ctrl-C deletes that dir — and every plaintext copy in
/// it — before exiting non-zero with no counts. Renders, then exits through
/// [`scan_verdict`], so a partial scan never passes as a complete one.
/// Test: not unit-tested (process-level entry point); `scan_palaces`,
/// `render`, `scan_verdict` and `sweep_stale_copies` are the testable surfaces
/// (`sweep_removes_only_stale_scratch_dirs`).
pub async fn handle_audit_secrets(opts: AuditSecretsOptions) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let registry_dir = crate::resolve_palace_registry_dir(data_dir);
    let tmp = std::env::temp_dir();
    // #8645: copies hold drawers in plaintext; remove any a killed run left.
    sweep_stale_copies(&tmp, STALE_COPY_AGE);
    let run_dir = tempfile::TempDir::with_prefix_in(SCRATCH_PREFIX, &tmp)
        .context("create scratch dir for the audit run")?;
    let run_path = run_dir.path().to_path_buf();
    let palace = opts.palace.clone();
    let scan = tokio::task::spawn_blocking(move || {
        scan_palaces(&registry_dir, palace.as_deref(), &run_path)
    });
    let rows = tokio::select! {
        joined = scan => joined.context("audit scan task")??,
        _ = tokio::signal::ctrl_c() => {
            // #8645: drop the run dir now; an interrupted exit skips destructors.
            drop(run_dir);
            anyhow::bail!("audit secrets: interrupted — store copies deleted, no counts reported");
        }
    };
    render(
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        &rows,
        opts.json,
    )?;
    scan_verdict(&rows)
}

#[cfg(test)]
#[path = "audit_secrets_tests.rs"]
mod tests;
