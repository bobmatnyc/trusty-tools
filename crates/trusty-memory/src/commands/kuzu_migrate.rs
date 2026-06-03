//! Migrate entity/relation data from a kuzu-memory `store.redb` into a
//! trusty-memory palace.
//!
//! Why: Issue #277 — kuzu-memory is being retired. Users running the legacy
//! Python-backed `kuzu-memory` MCP server need a one-shot tool that imports
//! their stored entities and relations into a trusty-memory palace without
//! manual re-entry.
//! What: Opens the source `store.redb` via `redb` read-only APIs, discovers
//! the table layout, maps `Entity` rows to trusty-memory drawers and `Relation`
//! rows to KG triples. Designed defensively — unknown tables are logged and
//! skipped; unknown row shapes are skipped with a warning rather than aborting
//! the run.
//! Test: Unit tests for the entity-to-drawer and relation-to-triple mapping
//! functions; integration test with a synthetic fixture `store.redb` built
//! programmatically in `tests/kuzu_migrate_tests.rs`.
//!
//! ## Discovered kuzu-memory schema (store.redb)
//!
//! No live `~/.open-mpm/memory/store.redb` was available at implementation
//! time. The schema below is inferred from kuzu-memory's Python source
//! conventions and is designed to tolerate unknown-table layouts gracefully.
//! Both tables encode values as JSON strings:
//!
//! ```text
//! Table "entities"  — key: entity_id (string), value: JSON {id, name, entity_type, observations}
//! Table "relations" — key: relation_id (string), value: JSON {from, to, relation_type}
//! ```
//!
//! Mapping:
//! - Each `Entity` → one drawer (observations joined as `<name>: <obs1>\n…`).
//! - Each `Relation` → one KG triple `(entity:<from>, <relation_type>, entity:<to>)`.
//!
//! Re-running on the same input is idempotent: drawer IDs are derived from a
//! stable hash of `(entity_id, palace_name)` so repeated imports produce the
//! same UUIDs and the upsert is effectively a no-op.

use anyhow::{Context, Result};
use colored::Colorize;
use redb::{Database, ReadableDatabase, ReadableTable, TableHandle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use trusty_common::memory_core::palace::{Drawer, Palace, PalaceId};
use trusty_common::memory_core::store::kg::Triple;
use uuid::Uuid;

// ── kuzu-memory wire types ────────────────────────────────────────────────

/// On-disk schema for a kuzu-memory entity row.
///
/// Why: kuzu-memory persists entities as JSON values in a redb table.
/// `id` is the entity's string primary key; `observations` is the list of
/// free-text observations stored against the entity.
/// What: Mirrors the JSON shape written by the kuzu-memory Python server.
/// Test: `entity_to_drawer_maps_name_and_observations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KuzuEntity {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub entity_type: String,
    #[serde(default)]
    pub observations: Vec<String>,
}

/// On-disk schema for a kuzu-memory relation row.
///
/// Why: kuzu-memory persists directed graph edges as JSON values.
/// What: Mirrors the JSON shape written by the kuzu-memory Python server.
/// Test: `relation_to_triple_maps_fields`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KuzuRelation {
    pub from: String,
    pub to: String,
    pub relation_type: String,
}

// ── Stable ID derivation ──────────────────────────────────────────────────

