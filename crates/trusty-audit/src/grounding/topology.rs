//! The crate graph a Rust workspace already declares (#6147).
//!
//! Why: the report's Code Quality & Architecture paragraph had no deterministic
//! architecture input. An LLM inferred the shape of the codebase from complexity
//! buckets, a language list and a LoC count — none of which says which crates
//! exist, which depends on which, or which one everything else is built on. A
//! Cargo workspace states all three in its own manifests, so they can be
//! measured rather than guessed (owner ruling: crate design is a factor for Rust
//! projects, and the report owes high-level architecture more attention).
//!
//! What: one leg, `cargo metadata --no-deps --format-version 1`, reduced to four
//! facts — member count, the direct internal dependency edges, any cycle over
//! them, and the most-depended-on members. [`write_into`] puts them under the
//! audited repository's manifest entry, where trusty-review reads them.
//!
//! ## Why this shells out rather than adding `cargo_metadata`
//!
//! `cargo_metadata` reaches this workspace's lock file only as a transitive
//! dependency of `tauri-utils`; no crate here depends on it directly. The facts
//! below need three JSON keys, `serde_json` is already a dependency of this
//! crate, and [`super::index`] next door already spawns its tool exactly this
//! way. A direct dependency would pin a second crate to cargo's metadata format
//! in exchange for a typed shape this module collapses on the next line.
//!
//! ## What counts as an edge
//!
//! A normal or build dependency whose name is another member's. Dev
//! dependencies are excluded because cargo PERMITS a dev-dependency cycle, so
//! counting them would report a routine test-only arrangement as an
//! architectural defect. A cycle over the edges that remain is one cargo itself
//! rejects, so finding one means the workspace does not build — worth saying.
//!
//! ## Degradation
//!
//! Three outcomes, and only one of them is a gap ([`Outcome`]). A repository
//! that is not a Cargo workspace is a DECLARED SKIP, not a degradation: a
//! TypeScript repository has no crate topology to miss, and a gap line claiming
//! otherwise would be noise in its report. A repository that IS a workspace and
//! whose metadata could not be read is a named gap, never a silent zero (#5620).
//!
//! Test: `super::topology_tests`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

/// How many most-depended-on members the shared-core list names.
///
/// Five, because the list answers "what is this workspace built on" — a
/// question a longer list stops answering. The full per-crate table carries
/// every member's inbound count regardless, so nothing is lost by capping this.
pub const SHARED_CORE_COUNT: usize = 5;

/// One member of the workspace and its place in the internal graph.
///
/// Why: the two numbers a reader wants per crate point in opposite directions.
/// `deps` is how much this crate depends ON — its coupling. `inbound` is how
/// much depends on IT — its blast radius. A crate high in both is the one worth
/// asking about.
/// What: the package name, its direct internal dependency names in sorted
/// order, and how many members name it.
/// Test: `super::topology_tests::edges_and_inbound_counts_come_from_the_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CrateNode {
    /// The cargo package name.
    pub name: String,
    /// Direct dependencies on other members, sorted, deduplicated.
    pub deps: Vec<String>,
    /// How many members declare a direct dependency on this one.
    pub inbound: usize,
}

/// The whole workspace graph, reduced to what a report can state.
///
/// Why: see the module docs. This is the deterministic input the architecture
/// paragraph was missing.
/// What: member count, total distinct internal edges, one [`CrateNode`] per
/// member sorted by name, and one entry per cycle found over the edge list.
/// Test: `super::topology_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Topology {
    /// How many crates the workspace declares as members.
    pub members: usize,
    /// Distinct `(from, to)` internal dependency edges.
    pub edges: usize,
    /// Every member, sorted by name.
    pub crates: Vec<CrateNode>,
    /// Each cycle found over the internal edges, members sorted within a cycle
    /// and cycles sorted among themselves. Empty for any workspace cargo
    /// itself will build.
    pub cycles: Vec<Vec<String>>,
}

impl Topology {
    /// The members the most other members depend on, most first.
    ///
    /// Why: the shared core is the thing a buyer's engineer asks about first —
    /// changing it touches everything downstream, and a workspace with no such
    /// crate has a different (flatter, more duplicative) shape than one with a
    /// dominant one. Either answer is worth the report stating.
    /// What: up to [`SHARED_CORE_COUNT`] members with at least one inbound
    /// edge, by inbound count descending then name ascending so the order is
    /// total and reproducible.
    /// Test: `super::topology_tests::the_shared_core_is_the_most_depended_on_members`.
    #[must_use]
    pub fn shared_core(&self) -> Vec<&CrateNode> {
        let mut ranked: Vec<&CrateNode> = self.crates.iter().filter(|c| c.inbound > 0).collect();
        ranked.sort_by(|a, b| b.inbound.cmp(&a.inbound).then_with(|| a.name.cmp(&b.name)));
        ranked.truncate(SHARED_CORE_COUNT);
        ranked
    }
}

