//! The rows `tm fleet init` compiles in (split from seed.rs, #10571).
//!
//! Why: seed.rs passed the 500-SLOC cap once the P1 dashboard set landed; the
//! table is data, the write and refresh logic stays in seed.rs.
//! What: [`EMBEDDED`], the non-script seed rows, the `scripts/` rows in
//! `seed_table_scripts.rs`, and the five row macros that build them from a
//! path under `src/assets/architect/`. [`rows`] yields both in write order.
//! `scripts/architect_bundle_meta.py history` reads every table file
//! (its `TABLES`) to list the rows.
//! Test: `every_script_asset_is_seeded`, `every_dashboard_asset_is_seeded`,
//! `every_schema_asset_is_seeded`, `every_template_asset_is_seeded`.

/// One embedded row of `files`: destination, bytes, executable.
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

/// Build one script row: `src/assets/architect/scripts/<name>` seeded at
/// `scripts/<name>`, on one line so the script table stays under the cap.
// #10571: one line per row; the prototype re-sync adds about 110 script rows.
macro_rules! script {
    ($name:literal, $exec:literal) => {
        (
            concat!("scripts/", $name),
            include_str!(concat!("../../../../assets/architect/scripts/", $name)),
            $exec,
        )
    };
}

// #10571: the `scripts/` rows live in their own file; it uses the macros above.
#[path = "seed_table_scripts.rs"]
mod scripts;

/// Every compiled-in seed row in write order: [`EMBEDDED`], then the scripts.
///
/// Why: the table spans two files (#10571); callers want one sequence.
/// What: chains [`EMBEDDED`] and `scripts::SCRIPTS`.
/// Test: `every_script_asset_is_seeded`, `every_template_asset_is_seeded`.
pub(super) fn rows() -> impl Iterator<Item = &'static Embedded> {
    EMBEDDED.iter().chain(scripts::SCRIPTS)
}

/// The non-script files compiled in, in write order, after `CLAUDE.md`.
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
    // #10571 Phase 2: the IL cadence jobs and clear thresholds il-cron.py and ctx-watch.py read.
    template!("local/il-schedule.json"),
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
    // #10896: the owner's Watch / Unwatch request queue.
    schema!("watch_changes.schema.json"),
    // #9997: the voice guide the manual and `architect-operating-rules` cite.
    embedded!("docs/architect/voice.md", "docs/voice.md", false),
];
