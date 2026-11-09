//! `--from adapter:<kind>`: an import through an adapter process
//! (SPEC-OKF-01 §2.15, §3 (m) A, §2.7.1 B10). Slice S9 of #10746.
//!
//! Why: the CLI is the caller that builds the adapter's environment list
//! from its own arguments (B10, r7), and it must refuse a run with no
//! account or selection before it starts anything (criterion 86).
//! What: [`spec_for`] builds the connector store row from `--select
//! KEY=VALUE` pairs (`true`/`false` become booleans; `document_ids` is a
//! comma-separated list); the store ID is `--store`, else the ID of the
//! row with the same kind, account and selection, else `import-<kind>`. A
//! store ID that names a different source kind, or another account or
//! selection, is refused. `--operator-initiated` marks the run (§2.15 "Inbound email").
//! [`source_for`] checks the selection, then spawns `--adapter` with the
//! `--adapter-arg` arguments and exactly the `--adapter-var NAME=VALUE`
//! list; the CLI reads no environment of its own (B3). Every refusal here
//! happens before any write (exit 2).
//! Test: `c97_cli_imports_through_an_adapter_process_and_reaps_on_cancel`,
//! `c86_no_selection_is_refused_before_any_spawn_or_write`,
//! `another_selection_never_reuses_an_adapter_store`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::import_cmd::fail;
use super::{Args, Fail};
use crate::build::registry::{ConnectorParams, Locator, SourceRegistry, SourceSpec};
use crate::format::concept::slugify;
use crate::import::adapter::{AdapterCommand, AdapterLimits, AdapterSource};
use crate::import::record::check_selection;
use crate::import::{ImportSource, ImportSpec};

/// `KEY=VALUE` split at the first `=`.
fn pair<'a>(flag: &str, s: &'a str) -> Result<(&'a str, &'a str), Fail> {
    s.split_once('=')
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| Fail::Usage(format!("{flag} {s}: expected KEY=VALUE")))
}

/// #10746: the parameters that name a store's items: the account and the
/// selection `check_selection` requires.
const SELECTION: [&str; 4] = ["account", "query", "folder_id", "document_ids"];

/// The selection parameters of a connector row, in [`SELECTION`] order.
fn selection(p: &ConnectorParams) -> [Option<&toml::Value>; 4] {
    SELECTION.map(|k| p.get(k))
}

/// The import spec for `--from adapter:<kind>` (see the module doc).
pub(crate) fn spec_for(a: &Args, root: &Path, now: &str) -> Result<ImportSpec, Fail> {
    let from = a.one("--from").unwrap_or_default();
    let kind = from.strip_prefix("adapter:").unwrap_or_default();
    if kind.is_empty() {
        return Err(Fail::Usage("--from adapter:<kind> names no kind".into()));
    }
    let mut params = ConnectorParams::new();
    for s in a.all("--select") {
        let (k, v) = pair("--select", &s)?;
        let value = match (k, v) {
            ("document_ids", v) => toml::Value::Array(
                v.split(',')
                    .filter(|x| !x.is_empty())
                    .map(|x| toml::Value::String(x.to_string()))
                    .collect(),
            ),
            (_, "true") => toml::Value::Boolean(true),
            (_, "false") => toml::Value::Boolean(false),
            (_, v) => toml::Value::String(v.to_string()),
        };
        params.insert(k, value);
    }
    let registry = SourceRegistry::load(root).map_err(|e| fail(&e))?;
    // #10746: with no `--store`, a row of this kind with the same selection
    // keeps its ID, as `dir:` keys a store on its path; a new selection
    // gets `import-<kind>`, refused below if that row holds another one.
    let same = |s: &&SourceSpec| {
        s.locator.kind() == kind
            && matches!(&s.locator, Locator::Connector { params: p, .. } if selection(p) == selection(&params))
    };
    let id = match a.one("--store") {
        Some(s) => s.to_string(),
        None => registry
            .sources
            .iter()
            .find(same)
            .map_or_else(|| format!("import-{kind}"), |row| row.id.clone()),
    };
    if let Some(row) = registry.get(&slugify(&id)) {
        if row.locator.kind() != kind {
            return Err(Fail::Code(
                2,
                format!(
                    "store {} already names a {} source",
                    row.id,
                    row.locator.kind()
                ),
            ));
        }
        // #10746: a run replaces the stored selection and tombstones what
        // the new listing lacks, so another account or selection must
        // never take over this store's items.
        if !same(&row) {
            return Err(Fail::Code(
                2,
                format!(
                    "store {} holds another selection or account; pass --store to import this one into its own store",
                    row.id
                ),
            ));
        }
    }
    let locator = Locator::Connector {
        kind: kind.to_string(),
        params,
    };
    let mut spec = ImportSpec::new(SourceSpec::new(&id, None, locator, now));
    // A docs store tombstones a deleted item on a deletion-safe listing.
    spec.store.tombstone_deleted = kind != "gmail";
    spec.operator_initiated = a.has("--operator-initiated");
    Ok(spec)
}

/// The adapter source for `spec` (see the module doc).
pub(crate) fn source_for(a: &Args, spec: &ImportSpec) -> Result<Box<dyn ImportSource>, Fail> {
    check_selection(spec).map_err(|e| fail(&e))?;
    let program = a
        .one("--adapter")
        .ok_or_else(|| Fail::Usage("--from adapter:<kind> needs --adapter PATH".into()))?;
    let mut cmd = AdapterCommand::new(PathBuf::from(program));
    cmd.args = a
        .all("--adapter-arg")
        .into_iter()
        .map(OsString::from)
        .collect();
    for s in a.all("--adapter-var") {
        let (k, v) = pair("--adapter-var", &s)?;
        cmd.environment.push((k.into(), v.into()));
    }
    let src = AdapterSource::spawn(&cmd, spec, AdapterLimits::default()).map_err(|e| fail(&e))?;
    Ok(Box::new(src))
}
