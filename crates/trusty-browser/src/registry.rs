//! `registry.sqlite3`: the schema and pairing rows shared with Python 0.7.0.
//!
//! Why: the cutover keeps the Python-written registry in place, and a rollback
//! hands a Rust-written one back to Python, so both must agree on the file,
//! the schema text and the row format (`enrollment.py` `database`).
//! What: [`Registry::open`] runs Python's private-file checks, `secure_delete`,
//! the same six `CREATE TABLE IF NOT EXISTS` statements (verbatim, so
//! `sqlite_master` matches) and the `route_versions` backfill; it adds no
//! table or column. Pairing rows are read and written through it.
//! Test: `fixture_registry_unseals_every_pairing`, `fresh_schema_matches_python_sqlite_master`,
//! `registry_refuses_a_group_readable_state_dir` in `tests/parity_registry.rs`.

use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::private;

/// The six schema statements, byte-identical to `enrollment.py`.
pub const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS routes (
        profile TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, workflow TEXT UNIQUE NOT NULL,
        url TEXT NOT NULL, account TEXT NOT NULL, browser_key TEXT NOT NULL, ticket TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS pending_credentials (
        workflow TEXT PRIMARY KEY, ticket TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS revoked (registration TEXT PRIMARY KEY)",
    "CREATE TABLE IF NOT EXISTS pending_removals (workflow TEXT PRIMARY KEY, ticket_hash TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS route_versions (workflow TEXT PRIMARY KEY, version TEXT NOT NULL, previous TEXT)",
    "CREATE TABLE IF NOT EXISTS pairings (
        profile TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, browser_key TEXT NOT NULL)",
];

const BACKFILL: &str = "INSERT INTO route_versions(workflow,version)
            SELECT workflow,lower(hex(randomblob(32))) FROM routes
            WHERE workflow NOT IN (SELECT workflow FROM route_versions)";

/// Why the registry could not be opened or used.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// State dir or registry file is not owner-only.
    #[error("{0}")]
    NotPrivate(&'static str),
    /// Filesystem failure.
    #[error("registry I/O: {0}")]
    Io(#[from] io::Error),
    /// SQLite failure.
    #[error("registry: {0}")]
    Sql(#[from] rusqlite::Error),
}

/// One `pairings` row; `browser_key` is still sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRow {
    /// Profile id (UUID v4).
    pub profile: String,
    /// Case-folded account email.
    pub email: String,
    /// `tb1.` sealed browser key.
    pub browser_key: String,
}

/// An open registry.
#[derive(Debug)]
pub struct Registry {
    conn: Connection,
    path: PathBuf,
}

impl Registry {
    /// Open or create `state/registry.sqlite3` as Python does.
    ///
    /// Why: one entry point keeps the open checks and schema identical.
    /// What: creates `state` 0700 if missing and refuses a shared one, creates
    /// the file 0600 with `O_NOFOLLOW` and refuses a shared or non-regular one,
    /// then schema and backfill.
    /// Test: `fresh_schema_matches_python_sqlite_master`, `registry_refuses_a_group_readable_state_dir`.
    pub fn open(state: &Path) -> Result<Registry, RegistryError> {
        private::ensure_private_dir(state).map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied => RegistryError::NotPrivate(
                "state must be a private directory owned by the current user",
            ),
            _ => RegistryError::Io(e),
        })?;
        let path = state.join("registry.sqlite3");
        let file = private::open_nofollow(&path, false)?;
        if !private::private_file(&file.metadata()?) {
            return Err(RegistryError::NotPrivate(
                "registry must be an owner-only regular file",
            ));
        }
        drop(file);
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA secure_delete=ON")?;
        for statement in SCHEMA {
            conn.execute(statement, [])?;
        }
        conn.execute(BACKFILL, [])?;
        Ok(Registry { conn, path })
    }

    /// The database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hand the connection to the broker, which owns it from here on.
    pub fn into_connection(self) -> Connection {
        self.conn
    }

    /// The raw connection, for callers that need their own queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Every pairing as `(profile, email)`, ordered by email (`pairing.pairings`).
    pub fn pairings(&self) -> Result<Vec<(String, String)>, RegistryError> {
        let mut stmt = self
            .conn
            .prepare("SELECT profile, email FROM pairings ORDER BY email")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The pairing of `profile`.
    pub fn pairing(&self, profile: &str) -> Result<Option<PairingRow>, RegistryError> {
        Ok(self
            .conn
            .query_row(
                "SELECT profile, email, browser_key FROM pairings WHERE profile=?",
                [profile],
                |r| {
                    Ok(PairingRow {
                        profile: r.get(0)?,
                        email: r.get(1)?,
                        browser_key: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Insert one pairing in its own `BEGIN IMMEDIATE` transaction.
    pub fn insert_pairing(&mut self, row: &PairingRow) -> Result<(), RegistryError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO pairings VALUES (?,?,?)",
            params![row.profile, row.email, row.browser_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Delete the pairing of `profile`; true when a row was removed.
    pub fn delete_pairing(&mut self, profile: &str) -> Result<bool, RegistryError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute("DELETE FROM pairings WHERE profile=?", [profile])?;
        tx.commit()?;
        Ok(n > 0)
    }
}
