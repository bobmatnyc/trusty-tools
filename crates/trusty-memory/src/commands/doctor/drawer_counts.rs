//! `doctor` drawer-count stability check, report and ack (#9283).
//!
//! Why: silent drawer loss (#8729) must turn doctor red, and the
//! trusty-memory 1.0 gate needs a seven-day "no unexplained drop" report.
//! What: [`check_drawer_counts`] turns [`analysis::judge`]'s per-palace
//! verdicts into one [`CheckResult`]; [`handle_drawer_report`] prints
//! [`analysis::report`] and exits 1 on any unexplained drop;
//! [`handle_ack_drop`] appends an operator ack. All three read the history
//! the daemon writes and the palace journals as plain files — a daemon-held
//! `kg.redb` is never opened. Only `--ack-drop` writes.
//! Test: `drawer_counts::tests` (the verdict and report tests) and
//! `commands::doctor::drawer_counts::tests`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::Args;
use colored::Colorize;
use trusty_common::memory_core::maintenance_log::{read_journal, MaintenanceDeletion};

use super::CheckResult;
use crate::drawer_counts::analysis::{self, DrawerReport, Verdict, STALE_AFTER};
use crate::drawer_counts::{self as counts, History};

const LABEL: &str = "drawer counts";

/// `doctor` flags for the drawer-count report and ack (#9283).
///
/// Why: the report and the ack are doctor modes, as `--fix-palaces` is.
/// What: `--drawer-report [--days N] [--json]` prints the report;
/// `--ack-drop <palace> --count N --reason TEXT [--ack-day YYYY-MM-DD]`
/// records an operator acknowledgement.
/// Test: `doctor_drawer_flags_parse`.
#[derive(Debug, Clone, Args)]
pub struct DrawerCountArgs {
    /// Print the per-palace drawer-count report and exit (1 on any
    /// unexplained drop).
    #[arg(long, conflicts_with = "ack_drop")]
    pub drawer_report: bool,
    /// Days the report covers, ending today (UTC).
    #[arg(long, default_value_t = 7, requires = "drawer_report")]
    pub days: u32,
    /// Print the report as JSON.
    #[arg(long, requires = "drawer_report")]
    pub json: bool,
    /// Acknowledge a drawer-count drop in this palace.
    #[arg(long, value_name = "PALACE", requires_all = ["count", "reason"])]
    pub ack_drop: Option<String>,
    /// Drawers the acknowledgement accounts for.
    #[arg(long, requires = "ack_drop")]
    pub count: Option<usize>,
    /// Why the drawers left.
    #[arg(long, requires = "ack_drop")]
    pub reason: Option<String>,
    /// Snapshot day the ack explains (default: the palace's newest).
    #[arg(long, requires = "ack_drop")]
    pub ack_day: Option<NaiveDate>,
}

impl DrawerCountArgs {
    /// Whether a drawer-count mode replaces the ordinary doctor run.
    pub fn is_mode(&self) -> bool {
        self.drawer_report || self.ack_drop.is_some()
    }
}

/// The palace root doctor reads, resolved as the daemon resolves it.
fn data_root() -> Result<PathBuf> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    Ok(crate::resolve_palace_registry_dir(data_dir))
}

/// Read one palace's journal from `<root>/<palace>/`.
fn load_journal(root: &Path, palace: &str) -> Result<Vec<MaintenanceDeletion>, String> {
    read_journal(&root.join(palace))
        .map(|j| j.records)
        .map_err(|e| format!("{e:#}"))
}

/// Doctor check: does any palace's newest drawer-count drop exceed its
/// journaled deletions?
///
/// Test: see [`verdict`].
pub fn check_drawer_counts() -> CheckResult {
    match data_root() {
        Ok(root) => verdict(&root, Utc::now()),
        Err(e) => CheckResult::unknown(LABEL, format!("{e:#}")),
    }
}

