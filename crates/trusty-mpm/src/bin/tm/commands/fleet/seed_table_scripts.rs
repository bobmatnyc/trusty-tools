//! The `scripts/` rows `tm fleet init` compiles in (split from seed_table.rs, #10571).
//!
//! Why: the Architect prototype re-sync adds about 110 more script rows; at
//! five lines per `embedded!` row seed_table.rs would pass the 500-SLOC cap.
//! What: [`SCRIPTS`], one `script!` row per line, in write order after
//! `super::EMBEDDED`. Each row ships `src/assets/architect/scripts/<name>` to
//! `scripts/<name>`. `scripts/architect_bundle_meta.py history` reads this file.
//! Test: `every_script_asset_is_seeded`,
//! `every_script_a_seeded_script_loads_by_path_is_seeded`.

use super::Embedded;

/// The script files compiled in, in write order, after `super::EMBEDDED`.
pub(super) const SCRIPTS: &[Embedded] = &[
    script!("fleet-poll.py", true),
    // #8891: fleet-poll.py loads this sibling by path; it must ship beside it.
    script!("fleet-classify.py", false),
    // #10571 Phase 2: fleet-poll.py loads these split modules by path.
    script!("fleet_poll_cfg.py", false),
    script!("fleet_poll_io.py", false),
    script!("fleet_poll_alerts.py", false),
    script!("fleet_poll_wake.py", false),
    script!("fleet_poll_watchdog.py", false),
    script!("fleet_poll_selfrefresh.py", false),
    // PR-B2: the dep_degraded step fleet-poll.py loads.
    script!("fleet_poll_deps.py", false),
    // #10857: fleet-classify.py loads this sibling by path (host-alert dedupe, hold gating).
    script!("fleet_alert_dedupe.py", false),
    script!("input-state.py", true),
    script!("quiet-sessions.py", false),
    script!("self-ctx.py", false),
    // See #10161: the poller's alert triage and Architect watchdog (P1b-0).
    script!("alert_triage.py", false),
    script!("architect-watchdog.py", false),
    // See #10161: PM intake, its digest and the Notification hook pair (P2).
    script!("pm_intake.py", false),
    // #11101: the Task-ID tag grammar pm_intake.py and datg_task.py import,
    // and the task pulse that writes the ledger `pulse` op.
    script!("task_tag.py", false),
    script!("task_pulse.py", true),
    script!("task_pulse_io.py", false),
    script!("pm-digest.py", true),
    script!("pm-notify.sh", true),
    script!("notify-supervisor.sh", true),
    // See #10161: the Intent Ledger set (P3). datg.py imports its four
    // datg_* modules from its own directory, so all five ship together.
    script!("datg.py", true),
    script!("datg_store.py", false),
    script!("datg_ops.py", false),
    script!("datg_log.py", false),
    script!("datg_read.py", false),
    // #10579: `datg.py ready`, the clear-readiness check.
    script!("datg_ready.py", false),
    // #10800: the G1-G3 guards datg.py, datg_ops.py and datg_read.py import.
    script!("datg_guards.py", false),
    // #11101: the Task check and WIP cap datg_ops.py and datg_read.py import.
    script!("datg_task.py", false),
    script!("datg-hook.sh", true),
    script!("ctx-watch.py", false),
    script!("il-cron.py", false),
    script!("self-clear.sh", true),
    script!("state-archive.py", true),
    script!("architect-idle.py", true),
    // See #10161: the dashboard store set (P4a-1). Each module imports its
    // siblings from its own directory, and dashboard_schema.py reads
    // `schemas/dashboard/` beside `scripts/`, so all of them ship together.
    script!("dashboard_store.py", true),
    script!("dashboard_jsonl.py", false),
    script!("dashboard_schema.py", false),
    script!("dashboard_derive.py", false),
    script!("dashboard_ids.py", false),
    script!("dashboard_qual.py", false),
    script!("owner_answers.py", false),
    script!("dashboard-db.py", true),
    script!("il-archive.py", false),
    script!("stale-tasks.py", true),
    // #10896: applies the owner's watch_changes requests through `tm fleet project`.
    script!("watch-apply.py", true),
    // See #10161: the dashboard server (D3b), split into a facade and three
    // modules it imports by name, and the read-only views it serves. The page,
    // the Live view and the reader libraries sit where the server reads them.
    script!("dashboard-serve.py", true),
    script!("dashboard_http.py", false),
    script!("dashboard_page.py", false),
    script!("dashboard_docs.py", false),
    script!("dashboard_il.py", false),
    script!("dashboard_media.py", false),
    script!("dashboard_viz.py", false),
    script!("dashboard_harness.py", false),
    // #10571: the P1 routes module, the harness split, and the siblings the
    // server and the pending-answers hook import by name or run by path.
    script!("dashboard_routes.py", false),
    script!("dashboard_fleet.py", false),
    script!("dashboard_harness_base.py", false),
    // #11065: the pane-tail masker the session-tail provider (phase 2) imports.
    script!("dashboard_mask.py", false),
    // #11065: the ephemeral pane-tail provider (phase 2); masks through dashboard_mask.
    script!("dashboard_pane_tail.py", false),
    script!("dashboard_attach.py", false),
    script!("dashboard_projects.py", false),
    // #10606: TOML on Python < 3.11; dashboard_projects imports it.
    script!("dashboard_toml.py", false),
    // #10896: the Projects view's watch state; dashboard_projects imports it.
    script!("dashboard_watch.py", false),
    // #10350: the Projects view's Secrets list (names only); dashboard-serve imports it.
    script!("dashboard_keynames.py", false),
    script!("dashboard_archctx.py", false),
    script!("brief_ids.py", false),
    script!("brief-id.py", true),
    // self-clear.sh runs this by path.
    script!("turn-active.py", true),
    script!("pm_cursor.py", false),
    script!("pending-answers.py", true),
    script!("pending_hook_dedupe.py", false),
    // #11064: the technical level reader; pending_hook_dedupe.py and dashboard_store.py import it.
    script!("technical_level.py", false),
    // #11064: the warn-only level lint; dashboard-db.py imports it on every item write.
    script!("level_lint.py", false),
    script!("pending-answers-hook.sh", true),
    script!("start-fleet-poll.sh", true),
    // #9997: the two-step relay send and the double-post guard the manual names.
    script!("relay.sh", true),
    script!("posted.sh", true),
    // #10780: relay.sh runs this sibling by path to check an owner label's citation.
    script!("relay_provenance.py", false),
    // #10960: the supersession check relay_provenance.py imports at load.
    script!("relay_supersession.py", false),
    // posted.sh runs this sibling by path.
    script!("relay-marker.py", false),
    // See #10161: the `local/architect.json` loader; a module, not a command.
    script!("architect_config.py", false),
    // See #10161 D3c: the event-extractor agent's claim checker and its
    // input module, imported by name from the same directory.
    script!("verify-claim.py", true),
    script!("verify_claim_input.py", false),
    // #10571 S1: the usage ledger, token watcher, effort governor and context bar;
    // usage.py and token_watch.py import their split halves by name, usage-probe.sh
    // sources rc-lib.sh.
    script!("usage.py", true),
    script!("usage_calc.py", false),
    script!("token_watch.py", false),
    script!("token_watch_rules.py", false),
    script!("effort-governor.py", true),
    script!("ctx_bar.py", false),
    script!("usage-probe.sh", true),
    script!("effort-send.sh", true),
    script!("statusline-tee.sh", true),
    script!("rc-lib.sh", false),
    // #10571 S2: the poller helpers and the #11369 cadence scripts. architect-start.sh and
    // poll-supervise.sh read poll_interval_s through architect_config.py; relaunch_brief.py
    // is the brief session-relaunch.sh --il renders. rc-lib.sh, which the rc set sources,
    // ships with S1.
    script!("architect-start.sh", true),
    script!("poll-supervise.sh", true),
    script!("poll-reset-wake.py", true),
    script!("rc-restore.sh", true),
    script!("rc-disconnect.sh", true),
    script!("session-relaunch.sh", true),
    script!("relaunch_brief.py", false),
    script!("fleet-restart.sh", true),
    script!("ids36.py", false),
    script!("il_timeline.py", false),
    script!("auto_sleep.py", false),
    script!("user_state.py", false),
    script!("morning_brief.py", false),
    script!("dream_catchup.py", false),
    script!("dashboard_updates.py", false),
    script!("il_remind.py", false),
    script!("dashboard_feed.py", false),
];
