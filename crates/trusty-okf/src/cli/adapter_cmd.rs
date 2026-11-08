//! `--from adapter:<kind>`: an import through an adapter process
//! (SPEC-OKF-01 §2.15, §3 (m) A, §2.7.1 B10). Slice S9 of #10746.
//!
//! Why: the CLI is the caller that builds the adapter's environment list
//! from its own arguments (B10, r7), and it must refuse a run with no
//! account or selection before it starts anything (criterion 86).
//! What: [`spec_for`] builds the connector store row from `--select
//! KEY=VALUE` pairs (`true`/`false` become booleans; `document_ids` is a
//! comma-separated list); the store ID is `--store`, else
//! `import-<kind>`. A store ID that names a different source kind is
//! refused. `--operator-initiated` marks the run (§2.15 "Inbound email").
//! [`source_for`] checks the selection, then spawns `--adapter` with the
//! `--adapter-arg` arguments and exactly the `--adapter-var NAME=VALUE`
//! list; the CLI reads no environment of its own (B3). Every refusal here
//! happens before any write (exit 2).
//! Test: `c97_cli_imports_through_an_adapter_process_and_reaps_on_cancel`,
//! `c86_no_selection_is_refused_before_any_spawn_or_write`.

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
    let id = a
        .one("--store")
        .map_or_else(|| format!("import-{kind}"), str::to_string);
    let registry = SourceRegistry::load(root).map_err(|e| fail(&e))?;
    if let Some(row) = registry.get(&slugify(&id))
        && row.locator.kind() != kind
    {
        return Err(Fail::Code(
            2,
            format!(
                "store {} already names a {} source",
                row.id,
                row.locator.kind()
            ),
        ));
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
