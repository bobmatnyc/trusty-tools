//! `kb_convert_tree` — normalise an arbitrary markdown tree into the model.
//!
//! Why: users already have doc trees (`/people`, `/projects`, loose notes). The
//! converter maps them into the OKF collection model WITHOUT ever destroying
//! content — it adds/normalises frontmatter, relocates confidently-classifiable
//! files into their collection, records provenance, and regenerates indexes. It
//! must be idempotent and byte-stable: a second pass over converted output is a
//! no-op (the primary regression guard).
//!
//! What (the 11-point conversion contract): every `.md` becomes exactly one of a
//! concept (frontmatter with at least `type`) or a reserved file; `type` is
//! always present (folder prior → schema.org type-of-collection → filename
//! heuristic → fail-open `Note`); body prose is never destroyed; unknown keys
//! are preserved/merged; provenance is recorded (original path → `sources[]`,
//! original filename → `alias` when renamed, moves appended to
//! `_state/conversion-log.md`); facts are never fabricated (empty is valid);
//! the typed-edge + body link graph is maintained and inverse edges materialised
//! ([`KbStore::reconcile_all`]); entities merge ONLY on an explicit
//! `same_as`/exact-alias match (this converter never guess-merges — distinct
//! files stay distinct); output is deterministic and byte-stable; and `index.md`
//! listings are refreshed after conversion. `report_only` (the DEFAULT) returns
//! the plan without writing.
//!
//! Test: `report_only_is_side_effect_free`, `convert_assigns_type_fail_open`,
//! `convert_in_place_is_idempotent`, `convert_maps_known_folder`.

use serde::Serialize;
use serde_yaml::{Mapping, Value};
use walkdir::WalkDir;

use crate::entity::{Entity, slugify};
use crate::schema::{Profile, is_reserved_file};
use crate::store::KbStore;

/// What the converter will do with one source file.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    /// A reserved file (index.md/log.md) — left as-is / regenerated.
    Reserved,
    /// Frontmatter normalised at the same path (unmapped or already placed).
    Normalize,
    /// Relocated into a known collection directory.
    Move,
}

/// One file's conversion plan entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanItem {
    /// Root-relative source path.
    pub source: String,
    /// Root-relative target path (equals `source` for Normalize/Reserved).
    pub target: String,
    /// The `type` that will be assigned (empty for reserved files).
    pub assigned_type: String,
    /// What will happen to the file.
    pub action: PlanAction,
    /// The collection the file maps to, if any.
    pub collection: Option<String>,
}

/// The result of a conversion run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConvertReport {
    /// The mode requested.
    pub mode: String,
    /// The source root (string form).
    pub source_root: String,
    /// Per-file plan, source-sorted.
    pub plan: Vec<PlanItem>,
    /// Root-relative paths of files that stayed put (unmapped, kept in place).
    pub unmapped: Vec<String>,
    /// Whether the plan was applied (false for report_only).
    pub applied: bool,
}

impl KbStore {
    /// Convert the tree rooted at this store's root into the collection model.
    ///
    /// Why/What/Test: see the module doc. `report_only` computes and returns the
    /// plan with zero side effects; `in_place` applies it, regenerates indexes,
    /// and reconciles edges.
    pub fn convert_tree(&self, report_only: bool, now: &str) -> anyhow::Result<ConvertReport> {
        let plan = self.build_plan(now)?;
        let unmapped: Vec<String> = plan
            .iter()
            .filter(|p| p.action == PlanAction::Normalize && p.collection.is_none())
            .map(|p| p.source.clone())
            .collect();

        if !report_only {
            self.apply_plan(&plan, now)?;
            self.ensure_structure()?;
            self.reconcile_all(now)?;
        }

        Ok(ConvertReport {
            mode: if report_only {
                "report_only"
            } else {
                "in_place"
            }
            .to_string(),
            source_root: self.root.to_string_lossy().to_string(),
            plan,
            unmapped,
            applied: !report_only,
        })
    }

    /// Walk the tree and compute the (source-sorted) plan without writing.
    fn build_plan(&self, now: &str) -> anyhow::Result<Vec<PlanItem>> {
        let reverse = reverse_type_map(&self.profile);
        let mut items = Vec::new();
        for entry in WalkDir::new(&self.root).sort_by_file_name() {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let rel = self.rel(path);
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if rel.starts_with("_state/") || rel.contains("/.") || rel.starts_with('.') {
                continue;
            }
            if !file_name.ends_with(".md") {
                continue;
            }
            if is_reserved_file(file_name) {
                items.push(PlanItem {
                    source: rel.clone(),
                    target: rel,
                    assigned_type: String::new(),
                    action: PlanAction::Reserved,
                    collection: None,
                });
                continue;
            }
            items.push(self.plan_concept(path, &rel, &reverse, now)?);
        }
        Ok(items)
    }