/// What the leg produced for one repository.
///
/// Why: "no topology" has two meanings that must not share a variant. A
/// TypeScript repository has none to produce and owes the report no
/// explanation; a Cargo workspace whose metadata failed to read owes one. #5620
/// is the same distinction one level up: a recorded skip permits, a blind
/// result does not.
/// What: the measurement, a declared skip carrying why the leg did not apply,
/// or a failure carrying the one line the caller turns into a gap.
/// Test: `super::topology_tests::{a_directory_with_no_cargo_toml_is_a_declared_skip,
/// a_single_crate_manifest_is_a_declared_skip}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The repository is not a Cargo workspace; the reason it does not apply.
    NotAWorkspace(String),
    /// The graph, measured.
    Measured(Topology),
    /// It IS a workspace and the graph could not be read; why not.
    Unavailable(String),
}

/// Measure `checkout`'s crate topology, or say why there is none.
///
/// Why/What: see the module docs. Runs nothing at all unless the checkout's own
/// root `Cargo.toml` declares `[workspace]`, so a repository in any other
/// language costs one `read_to_string` and no subprocess.
///
/// # Postconditions
/// Never panics and never returns an error: every failure is an
/// [`Outcome::Unavailable`] reason string, safe to show the recipient.
///
/// Test: `super::topology_tests`.
#[must_use]
pub fn measure(checkout: &Path) -> Outcome {
    let manifest = checkout.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Outcome::NotAWorkspace(
            "the repository root declares no Cargo.toml, so it is not a Cargo workspace"
                .to_string(),
        );
    };
    if !declares_workspace(&text) {
        return Outcome::NotAWorkspace(
            "the repository's root Cargo.toml declares no `[workspace]`, so it has no crate \
             topology to measure"
                .to_string(),
        );
    }
    match cargo_metadata(&manifest) {
        Ok(json) => match parse(&json) {
            Ok(topology) => Outcome::Measured(topology),
            Err(cause) => Outcome::Unavailable(cause),
        },
        Err(cause) => Outcome::Unavailable(cause),
    }
}

/// True when this manifest text declares a `[workspace]` table.
///
/// Parsed rather than grepped: a `[workspace]` inside a string value, and the
/// `workspace = true` keys every member manifest carries, both defeat a
/// substring test.
fn declares_workspace(text: &str) -> bool {
    text.parse::<toml_edit::DocumentMut>()
        .is_ok_and(|doc| doc.get("workspace").is_some())
}

