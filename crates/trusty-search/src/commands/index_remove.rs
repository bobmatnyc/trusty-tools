//! Handler for `trusty-search index remove [PATH]`.
//!
//! Why: registering a project is one half of an index's lifecycle — removing it
//!      cleanly is the other. Without this, users who run `trusty-search index`
//!      against a directory they later delete have to either DELETE the index
//!      manually via curl or hand-edit `indexes.toml` and the global config
//!      file. The `index remove` subcommand collapses both steps into one.
//! What: resolves PATH and `-i`/`--index`/`TRUSTY_INDEX` together via
//!       [`classify_remove_target`], finds the matching daemon-side index id
//!       via `search.index.status` (and/or `search.indexes.list`), calls
//!       `search.index.delete {index_id, delete_data}` over the daemon socket
//!       (#9214), then drops the matching entry from
//!       `~/.config/trusty-search/config.yaml`.
//!
//! Issue #1087: when `-i`/`--index` is given it MUST override CWD auto-detection
//! and never fall back to CWD detection.
//!
//! Issue #8175: #1087's fix used `explicit_index_id` unconditionally, so a
//! `TRUSTY_INDEX` value the operator never typed silently outranked an
//! explicit PATH argument — a live 236k-chunk index was deleted this way
//! because `TRUSTY_INDEX` was exported for an unrelated `tm` session.
//! [`classify_remove_target`] is the fix: a destructive verb never resolves
//! its target from `TRUSTY_INDEX` alone (it refuses and names the id), and a
//! PATH given alongside `-i`/`TRUSTY_INDEX` is checked for agreement — a
//! mismatch refuses and names both values rather than picking one silently.
//! [`IndexIdSource`] is what makes the distinction possible at all: clap folds
//! a real `-i` flag and the env fallback into one `Cli::index` value, so
//! `main.rs` reads `ArgMatches::value_source` before that fold happens and
//! passes the source down explicitly.
//!
//! Issue #6422: removing an index deletes its on-disk data by default, and
//! `--keep-data` is the explicit opt-out that deregisters and keeps the corpus.
//! The daemon's own default is still the opposite (`?delete_data` absent ⇒
//! preserve, #4123), so this command always sends the flag rather than relying
//! on a default either side might change.
//!
//! Test: `index_remove_resolves_path_*` unit tests cover the path resolution;
//!       `classify_remove_target_*` cover the #8175 precedence/refusal
//!       decision; `delete_params_*`, `confirmation_*` and `delete_body_*`
//!       cover the #6422 default and its failure paths; the socket round-trip,
//!       including the #8175 "touches neither index" proof, is exercised by
//!       `tests/index_remove_env_conflict_8175.rs`.

// #8737: the target-resolution rule moved to `explicit_target` so `reindex`,
// `quantize` and `index relocate` share it; re-exported for existing callers.
use super::explicit_target::{
    classify_explicit_target, resolve_explicit_target, with_source, ParkedTargets,
};
pub(crate) use super::explicit_target::{ExplicitTarget as RemoveTarget, IndexIdSource};
use super::index_remove_stale::registered_or_cleared;
use crate::config::GlobalConfig;
use crate::detect::detect_project;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::Value;
use std::io::IsTerminal;
use std::path::PathBuf;
use trusty_search::service::rpc::writes::METHOD_INDEX_DELETE;

/// Classify how `index remove` resolves its target, with no daemon call yet
/// (issue #8175): the shared [`classify_explicit_target`] rule, verb `remove`.
/// Test: `classify_remove_target_*` below.
pub(crate) fn classify_remove_target(
    cli_path: Option<PathBuf>,
    explicit_index: Option<(String, IndexIdSource)>,
) -> RemoveTarget {
    classify_explicit_target("remove", cli_path, explicit_index)
}

