//! The EXPOSED knowledge graph, read from an assistant's OKG tree (#7430).
//!
//! Why: the graph the assistant UI and agent tools show must be the OKG graph —
//! the triples and definitions held in the assistant's own `okg/` tree — and
//! nothing else. Before #7430 the four `/api/agents/:name/kg*` routes proxied
//! trusty-memory's palace-scoped `kg_*` surface instead, so the pane labelled
//! "Knowledge Graph" rendered the MEMORY knowledge graph: triples asserted into
//! a memory palace, which is a separate store with a separate lifecycle. That
//! is the leak epic #7425 item (f) names. This module is the OKG-side
//! replacement, and it reads one directory tree — it holds no memory client and
//! can reach no palace, so the leak cannot reappear by accident.
//!
//! What: [`read_graph`] walks a KB tree with [`KbStore`] and returns both halves
//! of the graph at once.
//!   - A DEFINITION is one entity: what the subject is (`type`), where it lives
//!     (`collection`), and its one-line summary.
//!   - A TRIPLE is one relationship edge: an entity's frontmatter key holding
//!     `[[wiki-link]]` targets, one triple per target. The envelope fields OKF
//!     defines as description rather than relationship ([`base_envelope`]) are
//!     excluded, so a `[[link]]` inside a prose `description` is not read as an
//!     edge.
//! [`resolve_okg_root`] maps an agent to the tree it browses, mirroring
//! [`super::binding`]'s resolution so an OKG tool and this reader can never
//! address different directories.
//!
//! Output is fully sorted, so two reads of an unchanged tree are byte-identical.
//!
//! Test: `reads_triples_and_definitions_from_a_tree`,
//! `resolves_the_home_tree_for_a_rooted_binding`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml::Value as Yaml;

use trusty_kb::entity::link_values;
use trusty_kb::schema::{Profile, base_envelope};
use trusty_kb::store::{KbStore, summarise};

use crate::assistants::{AssistantHome, OKG_DIR};
use crate::stores::StoresConfig;

/// One relationship edge, as a subject-predicate-object triple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OkgTriple {
    /// The entity the edge was read from — its `title`, else its file slug.
    pub subject: String,
    /// The frontmatter key holding the edge (`works_at`, `member_of`, …).
    pub predicate: String,
    /// One `[[wiki-link]]` target of that key.
    pub object: String,
    /// Tree-relative path of the entity file, so a reader can see the source.
    pub provenance: String,
}

/// One entity definition — what a subject IS, as distinct from what it links to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OkgDefinition {
    /// Same spelling as [`OkgTriple::subject`], so the two halves join.
    pub subject: String,
    /// The collection directory the entity lives in.
    pub collection: String,
    /// The file slug, which is also its address within the collection.
    pub slug: String,
    /// The OKF `type` field.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The `description` field, else the first non-blank body line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Tree-relative path of the entity file.
    pub path: String,
}

/// One subject and how many triples name it as subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OkgSubjectCount {
    /// The subject.
    pub subject: String,
    /// Triples whose subject this is. Zero for a defined entity with no edges —
    /// still a node in the graph, and still listed.
    pub count: usize,
}

/// Both halves of one tree's graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OkgGraph {
    /// Every relationship edge, sorted by subject, predicate, then object.
    pub triples: Vec<OkgTriple>,
    /// Every entity definition, sorted by collection then slug.
    pub definitions: Vec<OkgDefinition>,
}

