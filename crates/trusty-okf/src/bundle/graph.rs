//! Graph read: the triples and definitions held in a bundle tree (moved from
//! trusty-agents' graph reader module, #10746; SPEC-OKF-01 §2.7.2).
//!
//! Why: the graph an agent UI shows must be the graph held in the tree
//! itself and nothing else. The reader is generic: it takes a root path and
//! reads frontmatter only, so it lives here; mapping an agent to its tree
//! stays in trusty-agents.
//! What: [`read_graph`] walks the root's collection directories and returns
//! both halves of the graph at once.
//!   - A DEFINITION is one entity: what the subject is (`type`), where it
//!     lives (`collection`), and its one-line summary ([`summarise`]).
//!   - A TRIPLE is one relationship edge: an entity's frontmatter key
//!     holding `[[wiki-link]]` targets, one triple per target. The envelope
//!     fields ([`base_envelope`]) are excluded, so a `[[link]]` inside a
//!     prose `description` is not read as an edge.
//!
//! Output is fully sorted, so two reads of an unchanged tree are
//! byte-identical. #10746: the walk no longer follows symbolic links
//! (§2.11 file system rule 5); trusty-kb's walk did.
//!
//! Test: `graph_reads_triples_definitions_and_skips_links`; trusty-agents
//! keeps `reads_triples_and_definitions_from_a_tree` and the rest of its
//! `okg_graph_tests` against this reader through its re-export.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_yaml::Value;

use crate::OkfError;
use crate::bundle::paths::io_err;
use crate::format::concept::{Entity, link_values};
use crate::format::container::{base_envelope, is_reserved_file};

/// One relationship edge, as a subject-predicate-object triple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Triple {
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
pub struct Definition {
    /// Same spelling as [`Triple::subject`], so the two halves join.
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
pub struct SubjectCount {
    /// The subject.
    pub subject: String,
    /// Triples whose subject this is. Zero for a defined entity with no edges.
    pub count: usize,
}

/// Both halves of one tree's graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    /// Every relationship edge, sorted by subject, predicate, then object.
    pub triples: Vec<Triple>,
    /// Every entity definition, sorted by collection then slug.
    pub definitions: Vec<Definition>,
}

impl Graph {
    /// Every subject in the graph with its triple count, subject-sorted.
    ///
    /// Why: a browser's subject list must show an entity that has a
    /// definition but no edges yet.
    /// What: the union of definition subjects and triple subjects; the count
    /// is triples only.
    /// Test: `graph_reads_triples_definitions_and_skips_links`.
    pub fn subject_counts(&self) -> Vec<SubjectCount> {
        let mut counts: BTreeMap<&str, usize> = self
            .definitions
            .iter()
            .map(|d| (d.subject.as_str(), 0))
            .collect();
        for t in &self.triples {
            *counts.entry(t.subject.as_str()).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .map(|(subject, count)| SubjectCount {
                subject: subject.to_string(),
                count,
            })
            .collect()
    }

    /// The definitions for `subjects`, in this graph's own definition order.
    pub fn definitions_for(&self, subjects: &[String]) -> Vec<Definition> {
        self.definitions
            .iter()
            .filter(|d| subjects.contains(&d.subject))
            .cloned()
            .collect()
    }
}

/// A one-line summary of an entity: its `description`, else the first
/// non-blank body line that is not a `##` heading (moved from trusty-kb
/// `store.rs`, #10746). Test: `graph_reads_triples_definitions_and_skips_links`.
pub fn summarise(entity: &Entity) -> Option<String> {
    if let Some(Value::String(desc)) = entity.frontmatter.get("description") {
        let d = desc.trim();
        if !d.is_empty() {
            return Some(d.to_string());
        }
    }
    entity
        .body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("##"))
        .map(str::to_string)
}

/// Whether a frontmatter key carries relationship edges: every key except
/// the base-envelope names, so it fails open for a new envelope key that is
/// not added to [`base_envelope`].
fn is_edge_field(key: &str) -> bool {
    !base_envelope().iter().any(|f| f.name == key)
}