/// Entry point for `trusty-search index remove [PATH]`.
///
/// Why: keep the CLI handler thin — all reusable resolution / daemon logic lives
///      in helpers so the same flow can be invoked from a future MCP tool.
///
/// Issue #1087: `explicit_index_id` is the value of the PARENT command's
/// `-i`/`--index` flag (`Commands::Index { index_id }`). When it is `Some`,
/// the id is used directly and no path-based lookup is performed — this is
/// the fix for the bug where `index remove -i other` would remove the CWD
/// index instead of `other`.
///
/// Issue #8175: `explicit_index_id` alone is no longer enough — see
/// [`classify_remove_target`] and [`IndexIdSource`]'s docs for why a value
/// with no known source can no longer be trusted to act alone.
///
/// Issue #6422: `keep_data` is the opt-out from the destructive default. When
/// it is false the on-disk corpus goes with the registration and the operator
/// is asked to confirm first, unless `yes` already answered.
///
/// What: see module docs.
/// Test: `index_remove_resolves_path_*` below; `classify_remove_target_*` for
///       the #8175 precedence/refusal rule; `delete_params_*` /
///       `confirmation_*` / `delete_body_*` for #6422; the full socket path,
///       including the #8175 "touches neither index" proof, is covered by
///       `tests/index_remove_env_conflict_8175.rs`.
pub async fn handle_index_remove(
    cli_path: Option<PathBuf>,
    explicit_index_id: Option<String>,
    index_source: Option<IndexIdSource>,
    keep_data: bool,
    yes: bool,
) -> Result<()> {
    // #6422: purge by default; `--keep-data` is the explicit deregister-only
    // opt-out.
    let delete_data = !keep_data;
    // #8737: an env-only target refuses before any network call.
    let target = classify_remove_target(cli_path, with_source(explicit_index_id, index_source));
    if let RemoveTarget::Refuse(reason) = &target {
        bail!("{reason}");
    }
    // #9214: the socket only; nothing answering is an error naming it.
    let client = super::daemon_rpc::connect().await?;

    // #8175: resolve each shape against the daemon; PATH plus an id must agree
    // (see `resolve_explicit_target`), and `TRUSTY_INDEX` alone refuses.
    // #8687: a parked target resolves; a PATH (or CWD) nothing registers any
    // more clears its stale rows instead of refusing.
    let (index_id, registered_path, resolved_via) = match target {
        RemoveTarget::CwdAutoDetect | RemoveTarget::Path(_) => {
            let (path, via) = match target {
                RemoveTarget::Path(p) => (p, "the PATH argument"),
                _ => (resolve_target_path(None)?, "the current working directory"),
            };
            let Some((id, root)) = registered_or_cleared(&client, &path).await? else {
                return Ok(());
            };
            (id, root, via)
        }
        other => resolve_explicit_target("remove", &client, other, ParkedTargets::Resolve)
            .await?
            .context("remove target resolution returned no index")?,
    };

    // #8175: report how the target was resolved before anything destructive
    // happens, so a script's log (or a human re-reading a scrollback) can
    // tell an explicit choice from a guess after the fact.
    println!(
        "Resolving target: index \"{index_id}\" ({}), via {resolved_via}",
        registered_path.display()
    );

    // #6422: the confirmation gate. It runs after resolution so the prompt can
    // name the exact index and root path the operator is about to destroy.
    if confirmation_required(delete_data, yes) {
        if !std::io::stdin().is_terminal() {
            bail!(
                "refusing to delete index \"{index_id}\" and its on-disk data without \
                 confirmation; pass --yes to confirm, or --keep-data to deregister only"
            );
        }
        if !super::confirm(&format!(
            "Delete index \"{index_id}\" and its on-disk data ({})? This cannot be undone.",
            registered_path.display()
        ))? {
            println!("Aborted.");
            return Ok(());
        }
    }

    // #9214: a refused or broken delete returns here, before any local row
    // is touched; the daemon's own refusal text carries the reason.
    let body = super::daemon_rpc::call(
        &client,
        METHOD_INDEX_DELETE,
        delete_params(&index_id, delete_data),
    )
    .await
    .with_context(|| format!("could not delete index \"{index_id}\""))?;

    // #6422: a `200` is not proof — the daemon answers one for a delete that
    // removed no registration. The local cleanup below runs only when the
    // registration is confirmed gone, and it runs even when the DATA removal
    // failed, because leaving those rows behind would strand entries for an
    // index the daemon no longer has.
    let removed = registration_removed(&body);
    let data_outcome = interpret_delete_body(delete_data, &body);
    if !removed {
        match data_outcome {
            Ok(_) => bail!("the daemon removed no registration for \"{index_id}\""),
            Err(reason) => bail!("{reason}"),
        }
    }

    // Drop the matching entry from the global YAML config so a future daemon
    // restart does not auto-rediscover the project the user just removed.
    // Best-effort: a config-file write failure should not undo the daemon-side
    // delete that already succeeded.
    match GlobalConfig::load() {
        Ok(mut cfg) => {
            let dropped = cfg.remove_collection_by_path(&registered_path);
            if dropped.is_some() {
                if let Err(e) = cfg.save() {
                    tracing::warn!("could not update global config after removal: {e:#}");
                }
            }
        }
        Err(e) => {
            tracing::warn!("could not load global config to remove entry: {e:#}");
        }
    }

    // Issue #767: also remove from the opt-in allowlist so the path cannot
    // be re-registered without explicit re-approval.  Best-effort.
    if let Err(e) = crate::allowlist::remove_from_allowlist(&registered_path, None) {
        tracing::warn!(
            path = %registered_path.display(),
            error = %e,
            "could not remove path from allowlist after index removal"
        );
    }

    // #6422: report what the daemon said it DID. A registration that went while
    // its bytes stayed is a failure, not a removal with a caveat — printing a
    // tick there records the disk as reclaimed while every byte is still on it
    // (#3049).
    let data_deleted = match data_outcome {
        Ok(v) => v,
        Err(reason) => bail!("{reason}"),
    };

    println!(
        "{} Removed index {} ({}) — {}",
        "✓".green(),
        format!("\"{index_id}\"").bold(),
        registered_path.display(),
        if data_deleted {
            "on-disk data deleted"
        } else {
            "on-disk data kept (--keep-data)"
        }
    );
    Ok(())
}