impl OkgGraph {
    /// Every subject in the graph with its triple count, subject-sorted.
    ///
    /// Why: the browser's left-hand list must show an entity that has a
    /// definition but no edges yet — an OKG tree mid-ingest is mostly that, and
    /// a list built only from triples would render it as empty.
    /// What: the union of definition subjects and triple subjects; the count is
    /// triples only.
    /// Test: `subject_counts_include_edgeless_entities`.
    pub fn subject_counts(&self) -> Vec<OkgSubjectCount> {
        let mut counts: std::collections::BTreeMap<&str, usize> = self
            .definitions
            .iter()
            .map(|d| (d.subject.as_str(), 0))
            .collect();
        for t in &self.triples {
            *counts.entry(t.subject.as_str()).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .map(|(subject, count)| OkgSubjectCount {
                subject: subject.to_string(),
                count,
            })
            .collect()
    }

    /// The definitions for `subjects`, in this graph's own definition order.
    pub fn definitions_for(&self, subjects: &[String]) -> Vec<OkgDefinition> {
        self.definitions
            .iter()
            .filter(|d| subjects.contains(&d.subject))
            .cloned()
            .collect()
    }
}

/// Whether a frontmatter key carries relationship edges.
///
/// Why: OKF stores an edge as a snake_case key holding `[[wiki-links]]`, but the
/// base envelope's own fields are description, not relationship — a `[[link]]`
/// written inside a prose `description` must not become a predicate.
/// What: every key EXCEPT the base-envelope names, which come from
/// [`base_envelope`] rather than a second hand-kept list. The rule is
/// exclusion, so it fails OPEN: a new OKF envelope key that is NOT added to
/// [`base_envelope`] reads as a relationship here, and any `[[wiki-link]]` in
/// its value becomes a triple. Adding the key to that function is what keeps
/// the two in step — there is no second list to update.
/// Test: `envelope_fields_are_not_edges`.
fn is_edge_field(key: &str) -> bool {
    !base_envelope().iter().any(|f| f.name == key)
}

/// Read both halves of the graph out of the KB tree at `root`.
///
/// Why/What: see the module doc. A malformed entity file is SKIPPED rather than
/// failing the read — the same fail-open posture `KbStore::list` takes, so one
/// hand-edited file cannot blank the whole pane.
///
/// This walks and parses the whole tree on every call, so one browser page load
/// costs four walks. Deliberately uncached: the only cheap key is a directory
/// mtime, and a directory's mtime does not move when an entity file already in
/// it is edited in place — so an mtime cache would serve a stale graph after
/// exactly the edit a reader opened the pane to see. A cache here needs a
/// per-file stamp or an ingest-side invalidation hook. See #7430.
///
/// Test: `reads_triples_and_definitions_from_a_tree`,
/// `malformed_entity_is_skipped`.
pub fn read_graph(root: &Path) -> anyhow::Result<OkgGraph> {
    let store = KbStore::new(root.to_path_buf(), Profile::default_profile());
    let mut graph = OkgGraph::default();
    for collection in store.collection_dirs_on_disk()? {
        for (slug, path) in store.entity_files(&collection)? {
            let Ok(Some(entity)) = store.read_entity_at(&path) else {
                continue;
            };
            let subject = entity
                .get_str("title")
                .map(str::to_string)
                .unwrap_or_else(|| slug.clone());
            // Tree-relative by construction; the fallback rebuilds the same
            // relative spelling rather than falling back to the absolute path,
            // which would put the operator's directory layout in a payload
            // clients receive (#7430 security review).
            let rel = path
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| format!("{collection}/{slug}.md"));
            graph.definitions.push(OkgDefinition {
                subject: subject.clone(),
                collection: collection.clone(),
                slug: slug.clone(),
                kind: entity.get_str("type").map(str::to_string),
                summary: summarise(&entity),
                path: rel.clone(),
            });
            let Yaml::Mapping(map) = &entity.frontmatter else {
                continue;
            };
            for (key, value) in map {
                let Some(predicate) = key.as_str().filter(|k| is_edge_field(k)) else {
                    continue;
                };
                for object in link_values(value) {
                    graph.triples.push(OkgTriple {
                        subject: subject.clone(),
                        predicate: predicate.to_string(),
                        object,
                        provenance: rel.clone(),
                    });
                }
            }
        }
    }
    graph
        .definitions
        .sort_by(|a, b| (&a.collection, &a.slug).cmp(&(&b.collection, &b.slug)));
    graph.triples.sort_by(|a, b| {
        (&a.subject, &a.predicate, &a.object).cmp(&(&b.subject, &b.predicate, &b.object))
    });
    Ok(graph)
}

/// One resolved OKG tree: where to read it, and what to CALL it.
///
/// Why (#7430 security review): the two are deliberately separate. `root` is an
/// absolute filesystem path and belongs only to this process; `label` is the
/// name a client may see. Serving the path would hand every viewer of the
/// Knowledge-Graph pane the operator's home-directory layout, which no caller
/// needs and the retired memory-palace envelope never disclosed.
pub struct OkgTree {
    /// Absolute directory to read. Never leaves the server.
    pub root: PathBuf,
    /// The binding's own opaque name for the tree — `okg://<agent>`, or the
    /// home-relative `<agent>/<root>`. Never absolute, never a real path.
    pub label: String,
}

/// The OKG tree `agent` browses.
///
/// Why: an agent addresses its tree two ways — a `[[stores]].root` relative to
/// the assistant's own home (#4325), or the default `okg://<agent>` URI into the
/// shared knowledge pool. `super::binding::binding_for` already resolves exactly
/// this pair for the index feed; reading the tree a different way here would let
/// the browser and the ingest describe different directories.
/// What: `assistants_root` and `knowledge_dir` are injected so a test can point
/// both at a tempdir. An agent with no `[[stores]]` binding at all still has its
/// home's `okg/` tree, which is the #4325 default layout — that is a real tree,
/// not an error. The label is derived from what the BINDING declares, never from
/// the resolved path, so it cannot pick up an absolute prefix.
/// Test: `resolves_the_home_tree_for_a_rooted_binding`,
/// `resolves_the_shared_pool_for_a_plain_binding`,
/// `resolves_the_home_tree_when_no_store_is_bound`,
/// `every_resolution_arm_labels_the_tree_without_a_path`.
pub fn resolve_okg_root(
    agent: &str,
    stores: &StoresConfig,
    assistants_root: &Path,
    knowledge_dir: &Path,
) -> Result<OkgTree, String> {
    let home = || -> Result<AssistantHome, String> {
        let id = crate::assistants::AssistantInstanceId::new(agent).map_err(|e| e.to_string())?;
        Ok(AssistantHome::under(assistants_root, id))
    };
    match stores.primary() {
        Some(binding) if binding.root.is_some() => {
            let declared = binding.root.as_deref().unwrap_or(OKG_DIR).trim();
            Ok(OkgTree {
                root: home()?.store_root(binding).map_err(|e| e.to_string())?,
                label: format!("{agent}/{}", declared.trim_start_matches("./")),
            })
        }
        Some(binding) => {
            let label = binding.resolved_tree(agent);
            let root = super::binding::okg_tree_path(knowledge_dir, &label).ok_or_else(|| {
                format!(
                    "store `{}` declares tree `{label}`, which does not resolve to a \
                     knowledge-tree directory",
                    binding.name
                )
            })?;
            Ok(OkgTree { root, label })
        }
        None => Ok(OkgTree {
            root: home()?.okg_dir(),
            label: format!("{agent}/{OKG_DIR}"),
        }),
    }
}

// Split into a sibling file (issue #610's 500-SLOC production cap) —
// mirrors `status.rs` / `status_tests.rs` in this same directory.
#[cfg(test)]
#[path = "okg_graph_tests.rs"]
mod tests;
