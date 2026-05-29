//! Shared types, configuration, database schema, and error definitions.
//!
//! This module is the foundation that all other modules (`collect`,
//! `classify`, `report`) depend on.
//!
//! ## Submodules
//!
//! - [`config`] — YAML configuration loading and types
//! - [`db`] — SQLite database wrapper with WAL mode and versioned migrations
//! - [`errors`] — crate-wide error enum and `Result` alias
//! - [`models`] — domain structs for commits, authors, classifications, etc.
//! - [`quality`] — per-engineer-per-week quality scoring (1–5 T-shirt)
//! - [`revert`] — shared commit-message revert detection

pub mod config;
pub mod db;
pub mod effort;
pub mod effort_percentile;
pub mod errors;
pub mod models;
pub mod quality;
pub mod revert;

pub use errors::{Result, TgaError};

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn config_loads_from_yaml_file() {
        let mut tmp = tempfile_like("config.yaml");
        writeln!(
            tmp.file,
            "repositories:\n  - path: /tmp/repo-a\n    name: repo-a\noutput:\n  format: csv\n"
        )
        .expect("write");
        let cfg = config::Config::load(&tmp.path).expect("load");
        assert_eq!(cfg.repositories.len(), 1);
        assert_eq!(cfg.repositories[0].name.as_deref(), Some("repo-a"));
        assert_eq!(
            cfg.output.as_ref().and_then(|o| o.format.as_deref()),
            Some("csv")
        );
        cfg.validate().expect("validate");
    }

    #[test]
    fn config_validate_requires_repositories() {
        let cfg = config::Config::default();
        let err = cfg.validate().expect_err("should fail");
        match err {
            TgaError::ValidationError(_) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn database_opens_with_wal_and_migrations_apply() {
        let dir = tempfile_dir();
        let db_path = dir.path.join("test.db");
        let db = db::Database::open(&db_path).expect("open");

        // WAL mode must be active for on-disk databases.
        let mode = db.journal_mode().expect("journal mode");
        assert_eq!(mode.to_lowercase(), "wal");

        // v1 migration must have been applied.
        assert!(db.schema_version().expect("version") >= 1);

        // Core tables must exist.
        for table in [
            "commits",
            "authors",
            "classifications",
            "files",
            "pull_requests",
            "schema_migrations",
        ] {
            let n: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("query");
            assert_eq!(n, 1, "table {table} should exist");
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile_dir();
        let db_path = dir.path.join("idempotent.db");
        let _db1 = db::Database::open(&db_path).expect("first open");
        let db2 = db::Database::open(&db_path).expect("second open");
        // Running again must not duplicate or fail.
        let expected = db::migrations::MIGRATIONS
            .last()
            .map(|m| m.version)
            .unwrap_or(0);
        assert_eq!(db2.schema_version().expect("version"), expected);
    }

    // --- minimal tempfile helpers (avoid pulling in a `tempfile` dep) ---

    struct TempFile {
        path: std::path::PathBuf,
        file: std::fs::File,
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn tempfile_like(name: &str) -> TempFile {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "tga-core-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            name
        );
        path.push(unique);
        let file = std::fs::File::create(&path).expect("create temp file");
        TempFile { path, file }
    }

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn tempfile_dir() -> TempDir {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "tga-core-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("mkdir");
        TempDir { path }
    }
}