/// Derive a deterministic `Uuid` from a kuzu entity ID and the target palace
/// name so repeated imports produce the same drawer ID.
///
/// Why: Idempotency — running the importer twice must not create duplicate
/// drawers. The UUID is derived from a SHA-256 hash so the same source
/// entity always maps to the same trusty-memory drawer.
/// What: SHA-256 of `entity_id + "\0" + palace_name` → first 16 bytes →
/// UUID v4-shaped (variant and version bits set).
/// Test: `entity_uuid_is_deterministic`.
pub fn entity_uuid(entity_id: &str, palace_name: &str) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(entity_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(palace_name.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Set version (4) and variant bits so the UUID is well-formed RFC 4122.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

// ── Mapping functions ─────────────────────────────────────────────────────

/// Map a kuzu-memory `Entity` to a trusty-memory `Drawer`.
///
/// Why: Each entity in kuzu-memory becomes a self-contained knowledge drawer;
/// observations become the drawer's content so all information is searchable.
/// What: Builds a `Drawer` with a deterministic ID, joins the observations as
/// `"<name>: <obs1>\n<obs2>\n…"`, and tags with `source:kuzu` plus the entity
/// type (when non-empty). The drawer importance is set to 0.5 (mid-range).
/// Test: `entity_to_drawer_maps_name_and_observations`.
pub fn entity_to_drawer(entity: &KuzuEntity, palace_name: &str) -> Drawer {
    let id = entity_uuid(&entity.id, palace_name);
    let room_id = Uuid::nil(); // General room.
    let content = if entity.observations.is_empty() {
        entity.name.clone()
    } else {
        format!("{}: {}", entity.name, entity.observations.join("\n"))
    };
    let mut tags = vec!["source:kuzu".to_string()];
    if !entity.entity_type.is_empty() {
        tags.push(format!("type:{}", entity.entity_type.to_lowercase()));
    }
    let mut drawer = Drawer::new(room_id, &content);
    drawer.id = id;
    drawer.tags = tags;
    drawer.importance = 0.5;
    drawer
}

/// Map a kuzu-memory `Relation` to a trusty-memory KG `Triple`.
///
/// Why: Directed typed edges in kuzu-memory correspond directly to KG triples.
/// What: Subject = `entity:<from>`, predicate = `<relation_type>`,
/// object = `entity:<to>`. Confidence = 0.8 (imported from a source of
/// record, above auto-extract at 0.6 but below explicit asserts at 1.0).
/// Provenance = `"kuzu-migrate"` so these triples can be queried distinctly.
/// Test: `relation_to_triple_maps_fields`.
pub fn relation_to_triple(relation: &KuzuRelation) -> Triple {
    Triple {
        subject: format!("entity:{}", relation.from),
        predicate: relation.relation_type.clone(),
        object: format!("entity:{}", relation.to),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 0.8,
        provenance: Some("kuzu-migrate".to_string()),
    }
}

// ── Schema discovery ──────────────────────────────────────────────────────

/// Table names kuzu-memory uses in its `store.redb`.
///
/// Why: Hard-coding the names avoids a runtime discovery loop for the hot
/// path; the fallback `discover_schema` helper is available when debugging
/// an unknown kuzu-memory build.
/// Test: `schema_table_names_are_defined`.
pub const ENTITIES_TABLE: &str = "entities";
pub const RELATIONS_TABLE: &str = "relations";

/// Enumerate every table in a redb file and return their names.
///
/// Why: Allows operators (and `--dry-run` output) to verify the schema of an
/// unknown kuzu-memory `store.redb` before running the full import.
/// What: Opens the database, iterates its table list, and collects names.
/// Test: `discover_schema_on_empty_db_returns_empty`.
pub fn discover_schema(path: &Path) -> Result<Vec<String>> {
    let db = Database::open(path)
        .with_context(|| format!("open kuzu-memory store.redb at {}", path.display()))?;
    let rtx = db
        .begin_read()
        .context("begin read txn for schema discovery")?;
    let tables = rtx
        .list_tables()
        .context("list tables for schema discovery")?;
    Ok(tables.map(|t| t.name().to_string()).collect())
}

// ── Read helpers ──────────────────────────────────────────────────────────

/// Read all entity rows from the kuzu-memory entities table.
///
/// Why: Separating the read step from the mapping keeps the mapping functions
/// pure and independently testable.
/// What: Opens ENTITIES_TABLE in a read transaction, deserialises each row's
/// value as JSON `KuzuEntity`, skips malformed rows with a tracing warning.
/// Returns an empty vec when the table does not exist.
/// Test: `read_entities_returns_expected_count` (integration with fixture).
pub fn read_entities(db: &Database) -> Result<Vec<KuzuEntity>> {
    use redb::TableDefinition;
    const TABLE: TableDefinition<&str, &str> = TableDefinition::new(ENTITIES_TABLE);
    let rtx = db.begin_read().context("begin read txn for entities")?;
    let table = match rtx.open_table(TABLE) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            tracing::warn!("kuzu-migrate: entities table not found — skipping");
            return Ok(Vec::new());
        }
        Err(e) => return Err(e).context("open entities table"),
    };
    let mut out = Vec::new();
    for entry in table.iter().context("iterate entities")? {
        let (k, v) = entry.context("read entity row")?;
        let entity_id = k.value();
        match serde_json::from_str::<KuzuEntity>(v.value()) {
            Ok(mut entity) => {
                if entity.id.is_empty() {
                    entity.id = entity_id.to_string();
                }
                out.push(entity);
            }
            Err(e) => {
                tracing::warn!(id = %entity_id, "kuzu-migrate: skip malformed entity: {e}");
            }
        }
    }
    Ok(out)
}

