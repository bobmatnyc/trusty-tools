//! Documentation-only stub for storing schema-version stamps inside redb.
//!
//! Why: redb is not a direct dependency of `trusty-common` — it lives behind
//! the heavyweight `memory-core` feature flag — so this crate cannot offer
//! a generic redb-stamp helper without dragging redb into every consumer's
//! build graph. Crates that already depend on redb (`trusty-search`,
//! `trusty-memory`) can implement the equivalent five-line helper locally
//! using the recipe below.
//!
//! What: this module is intentionally empty of types. It exists so callers
//! grepping for "redb_stamp" find the convention documented in one place.
//!
//! Test: none — the recipe is exercised by the crate that adopts it.
//!
//! # Recipe
//!
//! ```text
//! // Local to the consumer crate, alongside its existing `redb::Database`:
//!
//! use redb::{Database, TableDefinition};
//! use trusty_common::migrations::SchemaVersion;
//! use anyhow::Result;
//!
//! const META_TABLE: TableDefinition<&'static str, u32> = TableDefinition::new("meta");
//! const STAMP_KEY: &str = "schema_version";
//!
//! pub fn read_redb_stamp(db: &Database) -> Result<SchemaVersion> {
//!     let txn = db.begin_read()?;
//!     let table = match txn.open_table(META_TABLE) {
//!         Ok(t) => t,
//!         // Table missing means the store predates the migration kernel.
//!         Err(redb::TableError::TableDoesNotExist(_)) => {
//!             return Ok(SchemaVersion::UNVERSIONED);
//!         }
//!         Err(e) => return Err(e.into()),
//!     };
//!     match table.get(STAMP_KEY)? {
//!         Some(v) => Ok(SchemaVersion(v.value())),
//!         None => Ok(SchemaVersion::UNVERSIONED),
//!     }
//! }
//!
//! pub fn write_redb_stamp(db: &Database, v: SchemaVersion) -> Result<()> {
//!     let txn = db.begin_write()?;
//!     {
//!         let mut table = txn.open_table(META_TABLE)?;
//!         table.insert(STAMP_KEY, v.0)?;
//!     }
//!     txn.commit()?;
//!     Ok(())
//! }
//! ```
//!
//! Pass `write_redb_stamp` as the `write_stamp` closure to
//! [`super::MigrationRunner::run`].