/// The check's verdict for the history under `root` at `now`.
///
/// Why/What: design §4. Fail when any palace's newest window is
/// unexplained; else Warn when a palace was removed, the newest snapshot is
/// stale, a palace's newest count is unavailable, or a palace's deletion
/// journal is unreadable (#9283: each named with its reason, because such a
/// palace can never go red); else Pass when at least one palace compared
/// clean; else Warn when every palace is waiting for its second count (the
/// first-day baseline); else Unknown (no history). A history holding lines
/// from a newer schema is refused: Unknown, never judged.
/// Test: `drawer_counts_doctor_goes_red_on_unjournaled_drop`,
/// `drawer_counts_doctor_green_when_journaled`,
/// `drawer_counts_doctor_green_for_user_forget`,
/// `v2_incompatible_reset_to_empty_turns_doctor_red`,
/// `future_schema_line_is_refused_not_dropped`,
/// `an_uncountable_palace_is_named_and_never_green`,
/// `a_first_day_baseline_warns_instead_of_undetermined`,
/// `an_unreadable_journal_is_named_and_never_green`.
pub(crate) fn verdict(root: &Path, now: DateTime<Utc>) -> CheckResult {
    let history = match counts::read_history(root) {
        Ok(h) => h,
        Err(e) => return CheckResult::unknown(LABEL, format!("{e:#}")),
    };
    if let Some(refusal) = refuse_newer(&history) {
        return CheckResult::unknown(LABEL, refusal);
    }
    if history.lines.is_empty() {
        return CheckResult::unknown(
            LABEL,
            "baseline pending: no snapshot yet (the daemon records one per UTC day)",
        );
    }
    let verdicts = analysis::judge(&history, &|p| load_journal(root, p));
    let (mut red, mut removed, mut unknown, mut clean) = (vec![], vec![], vec![], 0usize);
    let (mut uncountable, mut unreadable, mut pending) = (vec![], vec![], 0usize);
    for (palace, v) in &verdicts {
        match v {
            Verdict::Unexplained(w) => red.push(format!(
                "{palace}: {} -> {} on {}, {} unexplained",
                w.before, w.after, w.day, w.unexplained
            )),
            Verdict::Removed { last_day } => {
                removed.push(format!("{palace} (last seen {last_day})"))
            }
            Verdict::Clean(_) => clean += 1,
            Verdict::BaselinePending => {
                pending += 1;
                unknown.push(format!("{palace}: baseline pending"));
            }
            Verdict::Unavailable => uncountable.push(format!(
                "{palace}: {}",
                newest_reason(&history, palace).unwrap_or("reason not recorded")
            )),
            Verdict::JournalUnreadable(e) => unreadable.push(format!("{palace}: {e}")),
        }
    }
    let tail = if unknown.is_empty() {
        String::new()
    } else {
        format!("; unknown: {}", unknown.join(", "))
    };
    if !red.is_empty() {
        return CheckResult::fail(
            LABEL,
            format!(
                "drop exceeds journaled deletions — {} (see `trusty-memory doctor --drawer-report`)",
                red.join("; ")
            ),
        );
    }
    let stale = analysis::newest_snapshot(&history)
        .filter(|t| now - *t > STALE_AFTER)
        .map(|t| {
            format!(
                "newest snapshot {}h old (is the daemon running?)",
                (now - t).num_hours()
            )
        });
    let mut parts: Vec<String> = stale.into_iter().collect();
    if !removed.is_empty() {
        parts.push(format!("palace removed: {}", removed.join(", ")));
    }
    // #9283: an uncountable palace can never go red, so it never reads green.
    if !uncountable.is_empty() {
        parts.push(format!(
            "{} palace(s) uncountable: {}",
            uncountable.len(),
            uncountable.join("; ")
        ));
    }
    // #9283: without its journal a palace's drop cannot be judged either.
    if !unreadable.is_empty() {
        parts.push(format!("journal unreadable: {}", unreadable.join("; ")));
    }
    if !parts.is_empty() {
        return CheckResult::warn(LABEL, format!("{}{tail}", parts.join("; ")));
    }
    if clean > 0 {
        return CheckResult::pass(LABEL, format!("{clean} palace(s) explained{tail}"));
    }
    // #9283: the first day is an observed state, not an undetermined probe;
    // Unknown here failed every first-day doctor run (#4001).
    if pending > 0 {
        return CheckResult::warn(
            LABEL,
            format!(
                "baseline pending: {pending} palace(s) counted once; the first comparison \
                 follows the next UTC day's snapshot"
            ),
        );
    }
    CheckResult::unknown(LABEL, format!("no palace comparable yet{tail}"))
}

/// The reason recorded on `palace`'s newest snapshot line, if any (#9283).
fn newest_reason<'h>(history: &'h History, palace: &str) -> Option<&'h str> {
    history
        .lines
        .iter()
        .filter(|l| l.palace == palace && l.ack.is_none())
        .max_by_key(|l| l.at)
        .and_then(|l| l.reason.as_deref())
}

