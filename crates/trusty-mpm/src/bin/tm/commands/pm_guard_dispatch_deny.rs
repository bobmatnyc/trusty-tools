//! The deny text of the ADR-0048 / #4480 dispatch guard (#8257).
//!
//! Why: split out of `pm_guard_dispatch` when #8257 made the text name each
//! blocking record, because that file sat over the 500-SLOC cap. A deny that
//! names an agent TYPE and nothing else left a PM facing a finished agent's
//! stale record with no id to act on and no verb to act with; the text now
//! carries, per record, the agent type, agent id (delegation id when there is
//! none), owning session, age, and the exact command that clears it.
//! What: the two "a writer holds this checkout" messages and the per-record
//! paragraph both append.
//! Test: the `#[cfg(test)]` suite below.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::daemon::services::delegation_records::DelegationRecordView;

/// Build the deny message for a blocked concurrent dispatch.
///
/// Why: a bare "denied" leaves the model guessing and it retries the identical
/// call. The text has to name what is already running and say why git will not
/// catch the collision (the reader's prior is that it would). It offers exactly
/// ONE remedy — declare isolation — because that is the only one that always
/// works: `RUNNING_STALE_AFTER_SECS` is six hours, so a crashed subagent that
/// never emits `SubagentStop` holds its directory for that whole window, and
/// "wait for it to report back" would be advice to wait for something that may
/// never happen. Built per call rather than kept as a constant because naming
/// the actual sibling agent is most of its value.
///
/// #5649: the incident showed that single remedy can itself be unavailable, so
/// the message now names a second one — serialize. Serializing and waiting are
/// not the same offer: serializing means dispatching one file-mutating agent at
/// a time GOING FORWARD, which needs nothing from the agent already running, so
/// it always works. Waiting blocks on an agent that may never return, and stays
/// excluded for exactly the reason above.
/// What: a single-paragraph `permissionDecisionReason`; the caller appends
/// [`blocking_records`].
/// Test: `denies_a_second_concurrent_unisolated_engineer`,
/// `deny_reason_offers_only_remedies_that_always_work`.
pub(crate) fn deny_reason(agent: &str, cwd: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let running = names.join(", ");
    format!(
        "Concurrent shared-worktree dispatch denied (#4480): {running} is already running in \
         {} without a worktree of its own — possibly dispatched by a different session standing \
         in the same directory (ADR-0048) — and this {agent} dispatch would put a second \
         file-mutating agent on the same git HEAD. Git does not catch this — a `git checkout -b` \
         refuses only when a tracked file differs between both branches AND has an uncommitted \
         change, so untracked files and edits the two branches agree on transfer onto the wrong \
         branch silently, with no error at any step. Re-dispatch this agent with \
         `isolation: \"worktree\"` so it gets its own tree. If isolation is unavailable here, \
         serialize instead: dispatch one file-mutating agent at a time from now on. Do not \
         hand-roll a `git worktree add` in the prompt — this guard reads the declared \
         isolation parameter, never the prompt, so a self-made worktree still counts as \
         sharing this HEAD (#5649).",
        cwd.display()
    )
}

/// Build the deny message for a granted dispatch the checkout is not free for.
///
/// Why: [`deny_reason`]'s remedy is "re-dispatch with `isolation: \"worktree\"`",
/// which reads as self-contradictory here — the guard had already built exactly
/// that rewrite and then declined to emit it. The reason this path denies is
/// different from #4480's: the isolation is available, but the guard cannot rely
/// on the harness applying its `updatedInput` rewrite, and while another writer
/// holds the checkout an unapplied rewrite is the reported harm rather than a
/// hypothetical one.
///
/// The reorder this text belongs to also widens what a stale record blocks. A
/// record nothing ever closed used to block only an unisolated dispatch;
/// it now blocks every dispatch of a writer — and `Unknown` is a writer — from
/// this checkout, for the six hours of `RUNNING_STALE_AFTER_SECS`. The two
/// operator escape hatches still lift it, so that is friction rather than a
/// lockout, and the message names the possibility so a reader can recognise it.
/// What: names ADR-0048, the sibling the daemon reports, the directory, and the
/// three ways forward — dispatch with explicit isolation, serialize, or clear a
/// record believed stale with the command [`blocking_records`] appends (#8257).
/// Test: `granted_deny_reason_does_not_offer_the_isolation_it_already_built`.
pub(crate) fn granted_deny_reason(agent: &str, cwd: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "Dispatch denied in a shared main checkout (ADR-0048): {} is a project's main checkout, \
         and the daemon's delegation records name {} as running there with no worktree of its \
         own — possibly dispatched by a different session standing in the same directory. This \
         {agent} dispatch was granted a worktree of its own, but that grant is a rewrite of the \
         dispatch's arguments and this guard cannot confirm the harness applied it; if it did \
         not, a second file-mutating agent joins the same git HEAD, which is the reported \
         failure — a commit landing on another workstream's branch, with no error at any step. \
         Re-issue this dispatch with `isolation: \"worktree\"` declared explicitly, which needs \
         no rewrite to be applied. If isolation is unavailable here, serialize instead: dispatch \
         one file-mutating agent at a time. If a record below is stale — the agent finished \
         without its stop signal reaching the daemon — clear it with its command rather than \
         retrying.",
        cwd.display(),
        names.join(", ")
    )
}

