//! The crate topology a manifest declares, rendered and fed to synthesis (#6147).
//!
//! Why: the Code Quality & Architecture section had no deterministic
//! architecture input. Its paragraph was inferred from complexity buckets, a
//! language list and a LoC count — none of which says which crates exist, which
//! depends on which, or which one the rest is built on. trusty-audit measures
//! that from a Cargo workspace's own manifests and writes it here (owner ruling:
//! crate design is a factor for Rust projects).
//!
//! What: the manifest shape, the deterministic table the section renders, and
//! the fact block the synthesis prompt carries. All three in one module because
//! they are one contract: a column this file stops rendering is a fact the
//! prompt must stop asserting on the same edit.
//!
//! ## What absence means
//!
//! `crate_topology` is absent for every repository that is not a Cargo
//! workspace, which is most of them. Absence therefore renders NOTHING — the
//! whole sub-block is omitted and the section is byte-identical to what it was
//! before this existed. It is never an honesty marker and never a gap; the
//! producer decides what is worth a gap line, and a TypeScript repository has no
//! crate topology to miss.
//!
//! Test: `topology_tests`.

use serde::{Deserialize, Serialize};

use super::fill::Scope;
use super::model::ReportModel;
use super::provenance::{Provenance, tag};

/// Most crates the deterministic table lists per repository.
///
/// Fifteen, because the table answers "what is the shape of this workspace",
/// which a longer list stops answering — and the rows are ordered so the ones
/// that answer it are the ones kept. The summary line above the table always
/// states the true member count, so a truncated table never reads as a complete
/// one.
pub const TABLE_ROW_CAP: usize = 15;

/// How many most-depended-on crates the summary line names.
pub const SHARED_CORE_COUNT: usize = 5;

/// One member of an audited workspace, as the manifest declares it.
///
/// Why: `deps` and `inbound` point in opposite directions and a reader wants
/// both — `deps` is what this crate is coupled to, `inbound` is what breaks when
/// it changes.
/// What: the cargo package name, its direct internal dependency names, and how
/// many members depend on it.
/// Test: `topology_tests::rows_lead_with_the_shared_core`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CrateNode {
    /// The cargo package name.
    pub name: String,
    /// Direct dependencies on other workspace members.
    #[serde(default)]
    pub deps: Vec<String>,
    /// How many members declare a direct dependency on this one.
    #[serde(default)]
    pub inbound: usize,
}

/// One audited repository's crate graph, measured by trusty-audit (#6147).
///
/// Why: this is the DETERMINISTIC half of the architecture section. Everything
/// here was read out of cargo's own metadata, so the report states it as
/// measured and the model comments on it rather than inventing it.
/// What: member count, total internal dependency edges, one node per member,
/// and any cycle over those edges. A cycle is notable precisely because cargo
/// rejects one: finding it means the workspace does not build.
/// Test: `topology_tests`, including `a_manifest_crate_topology_is_parsed`,
/// which parses the exact TOML trusty-audit writes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CrateTopology {
    /// How many crates the workspace declares as members.
    #[serde(default)]
    pub members: usize,
    /// Distinct internal dependency edges among those members.
    #[serde(default)]
    pub edges: usize,
    /// Each cycle found over those edges. Empty for any workspace cargo builds.
    #[serde(default)]
    pub cycles: Vec<Vec<String>>,
    /// Every member and its place in the graph.
    #[serde(default)]
    pub crates: Vec<CrateNode>,
}

impl CrateTopology {
    /// The crates the table lists, foundation first.
    ///
    /// Why: "shallowest, most depended on first" puts the crates the workspace
    /// is BUILT ON at the top, which is the order that answers the architecture
    /// question. A crate with many dependents and few dependencies is a shared
    /// core; one with the reverse is a leaf, and a reader who reads only the
    /// first rows should meet the cores.
    /// What: inbound descending, then direct-dependency count ascending, then
    /// name — a total order, so the same manifest always renders the same table.
    /// Capped at [`TABLE_ROW_CAP`].
    /// Test: `topology_tests::rows_lead_with_the_shared_core`.
    #[must_use]
    pub fn table_rows(&self) -> Vec<&CrateNode> {
        let mut rows = self.sorted_rows();
        rows.truncate(TABLE_ROW_CAP);
        rows
    }

    /// Every crate, foundation first — the order [`table_rows`](Self::table_rows)
    /// truncates, before it truncates.
    ///
    /// Why (#6082): the rendered table used the capped list, and the cap is the
    /// point where a reader stops being told the truth. Sorting by inbound
    /// descending puts every crate NOTHING depends on last, so a cap of
    /// [`TABLE_ROW_CAP`] dropped exactly the leaves — 15 of this workspace's 30
    /// crates, every one of them with `inbound` 0 — while the summary line above
    /// still said "30 crates". A reader had no way to see that half the
    /// workspace was missing. The prompt still takes the capped list: it is a
    /// token budget, and the model is told it is reading the leading rows.
    /// What: the same total order — inbound descending, then direct-dependency
    /// count ascending, then name — with no truncation, so the same manifest
    /// always renders the same complete table.
    /// Test: `topology_tests::{every_crate_reaches_the_rendered_table,
    /// rows_lead_with_the_shared_core}`.
    pub(crate) fn sorted_rows(&self) -> Vec<&CrateNode> {
        let mut rows: Vec<&CrateNode> = self.crates.iter().collect();
        rows.sort_by(|a, b| {
            b.inbound
                .cmp(&a.inbound)
                .then_with(|| a.deps.len().cmp(&b.deps.len()))
                .then_with(|| a.name.cmp(&b.name))
        });
        rows
    }

