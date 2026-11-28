//! `dashboard-db.py`'s stderr warnings: never a refusal (#12029 P2g).
//!
//! Why: a question or action with no task, and text above the owner's
//! technical level, are worth a nudge but must never cost the write.
//! What: [`warn_no_task`] (the merged doc on an `update`) and [`warn_level`]
//! (`level_lint` on `title`, `detail` and each option `desc` at the level in
//! `meta/settings`; any failure is one "level lint skipped" line).
//! Test: `dashboard_db_recordings_replay_clean_against_the_port` (`warnings`),
//! `a_level_read_failure_is_a_warning_and_the_write_lands`.

use super::{Ctx, R};
use crate::dashboard::store::py::{Options as StoreOptions, PyStore, PyStoreError};
use crate::level_lint;
use crate::pyjson_load::PyValue;
use crate::pystr::{py_repr, strip};
use crate::pyval::{get, py_str};
use crate::technical_level as tl;

/// `warn_no_task(store, op, doc_id, data)`.
pub(crate) fn warn_no_task(ctx: &Ctx, op: &str, id: &PyValue, data: &PyValue) -> R<()> {
    if !matches!(data, PyValue::Dict(_)) {
        return Ok(());
    }
    let mut d = data.clone();
    if op == "update"
        && let Some(m) = ctx.merged_over("items", id, data)?
    {
        d = m;
    }
    let kind = get(&d, "kind");
    let qa = ["question", "action"]
        .iter()
        .any(|k| crate::pyval::is_str(kind, k));
    if qa && strip(&super::text_or(get(&d, "task"))).is_empty() {
        ctx.warn(&format!(
            "warning: item {} (kind {}) has no `task`; a decision names its T and a blocker names the T it unblocks",
            py_str(id),
            kind.map_or(String::new(), py_str)
        ));
    }
    Ok(())
}

/// `repr(e)` of a store exception, as `{e!r}` prints it.
fn exc_repr(e: &PyStoreError) -> String {
    match e {
        PyStoreError::Os { class, errno, msg } => {
            let strerror = msg
                .split_once("] ")
                .map_or(msg.as_str(), |(_, r)| r)
                .rsplit_once(": ")
                .map_or(msg.as_str(), |(head, _)| head);
            format!("{class}({errno}, {})", py_repr(strerror))
        }
        other => format!("{}({})", other.class(), py_repr(&other.to_string())),
    }
}

/// `technical_level.read(root)`: `meta/settings.technical_level` of a fresh
/// store at `root` (the default level when the doc is missing).
fn stored_level(ctx: &Ctx) -> Result<&'static str, PyStoreError> {
    let store = PyStore::new(
        &ctx.store.root,
        StoreOptions::default(),
        ctx.o.env,
        &ctx.o.cwd,
        ctx.schema_dir(),
        ctx.o.clock.clone(),
    )?;
    let meta = PyValue::Str("meta".into());
    match store.get(&meta, &PyValue::Str("settings".into())) {
        Ok(doc) => Ok(match get(&doc.data, "technical_level") {
            Some(PyValue::Str(l)) => tl::normalize(l),
            _ => tl::DEFAULT,
        }),
        Err(PyStoreError::NotFound(_)) => Ok(tl::DEFAULT),
        Err(e) => Err(e),
    }
}

/// Why: the owner reads the page at their technical level (ar-T16).
/// What: `warn_level(store, doc_id, data)`: one `warning: level_lint [...]`
/// line per finding in `title`, `detail` and each dict option's `desc`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`warnings`).
pub(crate) fn warn_level(ctx: &Ctx, id: &str, data: &PyValue) {
    if !matches!(data, PyValue::Dict(_)) {
        return;
    }
    let level = match stored_level(ctx) {
        Ok(l) => l,
        Err(e) => {
            ctx.warn(&format!("warning: level lint skipped: {}", exc_repr(&e)));
            return;
        }
    };
    let mut texts: Vec<(String, Option<&PyValue>)> = ["title", "detail"]
        .into_iter()
        .map(|f| (f.to_owned(), get(data, f)))
        .collect();
    if let Some(PyValue::List(opts)) = get(data, "options") {
        for (i, o) in opts.iter().enumerate() {
            if matches!(o, PyValue::Dict(_)) {
                texts.push((format!("options[{i}].desc"), get(o, "desc")));
            }
        }
    }
    for (field, text) in texts {
        let Some(PyValue::Str(text)) =
            text.filter(|t| matches!(t, PyValue::Str(x) if !x.is_empty()))
        else {
            continue;
        };
        let found = level_lint::lint(text, level);
        for ln in level_lint::format_warnings(&found, level, &format!("item {id} {field}")) {
            ctx.warn(&format!("warning: {ln}"));
        }
    }
}
