//! What `import kuzu` prints about each store and the run (#277).
//!
//! Why: the store line is the operator's only view of a run, and it carries
//! counts and `Memory.id` values only — never memory content or entity names.
//! What: [`format_report`] renders one store; [`print_totals`] the run.
//! Test: `store_line_names_palace_source_and_every_notice`.

use colored::Colorize;

use super::apply::StoreCounts;
use super::{StoreReport, StoreStatus};

/// Most `Memory.id` values listed per notice before the rest are counted.
const MAX_LISTED_IDS: usize = 20;

/// One store's report: the status line, then one line per notice.
///
/// What: the palace is printed with the rule that chose it (#277 H2), and
/// the notices are the failure, a flush failure, changed memories left
/// alone without `--update` (#277 L6), refused secret-shaped ids (#277 M6)
/// and ids another live store holds with other content (#277 H4).
/// Test: `store_line_names_palace_source_and_every_notice`.
pub fn format_report(r: &StoreReport, update: bool) -> String {
    let c = &r.counts;
    let label = match &r.status {
        StoreStatus::Imported => "imported".green(),
        StoreStatus::WouldImport => "would import".cyan(),
        StoreStatus::UpToDate => "up to date".green(),
        StoreStatus::Empty => "empty".dimmed(),
        StoreStatus::Partial => "partial".yellow(),
        StoreStatus::Failed(_) => "failed".red(),
    };
    let palace = r.palace.as_deref().unwrap_or("?");
    let source = r.source.unwrap_or("unresolved");
    let mut out = format!(
        "[{label}] {} -> {palace} ({source}): memories {} (new {}, unchanged {}, changed {}, \
         updated {}), edges {} (new triples {}, existing {}, dangling {}, skipped {}), failed {}",
        r.store.display(),
        c.memories,
        c.new_memories,
        c.unchanged,
        c.changed,
        c.updated,
        c.edges,
        c.new_triples,
        c.existing_triples,
        c.dangling_edges,
        c.skipped_edges,
        c.failed_writes
    );
    if let StoreStatus::Failed(e) = &r.status {
        out.push_str(&format!("\n    {e}"));
    }
    if let Some(e) = &r.flush_error {
        out.push_str(&format!("\n    palace flush failed: {e}"));
    }
    if c.changed > c.updated && !update {
        out.push_str(&format!(
            "\n    {} memory(ies) changed in kuzu since import; re-run with --update",
            c.changed - c.updated
        ));
    }
    if !c.refused_ids.is_empty() {
        out.push_str(&format!(
            "\n    refused {} secret-shaped memory(ies): {}",
            c.refused_ids.len(),
            list_ids(&c.refused_ids)
        ));
    }
    if !c.shared_ids.is_empty() {
        out.push_str(&format!(
            "\n    skipped {} memory id(s) another store holds with other content: {}",
            c.shared_ids.len(),
            list_ids(&c.shared_ids)
        ));
    }
    out
}

fn list_ids(ids: &[String]) -> String {
    let shown: Vec<&str> = ids
        .iter()
        .take(MAX_LISTED_IDS)
        .map(String::as_str)
        .collect();
    match ids.len().saturating_sub(MAX_LISTED_IDS) {
        0 => shown.join(", "),
        more => format!("{} (+{more} more)", shown.join(", ")),
    }
}

/// Print one line per store.
pub fn print_report(r: &StoreReport, update: bool) {
    println!("{}", format_report(r, update));
}

/// Print the run's totals.
pub fn print_totals(reports: &[StoreReport], update: bool) {
    let sum = |f: fn(&StoreCounts) -> usize| reports.iter().map(|r| f(&r.counts)).sum::<usize>();
    let failed = reports
        .iter()
        .filter(|r| matches!(r.status, StoreStatus::Failed(_)))
        .count();
    println!(
        "\n{} stores: {} memories, {} edges; new {}, unchanged {}, changed {}, updated {}, \
         new triples {}, refused {}, shared ids {}; {} store(s) failed",
        reports.len(),
        sum(|c| c.memories),
        sum(|c| c.edges),
        sum(|c| c.new_memories),
        sum(|c| c.unchanged),
        sum(|c| c.changed),
        sum(|c| c.updated),
        sum(|c| c.new_triples),
        sum(|c| c.refused_ids.len()),
        sum(|c| c.shared_ids.len()),
        failed
    );
    let changed = sum(|c| c.changed).saturating_sub(sum(|c| c.updated));
    if changed > 0 && !update {
        println!(
            "{changed} memory(ies) changed in kuzu since import; re-run with --update to rewrite them."
        );
    }
}
