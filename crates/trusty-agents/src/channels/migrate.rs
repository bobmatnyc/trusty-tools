//! One-shot drains of the legacy `[[listeners]]` tables into `[[channels]]`
//! (#7609, slices 2 and 3).
//!
//! Why: an operator who already configured listeners must not have to
//! hand-edit anything to keep them working after the merge. Both drains follow
//! the precedent `crate::mcp::shared::migrate` set for #7454: the decision is
//! taken from what is on disk, with no hidden "migrated" marker to disagree
//! with it, it is re-taken INSIDE the write lock, and a legacy table that will
//! not parse writes NOTHING rather than publishing a silently empty list.
//! What: [`migrate_global_if_absent`] merges `~/.trusty-agents/config.toml`'s
//! `[[listeners]]` into `[[channels]]` and DELETES the legacy table — the
//! in-memory absorb that made leaving it harmless is gone (#7609 slice 7), so a
//! table left behind would be an inert listener.
//! [`migrate_agent_channels_if_absent`] writes one assistant's
//! `agent.toml` `[[listeners]]` bindings into its `*.channels.json` when that
//! file is absent — only the ones that resolve to a storage record the
//! existing channel loader accepts, because writing a record it would reject
//! turns a working channel view into a 500.
//! Test: `crate::channels::migrate_tests` — the whole module.

// #7609: slices 2 and 3, the persisted half of the listeners→channels merge.
use super::model::{Channel, ChannelScope};
use crate::listeners::config::{AgentListenerBinding, ListenerConfig};
use serde::Serialize;
use std::path::Path;

/// Why a migration wrote nothing, when the reason was not simply "already
/// done".
///
/// Why: the fail-open posture this crate already has on `GlobalConfig::load`
/// must not spread. A `[[channels]]` or `[[listeners]]` table that does not
/// parse is an operator-visible problem, and returning it as an error is what
/// lets the caller name the file and the key in one warning instead of
/// treating a broken table as an empty one.
/// Test: `a_malformed_channels_table_is_reported_and_writes_nothing`,
/// `a_malformed_listeners_table_is_reported_and_writes_nothing`.
#[derive(Debug, thiserror::Error)]
pub enum ChannelMigrationError {
    #[error("{path}: `{key}` did not parse ({source}); nothing was migrated")]
    Parse {
        path: String,
        key: &'static str,
        #[source]
        source: toml::de::Error,
    },
    #[error("{path}: could not be read ({source}); nothing was migrated")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: the migrated channels could not be encoded ({source}); nothing was migrated")]
    Encode {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: could not be written ({source}); the legacy table is untouched")]
    Write {
        path: String,
        #[source]
        source: anyhow::Error,
    },
}

/// What one migration call moved.
///
/// Why: same reason `crate::mcp::shared::migrate::MigrationReport` exists — a
/// migration nobody can see is a migration nobody can debug, and a test needs
/// something to assert on besides the file's bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelMigrationReport {
    /// Ids of the channels written, in written order.
    pub channels: Vec<String>,
}

impl ChannelMigrationReport {
    /// One line naming every moved channel, for the startup log.
    pub fn summary(&self) -> String {
        self.channels.join(", ")
    }
}

/// Say once per process that an `agent.toml` `[[listeners]]` table is retired.
///
/// Why once, and why `error` (#7609 slice 7): the entries are no longer folded
/// into `channels`, so they are INERT — a configuration fault the operator has
/// to fix, not a deprecation they can defer. Once per process because every
/// dispatch re-reads every manifest.
/// Test: `the_retirement_notice_fires_at_most_once`.
pub fn warn_agent_listeners_deprecated() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::error!(
            file = "agent.toml",
            replacement = "<assistant>.channels.json",
            "`[[listeners]]` is retired and its entries are INERT; the startup sweep moves them \
             into the assistant's channels file and deletes the table (#7609)"
        );
    });
}

/// The `[[channels]]` array as its own document, for rendering and appending.
#[derive(Serialize)]
struct ChannelsDocument<'a> {
    channels: &'a [Channel],
}

fn parse_table(raw: &str, path: &Path) -> Result<toml::Table, ChannelMigrationError> {
    toml::from_str(raw).map_err(|source| ChannelMigrationError::Parse {
        path: path.display().to_string(),
        key: "the file",
        source,
    })
}