/// Sorted names of the entries in `dir` that pass `keep`, skipping links.
fn listing(
    dir: &Path,
    keep: impl Fn(&str, &std::fs::FileType) -> bool,
) -> Result<Vec<String>, OkfError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| io_err(dir, e))? {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let ft = entry.file_type().map_err(|e| io_err(&entry.path(), e))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !ft.is_symlink() && keep(&name, &ft) {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Read both halves of the graph out of the tree at `root`.
///
/// Why/What: see the module doc. A root that does not exist is an empty
/// graph (a home exists before its first ingest). A malformed entity file is
/// skipped rather than failing the read, so one hand-edited file cannot
/// blank the whole graph. Collections are the root's directories not
/// starting with `.` or `_`; entities are the `.md` files directly in each,
/// reserved files excluded.
/// Test: `graph_reads_triples_definitions_and_skips_links`.
pub fn read_graph(root: &Path) -> Result<Graph, OkfError> {
    let mut graph = Graph::default();
    if !root.is_dir() {
        return Ok(graph);
    }
    let collections = listing(root, |n, ft| {
        ft.is_dir() && !n.starts_with('.') && !n.starts_with('_')
    })?;
    for collection in collections {
        let dir = root.join(&collection);
        let files = listing(&dir, |n, ft| {
            ft.is_file() && n.ends_with(".md") && !is_reserved_file(n)
        })?;
        for name in files {
            let slug = name.trim_end_matches(".md").to_string();
            let Ok(text) = std::fs::read_to_string(dir.join(&name)) else {
                continue;
            };
            let Ok(entity) = Entity::from_content(&text) else {
                continue;
            };
            let subject = entity
                .get_str("title")
                .map(str::to_string)
                .unwrap_or_else(|| slug.clone());
            let rel = format!("{collection}/{name}");
            graph.definitions.push(Definition {
                subject: subject.clone(),
                collection: collection.clone(),
                slug: slug.clone(),
                kind: entity.get_str("type").map(str::to_string),
                summary: summarise(&entity),
                path: rel.clone(),
            });
            let Value::Mapping(map) = &entity.frontmatter else {
                continue;
            };
            for (key, value) in map {
                let Some(predicate) = key.as_str().filter(|k| is_edge_field(k)) else {
                    continue;
                };
                for object in link_values(value) {
                    graph.triples.push(Triple {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the moved reader must return both halves, keep envelope fields
    /// out of the edges, and not follow a link (§2.11 rule 5).
    /// What: two collections, one edge-bearing entity, a description holding
    /// a `[[link]]`, a malformed file and (Unix) a linked entity file.
    /// Test: self-contained.
    #[test]
    fn graph_reads_triples_definitions_and_skips_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("people")).unwrap();
        std::fs::create_dir_all(root.join("orgs")).unwrap();
        std::fs::create_dir_all(root.join("_sources")).unwrap();
        std::fs::write(
            root.join("people/bob.md"),
            "---\ntype: Person\ntitle: Bob\nworks_at: \"[[Duetto]]\"\n---\n\nThe owner.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("orgs/duetto.md"),
            "---\ntype: Org\ndescription: has [[Bob]]\n---\n",
        )
        .unwrap();
        std::fs::write(root.join("people/broken.md"), "---\n: : x : :\n---\n").unwrap();
        std::fs::write(root.join("_sources/x.md"), "---\ntype: X\n---\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("people/bob.md"), root.join("people/alias.md"))
            .unwrap();
        let g = read_graph(root).unwrap();
        let triples: Vec<_> = g
            .triples
            .iter()
            .map(|t| (t.subject.as_str(), t.predicate.as_str(), t.object.as_str()))
            .collect();
        assert_eq!(triples, vec![("Bob", "works_at", "Duetto")]);
        let defs: Vec<_> = g.definitions.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(defs, vec!["orgs/duetto.md", "people/bob.md"]);
        assert_eq!(g.definitions[1].summary.as_deref(), Some("The owner."));
        let counts: Vec<_> = g.subject_counts().into_iter().map(|c| c.count).collect();
        assert_eq!(counts, vec![1, 0]);
        assert_eq!(read_graph(&root.join("absent")).unwrap(), Graph::default());
    }
}
