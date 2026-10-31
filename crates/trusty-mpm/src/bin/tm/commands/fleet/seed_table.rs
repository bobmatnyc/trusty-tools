//! The rows `tm fleet init` compiles in (split from seed.rs, #10571).
//!
//! Why: seed.rs passed the 500-SLOC cap once the P1 dashboard set landed; the
//! table is data, the write and refresh logic stays in seed.rs.
//! What: [`EMBEDDED`], every compiled-in seed row in write order, and the four
//! row macros that build it from a path under `src/assets/architect/`.
//! `scripts/architect_bundle_meta.py history` reads this file to list the rows.
//! Test: `every_script_asset_is_seeded`, `every_dashboard_asset_is_seeded`,
//! `every_schema_asset_is_seeded`, `every_template_asset_is_seeded`.

/// One embedded [`files`] row: destination, bytes, executable.
pub(super) type Embedded = (&'static str, &'static str, bool);

/// Build one [`EMBEDDED`] row from a path under `src/assets/architect/`.
macro_rules! embedded {
    ($dest:literal, $asset:literal, $exec:literal) => {
        (
            $dest,
            include_str!(concat!("../../../../assets/architect/", $asset)),
            $exec,
        )
    };
}

/// Build one [`EMBEDDED`] row for a template seeded at the same relative path
/// under the Architect directory as under `src/assets/architect/templates/`.
macro_rules! template {
    ($rel:literal) => {
        (
            $rel,
            include_str!(concat!("../../../../assets/architect/templates/", $rel)),
            false,
        )
    };
}

/// Build one [`EMBEDDED`] row for a dashboard schema seeded at
/// `schemas/dashboard/<name>`, its path under `src/assets/architect/` (#10161).
macro_rules! schema {
    ($name:literal) => {
        (
            concat!("schemas/dashboard/", $name),
            include_str!(concat!(
                "../../../../assets/architect/schemas/dashboard/",
                $name
            )),
            false,
        )
    };
}

/// Build one [`EMBEDDED`] row for a dashboard page file seeded at
/// `templates/dashboard/<rel>`, its path under `src/assets/architect/dashboard/`
/// (#10161).
macro_rules! page {
    ($rel:literal) => {
        (
            concat!("templates/dashboard/", $rel),
            include_str!(concat!("../../../../assets/architect/dashboard/", $rel)),
            false,
        )
    };
}