/// Read one array-of-tables key, distinguishing "absent" from "will not parse".
///
/// Why: this is the whole fail-open guard. `None` means the key is genuinely
/// absent; an unparseable value is an error naming the key, never an empty
/// `Vec` that reads to the caller as "there was nothing there".
fn read_key<T: serde::de::DeserializeOwned>(
    table: &toml::Table,
    key: &'static str,
    path: &Path,
) -> Result<Option<Vec<T>>, ChannelMigrationError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    value
        .clone()
        .try_into::<Vec<T>>()
        .map(Some)
        .map_err(|source| ChannelMigrationError::Parse {
            path: path.display().to_string(),
            key,
            source,
        })
}

/// Drain `config.toml`'s `[[listeners]]` into `[[channels]]` and delete it.
///
/// Why the table is now DELETED (#7609 slice 7): for one release the drain left
/// it on disk and `GlobalConfig` absorbed it in memory on every load, so the
/// file having both tables was a normal, working state. The absorb is gone, so
/// a `[[listeners]]` table left behind would be inert — a listener that
/// silently stopped polling. Moving it and removing it in the same locked write
/// makes the drain idempotent and the legacy spelling genuinely absent
/// afterwards, which is what lets `GlobalConfig::load_or_create` treat a
/// surviving table as an error.
/// What: returns `Ok(None)` when the file is absent or declares no
/// `[[listeners]]`. Otherwise every legacy entry with no `[[channels]]` entry
/// of the same id is appended — a channel already there WINS, matching the
/// precedence the retired in-memory absorb applied — the legacy table is
/// removed through [`crate::channels::document`] so the comment block above it
/// survives, and the result is published by
/// [`crate::state_writer::atomic_update`]. The unlocked read only decides
/// whether there is work to do and reports a malformed table by key; what is
/// written is derived again from the LOCKED bytes, so neither table can change
/// between the decision and the write.
/// Test: `a_global_listener_migrates_once_and_never_again`,
/// `the_drain_removes_the_legacy_listeners_table_and_keeps_its_comment`,
/// `a_malformed_channels_table_is_reported_and_writes_nothing`.
pub fn migrate_global_if_absent(
    path: &Path,
) -> Result<Option<ChannelMigrationReport>, ChannelMigrationError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ChannelMigrationError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    // The unlocked read decides only whether there is work AND reports a
    // malformed table with its key; the bytes it produces are discarded. What
    // actually gets written is derived again from the LOCKED bytes below, so a
    // `[[listeners]]` edit that lands between the two reads cannot be written
    // over (critic, #7609).
    if global_channels_to_write(&raw, path)?.is_none() {
        return Ok(None);
    }
    let mut moved: Vec<String> = Vec::new();
    let wrote = crate::state_writer::atomic_update(path, |existing| {
        let Some(bytes) = existing else {
            return Ok(None);
        };
        let current = String::from_utf8_lossy(bytes).into_owned();
        // #7609: the whole decision is re-taken under the lock — what
        // `[[channels]]` and `[[listeners]]` now say.
        let Some(channels) =
            global_channels_to_write(&current, path).map_err(anyhow::Error::new)?
        else {
            return Ok(None);
        };
        moved = channels
            .appended
            .iter()
            .map(|channel| channel.id.clone())
            .collect();
        Ok(Some(
            render_drained(&current, &channels.merged)?.into_bytes(),
        ))
    })
    .map_err(|source| ChannelMigrationError::Write {
        path: path.display().to_string(),
        source,
    })?;
    Ok(wrote.then_some(ChannelMigrationReport { channels: moved }))
}

/// `current` with `[[channels]]` set to `merged` and `[[listeners]]` removed.
///
/// Why `toml_edit` rather than the text append this used to do: the legacy table
/// has to come OUT now, and removing it with a plain `DocumentMut::remove`
/// deletes the comment block above its header — the defect live verification
/// found on `PUT /api/channels` (#7609 slice 5 finding 1).
fn render_drained(current: &str, merged: &[Channel]) -> anyhow::Result<String> {
    use anyhow::Context as _;
    let rendered = toml::to_string_pretty(&ChannelsDocument { channels: merged })?;
    let mut document = current
        .parse::<toml_edit::DocumentMut>()
        .context("config.toml is not an editable document; nothing was migrated")?;
    let item = rendered
        .parse::<toml_edit::DocumentMut>()
        .context("re-reading the encoded [[channels]]")?
        .get("channels")
        .cloned()
        .context("the encoded document declared no [[channels]]")?;
    crate::channels::document::replace_array_of_tables(&mut document, "channels", item);
    crate::channels::document::remove_preserving_comments(&mut document, "listeners");
    Ok(document.to_string())
}