/// The per-record paragraph both writer denies end with (#8257).
///
/// Why: see the module doc. The command is the daemon's own
/// [`DelegationRecordView::repair_command`], so the text cannot name a verb
/// the repair route does not answer.
/// What: one clause per record — agent type, agent id or delegation id, owning
/// session by its caller-safe label (never its UUID, #8257 owner ruling), age,
/// clearing command — then the repair's refusal rule and the
/// `--list` command. With no records (a daemon older than #8257) only the
/// `--list` pointer.
/// Test: `blocking_records_name_the_id_age_and_command_8257`,
/// `no_writer_deny_names_the_owner_uuid_8257`.
pub(crate) fn blocking_records(cwd: &Path, records: &[DelegationRecordView]) -> String {
    let list = format!("`tm repair delegation --list {}`", cwd.display());
    if records.is_empty() {
        return format!(" List the records occupying it with {list} (#8257).");
    }
    let clauses: Vec<String> = records
        .iter()
        .map(|r| {
            let id = match r.agent_id.as_deref() {
                Some(agent_id) => format!("agent id {agent_id}"),
                None => format!("no agent id, delegation id {}", r.delegation_id),
            };
            // #8257 owner ruling: `owner` is the daemon's caller-safe label.
            format!(
                "{} ({id}; owned by {}; age {}) — clear with `{}`",
                r.agent,
                r.owner,
                format_age(r.age_secs),
                r.repair_command
            )
        })
        .collect();
    format!(
        // #8257 owner ruling: the owning session may clear its own record, so
        // the rule names that exception rather than read as an absolute refusal.
        " Blocking record(s): {}. The repair refuses while a live process holds the agent's \
         tree, or while its session is live, no stop has arrived and the record is under 6 h \
         old — unless that owning session runs the repair itself; {list} lists every record \
         here (#8257).",
        clauses.join("; ")
    )
}