/// The files still compiled in, in write order, after `CLAUDE.md`.
pub(super) const EMBEDDED: &[Embedded] = &[
    embedded!(".gitignore", "templates/gitignore", false),
    // #9997: the records layout is `local/`, never the old directory.
    template!("local/state.md"),
    template!("local/actions.md"),
    template!("local/goals.md"),
    template!("local/user.md"),
    template!("local/instance.md"),
    template!("local/projects/_template.md"),
    template!("local/owner/README.md"),
    template!("local/owner/business.md"),
    template!("local/owner/process.md"),
    template!("local/owner/decision-making.md"),
    template!("local/owner/risk-tolerance.md"),
    template!("local/owner/detail-and-communication.md"),
    template!("local/owner/contradictions.md"),
    template!("local/owner/dream-log.md"),
    template!("local/mandates/README.md"),
    template!("local/mandates/example.toml"),
    // See #10161: the instance config `scripts/architect_config.py` reads.
    template!("local/architect.json"),
    embedded!("scripts/fleet-poll.py", "scripts/fleet-poll.py", true),
    // #8891: fleet-poll.py loads this sibling by path; it must ship beside it.
    embedded!(
        "scripts/fleet-classify.py",
        "scripts/fleet-classify.py",
        false
    ),
    // #10571 Phase 2: fleet-poll.py loads these split modules by path.
    embedded!(
        "scripts/fleet_poll_cfg.py",
        "scripts/fleet_poll_cfg.py",
        false
    ),
    embedded!(
        "scripts/fleet_poll_io.py",
        "scripts/fleet_poll_io.py",
        false
    ),
    embedded!(
        "scripts/fleet_poll_alerts.py",
        "scripts/fleet_poll_alerts.py",
        false
    ),
    embedded!(
        "scripts/fleet_poll_wake.py",
        "scripts/fleet_poll_wake.py",
        false
    ),
    embedded!(
        "scripts/fleet_poll_watchdog.py",
        "scripts/fleet_poll_watchdog.py",
        false
    ),
    embedded!(
        "scripts/fleet_poll_selfrefresh.py",
        "scripts/fleet_poll_selfrefresh.py",
        false
    ),
    embedded!("scripts/input-state.py", "scripts/input-state.py", true),
    embedded!(
        "scripts/quiet-sessions.py",
        "scripts/quiet-sessions.py",
        false
    ),
    embedded!("scripts/self-ctx.py", "scripts/self-ctx.py", false),
    // See #10161: the poller's alert triage and Architect watchdog (P1b-0).
    embedded!("scripts/alert_triage.py", "scripts/alert_triage.py", false),
    embedded!(
        "scripts/architect-watchdog.py",
        "scripts/architect-watchdog.py",
        false
    ),
    // See #10161: PM intake, its digest and the Notification hook pair (P2).
    embedded!("scripts/pm_intake.py", "scripts/pm_intake.py", false),
    embedded!("scripts/pm-digest.py", "scripts/pm-digest.py", true),
    embedded!("scripts/pm-notify.sh", "scripts/pm-notify.sh", true),
    embedded!(
        "scripts/notify-supervisor.sh",
        "scripts/notify-supervisor.sh",
        true
    ),
    // See #10161: the Intent Ledger set (P3). datg.py imports its four
    // datg_* modules from its own directory, so all five ship together.
    embedded!("scripts/datg.py", "scripts/datg.py", true),
    embedded!("scripts/datg_store.py", "scripts/datg_store.py", false),
    embedded!("scripts/datg_ops.py", "scripts/datg_ops.py", false),
    embedded!("scripts/datg_log.py", "scripts/datg_log.py", false),
    embedded!("scripts/datg_read.py", "scripts/datg_read.py", false),
    // #10579: `datg.py ready`, the clear-readiness check.
    embedded!("scripts/datg_ready.py", "scripts/datg_ready.py", false),
    embedded!("scripts/datg-hook.sh", "scripts/datg-hook.sh", true),
    embedded!("scripts/ctx-watch.py", "scripts/ctx-watch.py", false),
    embedded!("scripts/il-cron.py", "scripts/il-cron.py", false),
    embedded!("scripts/self-clear.sh", "scripts/self-clear.sh", true),
    embedded!("scripts/state-archive.py", "scripts/state-archive.py", true),
    embedded!(
        "scripts/architect-idle.py",
        "scripts/architect-idle.py",
        true
    ),
    // See #10161: the dashboard store set (P4a-1). Each module imports its
    // siblings from its own directory, and dashboard_schema.py reads
    // `schemas/dashboard/` beside `scripts/`, so all of them ship together.
    embedded!(
        "scripts/dashboard_store.py",
        "scripts/dashboard_store.py",
        true
    ),
    embedded!(
        "scripts/dashboard_jsonl.py",
        "scripts/dashboard_jsonl.py",
        false
    ),
    embedded!(
        "scripts/dashboard_schema.py",
        "scripts/dashboard_schema.py",
        false
    ),
    embedded!(
        "scripts/dashboard_derive.py",
        "scripts/dashboard_derive.py",
        false
    ),
    embedded!(
        "scripts/dashboard_ids.py",
        "scripts/dashboard_ids.py",
        false
    ),
    embedded!(
        "scripts/dashboard_qual.py",
        "scripts/dashboard_qual.py",
        false
    ),
    embedded!(
        "scripts/owner_answers.py",
        "scripts/owner_answers.py",
        false
    ),
    embedded!("scripts/dashboard-db.py", "scripts/dashboard-db.py", true),
    embedded!("scripts/il-archive.py", "scripts/il-archive.py", false),
    embedded!("scripts/stale-tasks.py", "scripts/stale-tasks.py", true),
    // See #10161: the dashboard server (D3b), split into a facade and three
    // modules it imports by name, and the read-only views it serves. The page,
    // the Live view and the reader libraries sit where the server reads them.
    embedded!(
        "scripts/dashboard-serve.py",
        "scripts/dashboard-serve.py",
        true
    ),
    embedded!(
        "scripts/dashboard_http.py",
        "scripts/dashboard_http.py",
        false
    ),
    embedded!(
        "scripts/dashboard_page.py",
        "scripts/dashboard_page.py",
        false
    ),
    embedded!(
        "scripts/dashboard_docs.py",
        "scripts/dashboard_docs.py",
        false
    ),
    embedded!("scripts/dashboard_il.py", "scripts/dashboard_il.py", false),
    embedded!(
        "scripts/dashboard_media.py",
        "scripts/dashboard_media.py",
        false
    ),
    embedded!(
        "scripts/dashboard_viz.py",
        "scripts/dashboard_viz.py",
        false
    ),
    embedded!(
        "scripts/dashboard_harness.py",
        "scripts/dashboard_harness.py",
        false
    ),
    // #10571: the P1 routes module, the harness split, and the siblings the
    // server and the pending-answers hook import by name or run by path.
    embedded!(
        "scripts/dashboard_routes.py",
        "scripts/dashboard_routes.py",
        false
    ),
    embedded!(
        "scripts/dashboard_fleet.py",
        "scripts/dashboard_fleet.py",
        false
    ),
    embedded!(
        "scripts/dashboard_harness_base.py",
        "scripts/dashboard_harness_base.py",
        false
    ),
    embedded!(
        "scripts/dashboard_attach.py",
        "scripts/dashboard_attach.py",
        false
    ),
    embedded!(
        "scripts/dashboard_projects.py",
        "scripts/dashboard_projects.py",
        false
    ),
    embedded!(
        "scripts/dashboard_archctx.py",
        "scripts/dashboard_archctx.py",
        false
    ),
    embedded!("scripts/brief_ids.py", "scripts/brief_ids.py", false),
    embedded!("scripts/brief-id.py", "scripts/brief-id.py", true),
    // self-clear.sh runs this by path.
    embedded!("scripts/turn-active.py", "scripts/turn-active.py", true),
    embedded!("scripts/pm_cursor.py", "scripts/pm_cursor.py", false),
    embedded!(
        "scripts/pending-answers.py",
        "scripts/pending-answers.py",
        true
    ),
    embedded!(
        "scripts/pending_hook_dedupe.py",
        "scripts/pending_hook_dedupe.py",
        false
    ),
    embedded!(
        "scripts/pending-answers-hook.sh",
        "scripts/pending-answers-hook.sh",
        true
    ),
    page!("tokens.css"),
    // #10571: which prototype commit and content this bundle was built from.
    embedded!(
        "docs/architect/bundle-provenance.json",
        "provenance.json",
        false
    ),
    page!("index.html"),
    page!("live.html"),
    page!("vendor/marked.umd.js"),
    page!("vendor/purify.min.js"),
    page!("vendor/VERSIONS.md"),
    schema!("accounts.schema.json"),
    schema!("briefs.schema.json"),
    schema!("effort_tunes.schema.json"),
    schema!("goal_removals.schema.json"),
    schema!("goal_reorders.schema.json"),
    schema!("goals.schema.json"),
    schema!("il_steers.schema.json"),
    schema!("items.schema.json"),
    schema!("media.schema.json"),
    schema!("meta-gh_track.schema.json"),
    schema!("meta-seen.schema.json"),
    schema!("meta-settings.schema.json"),
    schema!("meta-state.schema.json"),
    schema!("questions.schema.json"),
    schema!("sessions.schema.json"),
    schema!("step_progress.schema.json"),
    embedded!(
        "scripts/start-fleet-poll.sh",
        "scripts/start-fleet-poll.sh",
        true
    ),
    // #9997: the two-step relay send and the double-post guard the manual names.
    embedded!("scripts/relay.sh", "scripts/relay.sh", true),
    embedded!("scripts/posted.sh", "scripts/posted.sh", true),
    // posted.sh runs this sibling by path.
    embedded!("scripts/relay-marker.py", "scripts/relay-marker.py", false),
    // See #10161: the `local/architect.json` loader; a module, not a command.
    embedded!(
        "scripts/architect_config.py",
        "scripts/architect_config.py",
        false
    ),
    // #9997: the voice guide the manual and `architect-operating-rules` cite.
    embedded!("docs/architect/voice.md", "docs/voice.md", false),
];