/// The refusal text for a history holding newer-schema lines.
fn refuse_newer(history: &History) -> Option<String> {
    (history.newer > 0).then(|| {
        format!(
            "{} line(s) in {} use a schema newer than v{}; upgrade trusty-memory to read them",
            history.newer,
            counts::DRAWER_COUNTS_FILENAME,
            counts::SCHEMA_VERSION
        )
    })
}

/// Build the report under `root`; `Err` carries a refusal.
pub(crate) fn build_report(root: &Path, now: DateTime<Utc>, days: u32) -> Result<DrawerReport> {
    let history = counts::read_history(root)?;
    if let Some(refusal) = refuse_newer(&history) {
        anyhow::bail!(refusal);
    }
    Ok(analysis::report(
        &history,
        &|p| load_journal(root, p),
        now,
        days,
    ))
}

/// `trusty-memory doctor --drawer-report [--days N] [--json]`.
///
/// Why/What: design §5. Prints the report and exits 1 on any unexplained
/// drop, an unreadable journal, or a refused history.
/// Test: `report_lists_seven_days_and_exits_one_on_unexplained` covers the
/// body; `render_text` is covered by `report_text_names_unexplained_days`.
pub fn handle_drawer_report(days: u32, json: bool) -> Result<()> {
    let report = match data_root().and_then(|root| build_report(&root, Utc::now(), days)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} drawer report: {e:#}", "✗".red());
            std::process::exit(1);
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_text(&report));
    }
    if report.failed() {
        std::process::exit(1);
    }
    Ok(())
}

/// The report as plain text.
pub(crate) fn render_text(r: &DrawerReport) -> String {
    let mut out = format!(
        "Drawer counts {} .. {} (UTC)\n",
        r.window_start, r.window_end
    );
    for p in &r.palaces {
        let counts: Vec<String> = r
            .days
            .iter()
            .map(|d| match p.counts.get(&d.day) {
                Some(Some(n)) => n.to_string(),
                Some(None) => "?".into(),
                None => "-".into(),
            })
            .collect();
        let net = p.net_delta.map_or("n/a".into(), |n| format!("{n:+}"));
        let journaled: Vec<String> = p
            .journaled
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect();
        out.push_str(&format!(
            "  {}  [{}]  net {net}  journaled {{{}}}",
            p.palace,
            counts.join(" "),
            journaled.join(", ")
        ));
        if p.acked > 0 {
            out.push_str(&format!("  acked {}", p.acked));
        }
        for (day, n) in &p.unexplained {
            out.push_str(&format!("  UNEXPLAINED {day}: {n}"));
        }
        if let Some(day) = p.removed_after {
            out.push_str(&format!("  removed after {day}"));
        }
        if let Some(e) = &p.journal_error {
            out.push_str(&format!("  journal unreadable: {e}"));
        }
        out.push('\n');
    }
    if let Some(t) = r.newest_snapshot.filter(|_| r.stale) {
        out.push_str(&format!("stale snapshot: newest is {t}\n"));
    } else if r.newest_snapshot.is_none() {
        out.push_str("no snapshot yet\n");
    }
    out.push_str(&format!(
        "window start {}; green days {}; {}\n",
        r.window_start, r.green_days, r.clean
    ));
    out
}

/// `trusty-memory doctor --ack-drop <palace> --count N --reason TEXT`.
///
/// Test: `an_ack_explains_an_unjournaled_drop` covers [`counts::append_ack`].
pub fn handle_ack_drop(args: &DrawerCountArgs) -> Result<()> {
    let (Some(palace), Some(count), Some(reason)) = (&args.ack_drop, args.count, &args.reason)
    else {
        anyhow::bail!("--ack-drop needs --count and --reason");
    };
    let line = counts::append_ack(
        &data_root()?,
        palace,
        count,
        reason,
        args.ack_day,
        Utc::now(),
    )?;
    let day = line.ack.map(|a| a.day.to_string()).unwrap_or_default();
    println!(
        "{} acknowledged {count} drawer(s) leaving {palace} on {day}",
        "✓".green()
    );
    Ok(())
}

#[cfg(test)]
#[path = "drawer_counts_tests.rs"]
mod drawer_counts_tests;
