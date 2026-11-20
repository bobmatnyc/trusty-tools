//! `secrets.migrate` request and response (#10524, owner ruling 2026-11-19).
//!
//! Why: the one-prompt unlock needs every trusty Keychain item in one vault
//! blob item; this method moves them there, removes the old items, or moves
//! them back.
//! What: [`MigrateStep`] picks the step; [`MigrateRequest`] adds a dry run;
//! [`MigrateResponse`] carries counts and key names, never a value.
//! Test: `migrate_records_counts_and_names_never_values`.

use serde::{Deserialize, Serialize};

/// One migration step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrateStep {
    /// Copy the per-item items into the blob, behind a verified backup.
    Migrate,
    /// Delete the old per-item items once the blob and backup exist.
    Finish,
    /// Write the blob's items back as per-item items, then delete the blob.
    Rollback,
}

/// `secrets.migrate` request, beside `project` (which the server ignores).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MigrateRequest {
    /// The step to run.
    pub step: MigrateStep,
    /// Report what the step would do and write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

impl MigrateRequest {
    /// A request for `step`, dry or not.
    pub fn new(step: MigrateStep, dry_run: bool) -> Self {
        Self { step, dry_run }
    }
}

/// `secrets.migrate` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MigrateResponse {
    /// The step that ran.
    pub step: MigrateStep,
    /// Whether it was a dry run.
    pub dry_run: bool,
    /// Nothing to do: already migrated, or already per-item.
    pub already: bool,
    /// Items moved (migrate, rollback) or old items deleted (finish).
    pub items: usize,
    /// The `vault/key` names of the value items moved, sorted.
    pub keys: Vec<String>,
    /// Rollback's source: `blob`, or `backup` when the blob was corrupt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