    /// Classify one concept file into a plan item.
    fn plan_concept(
        &self,
        path: &std::path::Path,
        rel: &str,
        reverse: &[(String, String)],
        _now: &str,
    ) -> anyhow::Result<PlanItem> {
        let entity = self.read_entity_at(path)?.unwrap_or_else(Entity::empty);
        let existing_type = entity.get_str("type").map(str::to_string);
        let parent = parent_dir_name(rel);
        let stem = file_stem(rel);

        // Collection precedence: known parent folder → type reverse-map → none.
        let collection = if self.profile.is_known_collection(&parent) {
            Some(parent.clone())
        } else {
            existing_type.as_ref().and_then(|t| {
                reverse
                    .iter()
                    .find(|(ty, _)| ty.eq_ignore_ascii_case(t))
                    .map(|(_, c)| c.clone())
            })
        };

        // Type: existing → collection default → fail-open Note.
        let assigned_type = existing_type.clone().unwrap_or_else(|| {
            collection
                .as_ref()
                .map(|c| self.profile.default_type_for(c))
                .unwrap_or_else(|| "Note".to_string())
        });

        let title = entity
            .get_str("title")
            .map(str::to_string)
            .unwrap_or_else(|| humanize(&stem));
        let slug = slugify(&title);

        let target = match &collection {
            Some(c) => format!("{c}/{slug}.md"),
            None => rel.to_string(),
        };
        let action = if target == rel {
            PlanAction::Normalize
        } else {
            PlanAction::Move
        };
        Ok(PlanItem {
            source: rel.to_string(),
            target,
            assigned_type,
            action,
            collection,
        })
    }

    /// Apply a computed plan: write normalised entities, move files, log moves.
    fn apply_plan(&self, plan: &[PlanItem], now: &str) -> anyhow::Result<()> {
        let mut moves: Vec<(String, String)> = Vec::new();
        for item in plan {
            if item.action == PlanAction::Reserved {
                continue;
            }
            let src_path = self.root.join(&item.source);
            let entity = self
                .read_entity_at(&src_path)?
                .unwrap_or_else(Entity::empty);
            let moved = item.action == PlanAction::Move;
            let normalised = normalise_concept(entity, item, &item.source, moved, now);
            let target_path = self.root.join(&item.target);
            self.write_entity_at(&target_path, &normalised)?;
            if moved && src_path != target_path {
                std::fs::remove_file(&src_path)?;
                moves.push((item.source.clone(), item.target.clone()));
            }
        }
        if !moves.is_empty() {
            self.append_conversion_log(&moves, now)?;
        }
        Ok(())
    }

    /// Append a dated move block to `_state/conversion-log.md`.
    fn append_conversion_log(&self, moves: &[(String, String)], now: &str) -> anyhow::Result<()> {
        let dir = self.root.join("_state");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("conversion-log.md");
        let mut out = std::fs::read_to_string(&path).unwrap_or_default();
        if out.is_empty() {
            out.push_str("# Conversion log\n\nAppend-only record of file moves.\n");
        }
        out.push_str(&format!("\n## {now}\n\n"));
        for (from, to) in moves {
            out.push_str(&format!("- `{from}` -> `{to}`\n"));
        }
        std::fs::write(&path, out)?;
        Ok(())
    }
}

/// Normalise a concept entity per the conversion contract (add type/title,
/// record provenance on move) with byte-stable created/updated handling.
fn normalise_concept(
    mut entity: Entity,
    item: &PlanItem,
    source: &str,
    moved: bool,
    now: &str,
) -> Entity {
    // Preserve unknown keys; only fill required/derived fields.
    let map = ensure_map(&mut entity.frontmatter);
    if !map.contains_key("type") {
        map.insert(sk("type"), Value::String(item.assigned_type.clone()));
    }
    let stem = file_stem(source);
    if !map.contains_key("title") {
        map.insert(sk("title"), Value::String(humanize(&stem)));
    }
    if moved {
        union_str(map, "sources", source);
        // Original filename → alias, when the slug renamed it.
        if slugify(&humanize(&stem)) != file_stem(&item.target).replace(".md", "") {
            union_str(map, "aliases", &humanize(&stem));
        }
    }
    if !map.contains_key("created") {
        map.insert(sk("created"), Value::String(now.to_string()));
    }
    if !map.contains_key("updated") {
        map.insert(sk("updated"), Value::String(now.to_string()));
    }
    entity
}

/// Reverse map: schema.org type (lowercased) → collection name.
fn reverse_type_map(profile: &Profile) -> Vec<(String, String)> {
    profile
        .collections
        .iter()
        .filter_map(|c| {
            c.schema_type
                .map(|t| (t.to_lowercase(), c.name.to_string()))
        })
        .collect()
}

