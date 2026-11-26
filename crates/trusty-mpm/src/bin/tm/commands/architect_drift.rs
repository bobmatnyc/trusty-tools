//! `tm architect drift` and the `tm architect up --strict` gate (#11773).
//!
//! Why: owner ruling on #11773 — "read-only reporting first, in `tm architect
//! drift`; strict is not the default". The classifier lives next to the seed
//! refresh (`fleet::seed_drift`); this file renders its report and maps it to
//! an exit code.
//! What: [`run`] resolves the Architect directory and the seed without a
//! fetch, classifies, prints text or JSON and returns 0, 1 (a file is edited)
//! or 2 (the directory or the seed cannot be read). [`strict_check`] is the
//! read-only gate `up --strict` runs before its lock and init.
//! Test: `the_text_report_lists_every_non_current_path`,
//! `strict_refuses_an_edited_script_and_writes_nothing`,
//! `strict_passes_unedited_scripts_and_an_edited_skill`,
//! `tm_architect_drift_11773::drift_json_exits_1_on_an_edited_script`.

use std::path::Path;

use trusty_mpm::core::content_source::Fetch;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::fleet::{self, seed_drift};

/// `tm architect drift [--json] [--dir DIR]`; returns the exit code.
pub(crate) fn run(json: bool, dir: Option<String>) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("tm architect drift: cannot resolve the home directory");
        return 2;
    };
    let dir = match fleet::resolve_dir(dir.as_deref(), &home) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tm architect drift: {e:#}");
            return 2;
        }
    };
    // Read-only: never fetch content (`Fetch::Never`, as `tm doctor`).
    let report = match ArchitectSeed::resolve_for(&dir, Fetch::Never)
        .map_err(anyhow::Error::from)
        .and_then(|seed| seed_drift::classify(&dir, &seed))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tm architect drift: {e:#}");
            return 2;
        }
    };
    if json {
        match serde_json::to_string_pretty(&json_value(&report)) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("tm architect drift: {e}");
                return 2;
            }
        }
    } else {
        print!("{}", render(&report));
    }
    report.exit()
}

/// The `--json` document: the report, a per-state count and the exit code.
pub(crate) fn json_value(report: &seed_drift::Report) -> serde_json::Value {
    let summary: serde_json::Map<String, serde_json::Value> = seed_drift::State::ALL
        .iter()
        .map(|s| (s.name().to_owned(), report.count(*s).into()))
        .collect();
    serde_json::json!({
        "dir": report.dir,
        "exit": report.exit(),
        "summary": summary,
        "files": report.files,
    })
}

/// The text report: one line per non-current path, then the counts.
///
/// Test: `the_text_report_lists_every_non_current_path`.
pub(crate) fn render(report: &seed_drift::Report) -> String {
    let mut out = format!("tm architect drift: {}\n", report.dir);
    for entry in report
        .files
        .iter()
        .filter(|e| e.state != seed_drift::State::Current)
    {
        out.push_str(&format!("  {:<8} {}\n", entry.state.name(), entry.path));
    }
    let counts: Vec<String> = seed_drift::State::ALL
        .iter()
        .map(|s| format!("{} {}", report.count(*s), s.name()))
        .collect();
    out.push_str(&format!("summary: {}\n", counts.join(", ")));
    out
}

/// The `up --strict` refusal text, or `None` when no managed script is edited.
///
/// Why: #11773 — only an edited `scripts/*` seed fails `--strict`.
/// Test: `strict_refuses_an_edited_script_and_writes_nothing`.
pub(crate) fn strict_refusal(report: &seed_drift::Report) -> Option<String> {
    let scripts = report.edited_scripts();
    if scripts.is_empty() {
        return None;
    }
    let mut msg = format!(
        "tm architect up --strict: refused, {} managed script(s) edited \
         (see `tm architect drift`):\n",
        scripts.len()
    );
    for path in scripts {
        msg.push_str(&format!("  edited   {path}\n"));
    }
    Some(msg)
}

/// The read-only gate `tm architect up --strict` runs before any write.
///
/// Why: #11773 AC — "`tm architect up --strict` fails if any managed script
/// is edited". What: resolves the seed as `up`'s init does, classifies `dir`
/// and returns the refusal text on an edited script or an unreadable seed.
/// Writes, locks and starts nothing.
/// Test: `strict_refuses_an_edited_script_and_writes_nothing`,
/// `tm_architect_drift_11773::up_strict_refuses_an_edited_script_and_writes_nothing`.
pub(crate) fn strict_check(dir: &Path) -> Result<(), String> {
    let cwd = std::env::current_dir().ok();
    let cache = trusty_common::content::default_cache_dir();
    strict_check_in(dir, cwd.as_deref(), cache.as_deref())
}

/// [`strict_check`] with the cwd and the content cache named.
///
/// Why: #11773 critic — a test needs a seed that cannot resolve (no checkout,
/// no cache) to prove the gate fails closed.
/// Test: `strict_fails_closed_on_an_unreadable_script`,
/// `strict_fails_closed_when_the_seed_cannot_resolve`.
pub(crate) fn strict_check_in(
    dir: &Path,
    cwd: Option<&Path>,
    cache: Option<&Path>,
) -> Result<(), String> {
    let seed = ArchitectSeed::resolve_in(dir, cwd, cache, Fetch::OnFirstUse)
        .map_err(|e| format!("tm architect up --strict: {e}\n"))?;
    let report = seed_drift::classify(dir, &seed)
        .map_err(|e| format!("tm architect up --strict: {e:#}\n"))?;
    strict_refusal(&report).map_or(Ok(()), Err)
}