/// The `search.index.delete` params for one index, carrying the data choice
/// explicitly.
///
/// Why (#6422): the CLI purges on-disk data by default while the daemon's own
/// default is still the opposite (`delete_data` absent ⇒ preserve, #4123).
/// Sending the flag on every call means this command's default does not depend
/// on which daemon version answers it.
/// What: `{"index_id": <id>, "delete_data": <bool>}` (#9214: the socket form).
/// Test: `delete_params_purge_by_default`, `delete_params_honour_keep_data`.
pub(crate) fn delete_params(id: &str, delete_data: bool) -> Value {
    serde_json::json!({ "index_id": id, "delete_data": delete_data })
}

/// Whether the operator must confirm before this delete runs.
///
/// Why (#6422): the owner ruling made the destructive path the default and kept
/// the confirmation. Deregister-only touches no data, so it keeps the
/// unprompted behaviour scripts already rely on.
/// What: true only when data is about to be deleted and `--yes` has not already
/// answered.
/// Test: `confirmation_is_required_only_for_a_data_delete`.
pub(crate) fn confirmation_required(delete_data: bool, yes: bool) -> bool {
    delete_data && !yes
}

/// Whether the daemon confirmed it removed the registration.
///
/// Why: `DELETE` answers `200 {"removed": false}` for a delete it declined to
/// perform, so the status code alone cannot say whether anything happened.
/// What: reads `removed`, treating a missing field as "not removed" — a body
/// that never said so has not said so.
/// Test: `delete_body_without_a_removal_is_a_failure`.
pub(crate) fn registration_removed(body: &Value) -> bool {
    body.get("removed").and_then(Value::as_bool) == Some(true)
}