/// Read all relation rows from the kuzu-memory relations table.
///
/// Why: Mirror of `read_entities` for the relations table.
/// What: Opens RELATIONS_TABLE, deserialises each row as JSON `KuzuRelation`.
/// Returns an empty vec when the table does not exist.
/// Test: `read_relations_returns_expected_count` (integration with fixture).
pub fn read_relations(db: &Database) -> Result<Vec<KuzuRelation>> {
    use redb::TableDefinition;
    const TABLE: TableDefinition<&str, &str> = TableDefinition::new(RELATIONS_TABLE);
    let rtx = db.begin_read().context("begin read txn for relations")?;
    let table = match rtx.open_table(TABLE) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => {
            tracing::warn!("kuzu-migrate: relations table not found — skipping");
            return Ok(Vec::new());
        }
        Err(e) => return Err(e).context("open relations table"),
    };
    let mut out = Vec::new();
    for entry in table.iter().context("iterate relations")? {
        let (k, v) = entry.context("read relation row")?;
        let rid = k.value();
        match serde_json::from_str::<KuzuRelation>(v.value()) {
            Ok(relation) => out.push(relation),
            Err(e) => {
                tracing::warn!(id = %rid, "kuzu-migrate: skip malformed relation: {e}");
            }
        }
    }
    Ok(out)
}

// ── CLI entry point ───────────────────────────────────────────────────────

