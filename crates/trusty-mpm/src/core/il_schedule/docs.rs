//! The IL schedule reference, generated from the preset (#11001, ADR-0077 D7).
//!
//! Why: docs written by hand drift from the shipped default. The page's
//! values, its example and the commands that reproduce the example all come
//! from [`super::PRESET`] and the [`super::fields`] tables.
//! What: [`render_docs`] returns the markdown body of the `il-schedule`
//! region of `crates/trusty-mpm/docs/il-schedule.md`.
//! Test: `il_schedule_reference_matches_the_preset` (`tests/generated_docs.rs`),
//! `the_documented_commands_rebuild_the_example`.

use std::fmt::Write as _;

use serde_json::Value;

use super::fields::{SESSION_KEYS, THRESHOLDS, TOP_LEVEL};
use super::{Schedule, Session};

/// The markdown body of the generated region.
///
/// What: three key tables (top level, session fields, thresholds with the
/// preset's value), the preset's `session_example` as JSON, and the
/// `tm il config` commands that enroll it ([`example_commands`]).
/// Test: `il_schedule_reference_matches_the_preset`.
pub fn render_docs() -> String {
    let preset = Schedule::preset();
    let mut out = String::new();
    out.push_str("Generated from the preset embedded in `tm` ");
    out.push_str(
        "(`crates/trusty-mpm/src/assets/architect/templates/local/il-schedule.json`).\n\n",
    );
    out.push_str("### Top-level keys\n\n| Key | Meaning |\n|---|---|\n");
    for (key, meaning) in TOP_LEVEL {
        let _ = writeln!(out, "| `{key}` | {meaning} |");
    }
    out.push_str("\n### Session fields\n\n| Field | Meaning |\n|---|---|\n");
    for (key, meaning) in SESSION_KEYS {
        let _ = writeln!(out, "| `{key}` | {meaning} |");
    }
    out.push_str("\n### Thresholds\n\n| Key | Preset value | Meaning |\n|---|---|---|\n");
    for (key, meaning) in THRESHOLDS {
        let value = preset
            .thresholds
            .get(*key)
            .map_or_else(|| "not set (reader default)".to_owned(), Value::to_string);
        let _ = writeln!(out, "| `{key}` | {value} | {meaning} |");
    }
    if let Some(example) = &preset.session_example {
        out.push_str("\n### Session example\n\nThe preset's `session_example`:\n\n```json\n");
        let json = serde_json::to_string_pretty(example).expect("a Session serializes");
        out.push_str(&json);
        out.push_str("\n```\n\nThe commands that enroll the same session:\n\n```bash\n");
        for cmd in example_commands(example) {
            let _ = writeln!(out, "{cmd}");
        }
        out.push_str("```\n");
    }
    out
}

/// The `tm il config` command lines that make `example` an enrolled session.
///
/// Why: the docs show the CLI path, and a test replays these lines so the
/// commands cannot drift from the example.
/// What: one `enroll` (name, project, code), then one `set --session` for
/// `signals`, `state_path` and each threshold override, in that order.
/// Test: `the_documented_commands_rebuild_the_example`.
pub fn example_commands(example: &Session) -> Vec<String> {
    let name = &example.name;
    let mut enroll = format!("tm il config enroll {name} --project {}", example.project);
    if let Some(code) = &example.code {
        let _ = write!(enroll, " --code {code}");
    }
    let mut cmds = vec![enroll];
    let set = |key: &str, value: &str| format!("tm il config set --session {name} {key} {value}");
    if let Some(signals) = &example.signals {
        cmds.push(set("signals", &signals.join(",")));
    }
    if let Some(path) = &example.state_path {
        cmds.push(set("state_path", path));
    }
    for (key, value) in &example.thresholds {
        cmds.push(set(&format!("thresholds.{key}"), &value.to_string()));
    }
    cmds
}
