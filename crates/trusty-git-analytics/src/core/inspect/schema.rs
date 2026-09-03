//! Live schema reading — every table, view, and column the open database holds.
//!
//! Why: #5218 — a reviewer is owed the schema that is in front of them, not the
//! one the migration files would produce. A database collected by an older
//! `tga` is missing later tables, and a database someone hand-edited may carry
//! extras; both are invisible to anyone reading `src/core/db/sql/`.
//! What: [`snapshot`] reads `sqlite_master` for tables and views and
//! `PRAGMA table_info` for each table's columns, then attaches the live row
//! count and the pinned text classification from [`super::text_columns`].
//! Test: `core::inspect::tests`.

use rusqlite::Connection;
use serde::Serialize;

use crate::core::errors::{Result, TgaError};

use super::text_columns::{classify, TextClass};

/// One column of one table, as the open database declares it.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Declared type (`TEXT`, `INTEGER`, …); empty when the DDL omitted one.
    pub declared_type: String,
    /// Whether the column carries `NOT NULL`.
    pub not_null: bool,
    /// 1-based position within the primary key, or 0 when not part of it.
    pub pk_position: i64,
    /// How this column's text is constrained — see [`TextClass`].
    /// `None` for a column whose declared type is not `TEXT`.
    pub text_class: Option<TextClass>,
}

/// Whether a `sqlite_master` entry is a table or a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    /// A base table.
    Table,
    /// A `CREATE VIEW` projection over base tables.
    View,
}

/// One table or view, with its columns and (for tables) its live row count.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TableInfo {
    /// Object name.
    pub name: String,
    /// Table or view.
    pub kind: ObjectKind,
    /// Declared columns, in DDL order.
    pub columns: Vec<ColumnInfo>,
    /// `SELECT COUNT(*)` at snapshot time; `None` for a view.
    pub row_count: Option<i64>,
}

/// Every table, view, and column the open database holds.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct SchemaSnapshot {
    /// Highest applied migration version, or `None` when the database has no
    /// `schema_migrations` table (i.e. it was not created by `tga`).
    pub schema_version: Option<i64>,
    /// Tables first, then views; each group in name order.
    pub objects: Vec<TableInfo>,
}

impl SchemaSnapshot {
    /// Every declared-`TEXT` column, paired with its table name.
    ///
    /// Why: the attestation and the classification-coverage test both walk the
    /// same set, and neither should re-derive the "declared TEXT" predicate.
    /// What: `(table, column)` pairs in snapshot order.
    /// Test: `tests::snapshot_reads_every_table_and_column`.
    pub fn text_columns(&self) -> Vec<(&TableInfo, &ColumnInfo)> {
        self.objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Table)
            .flat_map(|t| {
                t.columns
                    .iter()
                    .filter(|c| c.declared_type.eq_ignore_ascii_case("TEXT"))
                    .map(move |c| (t, c))
            })
            .collect()
    }
}

/// Read the complete schema of an open connection.
///
/// Why: see the module docs — this is the evidence half of `tga inspect`.
/// What: one `sqlite_master` query for the object list, then per table a
/// `PRAGMA table_info` and a `SELECT COUNT(*)`. Views get columns but no count,
/// because counting one re-runs its underlying aggregation for no reader
/// benefit.
/// Test: `tests::snapshot_reads_every_table_and_column`,
/// `tests::snapshot_reports_row_counts`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if any of those reads fails.
pub fn snapshot(conn: &Connection) -> Result<SchemaSnapshot> {
    let mut objects = Vec::new();
    for (kind, name) in object_names(conn)? {
        let columns = columns_of(conn, &name)?;
        let row_count = match kind {
            ObjectKind::Table => Some(count_rows(conn, &name)?),
            ObjectKind::View => None,
        };
        objects.push(TableInfo {
            name,
            kind,
            columns,
            row_count,
        });
    }
    Ok(SchemaSnapshot {
        schema_version: schema_version(conn)?,
        objects,
    })
}

/// List user tables and views, tables first, each group in name order.
fn object_names(conn: &Connection) -> Result<Vec<(ObjectKind, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name FROM sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
             ORDER BY type = 'view', name",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            let kind: String = row.get(0)?;
            let name: String = row.get(1)?;
            Ok((kind, name))
        })
        .map_err(TgaError::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (kind, name) = row.map_err(TgaError::from)?;
        let kind = if kind == "view" {
            ObjectKind::View
        } else {
            ObjectKind::Table
        };
        out.push((kind, name));
    }
    Ok(out)
}

/// Read one object's columns via `PRAGMA table_info`.
///
/// The table name is interpolated rather than bound because SQLite does not
/// accept a parameter in a `PRAGMA` argument. Every name reaching here came
/// from `sqlite_master` in this same connection, so it is not caller input; it
/// is still quoted so an exotic identifier cannot change the statement's shape.
fn columns_of(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>> {
    let quoted = table.replace('"', "\"\"");
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{quoted}\")"))
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ColumnInfo {
                name: row.get::<_, String>(1)?,
                declared_type: row.get::<_, String>(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                pk_position: row.get::<_, i64>(5)?,
                text_class: None,
            })
        })
        .map_err(TgaError::from)?;

    let mut out = Vec::new();
    for row in rows {
        let mut col = row.map_err(TgaError::from)?;
        if col.declared_type.eq_ignore_ascii_case("TEXT") {
            col.text_class = Some(classify(table, &col.name));
        }
        out.push(col);
    }
    Ok(out)
}

/// Live row count for one table.
fn count_rows(conn: &Connection, table: &str) -> Result<i64> {
    let quoted = table.replace('"', "\"\"");
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
        row.get(0)
    })
    .map_err(TgaError::from)
}

/// Highest applied migration version, or `None` when the database carries no
/// `schema_migrations` table.
fn schema_version(conn: &Connection) -> Result<Option<i64>> {
    let present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    if present == 0 {
        return Ok(None);
    }
    let version: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(TgaError::from)?;
    Ok(version)
}