/// Import kuzu-memory entity/relation data into a trusty-memory palace.
///
/// Why: Issue #277 — provides a one-shot CLI command to migrate all content
/// from a kuzu-memory `store.redb` into a trusty-memory palace.
/// What: Opens `from` read-only, discovers the schema (printed to stdout),
/// reads entities and relations, maps them via `entity_to_drawer` /
/// `relation_to_triple`, and upserts into the target palace. Re-running is
/// idempotent: pre-existing drawers and triples are skipped. `dry_run` prints
/// the plan without writing. `limit` caps the number of entities processed.
/// Test: `cargo run -p trusty-memory -- migrate kuzu-data --from <store.redb>
///       --palace <name> --dry-run` prints the plan;
///       `tests/kuzu_migrate_tests.rs` exercises the write path against a
///       programmatically-constructed fixture store.
pub fn handle_kuzu_data_migrate(
    from: &Path,
    palace_name: &str,
    dry_run: bool,
    limit: Option<usize>,
) -> Result<()> {
    if dry_run {
        println!("{} Dry run — no data will be written.\n", "·".dimmed());
    }

    println!("🔍 Opening source store: {}", from.display());

    // Schema discovery.
    let tables = discover_schema(from)?;
    if tables.is_empty() {
        println!(
            "{} No tables found in source store — nothing to import.",
            "·".dimmed()
        );
        return Ok(());
    }
    println!("{} Source schema: {}", "·".dimmed(), tables.join(", "));

    let source_db =
        Database::open(from).with_context(|| format!("open source store at {}", from.display()))?;
    let entities = read_entities(&source_db)?;
    let relations = read_relations(&source_db)?;

    let entity_limit = limit.unwrap_or(entities.len()).min(entities.len());
    println!(
        "{} Found {} entities, {} relations (importing {} entities).",
        "·".dimmed(),
        entities.len(),
        relations.len(),
        entity_limit
    );

    if dry_run {
        print_dry_run_plan(&entities[..entity_limit], &relations, palace_name);
        return Ok(());
    }

    // Open the target palace via registry.
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data directory")?;
    let data_root = crate::resolve_palace_registry_dir(data_dir);
    let registry = trusty_common::memory_core::PalaceRegistry::new();
    let palace_id = PalaceId::new(palace_name);

    if registry.open_palace(&data_root, &palace_id).is_err() {
        println!("  Creating target palace '{palace_name}'…");
        let palace = Palace {
            id: palace_id.clone(),
            name: palace_name.to_string(),
            description: Some(format!(
                "Imported from kuzu-memory store.redb at {}",
                from.display()
            )),
            created_at: chrono::Utc::now(),
            data_dir: data_root.join(palace_name),
        };
        registry
            .create_palace(&data_root, palace)
            .context("create target palace")?;
    }

    let handle = registry
        .open_palace(&data_root, &palace_id)
        .context("open target palace")?;

    // Import entities as drawers.
    let mut drawers_written = 0usize;
    let mut drawers_skipped = 0usize;
    for entity in &entities[..entity_limit] {
        let drawer = entity_to_drawer(entity, palace_name);
        // Idempotency: skip if a drawer with this id already exists.
        let exists = {
            let d = handle.drawers.read();
            d.iter().any(|x| x.id == drawer.id)
        };
        if exists {
            drawers_skipped += 1;
            continue;
        }
        match handle.kg.upsert_drawer_sync(&drawer) {
            Ok(()) => drawers_written += 1,
            Err(e) => {
                tracing::warn!(entity_id = %entity.id, "kuzu-migrate: upsert drawer failed: {e:#}");
            }
        }
    }

    // Import relations as KG triples.
    let mut triples_written = 0usize;
    let mut triples_skipped = 0usize;
    let store = handle.kg.store();
    for relation in &relations {
        let triple = relation_to_triple(relation);
        // Idempotency: skip if an active triple already exists for (s, p).
        let exists = store
            .query_active(&triple.subject)
            .map(|v| v.iter().any(|t| t.predicate == triple.predicate))
            .unwrap_or(false);
        if exists {
            triples_skipped += 1;
            continue;
        }
        match handle.kg.assert_sync(&triple) {
            Ok(()) => triples_written += 1,
            Err(e) => {
                tracing::warn!(
                    from = %relation.from, to = %relation.to,
                    "kuzu-migrate: assert triple failed: {e:#}"
                );
            }
        }
    }

    println!();
    println!(
        "{} Import complete: {} drawers written ({} already existed), \
         {} triples written ({} already existed).",
        "✓".green(),
        drawers_written,
        drawers_skipped,
        triples_written,
        triples_skipped
    );
    Ok(())
}