/// What a global drain would write: the whole merged list, and the subset the
/// legacy table contributed.
struct DrainedChannels {
    /// The complete `[[channels]]` array to publish.
    merged: Vec<Channel>,
    /// Only the entries this drain added, for the report.
    appended: Vec<Channel>,
}

/// The channels a global migration would write, or `None` when it must not.
///
/// What: `None` when `[[listeners]]` is absent or empty — there is nothing to
/// move and nothing to delete. A `[[channels]]` entry of the same id WINS, so a
/// file the operator has already migrated by hand merges to itself and the
/// drain's only effect is removing the legacy table.
fn global_channels_to_write(
    raw: &str,
    path: &Path,
) -> Result<Option<DrainedChannels>, ChannelMigrationError> {
    let table = parse_table(raw, path)?;
    // `[[channels]]` is parsed even when the migration will add nothing, so a
    // malformed one is reported rather than read as "already migrated, fine".
    let mut merged = read_key::<Channel>(&table, "channels", path)?.unwrap_or_default();
    let listeners = read_key::<ListenerConfig>(&table, "listeners", path)?;
    let Some(listeners) = listeners.filter(|entries| !entries.is_empty()) else {
        return Ok(None);
    };
    let mut appended = Vec::new();
    for listener in listeners {
        if merged.iter().any(|channel| channel.id == listener.name) {
            continue;
        }
        appended.push(Channel::from(listener));
    }
    merged.extend(appended.iter().cloned());
    Ok(Some(DrainedChannels { merged, appended }))
}

/// Drain one assistant's `agent.toml` `[[listeners]]` into its channels file.
///
/// Why: the per-assistant half of the same merge. It writes through the shape
/// `crate::api::server::agent_channels` already reads, so the channel view and
/// every existing consumer see the migrated bindings without a format change.
/// What: returns `Ok(None)` when `agent.toml` is absent, declares no
/// `[[listeners]]`, when the channels file already exists, or when no binding
/// resolves to a storable record. A binding is storable only when the global
/// channel it names resolves to a registered provider AND a destination that
/// provider accepts; anything else is logged and LEFT in `agent.toml`, because
/// `agent_channels::load_at` validates every record it reads and a rejected
/// one would break the assistant's channel view. `agent.toml` is never
/// modified.
/// Test: `an_agent_binding_migrates_once_and_never_again`,
/// `an_unstorable_binding_is_left_in_agent_toml`,
/// `a_malformed_agent_listeners_table_is_reported_and_writes_nothing`.
pub fn migrate_agent_channels_if_absent(
    agent_toml: &Path,
    channels_json: &Path,
    globals: &[Channel],
) -> Result<Option<ChannelMigrationReport>, ChannelMigrationError> {
    if channels_json.exists() {
        return Ok(None);
    }
    let raw = match std::fs::read_to_string(agent_toml) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ChannelMigrationError::Read {
                path: agent_toml.display().to_string(),
                source,
            });
        }
    };
    let table = parse_table(&raw, agent_toml)?;
    let bindings =
        read_key::<AgentListenerBinding>(&table, "listeners", agent_toml)?.unwrap_or_default();
    let records = storable_records(bindings, globals);
    if records.is_empty() {
        return Ok(None);
    }
    let report = ChannelMigrationReport {
        channels: records.iter().map(|b| b.id.clone()).collect(),
    };
    let bytes =
        serde_json::to_vec_pretty(&records).map_err(|source| ChannelMigrationError::Encode {
            path: channels_json.display().to_string(),
            source,
        })?;
    let wrote = crate::state_writer::atomic_update(channels_json, move |existing| {
        if existing.is_some() {
            return Ok(None);
        }
        Ok(Some(bytes))
    })
    .map_err(|source| ChannelMigrationError::Write {
        path: channels_json.display().to_string(),
        source,
    })?;
    Ok(wrote.then_some(report))
}