/// What the daemon's delete body says actually happened to the data.
///
/// Why (#6422): a delete that removed the registration and left every byte on
/// disk must not print as a clean removal — the operator would record the disk
/// as reclaimed while the corpus is still there (#3049). The daemon answers
/// `500` when the durable cleanup fails, but `200` with `data_deleted: false`
/// is a success on the wire, so the body is read rather than the status alone.
/// What: `Ok(true)` when the data went, `Ok(false)` when only the registration
/// did and that is what was asked for, `Err(reason)` for every other shape.
/// Test: `delete_body_confirms_a_purge`,
/// `delete_body_reporting_undeleted_data_is_a_failure`,
/// `delete_body_without_a_removal_is_a_failure`,
/// `delete_body_of_a_keep_data_delete_reports_the_data_kept`.
pub(crate) fn interpret_delete_body(delete_data: bool, body: &Value) -> Result<bool, String> {
    if !registration_removed(body) {
        return Err(format!(
            "the daemon answered successfully but removed no registration{}",
            match body.get("quiesced").and_then(Value::as_bool) {
                Some(false) => " (in-flight writers never quiesced, so the teardown was abandoned)",
                _ => "",
            }
        ));
    }
    let data_deleted = body.get("data_deleted").and_then(Value::as_bool) == Some(true);
    if delete_data && !data_deleted {
        return Err(
            "the registration was removed but the on-disk data was NOT deleted; \
             the corpus is still on disk — re-run the delete to reclaim it"
                .to_string(),
        );
    }
    Ok(data_deleted)
}