    /// The one-sentence characterisation above the table.
    ///
    /// Why: the table is per crate; the reader needs the whole-workspace facts
    /// (how many crates, how coupled, does it build) before the rows mean
    /// anything. It also states the true member count, so a table capped at
    /// [`TABLE_ROW_CAP`] never reads as the complete list.
    /// What: member and edge counts, the cycle verdict naming the members of
    /// each cycle, and the [`SHARED_CORE_COUNT`] most-depended-on crates with
    /// their inbound counts.
    /// Test: `topology_tests::{the_summary_states_members_edges_and_cycles,
    /// a_cycle_is_named_in_the_summary}`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} crates, {} internal dependency edge(s). ",
            self.members, self.edges
        );
        if self.cycles.is_empty() {
            out.push_str("No dependency cycles. ");
        } else {
            let named: Vec<String> = self.cycles.iter().map(|cycle| cycle.join(" ↔ ")).collect();
            out.push_str(&format!(
                "{} dependency cycle(s) — {} — which cargo itself rejects, so the workspace does \
                 not build as declared. ",
                self.cycles.len(),
                named.join("; ")
            ));
        }
        let core = self.shared_core();
        if core.is_empty() {
            out.push_str(
                "No crate is depended on by another: the members are independent rather than \
                 built on a shared core.",
            );
        } else {
            let named: Vec<String> = core
                .iter()
                .map(|c| format!("{} ({})", c.name, c.inbound))
                .collect();
            out.push_str(&format!("Most depended on: {}.", named.join(", ")));
        }
        out
    }

    /// The most-depended-on members, most first, ties by name.
    #[must_use]
    pub fn shared_core(&self) -> Vec<&CrateNode> {
        let mut ranked: Vec<&CrateNode> = self.crates.iter().filter(|c| c.inbound > 0).collect();
        ranked.sort_by(|a, b| b.inbound.cmp(&a.inbound).then_with(|| a.name.cmp(&b.name)));
        ranked.truncate(SHARED_CORE_COUNT);
        ranked
    }
}

/// Push the Code Quality & Architecture section's crate-topology sub-block.
///
/// Why: the section's numbers must be rendered deterministically, not asserted
/// by a model. Filling a scope rather than writing markdown is also what admits
/// these figures to the numeric guardrail — `figures::printed_figures` walks the
/// filled scope, so every count below is in-model by construction and a
/// synthesis paragraph may quote it (#6137).
/// What: one `crate_topology` scope per repository declaring one, carrying the
/// summary line and one nested `ct_row` per crate — EVERY crate
/// ([`CrateTopology::sorted_rows`]), not the capped leading slice, so the row
/// count matches the member count the summary states. `ct_crate` is prefixed
/// with the application name only when more than one repository has a topology,
/// so a single-repository report does not repeat its own name down a column.
/// Nothing is pushed when no repository declares one, and the whole block then
/// renders as nothing (`fill::render_scope`'s omit-empty rule).
/// Test: `topology_tests::{a_declared_topology_renders_rows,
/// every_crate_reaches_the_rendered_table, no_topology_renders_nothing,
/// several_repositories_are_named_in_the_rows}`.
pub fn push_crate_topology(root: &mut Scope, model: &ReportModel) {
    let declared: Vec<(&str, &CrateTopology)> = model
        .repositories
        .iter()
        .filter_map(|repo| Some((repo.name.as_str(), repo.crate_topology.as_ref()?)))
        .collect();
    let qualify = declared.len() > 1;

    for (app, topology) in declared {
        let mut block = Scope::new();
        block.set("ct_summary", tag(topology.summary(), Provenance::Measured));
        for node in topology.sorted_rows() {
            let mut row = Scope::new();
            let name = match qualify {
                true => format!("{app} / {}", node.name),
                false => node.name.clone(),
            };
            row.set("ct_crate", name);
            row.set(
                "ct_deps",
                match node.deps.is_empty() {
                    true => "—".to_string(),
                    false => node.deps.join(", "),
                },
            );
            row.set("ct_inbound", node.inbound.to_string());
            block.push_block("ct_row", row);
        }
        root.push_block("crate_topology", block);
    }
}

/// The crate-topology facts the synthesis prompt carries, one block per
/// repository that declares one.
///
/// Why: the architecture paragraph is only as good as what the prompt states.
/// Feeding the same numbers the table renders is what lets the model comment on
/// the workspace's real shape instead of inferring one — and every figure here
/// is already in the guardrail's allowed set, because the table printed it.
/// What: an empty string when no repository declares a topology, so the digest
/// gains no headed-but-empty section; otherwise the summary line plus the
/// leading table rows as `name: deps → inbound` lines.
/// Test: `topology_tests::{prompt_facts_carry_the_summary_and_rows,
/// prompt_facts_are_empty_without_a_topology}`.
#[must_use]
pub fn prompt_facts(model: &ReportModel) -> String {
    let mut out = String::new();
    for repo in &model.repositories {
        let Some(topology) = &repo.crate_topology else {
            continue;
        };
        if out.is_empty() {
            out.push_str(
                "\n## Crate topology (measured from cargo metadata — deterministic, not inferred)\n\n",
            );
        }
        out.push_str(&format!("### {}\n", repo.name));
        out.push_str(&format!("- {}\n", topology.summary()));
        for node in topology.table_rows() {
            out.push_str(&format!(
                "- `{}`: depends on {} internal crate(s){}; depended on by {}\n",
                node.name,
                node.deps.len(),
                match node.deps.is_empty() {
                    true => String::new(),
                    false => format!(" ({})", node.deps.join(", ")),
                },
                node.inbound
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod topology_tests;