/// What one startup sweep moved, both halves.
///
/// Why: the sweep is fire-and-forget, so its only other observable is the log.
/// A test needs something to assert on besides the files' bytes — including
/// the drain's FAILURE, which is why `global` is a `Result` the caller may
/// ignore rather than an `Option` that reads the same for "nothing to do" and
/// "could not be written".
#[derive(Debug)]
pub struct StartupMigrationReport {
    /// The global `config.toml` drain: `Ok(None)` when there was nothing to
    /// move or no path to move it in, `Err` when it was tried and failed.
    pub global: Result<Option<ChannelMigrationReport>, ChannelMigrationError>,
    /// One entry per assistant whose channels file was seeded.
    pub assistants: Vec<(String, ChannelMigrationReport)>,
}

/// Run both halves of the one-shot channel migration, global half first.
///
/// Why (#7609): [`migrate_global_if_absent`] was reachable only from
/// `GlobalConfig::load_or_create`, which only the REPL routing command calls.
/// Every daemon entry point reads the config through `GlobalConfig::load`,
/// which absorbed the legacy table in memory and never persisted — so a
/// supervised `tagent --api` or `tagent --slack` left `config.toml` unchanged
/// across every restart. This is the one entry point those starts call, and it
/// keeps the hot `load` path read-only.
/// What: drains `config_path` FIRST, so the assistant sweep's `route_to`
/// backfill finds the `[[channels]]` entry it appends to; then sweeps `dirs`.
/// A drain failure is logged and does NOT stop the sweep: the assistant half
/// migrates files the global half never touches, and taking it down too would
/// widen one unwritable `config.toml` into a whole host's worth of inert
/// bindings. An empty `dirs` runs the global drain alone, which is what
/// [`spawn_global_migration`] wants.
/// Test: `a_daemon_start_drains_the_global_config_once`,
/// `a_daemon_start_survives_a_malformed_global_config`.
pub fn run_startup_migration(
    config_path: Option<&Path>,
    dirs: &[std::path::PathBuf],
    globals: &[Channel],
) -> StartupMigrationReport {
    // #7609: the global drain, on the startup path at last. It runs BEFORE the
    // sweep below so `backfill_assistant_routes` finds the `[[channels]]` entry
    // this just wrote and can name the bound assistant in its `route_to`.
    let global = match config_path {
        Some(path) => drain_global(path),
        None => Ok(None),
    };
    // #7609 slice 7: re-read the globals the drain just published rather than
    // trusting the caller's boot-time snapshot. Before the in-memory absorb was
    // retired that snapshot already carried the legacy listener, so a pre-drain
    // list was good enough; now an undrained file yields NO channel and the
    // backfill would find nothing to name the bound assistant in on the very
    // start that needs it.
    let drained = match (config_path, &global) {
        (Some(path), Ok(Some(_))) => published_globals(path),
        _ => None,
    };
    let globals = drained.as_deref().unwrap_or(globals);
    StartupMigrationReport {
        global,
        assistants: migrate_assistant_channels(dirs, globals, config_path),
    }
}

/// The global channels `config_path` declares, after a drain wrote it.
///
/// What: `None` when the file cannot be read or parsed, which leaves the caller
/// on its own snapshot rather than on an empty list — a failure here must not
/// make the sweep look like a host with no channels.
fn published_globals(config_path: &Path) -> Option<Vec<Channel>> {
    let raw = std::fs::read_to_string(config_path)
        .inspect_err(|error| {
            tracing::warn!(
                path = %config_path.display(),
                %error,
                "channel migration: the drained config could not be re-read (#7609)",
            );
        })
        .ok()?;
    crate::mcp::config::GlobalConfig::from_toml_str(&raw)
        .inspect_err(|error| {
            tracing::warn!(
                path = %config_path.display(),
                %error,
                "channel migration: the drained config could not be re-parsed (#7609)",
            );
        })
        .ok()
        .map(|config| config.channels)
}

/// The global drain plus its log arms, so the sweep above reads as two steps.
///
/// Why: a drain failure is an operator-visible problem — a read-only or
/// hand-broken `config.toml` — that must name its path and its cause in the
/// daemon's own log. Nothing absorbs `[[listeners]]` at parse time any more
/// (#7609 slice 7), so a failed drain is not a recoverable state the daemon
/// papers over: the legacy table stays on disk and every entry in it is INERT
/// until an operator moves it to `[[channels]]` by hand. The warn arm says
/// exactly that, because it is the only notice the operator gets.
/// Test: `a_daemon_start_survives_a_malformed_global_config`.
fn drain_global(path: &Path) -> Result<Option<ChannelMigrationReport>, ChannelMigrationError> {
    let outcome = migrate_global_if_absent(path);
    match &outcome {
        Ok(Some(report)) => tracing::info!(
            path = %path.display(),
            moved = %report.summary(),
            "channel migration: moved [[listeners]] into [[channels]] (#7609)",
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            %error,
            "channel migration: the global drain wrote nothing; the legacy [[listeners]] table \
             stays on disk and its entries are INERT — move them to [[channels]] by hand \
             (#7609)",
        ),
    }
    outcome
}

