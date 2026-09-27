//! Target resolution for the CLI verbs that destroy or rewrite an index:
//! `index remove`, `reindex`, `quantize` and `index relocate`.
//!
//! Why: clap folds a real `-i`/`--index` flag and the `TRUSTY_INDEX` env
//! fallback into one `Cli::index` value. A verb that trusted that value alone
//! deleted a live index (#8175), and `reindex`/`quantize`/`relocate` had the
//! same shape (#8737): an exported `TRUSTY_INDEX` could re-point a live index
//! at an unrelated PATH and overwrite its corpus.
//! What: [`classify_explicit_target`] is the pure precedence rule — an
//! explicit `-i` or PATH wins, `TRUSTY_INDEX` alone refuses, PATH plus an id
//! defers to a daemon-backed agreement check. [`resolve_explicit_target`] runs
//! that check; any daemon failure during it is a refusal, never a guess.
//! Test: `classify_explicit_target_names_the_verb`, `flag_only_index_*` below;
//! `classify_remove_target_*` in `index_remove.rs`; the HTTP round-trips in
//! `tests/index_remove_env_conflict_8175.rs` and
//! `tests/reindex_quantize_env_conflict_8737.rs`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Where an explicit `-i`/`--index` value came from (issue #8175).
///
/// Why: a destructive verb must refuse when the ONLY source is the
/// environment, but proceed when the operator typed the flag.
/// What: `main.rs` reads `ArgMatches::value_source("index")` before the
/// derive-based parse consumes the matches, and passes the answer down.
/// Test: `classify_remove_target_env_only_refuses`,
/// `classify_remove_target_cli_flag_alone_is_used_directly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexIdSource {
    /// A real `-i`/`--index` flag on the command line.
    CliFlag,
    /// Only the `TRUSTY_INDEX` environment fallback — no flag was typed.
    EnvVar,
}

/// Build the `Option<IndexIdSource>` the destructive handlers take, from
/// `cli.index.is_some()` and whether clap saw a real flag.
/// Test: covered end-to-end by `tests/index_remove_env_conflict_8175.rs` and
/// `tests/reindex_quantize_env_conflict_8737.rs`.
pub(crate) fn index_id_source(has_value: bool, from_cli_flag: bool) -> Option<IndexIdSource> {
    has_value.then_some(if from_cli_flag {
        IndexIdSource::CliFlag
    } else {
        IndexIdSource::EnvVar
    })
}

/// The shape PATH, `-i`/`--index` and their source take together, before any
/// of it is checked against the daemon.
/// Test: `classify_remove_target_*` in `index_remove.rs`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExplicitTarget {
    /// Neither PATH nor an id was given — auto-detect from CWD, unchanged.
    CwdAutoDetect,
    /// A PATH argument only.
    Path(PathBuf),
    /// An id from a real `-i` flag, with no PATH to check it against.
    Id(String),
    /// PATH and an id were both given; they must agree via the daemon.
    PathAndId(PathBuf, String),
    /// Refuse before any network call: the target came from `TRUSTY_INDEX`
    /// alone.
    Refuse(String),
}

/// The refusal for an id known only from `TRUSTY_INDEX`.
fn env_only_refusal(verb: &str, id: &str) -> String {
    format!(
        "refusing to {verb} index \"{id}\" resolved only from TRUSTY_INDEX; \
         destructive commands need an explicit -i/--index flag (or a PATH \
         argument, where the command takes one) (issues #8175, #8737) — pass \
         one, or unset TRUSTY_INDEX and re-run from inside the project"
    )
}

/// Classify how a destructive `verb` resolves its target, with no daemon call.
///
/// Why: the #8175/#8737 precedence rule as a pure, unit-testable function.
/// What: an env-sourced id with no PATH → [`ExplicitTarget::Refuse`] naming
/// `verb` and the id; PATH plus an id from either source →
/// [`ExplicitTarget::PathAndId`], because agreement needs the daemon.
/// Test: `classify_explicit_target_names_the_verb`, `classify_remove_target_*`.
pub(crate) fn classify_explicit_target(
    verb: &str,
    cli_path: Option<PathBuf>,
    explicit_index: Option<(String, IndexIdSource)>,
) -> ExplicitTarget {
    match (cli_path, explicit_index) {
        (None, None) => ExplicitTarget::CwdAutoDetect,
        (Some(p), None) => ExplicitTarget::Path(p),
        (None, Some((id, IndexIdSource::CliFlag))) => ExplicitTarget::Id(id),
        (None, Some((id, IndexIdSource::EnvVar))) => {
            ExplicitTarget::Refuse(env_only_refusal(verb, &id))
        }
        (Some(p), Some((id, _))) => ExplicitTarget::PathAndId(p, id),
    }
}

