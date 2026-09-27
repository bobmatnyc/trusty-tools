//! What `import kuzu` prints about each store and the run (#277).
//!
//! Why: the store line is the operator's only view of a run, and it carries
//! counts, rule labels, table names and `Memory.id` values only — never memory
//! content, entity names or a refused token.
//! What: [`format_report`] renders one store; [`format_totals`] the run.
//! Test: `store_line_names_palace_source_and_every_notice`,
//! `totals_count_empty_rows_and_every_notice`.

use std::collections::BTreeMap;

use colored::Colorize;

use super::apply::StoreCounts;
use super::screen::RuleTally;
use super::{StoreReport, StoreStatus};

/// Most `Memory.id` values listed per notice before the rest are counted.
const MAX_LISTED_IDS: usize = 20;

/// One store's report: the status line, then one line per notice.
///
/// What: the palace is printed with the rule that chose it (#277 H2), and
/// the notices are the failure, a flush failure, changed memories left
/// alone without `--update` (#277 L6), refused secret-shaped ids (#277 M6),
/// dropped tags and triples with the refusals' rule tally (#277 MEDIUM-1,
/// MEDIUM-3), ids another live store holds with other content (#277 H4),
/// unmapped relationship tables (#277 LOW-2), and one line per retracted edge
/// with its memory id (#277 MEDIUM-2).
/// Test: `store_line_names_palace_source_and_every_notice`,
/// `refusals_are_tallied_by_rule_class_without_tokens`,
/// `each_retraction_is_reported_with_its_memory_id`.
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
         updated {}, empty {}), edges {} (new triples {}, existing {}, dangling {}, skipped {}, \
         retracted {}), failed {}",
        r.store.display(),
        c.memories,
        c.new_memories,
        c.unchanged,
        c.changed,
        c.updated,
        c.skipped_empty,
        c.edges,
        c.new_triples,
        c.existing_triples,
        c.dangling_edges,
        c.skipped_edges,
        c.retracted.len(),
        c.failed_writes
    );
    let mut note = |s: String| out.push_str(&format!("\n    {s}"));
    if let StoreStatus::Failed(e) = &r.status {
        note(e.to_string());
    }
    if let Some(e) = &r.flush_error {
        note(format!("palace flush failed: {e}"));
    }
    if c.changed > c.updated && !update {
        note(format!(
            "{} memory(ies) changed in kuzu since import; re-run with --update",
            c.changed - c.updated
        ));
    }
    if !c.refused_ids.is_empty() {
        note(format!(
            "refused {} secret-shaped memory(ies): {}",
            c.refused_ids.len(),
            list_ids(&c.refused_ids)
        ));
    }
    if c.refused_tags + c.refused_triples > 0 {
        note(format!(
            "dropped {} secret-shaped tag(s) and {} triple(s)",
            c.refused_tags, c.refused_triples
        ));
    }
    if !c.refusal_rules.is_empty() {
        note(format!(
            "refusals by rule: {}",
            tally_line(&c.refusal_rules)
        ));
    }
    if !c.shared_ids.is_empty() {
        note(format!(
            "skipped {} memory id(s) another store holds with other content: {}",
            c.shared_ids.len(),
            list_ids(&c.shared_ids)
        ));
    }
    if let Some(line) = unsupported_line(&c.unsupported_edges, c.unsupported_error.as_deref()) {
        note(line);
    }
    // #277 MEDIUM-2: every retraction, with its memory id, unabridged.
    for x in &c.retracted {
        note(format!(
            "retracted a {} edge of memory {}: the source no longer has it",
            x.family, x.memory_id
        ));
    }
    out
}

/// `label n, label n` in label order.
fn tally_line<K: std::fmt::Display>(t: &BTreeMap<K, usize>) -> String {
    t.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The unmapped-tables notice, when there is anything to say.
fn unsupported_line(tables: &BTreeMap<String, usize>, error: Option<&str>) -> Option<String> {
    match (tables.is_empty(), error) {
        (true, None) => None,
        (_, Some(e)) => Some(format!(
            "could not count unmapped relationship tables ({e}); their edges are not imported"
        )),
        (false, None) => Some(format!(
            "not imported: {} edge(s) in unmapped relationship tables ({})",
            tables.values().sum::<usize>(),
            tally_line(tables)
        )),
    }
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

/// The run's totals: one summary line, then the run-wide notices.
///
/// Test: `totals_count_empty_rows_and_every_notice`.
pub fn format_totals(reports: &[StoreReport], update: bool) -> String {
    let sum = |f: fn(&StoreCounts) -> usize| reports.iter().map(|r| f(&r.counts)).sum::<usize>();
    let failed = reports
        .iter()
        .filter(|r| matches!(r.status, StoreStatus::Failed(_)))
        .count();
    let mut out = format!(
        "\n{} stores: {} memories, {} edges; new {}, unchanged {}, changed {}, updated {}, \
         empty {}, new triples {}, retracted {}, refused {}, dropped tags {}, dropped triples \
         {}, shared ids {}; {} store(s) failed",
        reports.len(),
        sum(|c| c.memories),
        sum(|c| c.edges),
        sum(|c| c.new_memories),
        sum(|c| c.unchanged),
        sum(|c| c.changed),
        sum(|c| c.updated),
        sum(|c| c.skipped_empty),
        sum(|c| c.new_triples),
        sum(|c| c.retracted.len()),
        sum(|c| c.refused_ids.len()),
        sum(|c| c.refused_tags),
        sum(|c| c.refused_triples),
        sum(|c| c.shared_ids.len()),
        failed
    );
    let mut rules = RuleTally::new();
    let mut tables: BTreeMap<String, usize> = BTreeMap::new();
    for c in reports.iter().map(|r| &r.counts) {
        for (k, n) in &c.refusal_rules {
            *rules.entry(k).or_default() += n;
        }
        for (k, n) in &c.unsupported_edges {
            *tables.entry(k.clone()).or_default() += n;
        }
    }
    if !rules.is_empty() {
        out.push_str(&format!("\nrefusals by rule: {}", tally_line(&rules)));
    }
    let uncounted = reports
        .iter()
        .filter(|r| r.counts.unsupported_error.is_some())
        .count();
    if let Some(line) = unsupported_line(&tables, None) {
        out.push_str(&format!("\n{line}"));
    }
    if uncounted > 0 {
        out.push_str(&format!(
            "\n{uncounted} store(s) could not count their unmapped relationship tables"
        ));
    }
    let changed = sum(|c| c.changed).saturating_sub(sum(|c| c.updated));
    if changed > 0 && !update {
        out.push_str(&format!(
            "\n{changed} memory(ies) changed in kuzu since import; re-run with --update to rewrite them."
        ));
    }
    out
}

/// Print the run's totals.
pub fn print_totals(reports: &[StoreReport], update: bool) {
    println!("{}", format_totals(reports, update));
}
