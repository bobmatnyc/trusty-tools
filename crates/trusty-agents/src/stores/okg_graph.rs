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
//! What: [`read_graph`] (now in trusty-okf, re-exported here, #10746) walks a
//! KB tree and returns both halves of the graph at once.
//!   - A DEFINITION is one entity: what the subject is (`type`), where it lives
//!     (`collection`), and its one-line summary.
//!   - A TRIPLE is one relationship edge: an entity's frontmatter key holding
//!     `[[wiki-link]]` targets, one triple per target. The envelope fields OKF
//!     defines as description rather than relationship are excluded, so a
//!     `[[link]]` inside a prose `description` is not read as an edge.
//! [`resolve_okg_root`] maps an agent to the tree it browses, mirroring
//! [`super::binding`]'s resolution so an OKG tool and this reader can never
//! address different directories.
//!
//! Output is fully sorted, so two reads of an unchanged tree are byte-identical.
//!
//! Test: `reads_triples_and_definitions_from_a_tree`,
//! `resolves_the_home_tree_for_a_rooted_binding`.

use std::path::{Path, PathBuf};

use crate::assistants::{AssistantHome, OKG_DIR};
use crate::stores::StoresConfig;

// #10746 (S3): the generic reader and its types moved to trusty-okf
// (`bundle::graph`, SPEC-OKF-01 §2.7.2); S11 imports them from trusty-okf
// directly. The old names stay, so every caller and `okg_graph_tests` are
// unchanged.
pub use trusty_okf::bundle::graph::{
    Definition as OkgDefinition, Graph as OkgGraph, SubjectCount as OkgSubjectCount,
    Triple as OkgTriple, read_graph,
};

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