/// Start the startup sweep detached, off the caller's critical path.
///
/// Why: the sweep publishes each assistant's channels file through
/// [`crate::state_writer::atomic_update`], which takes a BLOCKING `fs4`
/// advisory lock with no timeout. Awaiting it on the API server's startup path
/// lets one held lock — or simply a large roster — stall the TCP bind, the
/// same defect shape as trusty-mpm #7965. Nothing on the request path reads
/// the sweep's result, so it is fire-and-forget like the docs index and the
/// log drain beside it in `api::server::routes`, and its report reaches the
/// log when it finishes.
/// What: returns IMMEDIATELY. Requires a tokio runtime. `config_path` is the
/// global `config.toml` the drain and the `route_to` backfill write (#7609);
/// `None` runs the seeding half of the sweep with both disabled, which is what
/// a host whose config path will not resolve gets instead of no sweep at all
/// (#7609 review).
/// Test: `the_assistant_sweep_never_blocks_its_caller`,
/// `the_sweep_seeds_channels_with_no_backfill_target`.
pub fn spawn_startup_migration(
    dirs: Vec<std::path::PathBuf>,
    globals: Vec<Channel>,
    config_path: Option<std::path::PathBuf>,
) {
    tokio::task::spawn(async move {
        match tokio::task::spawn_blocking(move || {
            run_startup_migration(config_path.as_deref(), &dirs, &globals)
        })
        .await
        {
            Ok(report) => {
                // #7609 review: three states, not a bool — a `false` that
                // covered both "nothing to drain" and "the drain failed" is
                // exactly the ambiguity this log line exists to resolve.
                let global = match &report.global {
                    Ok(Some(_)) => "moved",
                    Ok(None) => "nothing",
                    Err(_) => "failed",
                };
                if global != "nothing" || !report.assistants.is_empty() {
                    tracing::info!(
                        assistants = report.assistants.len(),
                        global,
                        "channel migration: the startup sweep finished (#7609)",
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "channel migration: startup sweep task failed"),
        }
    });
}

/// The global drain alone, for a start with no assistant roster to sweep.
///
/// Why (#7609): `--api` returns early from `runtime::startup` and runs the full
/// sweep from `api::server::routes`. Every OTHER `tagent` start goes through
/// the shared runtime startup hook, which owns no assistant roster but still
/// has to drain the global config exactly once. Detached for the same reason as
/// the full sweep.
/// Test: `the_startup_hook_drains_the_global_config`.
pub fn spawn_global_migration(config_path: std::path::PathBuf) {
    spawn_startup_migration(Vec::new(), Vec::new(), Some(config_path));
}

/// Seed every discovered assistant's channels file from its `agent.toml`.
///
/// Why: the per-assistant half of the migration has to run somewhere once per
/// process, and startup is the only place that already knows the global
/// channels each binding resolves its provider from.
/// What: one [`migrate_agent_channels_if_absent`] call per assistant, each
/// independent — a failure on one is logged and the sweep continues, because
/// one unreadable manifest must not stop the others from migrating. Returns
/// the reports for the assistants that moved something.
///
/// #7609 slice 4: the sweep ALSO backfills `route_to`. A legacy per-assistant
/// binding for global `G` naming assistant `A` is the two-stage opt-in the
/// owner's 2026-09-14 ruling retires, so `A` is appended to `G.route_to` and
/// the pair moves from dispatch source (3) to source (2) with no manual edit.
/// That runs for every discovered assistant, including one whose channels file
/// already exists and therefore migrates nothing. `config_path` is `None` when
/// the global config path would not resolve: the seeding half still runs, the
/// backfill alone is skipped (#7609 review).
/// Test: `the_sweep_skips_an_assistant_with_no_manifest`,
/// `the_sweep_backfills_route_to_for_a_legacy_binding`,
/// `the_sweep_seeds_channels_with_no_backfill_target`.
pub fn migrate_assistant_channels(
    dirs: &[std::path::PathBuf],
    globals: &[Channel],
    config_path: Option<&Path>,
) -> Vec<(String, ChannelMigrationReport)> {
    let mut reports = Vec::new();
    for id in crate::assistants::discover_instances(dirs) {
        let Some((manifest, _)) =
            crate::api::server::agent_patch::resolve_agent_paths(dirs, id.as_str())
        else {
            continue;
        };
        if let Some(config_path) = config_path {
            backfill_assistant_routes(config_path, &manifest, globals, id.as_str());
        }
        let channels_json = manifest.with_extension("channels.json");
        // #7609 slice 7: seed first, then take the retired table out — the
        // retirement only fires once every binding is in the channels file.
        let outcome = migrate_agent_channels_if_absent(&manifest, &channels_json, globals);
        super::retire::retire_agent_listeners(&manifest, &channels_json);
        match outcome {
            Ok(Some(report)) => {
                tracing::info!(
                    assistant = %id.as_str(),
                    path = %channels_json.display(),
                    moved = %report.summary(),
                    "channel migration: seeded the assistant's channels file (#7609)",
                );
                reports.push((id.as_str().to_string(), report));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                assistant = %id.as_str(),
                error = %e,
                "channel migration: nothing moved for this assistant",
            ),
        }
    }
    reports
}

/// Append `assistant` to the `route_to` of every global channel its
/// `agent.toml` declares a legacy `[[listeners]]` binding for.
///
/// Why (#7609 slice 4): the owner's 2026-09-14 ruling retires the two-stage
/// opt-in — a global channel now names the assistants it fans out to. Leaving
/// the existing pairs on the legacy source works, but only until the
/// deprecation window closes, and asking an operator to hand-edit one entry per
/// binding is exactly the migration burden slice 2 refused to impose.
/// What: best-effort and idempotent. Every failure is logged and skipped;
/// nothing here is worth failing startup over, because dispatch source (3)
/// still handles an un-backfilled pair.
/// Test: `the_sweep_backfills_route_to_for_a_legacy_binding`.
fn backfill_assistant_routes(
    config_path: &Path,
    agent_toml: &Path,
    globals: &[Channel],
    assistant: &str,
) {
    for channel_id in legacy_binding_targets(agent_toml, globals) {
        match backfill_route_to(config_path, &channel_id, assistant) {
            Ok(true) => tracing::info!(
                channel = %channel_id,
                assistant = %assistant,
                "channel migration: backfilled `route_to` from a legacy binding (#7609)",
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(
                channel = %channel_id,
                assistant = %assistant,
                error = %e,
                "channel migration: `route_to` backfill wrote nothing",
            ),
        }
    }
}

/// The global channel ids an `agent.toml`'s legacy `[[listeners]]` bindings
/// name.
///
/// What: ids only, deduplicated, and only for globals this host actually
/// declares — a binding naming a listener that does not exist backfills
/// nothing.
///
/// #7609 review: each failure arm logs its path and error. A manifest that will
/// not read or parse disables this assistant's backfill for the life of the
/// process, and all three arms used to return an empty list with nothing said —
/// indistinguishable from an assistant that simply declares no bindings.
/// Test: `the_sweep_backfills_route_to_for_a_legacy_binding`.
fn legacy_binding_targets(agent_toml: &Path, globals: &[Channel]) -> Vec<String> {
    let warn = |stage: &str, error: &dyn std::fmt::Display| {
        tracing::warn!(
            path = %agent_toml.display(),
            %stage,
            %error,
            "channel migration: this manifest's legacy bindings could not be read; no `route_to` \
             backfill for it (#7609)",
        );
    };
    let raw = match std::fs::read_to_string(agent_toml) {
        Ok(raw) => raw,
        Err(e) => {
            warn("read", &e);
            return Vec::new();
        }
    };
    let table = match parse_table(&raw, agent_toml) {
        Ok(table) => table,
        Err(e) => {
            warn("parse", &e);
            return Vec::new();
        }
    };
    let bindings = match read_key::<AgentListenerBinding>(&table, "listeners", agent_toml) {
        Ok(Some(bindings)) => bindings,
        Ok(None) => return Vec::new(),
        Err(e) => {
            warn("listeners", &e);
            return Vec::new();
        }
    };
    let mut ids: Vec<String> = bindings
        .into_iter()
        .map(|binding| binding.name)
        .filter(|name| {
            globals
                .iter()
                .any(|c| c.scope == ChannelScope::Global && &c.id == name)
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Add `assistant` to one global channel's `route_to`, once.
///
/// Why: this is a read-modify-write of a file other processes also write, so it
/// runs through [`crate::state_writer::atomic_update`] like every other channel
/// write — the decision is re-taken under the lock, and a name already present
/// writes nothing at all.
/// What: `Ok(true)` when the file changed. `Ok(false)` when the file is absent,
/// declares no `[[channels]]` array of tables, has no channel of that id, or
/// already routes to `assistant`. Comments, key order and every unmodelled
/// table survive, because `toml_edit` edits the document rather than
/// re-serializing a parsed struct.
/// Test: `the_route_to_backfill_is_idempotent_and_keeps_comments`.
pub fn backfill_route_to(
    config_path: &Path,
    channel_id: &str,
    assistant: &str,
) -> Result<bool, ChannelMigrationError> {
    crate::state_writer::atomic_update(config_path, |existing| {
        let Some(bytes) = existing else {
            return Ok(None);
        };
        let mut doc: toml_edit::DocumentMut = String::from_utf8_lossy(bytes).parse()?;
        let Some(channels) = doc
            .get_mut("channels")
            .and_then(toml_edit::Item::as_array_of_tables_mut)
        else {
            return Ok(None);
        };
        let Some(table) = channels
            .iter_mut()
            .find(|t| t.get("id").and_then(toml_edit::Item::as_str) == Some(channel_id))
        else {
            return Ok(None);
        };
        let entry = table
            .entry("route_to")
            .or_insert_with(|| toml_edit::value(toml_edit::Array::new()));
        let Some(array) = entry.as_array_mut() else {
            // #7609 review: `route_to = "izzie"` parses but is not an array, so
            // the backfill can never write and would otherwise report `false`
            // forever with nothing said.
            tracing::warn!(
                path = %config_path.display(),
                channel = %channel_id,
                "channel migration: `route_to` is not an array of assistant names; this channel \
                 cannot be backfilled until it is fixed (#7609)",
            );
            return Ok(None);
        };
        if array.iter().any(|value| value.as_str() == Some(assistant)) {
            return Ok(None);
        }
        array.push(assistant);
        Ok(Some(doc.to_string().into_bytes()))
    })
    .map_err(|source| ChannelMigrationError::Write {
        path: config_path.display().to_string(),
        source,
    })
}

/// The subset of `bindings` that the existing channel storage would accept.
///
/// Why: `agent_channels::load_at` validates every record it reads, so writing
/// one it would reject turns a working channel view into a 500.
/// What: the binding's provider comes from the global channel it names, through
/// [`super::adapter_id`] — a Gmail listener's provider is its `connector`,
/// `gmail`, while the adapter that addresses its events is `gworkspace`. The
/// gate is [`crate::api::server::agent_channels::Binding::validate_in`], the
/// same check the read path applies, rather than a target check that agreed
/// with it only by coincidence. An OVERLAY (the global is account-wide, so the
/// binding has no destination of its own) passes that gate as of slice 4 and is
/// now written — which is what closes slice 3's open item — and carries no
/// `credential_ref`, because it has no destination to send to and the send
/// credential belongs to the global.
/// Test: `an_overlay_of_an_account_wide_global_is_now_stored`,
/// `an_unstorable_binding_is_left_in_agent_toml`.
fn storable_records(
    bindings: Vec<AgentListenerBinding>,
    globals: &[Channel],
) -> Vec<crate::api::server::agent_channels::Binding> {
    let mut records = Vec::new();
    for binding in bindings {
        let Some(global) = globals
            .iter()
            .filter(|c| c.scope == ChannelScope::Global)
            .find(|c| c.id == binding.name)
        else {
            tracing::debug!(
                binding = %binding.name,
                "channel migration: no global channel of that id; left in agent.toml"
            );
            continue;
        };
        let mut channel = Channel::from_agent_binding(binding, super::adapter_id(&global.provider));
        channel.target = global.target.clone();
        if !channel.target.is_empty() {
            channel.credential_ref = global.credential_ref.clone();
        }
        let record: crate::api::server::agent_channels::Binding = (&channel).into();
        if record.validate_in(globals).is_err() {
            tracing::info!(
                channel = %channel.id,
                provider = %channel.provider,
                "channel migration: the channel store would reject this record; left in agent.toml (#7609)"
            );
            continue;
        }
        records.push(record);
    }
    records
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod migrate_tests;