fn sk(k: &str) -> Value {
    Value::String(k.to_string())
}

fn ensure_map(v: &mut Value) -> &mut Mapping {
    if !matches!(v, Value::Mapping(_)) {
        *v = Value::Mapping(Mapping::new());
    }
    match v {
        Value::Mapping(m) => m,
        _ => unreachable!(),
    }
}

/// Union a scalar string into a list-valued frontmatter field (dedup, sorted).
fn union_str(map: &mut Mapping, field: &str, value: &str) {
    let key = sk(field);
    let mut items: Vec<String> = match map.get(&key) {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    if !items.iter().any(|i| i == value) {
        items.push(value.to_string());
    }
    items.sort();
    items.dedup();
    let seq = items.into_iter().map(Value::String).collect();
    map.insert(key, Value::Sequence(seq));
}

/// The immediate parent directory name of a root-relative path ("" at root).
fn parent_dir_name(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _)) => dir.rsplit('/').next().unwrap_or(dir).to_string(),
        None => String::new(),
    }
}

/// The filename stem (no directory, no `.md`).
fn file_stem(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.trim_end_matches(".md").to_string()
}

/// Humanise a slug/stem into a title ("ada-lovelace" → "Ada Lovelace").
fn humanize(stem: &str) -> String {
    stem.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(files: &[(&str, &str)]) -> (tempfile::TempDir, KbStore) {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let s = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
        (tmp, s)
    }

    /// Why: report_only is the REQUIRED default and must never touch disk.
    /// What: runs report_only, asserts the source file is byte-unchanged and the
    /// report carries a plan.
    /// Test: self-contained.
    #[test]
    fn report_only_is_side_effect_free() {
        let (_t, s) = store_with(&[("notes/hello.md", "# Hello\n\nbody\n")]);
        let before = std::fs::read_to_string(s.root.join("notes/hello.md")).unwrap();
        let report = s.convert_tree(true, "2026-01-01T00:00:00Z").unwrap();
        let after = std::fs::read_to_string(s.root.join("notes/hello.md")).unwrap();
        assert_eq!(before, after);
        assert!(!report.applied);
        assert!(!report.plan.is_empty());
    }

    /// Why: fail-open typing — an unclassifiable file still gets a `type`.
    /// What: converts a loose note; asserts it is normalised in place with
    /// `type: Note` and stays unmapped.
    /// Test: self-contained.
    #[test]
    fn convert_assigns_type_fail_open() {
        let (_t, s) = store_with(&[("hello.md", "# Hello\n\nbody\n")]);
        s.convert_tree(false, "2026-01-01T00:00:00Z").unwrap();
        let e = Entity::from_content(&std::fs::read_to_string(s.root.join("hello.md")).unwrap())
            .unwrap();
        assert_eq!(e.get_str("type"), Some("Note"));
        assert!(e.body.contains("# Hello"));
    }

    /// Why: a known-named folder maps its files directly into the collection.
    /// What: converts `people/ada-lovelace.md`; asserts it becomes a Person and
    /// records provenance.
    /// Test: self-contained.
    #[test]
    fn convert_maps_known_folder() {
        let (_t, s) = store_with(&[("people/ada-lovelace.md", "First programmer.\n")]);
        s.convert_tree(false, "2026-01-01T00:00:00Z").unwrap();
        let e = Entity::from_content(
            &std::fs::read_to_string(s.root.join("people/ada-lovelace.md")).unwrap(),
        )
        .unwrap();
        assert_eq!(e.get_str("type"), Some("Person"));
        assert_eq!(e.get_str("title"), Some("Ada Lovelace"));
    }

    /// Why: byte-stability across passes is the primary regression guard.
    /// What: converts twice; asserts the whole tree is byte-identical after the
    /// second pass.
    /// Test: self-contained.
    #[test]
    fn convert_in_place_is_idempotent() {
        let (_t, s) = store_with(&[
            (
                "people/ada.md",
                "---\ntype: Person\n---\n\nMathematician.\n",
            ),
            ("random.md", "loose note\n"),
        ]);
        s.convert_tree(false, "2026-01-01T00:00:00Z").unwrap();
        let snap1 = snapshot(&s.root);
        let report = s.convert_tree(false, "2026-09-09T00:00:00Z").unwrap();
        let snap2 = snapshot(&s.root);
        assert_eq!(snap1, snap2, "second convert pass must be a no-op");
        assert!(report.plan.iter().all(|p| p.action != PlanAction::Move));
    }

    /// Snapshot every file under `root` as sorted (rel, bytes) for comparison.
    fn snapshot(root: &std::path::Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for entry in WalkDir::new(root).sort_by_file_name() {
            let entry = entry.unwrap();
            if entry.path().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.push((rel, std::fs::read_to_string(entry.path()).unwrap()));
            }
        }
        out.sort();
        out
    }
}