/// Resolve the path argument: CLI value wins; otherwise auto-detect from CWD.
///
/// Why: same precedence rule as `search`, `watch`, and `reindex` — keeps the
///      mental model consistent across project-scoped commands.
/// What: returns the CLI path verbatim when present, otherwise walks upward
///       from CWD looking for `.git` / `.trusty-search` markers via
///       `detect::detect_project`. Falls back to the CWD itself if no marker
///       is found (mirrors the `Fallback` branch elsewhere), and errors when
///       detection refuses that root (#6550).
/// Test: `index_remove_resolves_path_uses_cli` and
///       `index_remove_resolves_path_falls_back_to_cwd`.
fn resolve_target_path(cli_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = cli_path {
        return Ok(p);
    }
    let cwd = std::env::current_dir().context("could not resolve current directory")?;
    // #6550: a refused root is an error here, not a path to remove by guess.
    let ctx = detect_project(&cwd)?;
    Ok(ctx.root_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_remove_resolves_path_uses_cli() {
        let p = resolve_target_path(Some(PathBuf::from("/explicit/path"))).unwrap();
        assert_eq!(p, PathBuf::from("/explicit/path"));
    }

    #[test]
    fn index_remove_resolves_path_falls_back_to_cwd() {
        // We don't assert a specific path (depends on the test runner CWD) but
        // the call must succeed and return a non-empty path.
        let p = resolve_target_path(None).unwrap();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn index_remove_resolves_path_uses_detected_root() {
        // When CWD is inside a directory that has a `.git` marker, we should
        // detect that root rather than returning the CWD-as-is.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("trusty-idxrm-{pid}-{nanos}"));
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a");
        fs::create_dir_all(&nested).unwrap();

        // Exercise the same helper detect uses by passing through detect_project
        // directly — we cannot safely change CWD inside a parallel test runner.
        let ctx = detect_project(&nested).expect("a git-rooted fixture is indexable");
        assert_eq!(ctx.root_path, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── #8175: PATH vs `-i`/TRUSTY_INDEX precedence and refusal ─────────────

    /// Regression test for issue #1087 (carried forward under #8175's new
    /// function) — an explicit `-i <id>` with no PATH argument MUST bypass
    /// CWD detection.
    /// Why: this is the #1087 behaviour `classify_remove_target` must
    /// preserve — a real flag with nothing to check it against is used
    /// directly, never silently redirected to the CWD.
    /// What: `Some((id, CliFlag))`, no path → `RemoveTarget::Id(id)`.
    /// Test: this test.
    #[test]
    fn classify_remove_target_cli_flag_alone_is_used_directly() {
        let target = classify_remove_target(
            None,
            Some(("other-project".to_string(), IndexIdSource::CliFlag)),
        );
        assert_eq!(target, RemoveTarget::Id("other-project".to_string()));
    }

    /// Why: nothing given at all must fall back to the pre-existing,
    /// unaffected CWD auto-detect default — #8175 only tightens the cases
    /// where a target was actually named.
    /// What: `None`, no path → `RemoveTarget::CwdAutoDetect`.
    /// Test: this test.
    #[test]
    fn classify_remove_target_nothing_given_uses_cwd() {
        assert_eq!(
            classify_remove_target(None, None),
            RemoveTarget::CwdAutoDetect
        );
    }

    /// Why: a bare `remove <PATH>` with no `-i`/`TRUSTY_INDEX` in play must
    /// keep working exactly as it always has.
    /// What: a path, no id → `RemoveTarget::Path`.
    /// Test: this test.
    #[test]
    fn classify_remove_target_path_alone_is_used_directly() {
        let target = classify_remove_target(Some(PathBuf::from("/explicit/path")), None);
        assert_eq!(target, RemoveTarget::Path(PathBuf::from("/explicit/path")));
    }

    /// Core regression for issue #8175: an id resolved ONLY from
    /// `TRUSTY_INDEX`, with no PATH and no real `-i` flag, must refuse rather
    /// than act — this is exactly the shape of the incident (`index remove
    /// <scratch-path>` with `TRUSTY_INDEX` pointing at the live project
    /// index; the pre-fix code used the env value unconditionally). Run
    /// against the pre-#8175 `handle_index_remove`, an equivalent call would
    /// have deleted the `TRUSTY_INDEX` index outright with no refusal at all.
    /// What: `Some((id, EnvVar))`, no path → `RemoveTarget::Refuse` naming the
    /// id and pointing at an explicit argument or `TRUSTY_INDEX` unset.
    /// Test: this test.
    #[test]
    fn classify_remove_target_env_only_refuses() {
        let target = classify_remove_target(
            None,
            Some(("trusty-tools-4e2cf878".to_string(), IndexIdSource::EnvVar)),
        );
        let RemoveTarget::Refuse(reason) = target else {
            panic!("an id known only from TRUSTY_INDEX must refuse, got {target:?}");
        };
        assert!(
            reason.contains("trusty-tools-4e2cf878"),
            "the refusal must name the index it declined to touch: {reason}"
        );
        assert!(
            reason.contains("TRUSTY_INDEX"),
            "the refusal must name the source it declined to trust alone: {reason}"
        );
    }

    /// Why: PATH and an id together — from either source — carry a genuine
    /// conflict risk that can only be checked once PATH is resolved against
    /// the daemon, so `classify_remove_target` defers the comparison rather
    /// than guessing; `handle_index_remove` is what actually refuses on a
    /// mismatch (see `tests/index_remove_env_conflict_8175.rs`).
    /// What: a path AND an id (either source) → `RemoveTarget::PathAndId`.
    /// Test: this test.
    #[test]
    fn classify_remove_target_path_and_id_defers_to_daemon_check() {
        for source in [IndexIdSource::CliFlag, IndexIdSource::EnvVar] {
            let target = classify_remove_target(
                Some(PathBuf::from("/scratch/proj")),
                Some(("other-id".to_string(), source)),
            );
            assert_eq!(
                target,
                RemoveTarget::PathAndId(PathBuf::from("/scratch/proj"), "other-id".to_string()),
                "source {source:?} must still defer to the daemon-backed agreement check"
            );
        }
    }

    // ── #6422: deleting an index purges its data by default ─────────────────

    /// Why (#6422, closure condition 1): the owner ruling. Against the pre-fix
    /// code this command issued `DELETE /indexes/{id}` with NO query at all,
    /// which the daemon reads as `delete_data=false` (#4123) — so this
    /// assertion fails there, on the substring and on the whole URL alike.
    /// Test: this is the test.
    #[test]
    fn delete_params_purge_by_default() {
        assert_eq!(
            super::delete_params("rustbot", true),
            serde_json::json!({ "index_id": "rustbot", "delete_data": true }),
            "the default delete must ask the daemon for the data too"
        );
    }

    /// Why (#6422, closure condition 2): `--keep-data` is the opt-out, and it
    /// must send `false` EXPLICITLY rather than omit the parameter — a delete
    /// that leans on the daemon's default is one daemon version away from
    /// destroying the corpus it promised to keep.
    /// Test: this is the test.
    #[test]
    fn delete_params_honour_keep_data() {
        assert_eq!(
            super::delete_params("rustbot", false),
            serde_json::json!({ "index_id": "rustbot", "delete_data": false })
        );
    }

    /// Why (#6422, closure condition 3): the destructive default keeps its
    /// confirmation. Deregister-only destroys nothing, so it keeps the
    /// unprompted behaviour scripts already rely on.
    /// Test: this is the test.
    #[test]
    fn confirmation_is_required_only_for_a_data_delete() {
        assert!(
            super::confirmation_required(true, false),
            "a data delete must be confirmed"
        );
        assert!(
            !super::confirmation_required(true, true),
            "--yes answers the confirmation"
        );
        assert!(
            !super::confirmation_required(false, false),
            "--keep-data destroys nothing and must not prompt"
        );
    }

    /// Why: the success path must stay reachable and must report that the data
    /// actually went.
    /// Test: this is the test.
    #[test]
    fn delete_body_confirms_a_purge() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": true
        });
        assert!(super::registration_removed(&body));
        assert_eq!(super::interpret_delete_body(true, &body), Ok(true));
    }

    /// Why (#6422 failure path, #3049): the registration went and every byte
    /// stayed. Printing a tick there records the corpus as reclaimed while it
    /// is still on disk, which is the silent half-deleted state this command
    /// must never leave behind.
    /// Test: this is the test.
    #[test]
    fn delete_body_reporting_undeleted_data_is_a_failure() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": false
        });
        let err = super::interpret_delete_body(true, &body)
            .expect_err("undeleted data must not read as a clean removal");
        assert!(
            err.contains("was NOT deleted"),
            "the failure must say the data survived: {err}"
        );
    }

    /// Why: `200 {"removed": false}` is the daemon's honest no-op, and an
    /// abandoned teardown (`quiesced: false`) is a distinct condition the
    /// operator can act on by retrying.
    /// Test: this is the test.
    #[test]
    fn delete_body_without_a_removal_is_a_failure() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({ "removed": false, "data_deleted": false }),
        ] {
            assert!(!super::registration_removed(&body), "body: {body}");
            assert!(
                super::interpret_delete_body(true, &body).is_err(),
                "body: {body}"
            );
        }

        let abandoned =
            serde_json::json!({ "removed": false, "data_deleted": false, "quiesced": false });
        let err = super::interpret_delete_body(true, &abandoned).expect_err("abandoned");
        assert!(
            err.contains("never quiesced"),
            "an abandoned teardown must say so: {err}"
        );
    }

    /// Why (#6422): the opt-out's success path. A deregister-only delete that
    /// left the data alone is exactly what was asked for, and must read as a
    /// success reporting the data kept.
    /// Test: this is the test.
    #[test]
    fn delete_body_of_a_keep_data_delete_reports_the_data_kept() {
        let body = serde_json::json!({
            "id": "rustbot", "ok": true, "removed": true, "data_deleted": false
        });
        assert_eq!(super::interpret_delete_body(false, &body), Ok(false));
    }
}
