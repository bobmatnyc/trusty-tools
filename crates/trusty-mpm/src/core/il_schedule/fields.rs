//! The schedule's documented keys: one table feeds validation and the docs (#11001).
//!
//! Why: ADR-0077 D7. The docs are generated, so the meaning of each key lives
//! here once; `set` accepts only a key listed here.
//! What: [`TOP_LEVEL`], [`SESSION_KEYS`] and [`THRESHOLDS`], each
//! `(key, meaning)`. The threshold keys are the ones `ctx-watch.py`
//! (`DEFAULT_THRESHOLDS`) and `ctx_bar.py` read.
//! Test: `every_preset_key_is_documented`.

/// Top-level keys of `il-schedule.json`.
pub const TOP_LEVEL: &[(&str, &str)] = &[
    (
        "jobs",
        "Cadence jobs `il-cron.py` runs: `{name, due, every, command}`. Not edited by `tm il config`.",
    ),
    (
        "sessions",
        "Enrolled sessions, one entry each (see Session fields). Edited by `enroll`, `set --session` and `remove`.",
    ),
    (
        "thresholds",
        "Global clear thresholds (see Thresholds). Edited by `set thresholds.<key>`.",
    ),
    (
        "do_not_clear",
        "Projects whose sessions are never cleared (SPEC-IL-01 section 8, gate 8).",
    ),
    (
        "paused_sessions",
        "Sessions on the pause list; the cycle skips them (SPEC-IL-01 section 8, gate 8).",
    ),
    (
        "session_example",
        "The documented session example. No reader acts on it; copy its shape with `enroll` and `set`.",
    ),
];

/// Fields of one `sessions` entry; the first two are required.
pub const SESSION_KEYS: &[(&str, &str)] = &[
    (
        "name",
        "Required. The tmux session name; `enroll <session>` sets it.",
    ),
    (
        "project",
        "Required. The project directory holding `.trusty-mpm/datg/`; stored as an absolute path.",
    ),
    ("code", "The IL id code, 2-8 letters or digits."),
    (
        "signals",
        "The clear signals evaluated, a subset of S1-S5; absent means all five. `set` takes a comma list.",
    ),
    (
        "thresholds",
        "Per-session overrides of the global thresholds; `set --session <name> thresholds.<key> <n>`.",
    ),
    (
        "state_path",
        "An absolute path to the IL `state.json`; absent means `<project>/.trusty-mpm/datg/[<name>/]state.json`.",
    ),
];

/// Threshold keys; every value is a non-negative integer.
pub const THRESHOLDS: &[(&str, &str)] = &[
    (
        "floor_ctx",
        "Tokens. S1, S3 and S4 fire only at or above this context size.",
    ),
    (
        "stale_pct",
        "Percent, 0-100. S1 fires when the self-reported stale share reaches it.",
    ),
    (
        "bar_ctx",
        "Tokens. S2 clears at a boundary at or above this context size.",
    ),
    (
        "approach_ctx",
        "Tokens. At or above it S2 asks the PM to reach a boundary.",
    ),
    ("guard_ctx", "Tokens. S5, the compaction guard."),
    (
        "fixed_base",
        "Tokens. The context a clear cannot recover; used for the stale proxy.",
    ),
    (
        "s2_refire_s",
        "Seconds between two S2 firings for one session.",
    ),
    (
        "s3_min_idle_s",
        "Seconds. S3 needs at least this much idle time.",
    ),
    (
        "s3_max_idle_s",
        "Seconds. S3 never fires after this much idle time.",
    ),
    ("s4_idle_s", "Seconds. S4 needs more idle time than this."),
    (
        "boundary_slack_s",
        "Seconds a last turn may trail `boundary_at` and still be at the boundary.",
    ),
    ("il_max_age_s", "Seconds. An IL older than this is stale."),
    (
        "rate_per_hour",
        "Automatic clears allowed per session per hour.",
    ),
    (
        "rate_per_day",
        "Automatic clears allowed per session per day.",
    ),
    (
        "s2_after_clear_s",
        "Seconds after a clear in which S2 does not clear the session again.",
    ),
    (
        "architect_bar_ctx",
        "Tokens. The Architect session's own context bar; absent means the PM bar.",
    ),
];

/// Whether `key` is a documented threshold.
pub(crate) fn is_threshold(key: &str) -> bool {
    THRESHOLDS.iter().any(|(k, _)| *k == key)
}