/// Pair `cli.index` with its source; a value with no known source is treated
/// as env-sourced, so an unknown origin fails closed.
pub(crate) fn with_source(
    explicit_index: Option<String>,
    source: Option<IndexIdSource>,
) -> Option<(String, IndexIdSource)> {
    explicit_index.map(|id| (id, source.unwrap_or(IndexIdSource::EnvVar)))
}

/// The id a PATH-less destructive verb (`quantize`, `relocate`) may act on.
///
/// Why (#8737): these verbs take no PATH, so the only safe sources are a real
/// `-i` flag or CWD auto-detection; `TRUSTY_INDEX` alone refuses.
/// What: `Ok(None)` → auto-detect from CWD; `Ok(Some(id))` → the flag's id;
/// `Err` → env-only refusal, raised before any network call.
/// Test: `flag_only_index_refuses_env_only`,
/// `flag_only_index_passes_flag_and_absence_through`.
pub(crate) fn flag_only_index(
    verb: &str,
    explicit_index: &Option<String>,
    source: Option<IndexIdSource>,
) -> Result<Option<String>> {
    match with_source(explicit_index.clone(), source) {
        None => Ok(None),
        Some((id, IndexIdSource::CliFlag)) => Ok(Some(id)),
        Some((id, IndexIdSource::EnvVar)) => bail!(env_only_refusal(verb, &id)),
    }
}

/// Resolve every non-CWD [`ExplicitTarget`] against the daemon.
///
/// Why: PATH-plus-id agreement is the one check that needs the daemon, and a
/// failure to perform it (daemon down, 404, 503) must refuse rather than fall
/// back to either value (#8737 error arms).
/// What: returns `Ok(None)` for [`ExplicitTarget::CwdAutoDetect`] (the caller
/// keeps its own default), otherwise `(id, registered_root, resolved_via)`.
/// A PATH that resolves to a different id than `-i`/`TRUSTY_INDEX` refuses,
/// naming both.
/// Test: `tests/reindex_quantize_env_conflict_8737.rs`,
/// `tests/index_remove_env_conflict_8175.rs`.
pub(crate) async fn resolve_explicit_target(
    verb: &str,
    client: &reqwest::Client,
    base: &str,
    target: ExplicitTarget,
) -> Result<Option<(String, PathBuf, &'static str)>> {
    let resolved = match target {
        ExplicitTarget::CwdAutoDetect => return Ok(None),
        ExplicitTarget::Refuse(reason) => bail!(reason),
        ExplicitTarget::Path(p) => {
            let (id, root) = find_index_by_path(client, base, &p).await?;
            (id, root, "the PATH argument")
        }
        ExplicitTarget::Id(id) => {
            let (id, root) = find_index_by_id(client, base, &id).await?;
            (id, root, "the -i/--index flag")
        }
        ExplicitTarget::PathAndId(p, id) => {
            let (path_id, path_root) =
                find_index_by_path(client, base, &p)
                    .await
                    .with_context(|| {
                        format!(
                            "refusing to {verb}: could not confirm PATH {} agrees with \
                         index \"{id}\"",
                            p.display()
                        )
                    })?;
            if path_id != id {
                bail!(
                    "refusing to {verb}: PATH {} resolves to index \"{path_id}\", but \
                     -i/--index (or TRUSTY_INDEX) names \"{id}\" — pass matching values, \
                     or drop one of them (issues #8175, #8737)",
                    p.display()
                );
            }
            (
                path_id,
                path_root,
                "the PATH argument (confirmed to agree with -i/--index)",
            )
        }
    };
    Ok(Some(resolved))
}

