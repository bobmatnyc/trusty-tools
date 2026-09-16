//! Deleting the retired `[[listeners]]` table from an `agent.toml` (#7609
//! slice 7).
//!
//! Why: `AgentConfig` folded that table into `channels` in memory on every
//! parse for one release, which made leaving it on disk harmless. The fold is
//! gone, so a table left behind is an INERT binding — an assistant that looks
//! configured and never wakes. The persisted move
//! ([`crate::channels::migrate::migrate_agent_channels_if_absent`]) seeds
//! `<name>.channels.json`; this is the second half that takes the legacy table
//! out once that file covers it.
//! What: [`retire_agent_listeners`] removes the table ONLY when every legacy
//! binding's `name` is already an id in the channels file. A binding that is
//! not represented — the `an_unstorable_binding_is_left_in_agent_toml` case —
//! leaves the table alone and logs which binding blocked it, because deleting
//! it would destroy configuration the migration could not carry over.
//! `agent.toml` is otherwise untouched: the comment above the removed table is
//! preserved by [`crate::channels::document`], and no other key is rewritten.
//! Test: `super::retire_tests` — the whole module.

use std::path::Path;

/// Delete `agent_toml`'s retired `[[listeners]]` table when `channels_json`
/// covers every one of its bindings.
///
/// Why best-effort: this runs inside the detached startup sweep, where one
/// unreadable manifest must not stop the others. Every arm that declines says
/// why, in the log, naming the file — an operator whose binding blocks the
/// retirement has to be able to find it.
/// What: `true` when the table was removed. `false` when there was nothing to
/// remove, when the channels file does not cover every binding, or when any
/// step failed.
/// Test: `retire_tests::a_covered_legacy_table_is_removed_with_its_comment_kept`,
/// `retire_tests::an_uncovered_binding_keeps_the_legacy_table`.
pub(crate) fn retire_agent_listeners(agent_toml: &Path, channels_json: &Path) -> bool {
    let raw = match std::fs::read_to_string(agent_toml) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            tracing::warn!(
                path = %agent_toml.display(),
                %error,
                "channel retirement: this manifest could not be read (#7609)",
            );
            return false;
        }
    };
    let names = match legacy_names(&raw) {
        Ok(names) if names.is_empty() => return false,
        Ok(names) => names,
        Err(error) => {
            tracing::warn!(
                path = %agent_toml.display(),
                %error,
                "channel retirement: this manifest's `[[listeners]]` could not be read, so it \
                 stays (#7609)",
            );
            return false;
        }
    };
    let stored = stored_ids(channels_json);
    if let Some(missing) = names.iter().find(|name| !stored.contains(*name)) {
        tracing::warn!(
            path = %agent_toml.display(),
            binding = %missing,
            channels = %channels_json.display(),
            "channel retirement: this binding is not in the assistant's channels file, so the \
             retired `[[listeners]]` table stays and its entries are INERT (#7609)",
        );
        return false;
    }
    match remove_table(agent_toml) {
        Ok(removed) => {
            if removed {
                tracing::info!(
                    path = %agent_toml.display(),
                    bindings = names.len(),
                    "channel retirement: removed the retired `[[listeners]]` table (#7609)",
                );
            }
            removed
        }
        Err(error) => {
            tracing::warn!(
                path = %agent_toml.display(),
                %error,
                "channel retirement: the retired `[[listeners]]` table could not be removed \
                 (#7609)",
            );
            false
        }
    }
}

/// The `name` of every `[[listeners]]` binding `raw` declares.
///
/// What: an absent table is an empty list. A table that will not parse is an
/// error, never an empty list — reading it as "no bindings" would let the
/// caller delete it as fully covered.
fn legacy_names(raw: &str) -> anyhow::Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        listeners: Vec<Binding>,
    }
    #[derive(serde::Deserialize)]
    struct Binding {
        name: String,
    }
    let manifest: Manifest = toml::from_str(raw)?;
    Ok(manifest
        .listeners
        .into_iter()
        .map(|binding| binding.name)
        .collect())
}

/// The channel ids `channels_json` declares, or an empty set when it is absent
/// or unreadable — either way, nothing is covered and nothing is removed.
fn stored_ids(channels_json: &Path) -> std::collections::HashSet<String> {
    #[derive(serde::Deserialize)]
    struct Record {
        id: String,
    }
    std::fs::read_to_string(channels_json)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<Record>>(&raw).ok())
        .map(|records| records.into_iter().map(|record| record.id).collect())
        .unwrap_or_default()
}

/// Publish `agent_toml` without its `[[listeners]]` table, under the file's lock.
///
/// What: the decision is re-taken from the LOCKED bytes, so a manifest edited
/// between the check above and this write is not overwritten — a table that is
/// gone by then writes nothing at all.
fn remove_table(agent_toml: &Path) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    crate::state_writer::atomic_update(agent_toml, |existing| {
        let Some(bytes) = existing else {
            return Ok(None);
        };
        let current = String::from_utf8(bytes.to_vec())
            .context("agent.toml is not valid UTF-8; nothing was changed")?;
        let mut document = current
            .parse::<toml_edit::DocumentMut>()
            .context("agent.toml is not an editable document; nothing was changed")?;
        if !crate::channels::document::remove_preserving_comments(&mut document, "listeners") {
            return Ok(None);
        }
        Ok(Some(document.to_string().into_bytes()))
    })
}

#[cfg(test)]
#[path = "retire_tests.rs"]
mod retire_tests;
