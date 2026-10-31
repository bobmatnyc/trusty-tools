//! Every seeded Architect script passes the pm-guard credential rules at its
//! installed path (#10571, #10579).
//!
//! Why: the guard refused the forms the Intent Ledger refresh runs ("runs a
//! credential-reading command"), so an installed Architect could not run its
//! own scripts. The verdict is the guard's own decision functions, composed as
//! `pm_guard_architect_envfile` composes them; nothing is executed.
//! What: `tm fleet init` into a temp fleet; each seeded command (a shell
//! script, or Python with a `__main__`) under `scripts/` is checked run relative (`python3 scripts/x.py`, from the fleet
//! dir) and by its absolute path, plus the exact refresh forms; three
//! credential reads must still be refused. [`KNOWN_EXCEPTIONS`] lists guard
//! false positives tracked in #10598, never a weakened rule.

use std::path::Path;

use super::{Fixture, seeded_files};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;
use crate::commands::pm_guard_secret_script::evaluate_script_body_secret_read;

/// Guard false positives tracked in #10598 (script, reason); empty when none trips.
const KNOWN_EXCEPTIONS: &[(&str, &str)] = &[(
    "self-clear.sh",
    "#10599 owns its ROOT: `$ROOT/scripts/...` is a computed path the guard cannot judge (guard false positives: #10598)",
)];

/// The credential verdict for a Bash `command` run from `cwd`.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

/// The interpreter a seeded script runs under.
fn runner(rel: &str) -> &'static str {
    if rel.ends_with(".sh") {
        "/bin/sh"
    } else {
        "python3"
    }
}

#[test]
fn every_seeded_script_passes_the_guard_at_its_installed_path() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = std::fs::canonicalize(fx.dir()).unwrap();
    let mut refused = Vec::new();
    let mut checked = 0;
    for file in seeded_files() {
        let Some(name) = file.dest.strip_prefix("scripts/") else {
            continue;
        };
        // A command, not a module a command imports: a shell script, or Python with a
        // `__main__` entry point (dashboard_store.py is seeded 0755 but is a module).
        let command = name.ends_with(".sh")
            || (name.ends_with(".py") && file.contents.contains("if __name__ == \"__main__\""));
        if !command {
            continue;
        }
        if KNOWN_EXCEPTIONS.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let abs = dir.join(&file.dest);
        for command in [
            format!("{} scripts/{name}", runner(name)),
            format!("{} {}", runner(name), abs.display()),
        ] {
            checked += 1;
            if let Some(why) = verdict(&command, &dir) {
                refused.push(format!("{command}: {why}"));
            }
        }
    }
    assert!(checked > 30, "checked only {checked} commands");
    assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
}

#[test]
fn the_intent_ledger_refresh_forms_pass_the_guard() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = std::fs::canonicalize(fx.dir()).unwrap();
    let il = dir.join("local/il");
    let mut refused = Vec::new();
    for (script, args) in [
        ("datg.py", "digest"),
        ("datg.py", "fresh"),
        ("datg.py", "session set --next x"),
        ("datg.py", "init --code t"),
        ("dashboard-db.py", "summary"),
        ("datg-hook.sh", "tm-architect"),
    ] {
        let run = runner(script);
        let abs = dir.join("scripts").join(script);
        for command in [
            format!("{run} scripts/{script} {args}"),
            format!("{run} {} {args}", abs.display()),
            format!("{run} {} --project {} {args}", abs.display(), il.display()),
        ] {
            if let Some(why) = verdict(&command, &dir) {
                refused.push(format!("{command}: {why}"));
            }
        }
    }
    assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
}

/// (d) The Intent Ledger hook command `tm fleet init` installs passes the guard.
#[test]
fn the_installed_ledger_hook_passes_the_guard() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let command = super::super::il_init::hook_command(&dir, "tm-architect");
    assert_eq!(verdict(&command, &dir), None, "{command}");
}

/// The guard itself is unchanged: real credential reads are still refused.
#[test]
fn credential_reads_are_still_refused_from_the_fleet_dir() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = std::fs::canonicalize(fx.dir()).unwrap();
    for command in [
        "op read op://fake/item/field",
        "security find-generic-password -s fake -w",
        "cat local/token.json",
    ] {
        assert!(
            verdict(command, &dir).is_some(),
            "{command} must stay refused"
        );
    }
}

/// #10579: the seeded instructions end every owner reply with the dashboard
/// footer, resolved from the instance config, never a typed address.
#[test]
fn the_seeded_instructions_require_the_dashboard_footer() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let text = std::fs::read_to_string(fx.dir().join("CLAUDE.md")).unwrap();
    for needle in [
        "End every reply the user reads with the dashboard footer",
        "`python3 scripts/dashboard-db.py footer`",
        "`Dashboard: [local](<this install's address>)`",
        "only when a tailnet address is set",
        "`dashboard_port`",
        "`tailnet_url` in `local/architect.json`",
    ] {
        assert!(text.contains(needle), "missing {needle:?}");
    }
    for host_specific in ["127.0.0.1", "localhost:", ".ts.net", "http://", "https://"] {
        assert!(
            !text.contains(host_specific),
            "seeded text names {host_specific:?}"
        );
    }
}