/// The record views a shared-tree answer carries, if any (#8257).
///
/// What: an absent or unparseable `records` field is an empty list — the text
/// is advisory, and the verdict is decided by the names alone.
pub(crate) fn records_in(body: &Value) -> Vec<DelegationRecordView> {
    body.get("records")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// `42m`, `3h07m` — an age a reader compares against the 6 h threshold.
fn format_age(secs: i64) -> String {
    let mins = secs.max(0) / 60;
    if mins < 60 {
        format!("{mins}m")
    } else {
        format!("{}h{:02}m", mins / 60, mins % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent::DelegationStatus;

    fn view(agent_id: Option<&str>, age_secs: i64) -> DelegationRecordView {
        let delegation_id = "64237d1c-0aa4-4090-bd14-d3c273da7e95".to_string();
        DelegationRecordView {
            repair_command: match agent_id {
                Some(id) => format!("tm repair delegation {id}"),
                None => format!("tm repair delegation --delegation-id {delegation_id}"),
            },
            delegation_id,
            agent: "version-control".to_string(),
            agent_id: agent_id.map(str::to_string),
            owner: "session `tm-trusty-tools`".to_string(),
            status: DelegationStatus::Running,
            age_secs,
            cwd: None,
            worktree_path: None,
            blocks_dispatch: true,
        }
    }

    // #8257: the reported deny named only an agent TYPE. Each record must now
    // carry its id, owner, age and the exact command that clears it.
    #[test]
    fn blocking_records_name_the_id_age_and_command_8257() {
        let text = blocking_records(
            Path::new("/repo"),
            &[view(Some("a1b2c3d4e5f6a7b8c"), 2520), view(None, 11_220)],
        );
        assert!(text.contains("agent id a1b2c3d4e5f6a7b8c"), "{text}");
        assert!(
            text.contains("`tm repair delegation a1b2c3d4e5f6a7b8c`"),
            "{text}"
        );
        assert!(text.contains("age 42m"), "{text}");
        // The id-less record is addressed by its delegation id.
        assert!(
            text.contains("no agent id, delegation id 64237d1c-0aa4-4090-bd14-d3c273da7e95"),
            "{text}"
        );
        assert!(
            text.contains(
                "`tm repair delegation --delegation-id 64237d1c-0aa4-4090-bd14-d3c273da7e95`"
            ),
            "{text}"
        );
        assert!(text.contains("age 3h07m"), "{text}");
        assert!(
            text.contains("owned by session `tm-trusty-tools`"),
            "{text}"
        );
        assert!(
            text.contains("`tm repair delegation --list /repo`"),
            "{text}"
        );
        // #8257 owner ruling: the owning session is told it may clear its own.
        assert!(
            text.contains("unless that owning session runs the repair itself"),
            "{text}"
        );
    }

    // #8257 owner ruling: a denied caller must never read the owner's UUID,
    // which it could replay as `CLAUDE_CODE_SESSION_ID`. Both writer denies,
    // over views the daemon itself projects, for a named and an unknown owner.
    #[test]
    fn no_writer_deny_names_the_owner_uuid_8257() {
        use trusty_mpm::core::agent::Delegation;
        use trusty_mpm::core::session::{ControlModel, Session, SessionId};
        use trusty_mpm::daemon::state::DaemonState;

        let state = DaemonState::new();
        let named = SessionId::new();
        let unknown = SessionId::new();
        state.register_session(Session::new(
            named,
            "/repo",
            ControlModel::Tmux,
            Some(Path::new("/repo")),
        ));
        let now = chrono::Utc::now();
        let views: Vec<_> = [named, unknown]
            .into_iter()
            .map(|s| {
                let d = Delegation::observed(s, "version-control", "task", None);
                DelegationRecordView::of(&state, &d, now, true)
            })
            .collect();
        let live = ["version-control".to_string()];
        let cwd = Path::new("/repo");

        for text in [
            deny_reason("rust-engineer", cwd, &live) + &blocking_records(cwd, &views),
            granted_deny_reason("rust-engineer", cwd, &live) + &blocking_records(cwd, &views),
        ] {
            for s in [named, unknown] {
                for form in [s.0.hyphenated().to_string(), s.0.simple().to_string()] {
                    assert!(!text.contains(&form), "owner UUID {form} leaked: {text}");
                }
            }
            assert!(text.contains("owned by session `tm-repo`"), "{text}");
        }
    }

    #[test]
    fn blocking_records_points_at_the_listing_when_the_daemon_sent_none_8257() {
        let text = blocking_records(Path::new("/repo"), &[]);
        assert!(
            text.contains("`tm repair delegation --list /repo`"),
            "{text}"
        );
    }

    #[test]
    fn records_in_reads_the_wire_and_tolerates_its_absence_8257() {
        let body = serde_json::json!({ "records": [view(None, 60)] });
        assert_eq!(records_in(&body), vec![view(None, 60)]);
        assert!(records_in(&serde_json::json!({ "agents": [] })).is_empty());
    }

    #[test]
    fn deny_reason_offers_only_remedies_that_always_work() {
        // `RUNNING_STALE_AFTER_SECS` is six hours, so a crashed subagent that
        // never emits `SubagentStop` holds its directory for that whole window.
        // Telling the PM to wait for it would be advice to wait for something
        // that may never arrive; declaring isolation works immediately.
        //
        // #5649: serialize joins isolation as a second offered remedy, because
        // the incident showed isolation can itself be unavailable. Serializing
        // constrains only FUTURE dispatches and so needs nothing from the agent
        // already running — waiting stays banned for the reason above.
        let reason = deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            &["python-engineer".to_string()],
        );
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(
            reason.contains("serialize"),
            "the deny must offer the serialize fallback for when isolation is unavailable: \
             {reason}"
        );
        for banned in ["wait for", "wait on", "wait until", "waiting for"] {
            assert!(
                !reason.contains(banned),
                "the deny must not advise waiting on an agent that may never report \
                 (found {banned:?}): {reason}"
            );
        }
    }

    #[test]
    fn granted_deny_reason_does_not_offer_the_isolation_it_already_built() {
        // #5769: this path denies a dispatch the guard had ALREADY rewritten to
        // carry `isolation: "worktree"`. Reusing #4480's text told the reader to
        // do the thing the guard had just done and declined to emit, which reads
        // as arbitrary and gets retried identically.
        let reason = granted_deny_reason(
            "rust-engineer",
            Path::new("/repo/main"),
            &["python-engineer".to_string(), "python-engineer".to_string()],
        );
        assert!(reason.contains("ADR-0048"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        // The sibling is named once, and attributed rather than asserted.
        assert_eq!(reason.matches("python-engineer").count(), 1, "{reason}");
        assert!(
            reason.contains("the daemon's delegation records name"),
            "{reason}"
        );
        // It must say WHY a grant is not enough here — the rewrite may not be
        // applied — rather than offering the grant back as the remedy.
        assert!(
            reason.contains("cannot confirm the harness applied it"),
            "{reason}"
        );
        assert!(reason.contains("declared explicitly"), "{reason}");
        assert!(reason.contains("serialize"), "{reason}");
        // A stale record is the friction case the reorder widened; naming it is
        // what lets a reader recognise it instead of retrying.
        assert!(reason.contains("stale"), "{reason}");
        for banned in ["wait for", "wait on", "wait until", "waiting for"] {
            assert!(!reason.contains(banned), "found {banned:?}: {reason}");
        }
    }

    #[test]
    fn deny_reason_dedupes_concurrent_siblings() {
        // Two concurrent `rust-engineer`s are the realistic shape; the message
        // must read as one name, not a repeated list.
        let reason = deny_reason(
            "rust-engineer",
            Path::new("/repo"),
            &["rust-engineer".to_string(), "rust-engineer".to_string()],
        );
        assert_eq!(
            reason.matches("rust-engineer is already").count(),
            1,
            "{reason}"
        );
    }
}