/// Fetch the registered `root_path` for a known index id via
/// `GET /indexes/:id/status`; any non-2xx answer is an error.
/// Test: `tests/reindex_quantize_env_conflict_8737.rs`
/// (`reindex_flag_alone_targets_the_registered_root`).
pub(crate) async fn find_index_by_id(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> Result<(String, PathBuf)> {
    let url = format!("{base}/indexes/{id}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon returned an error for {url}"))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .context("could not parse status response")?;
    let root = body
        .get("root_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .with_context(|| format!("status response for '{id}' is missing root_path"))?;
    Ok((id.to_string(), root))
}

/// Find the daemon-side index id whose `root_path` canonicalises to `target`.
///
/// Why (#8737 review): skipping an index whose status failed let the lookup
/// match a DIFFERENT id registered at the same root, so a destructive verb
/// could act on an index the operator never named.
/// What: lists `GET /indexes`, then reads EVERY `GET /indexes/:id/status`
/// before deciding. Any unreadable status (unreachable, non-2xx such as a
/// `503`, unparseable, no `root_path`) fails the whole lookup with an error
/// naming that index; the first matching id wins only when all were read.
/// Test: `reindex_path_refuses_when_a_same_root_index_status_503s`,
/// `reindex_path_and_flag_when_status_fails_sends_no_reindex` in
/// `tests/reindex_quantize_env_conflict_8737.rs`.
pub(crate) async fn find_index_by_path(
    client: &reqwest::Client,
    base: &str,
    target: &Path,
) -> Result<(String, PathBuf)> {
    let list_url = format!("{base}/indexes");
    let list_body: serde_json::Value = client
        .get(&list_url)
        .send()
        .await
        .with_context(|| format!("could not reach daemon at {base}"))?
        .error_for_status()
        .with_context(|| format!("daemon error for {list_url}"))?
        .json()
        .await
        .context("could not parse /indexes response")?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let ids: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let canonical_target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());

    let mut matched: Option<(String, PathBuf)> = None;
    for id in ids {
        // #8737: fail closed — an unread index could share this root.
        let (_, root) = find_index_by_id(client, base, &id).await.with_context(|| {
            format!(
                "could not read the status of index \"{id}\" while resolving PATH {}; \
                     refusing rather than matching another index by root path",
                target.display()
            )
        })?;
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if matched.is_none() && canonical_root == canonical_target {
            matched = Some((id, root));
        }
    }
    matched.with_context(|| {
        format!(
            "no index registered for path {}; run `trusty-search list` to see registered indexes",
            target.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #8737: the env-only refusal names the verb that refused, so a
    /// `reindex` refusal never reads as a `remove` one.
    #[test]
    fn classify_explicit_target_names_the_verb() {
        let target = classify_explicit_target(
            "reindex",
            None,
            Some(("live-idx".to_string(), IndexIdSource::EnvVar)),
        );
        let ExplicitTarget::Refuse(reason) = target else {
            panic!("env-only must refuse, got {target:?}");
        };
        assert!(reason.contains("refusing to reindex"), "{reason}");
        assert!(reason.contains("live-idx"), "{reason}");
    }

    /// #8737: `quantize`/`relocate` refuse an id known only from
    /// `TRUSTY_INDEX`, and a value with no recorded source fails closed.
    #[test]
    fn flag_only_index_refuses_env_only() {
        for source in [Some(IndexIdSource::EnvVar), None] {
            let err = flag_only_index("quantize", &Some("live-idx".into()), source)
                .expect_err("env-only must refuse");
            let msg = err.to_string();
            assert!(
                msg.contains("live-idx") && msg.contains("TRUSTY_INDEX"),
                "{msg}"
            );
        }
    }

    /// A real flag passes through, and no id at all keeps CWD auto-detection.
    #[test]
    fn flag_only_index_passes_flag_and_absence_through() {
        let id = flag_only_index(
            "relocate",
            &Some("mine".into()),
            Some(IndexIdSource::CliFlag),
        )
        .expect("flag");
        assert_eq!(id.as_deref(), Some("mine"));
        assert_eq!(
            flag_only_index("relocate", &None, None).expect("none"),
            None
        );
    }
}