/// Run `cargo metadata --no-deps` against one manifest.
///
/// `--no-deps` is what keeps this offline and fast: cargo reads the member
/// manifests and never resolves or fetches the dependency graph. `CARGO` is
/// preferred over the bare name for the same reason every cargo subcommand
/// prefers it — it names the toolchain that is actually driving this process.
fn cargo_metadata(manifest: &Path) -> Result<String, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest)
        .output()
        .map_err(|e| format!("`{cargo} metadata` could not be run ({e})"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{cargo} metadata` failed for {} ({})",
            manifest.display(),
            stderr.lines().next().unwrap_or("no diagnostic").trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| format!("`{cargo} metadata` produced output that is not UTF-8 ({e})"))
}

/// Reduce one `cargo metadata --no-deps` document to a [`Topology`].
///
/// Why: split from [`measure`] so every graph shape — a cycle, a hub, an
/// isolated member — is testable against a literal document, with no cargo, no
/// checkout and no subprocess in the test.
/// What: members are the `packages` array (`--no-deps` emits workspace members
/// only); an edge is a `dependencies` entry whose `name` is another member's
/// and whose `kind` is normal or `build` (see the module docs on dev
/// dependencies). Self-edges and duplicates collapse.
///
/// # Errors
/// One line when the document is not JSON, or carries no `packages` array.
///
/// Test: `super::topology_tests::{edges_and_inbound_counts_come_from_the_metadata,
/// dev_dependencies_are_not_architecture_edges, metadata_that_is_not_json_is_a_reason}`.
pub fn parse(json: &str) -> Result<Topology, String> {
    let doc: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("`cargo metadata` output is not readable as JSON ({e})"))?;
    let packages = doc
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "`cargo metadata` output declares no `packages` array".to_string())?;

    let names: BTreeSet<&str> = packages
        .iter()
        .filter_map(|p| p.get("name").and_then(serde_json::Value::as_str))
        .collect();

    let mut deps_of: BTreeMap<&str, BTreeSet<&str>> =
        names.iter().map(|n| (*n, BTreeSet::new())).collect();
    for package in packages {
        let Some(name) = package.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let declared = package
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for dependency in declared {
            let Some(target) = dependency.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if target == name || !names.contains(target) || !is_architecture_kind(dependency) {
                continue;
            }
            deps_of.entry(name).or_default().insert(target);
        }
    }

    let mut inbound: BTreeMap<&str, usize> = names.iter().map(|n| (*n, 0)).collect();
    let mut edges = 0usize;
    for targets in deps_of.values() {
        edges += targets.len();
        for target in targets {
            *inbound.entry(target).or_default() += 1;
        }
    }

    let crates: Vec<CrateNode> = names
        .iter()
        .map(|name| CrateNode {
            name: (*name).to_string(),
            deps: deps_of
                .get(name)
                .into_iter()
                .flatten()
                .map(|d| (*d).to_string())
                .collect(),
            inbound: inbound.get(name).copied().unwrap_or_default(),
        })
        .collect();

    Ok(Topology {
        members: names.len(),
        edges,
        cycles: cycles(&crates),
        crates,
    })
}

/// True when this dependency entry is a normal or build dependency.
///
/// `kind` is `null` for a normal dependency and `"build"` / `"dev"` otherwise.
/// A missing key reads as normal, which is the shape cargo emits.
fn is_architecture_kind(dependency: &serde_json::Value) -> bool {
    match dependency.get("kind") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(kind)) => kind == "build",
        Some(_) => false,
    }
}

/// Every cycle over the internal edges, as sorted member-name lists.
///
/// Tarjan's algorithm: each strongly-connected component of more than one
/// member IS a cycle, and one pass finds all of them rather than the first.
/// Recursion depth is bounded by the member count — tens of crates for the
/// largest workspace this has been run against.
fn cycles(crates: &[CrateNode]) -> Vec<Vec<String>> {
    let index: BTreeMap<&str, usize> = crates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();
    let adjacency: Vec<Vec<usize>> = crates
        .iter()
        .map(|c| {
            c.deps
                .iter()
                .filter_map(|d| index.get(d.as_str()).copied())
                .collect()
        })
        .collect();

    let mut tarjan = Tarjan::new(adjacency.len());
    for node in 0..adjacency.len() {
        if tarjan.order[node].is_none() {
            tarjan.walk(node, &adjacency);
        }
    }

    let mut found: Vec<Vec<String>> = tarjan
        .components
        .into_iter()
        .filter(|component| component.len() > 1)
        .map(|component| {
            let mut names: Vec<String> = component
                .into_iter()
                .map(|i| crates[i].name.clone())
                .collect();
            names.sort();
            names
        })
        .collect();
    found.sort();
    found
}

/// Tarjan's strongly-connected-components state.
struct Tarjan {
    /// Discovery order per node, `None` until visited.
    order: Vec<Option<usize>>,
    /// Lowest discovery order reachable from each node.
    low: Vec<usize>,
    /// Whether each node is currently on the stack.
    stacked: Vec<bool>,
    /// The current component stack.
    stack: Vec<usize>,
    /// Next discovery order to assign.
    next: usize,
    /// Every component found so far.
    components: Vec<Vec<usize>>,
}

impl Tarjan {
    fn new(nodes: usize) -> Self {
        Self {
            order: vec![None; nodes],
            low: vec![0; nodes],
            stacked: vec![false; nodes],
            stack: Vec::new(),
            next: 0,
            components: Vec::new(),
        }
    }

    fn walk(&mut self, node: usize, adjacency: &[Vec<usize>]) {
        self.order[node] = Some(self.next);
        self.low[node] = self.next;
        self.next += 1;
        self.stack.push(node);
        self.stacked[node] = true;

        for &next in &adjacency[node] {
            match self.order[next] {
                None => {
                    self.walk(next, adjacency);
                    self.low[node] = self.low[node].min(self.low[next]);
                }
                Some(order) if self.stacked[next] => {
                    self.low[node] = self.low[node].min(order);
                }
                Some(_) => {}
            }
        }

        if Some(self.low[node]) == self.order[node] {
            let mut component = Vec::new();
            while let Some(member) = self.stack.pop() {
                self.stacked[member] = false;
                component.push(member);
                if member == node {
                    break;
                }
            }
            self.components.push(component);
        }
    }
}

