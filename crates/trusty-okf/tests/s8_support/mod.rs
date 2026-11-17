//! Shared fixtures for the slice S8 import criteria (#10746): a source tree
//! and a bundle in sibling directories, the document-tree store, one-call
//! plan and import, and byte snapshots of a tree.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use sha2::{Digest, Sha256};
use trusty_okf::OkfError;
use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::build::registry::{Locator, SourceSpec};
use trusty_okf::bundle::Bundle;
use trusty_okf::extract::{Extractor, NoWorker};
use trusty_okf::format::ConceptId;
use trusty_okf::import::{
    DocTreeSource, ImportPlan, ImportReport, ImportSpec, import, plan_import,
};

pub const STORE: &str = "import-src";

pub fn instant(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

pub fn now() -> DateTime<FixedOffset> {
    instant("2026-11-04T12:00:00Z")
}

pub fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

/// A source tree `src/` and an empty bundle `bundle/` under one temp dir.
pub struct Fx {
    pub tmp: tempfile::TempDir,
    pub src: PathBuf,
    pub root: PathBuf,
}

impl Fx {
    pub fn new(files: &[(&str, &str)]) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (src, root) = (base.join("src"), base.join("bundle"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let fx = Fx { tmp, src, root };
        for (rel, text) in files {
            fx.write(rel, text);
        }
        fx
    }

    pub fn write(&self, rel: &str, text: &str) {
        write_bytes(&self.src, rel, text.as_bytes());
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.src.join(rel)).unwrap();
    }

    /// The document-tree spec for `src/`, tombstoning on.
    pub fn spec(&self) -> ImportSpec {
        let mut store = SourceSpec::new(
            STORE,
            None,
            Locator::DocStore {
                path: self.src.to_string_lossy().into_owned(),
                extensions: Vec::new(),
                recursive: true,
            },
            "2026-11-04T00:00:00Z",
        );
        store.tombstone_deleted = true;
        ImportSpec::new(store)
    }

    pub fn source(&self) -> DocTreeSource {
        DocTreeSource::open(&self.src, &DocStorePolicy::new(vec![self.src.clone()])).unwrap()
    }

    pub fn plan(&self, spec: &ImportSpec) -> Result<ImportPlan, OkfError> {
        let b = Bundle::open(&self.root)?.0;
        plan_import(&b, spec, &mut self.source())
    }

    pub fn import_with(
        &self,
        spec: &ImportSpec,
        ex: &dyn Extractor,
        at: DateTime<FixedOffset>,
    ) -> Result<ImportReport, OkfError> {
        let plan = self.plan(spec)?;
        let mut b = Bundle::open(&self.root)?.0;
        import(&mut b, &plan, &mut self.source(), ex, at)
    }

    /// Plan and import with no worker, at [`now`].
    pub fn import(&self) -> ImportReport {
        self.import_with(&self.spec(), &NoWorker, now()).unwrap()
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }
}

pub fn write_bytes(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// Every file's SHA-256 and every directory's sorted listing under `dir`,
/// links included as their target text; nothing followed.
pub fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let e = e.unwrap();
        let rel = e
            .path()
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let ft = e.file_type();
        let v = if ft.is_symlink() {
            format!("link:{}", std::fs::read_link(e.path()).unwrap().display())
        } else if ft.is_dir() {
            let mut names: Vec<String> = std::fs::read_dir(e.path())
                .unwrap()
                .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            format!("dir:{}", names.join(","))
        } else {
            let bytes = std::fs::read(e.path()).unwrap();
            format!("{:x}", Sha256::digest(&bytes))
        };
        out.insert(rel, v);
    }
    out
}

/// [`snapshot`] without `_sources/` (bookkeeping run IDs differ between an
/// interrupted and an uninterrupted run).
pub fn concepts_snapshot(dir: &Path) -> BTreeMap<String, String> {
    snapshot(dir)
        .into_iter()
        .filter(|(k, _)| !k.starts_with("_sources") && !k.is_empty())
        .collect()
}

/// The frontmatter mapping of a bundle file.
pub fn frontmatter(root: &Path, rel: &str) -> serde_yaml::Mapping {
    let text = std::fs::read_to_string(root.join(rel)).unwrap();
    let yaml = text.split("---\n").nth(1).unwrap();
    serde_yaml::from_str(yaml).unwrap()
}

/// The write outcome and reason of one item in a report.
pub fn outcome(r: &ImportReport, item: &str) -> (String, String, Option<String>) {
    let i = r
        .items
        .iter()
        .find(|i| i.item_id == item)
        .unwrap_or_else(|| panic!("{item} not in report: {:?}", r.items));
    (i.read.clone(), i.write.clone(), i.reason.clone())
}