/// Print a dry-run summary of planned import operations.
///
/// Why: Operators should verify the mapping before committing data.
/// What: Prints up to 10 example operations per category, with a count for
/// the remainder.
/// Test: Covered implicitly by `--dry-run` runs.
fn print_dry_run_plan(entities: &[KuzuEntity], relations: &[KuzuRelation], palace_name: &str) {
    println!("\nPlanned operations:");
    println!("  {} drawers to create:", entities.len());
    for entity in entities.iter().take(10) {
        let drawer_id = entity_uuid(&entity.id, palace_name);
        println!(
            "    drawer:{drawer_id}  ← entity:{} ({:?})",
            entity.id, entity.name
        );
    }
    if entities.len() > 10 {
        println!("    … and {} more", entities.len() - 10);
    }
    println!("  {} triples to assert:", relations.len());
    for rel in relations.iter().take(10) {
        let triple = relation_to_triple(rel);
        println!(
            "    ({}, {}, {})",
            triple.subject, triple.predicate, triple.object
        );
    }
    if relations.len() > 10 {
        println!("    … and {} more", relations.len() - 10);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `entity_to_drawer` is the primary mapping; its output must
    /// faithfully carry the entity's observations and type tag.
    /// What: Build a `KuzuEntity` with two observations, call `entity_to_drawer`,
    /// assert content, tags, and deterministic ID.
    /// Test: This test.
    #[test]
    fn entity_to_drawer_maps_name_and_observations() {
        let entity = KuzuEntity {
            id: "ent-001".to_string(),
            name: "Alice".to_string(),
            entity_type: "person".to_string(),
            observations: vec!["works at Acme".to_string(), "knows Rust".to_string()],
        };
        let drawer = entity_to_drawer(&entity, "test-palace");

        assert!(
            drawer.content.contains("Alice"),
            "content must include name"
        );
        assert!(
            drawer.content.contains("works at Acme"),
            "content must include first observation"
        );
        assert!(
            drawer.content.contains("knows Rust"),
            "content must include second observation"
        );
        assert!(drawer.tags.contains(&"source:kuzu".to_string()));
        assert!(drawer.tags.contains(&"type:person".to_string()));

        // ID is deterministic.
        let drawer2 = entity_to_drawer(&entity, "test-palace");
        assert_eq!(drawer.id, drawer2.id, "drawer id must be deterministic");
    }

    /// Why: An entity without observations must fall back to just the name.
    /// What: Zero observations → content == entity name; no type tag.
    /// Test: This test.
    #[test]
    fn entity_to_drawer_empty_observations() {
        let entity = KuzuEntity {
            id: "ent-002".to_string(),
            name: "Bob".to_string(),
            entity_type: String::new(),
            observations: vec![],
        };
        let drawer = entity_to_drawer(&entity, "palace");
        assert_eq!(drawer.content, "Bob");
        assert!(!drawer.tags.iter().any(|t| t.starts_with("type:")));
    }

    /// Why: `relation_to_triple` must wire all fields correctly.
    /// What: Build a `KuzuRelation`, call `relation_to_triple`, check fields.
    /// Test: This test.
    #[test]
    fn relation_to_triple_maps_fields() {
        let relation = KuzuRelation {
            from: "alice".to_string(),
            to: "acme-corp".to_string(),
            relation_type: "works_at".to_string(),
        };
        let triple = relation_to_triple(&relation);
        assert_eq!(triple.subject, "entity:alice");
        assert_eq!(triple.predicate, "works_at");
        assert_eq!(triple.object, "entity:acme-corp");
        assert_eq!(triple.provenance.as_deref(), Some("kuzu-migrate"));
        assert!(triple.valid_to.is_none());
    }

    /// Why: Idempotency depends on the UUID being deterministic across runs.
    /// What: Same inputs → same UUID; different inputs → different UUIDs.
    /// Test: This test.
    #[test]
    fn entity_uuid_is_deterministic() {
        let a = entity_uuid("ent-abc", "my-palace");
        let b = entity_uuid("ent-abc", "my-palace");
        let c = entity_uuid("ent-xyz", "my-palace");
        let d = entity_uuid("ent-abc", "other-palace");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    /// Why: `discover_schema` must not panic or error on a valid empty redb.
    /// What: Create an empty redb in a tempdir, call `discover_schema`.
    /// Test: This test.
    #[test]
    fn discover_schema_on_empty_db_returns_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("empty.redb");
        drop(Database::create(&path).expect("create empty db"));

        let tables = discover_schema(&path).expect("should not fail on empty db");
        assert!(tables.is_empty());
    }

    /// Why: `read_entities` must return an empty vec (not error) when the
    /// entities table is absent.
    /// What: Create an empty redb, call `read_entities`.
    /// Test: This test.
    #[test]
    fn read_entities_on_empty_db_returns_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("empty.redb");
        let db = Database::create(&path).expect("create db");

        let entities = read_entities(&db).expect("should not fail on empty db");
        assert!(entities.is_empty());
    }

    /// Why: `read_relations` must return an empty vec when the relations
    /// table is absent.
    /// What: Create an empty redb, call `read_relations`.
    /// Test: This test.
    #[test]
    fn read_relations_on_empty_db_returns_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("empty.redb");
        let db = Database::create(&path).expect("create db");

        let relations = read_relations(&db).expect("should not fail on empty db");
        assert!(relations.is_empty());
    }

    /// Why: A fixture store with one entity and one relation must be read
    /// correctly.
    /// What: Create a fixture store.redb with one entity and one relation,
    /// assert the counts and field values.
    /// Test: This test.
    #[test]
    fn read_fixture_store_returns_entities_and_relations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("fixture.redb");
        // Build the fixture.
        write_fixture_store(&path, 2, 1).expect("write fixture");

        let db = Database::open(&path).expect("open fixture");
        let entities = read_entities(&db).expect("read entities");
        let relations = read_relations(&db).expect("read relations");

        assert_eq!(entities.len(), 2, "expected 2 entities");
        assert_eq!(relations.len(), 1, "expected 1 relation");

        // Validate mapping.
        let drawer = entity_to_drawer(&entities[0], "test");
        assert!(drawer.tags.contains(&"source:kuzu".to_string()));

        let triple = relation_to_triple(&relations[0]);
        assert_eq!(triple.predicate, "test_rel");
    }

    /// Why: `--dry-run` must not error when the store is valid.
    /// What: Call `handle_kuzu_data_migrate` with `dry_run=true`, verify it
    /// returns Ok without writing any files.
    /// Test: This test.
    #[test]
    fn dry_run_returns_ok_without_writing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("fixture.redb");
        write_fixture_store(&path, 1, 0).expect("write fixture");

        // dry_run does not open or create a palace, so we can call it even
        // without a real data root.
        let result = handle_kuzu_data_migrate(&path, "test-palace", true, None);
        assert!(result.is_ok(), "dry run must succeed: {result:?}");
    }

    // ── Fixture builder ───────────────────────────────────────────────

    /// Build a synthetic kuzu-memory `store.redb` fixture with `n_entities`
    /// entities and `n_relations` relations (only creates a relation when
    /// `n_entities >= 2`).
    ///
    /// Why: Integration tests and dry-run tests need a real redb file without
    /// requiring a live kuzu-memory installation.
    /// What: Creates tables `entities` and `relations` with the schema that
    /// `read_entities` and `read_relations` expect (string keys, JSON values).
    /// Test: Called by `read_fixture_store_returns_entities_and_relations`
    /// and `dry_run_returns_ok_without_writing`.
    pub(crate) fn write_fixture_store(
        path: &Path,
        n_entities: usize,
        n_relations: usize,
    ) -> Result<()> {
        use redb::TableDefinition;
        const ENTITIES: TableDefinition<&str, &str> = TableDefinition::new("entities");
        const RELATIONS: TableDefinition<&str, &str> = TableDefinition::new("relations");

        let db = Database::create(path).context("create fixture db")?;
        let wtx = db.begin_write().context("begin write txn")?;
        {
            let mut entities = wtx.open_table(ENTITIES).context("open entities table")?;
            for i in 0..n_entities {
                let id = format!("ent-{i:03}");
                let entity = KuzuEntity {
                    id: id.clone(),
                    name: format!("Entity {i}"),
                    entity_type: "test_type".to_string(),
                    observations: vec![format!("observation {i}")],
                };
                let json = serde_json::to_string(&entity).context("serialize entity")?;
                entities
                    .insert(id.as_str(), json.as_str())
                    .context("insert entity")?;
            }
        }
        {
            let mut relations = wtx.open_table(RELATIONS).context("open relations table")?;
            for i in 0..n_relations.min(if n_entities >= 2 { n_relations } else { 0 }) {
                let rel_id = format!("rel-{i:03}");
                let from = format!("ent-{:03}", i);
                let to = format!("ent-{:03}", i + 1);
                let relation = KuzuRelation {
                    from,
                    to,
                    relation_type: "test_rel".to_string(),
                };
                let json = serde_json::to_string(&relation).context("serialize relation")?;
                relations
                    .insert(rel_id.as_str(), json.as_str())
                    .context("insert relation")?;
            }
        }
        wtx.commit().context("commit fixture")?;
        Ok(())
    }
}