/// Declare `topology` on the `[[repositories]]` entry whose path is `checkout`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A graph this
/// process measures and does not write reaches no renderer — not the sweep's,
/// and not the recipient's own re-render of the delivered package.
/// What: a `crate_topology` sub-table carrying `members`, `edges`, `cycles` and
/// one `crates` row per member. Written format-preserving, exactly as
/// [`super::priority::write_into`] writes its ranking, so the two other crates
/// that own this document keep their key order and their comments.
///
/// # Errors
///
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, matched, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, the repository whose `path` is `checkout` declares exactly this
/// topology, and nothing else in the document changed.
///
/// Test: `super::topology_tests::{the_topology_lands_on_the_matching_repository,
/// a_written_topology_round_trips, a_topology_with_no_matching_entry_is_refused}`.
pub fn write_into(manifest: &Path, checkout: &Path, topology: &Topology) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("{} could not be read ({e})", manifest.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", manifest.display()))?;

    let repositories = doc
        .get_mut("repositories")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "the manifest declares no `[[repositories]]` entry".to_string())?;
    let entry = repositories
        .iter_mut()
        .find(|table| super::priority::names_checkout(table, checkout))
        .ok_or_else(|| {
            format!(
                "no `[[repositories]]` entry names the checkout at {}",
                checkout.display()
            )
        })?;
    entry.insert("crate_topology", Item::Table(as_table(topology)));

    std::fs::write(manifest, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", manifest.display()))
}

/// The topology as a TOML sub-table.
fn as_table(topology: &Topology) -> Table {
    let mut table = Table::new();
    table.insert(
        "members",
        Item::Value(Value::from(
            i64::try_from(topology.members).unwrap_or(i64::MAX),
        )),
    );
    table.insert(
        "edges",
        Item::Value(Value::from(
            i64::try_from(topology.edges).unwrap_or(i64::MAX),
        )),
    );
    let mut cycles = Array::new();
    for cycle in &topology.cycles {
        cycles.push(cycle.iter().map(String::as_str).collect::<Array>());
    }
    table.insert("cycles", Item::Value(Value::Array(cycles)));
    table.insert("crates", Item::Value(Value::Array(rows(&topology.crates))));
    table
}

/// The per-member rows as a multi-line TOML array of inline tables.
fn rows(crates: &[CrateNode]) -> Array {
    let mut array = Array::new();
    for node in crates {
        let mut row = InlineTable::new();
        row.insert("name", Value::from(node.name.as_str()));
        row.insert(
            "deps",
            Value::Array(node.deps.iter().map(String::as_str).collect::<Array>()),
        );
        row.insert(
            "inbound",
            Value::from(i64::try_from(node.inbound).unwrap_or(i64::MAX)),
        );
        let mut value = Value::InlineTable(row);
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    if !crates.is_empty() {
        array.set_trailing("\n");
        array.set_trailing_comma(true);
    }
    array
}

/// [`measure`], then write what it produced into `manifest`.
///
/// Why: the same shape [`super::ground_manifest`] gives every other leg — the
/// caller gets gap lines and nothing else to decide about.
/// What: the declared skip writes nothing and says nothing; a measurement is
/// written and a write failure becomes a gap of its own; an unavailable graph
/// is one gap naming `display` and what the report therefore will not carry.
/// Test: `super::topology_tests::{a_declared_skip_writes_nothing_and_says_nothing,
/// an_unreadable_graph_is_a_named_gap}`.
pub fn ground_into(manifest: &Path, checkout: &Path, display: &str) -> Vec<String> {
    match measure(checkout) {
        Outcome::NotAWorkspace(_) => Vec::new(),
        Outcome::Unavailable(cause) => vec![format!(
            "{display}: {cause} — the report's Code Quality & Architecture section states no \
             crate topology for it, and its architecture paragraph is inferred without one"
        )],
        Outcome::Measured(topology) => match write_into(manifest, checkout, &topology) {
            Ok(()) => Vec::new(),
            Err(cause) => vec![format!(
                "{display}: {cause} — the report's Code Quality & Architecture section states no \
                 crate topology for it"
            )],
        },
    }
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod topology_tests;
