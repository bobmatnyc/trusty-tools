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
//! `every_seeded_architect_script_passes_10570` judges every seeded script,
//! modules too; [`STILL_REFUSED_10570`] names the ones that still refuse.

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

/// Seeded scripts that still refuse under the unchanged guard, with the exact
/// reason each cannot be changed without changing what it does (#10570).
/// The four Python rows are modules with no `__main__`; the commands that
/// import them (`datg.py`, `dashboard-serve.py`) pass.
const STILL_REFUSED_10570: &[(&str, &str)] = &[
    (
        "datg_ops.py",
        "the error texts `refs.key must match` (a `.key` file name) and `unknown op` read as \
         a 1Password call; a passing body changes the message the caller sees",
    ),
    (
        "dashboard_docs.py",
        "DENY_NAMES is the `*secret*`/`*credential*`/`*.env*` glob list the doc route refuses; \
         it is the script's own deny list",
    ),
    (
        "dashboard_harness.py",
        "DOC_DENY is the secret-file glob list the harness refuses; it is the script's own deny list",
    ),
    (
        "dashboard_projects.py",
        "SECRET_NAME is the secret-name regex the project scan skips; it is the script's own deny list",
    ),
    (
        "self-clear.sh",
        "#10599 owns its ROOT: `$ROOT/scripts/input-state.py` is a computed path, and \
         ARCHITECT_ROOT is the override its tests and deployments set",
    ),
];

/// #10641: the text each [`STILL_REFUSED_10570`] row's refusal must carry, so a
/// row that starts refusing for another reason is seen.
const REFUSAL_TEXT_10570: &[(&str, &str)] = &[
    // #10641: each text is the phrase only the intended refusal carries.
    (
        "datg_ops.py",
        "runs a credential-reading command (a Keychain, token or secret CLI",
    ),
    ("dashboard_docs.py", "names a secret-bearing file (#7266)"),
    (
        "dashboard_harness.py",
        "names a secret-bearing file (#7266)",
    ),
    (
        "dashboard_projects.py",
        "names a secret-bearing file (#7266)",
    ),
    (
        "self-clear.sh",
        "runs the script `$ROOT/scripts/input-state.py`: its path is computed at run time",
    ),
];

/// 🔴 REGRESSION (#10570, #10598): every script `tm fleet init` seeds under
/// `scripts/`, module or command, passes the guard by interpreter, with
/// `--help`, and by path. The loop reads the real seeded rows, so a new seed is
/// judged with no edit here; [`STILL_REFUSED_10570`] names each script that
/// still refuses, and is checked to refuse, so a fixed one leaves the list.
#[test]
fn every_seeded_architect_script_passes_10570() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = std::fs::canonicalize(fx.dir()).unwrap();
    let (mut runs, mut refused, mut listed) = (0, Vec::new(), Vec::new());
    let mut wrong_text = Vec::new();
    let files = seeded_files();
    for f in &files {
        let Some(name) = f.dest.strip_prefix("scripts/") else {
            continue;
        };
        let run = if name.ends_with(".sh") {
            "bash"
        } else {
            "python3"
        };
        let mut commands = vec![
            format!("{run} {}", f.dest),
            format!("{run} {} --help", f.dest),
        ];
        if f.executable {
            commands.push(format!("./{}", f.dest));
        }
        runs += 1;
        let why: Vec<String> = commands
            .iter()
            .filter_map(|c| verdict(c, &dir).map(|why| format!("{c}: {why}")))
            .collect();
        if STILL_REFUSED_10570.iter().any(|(n, _)| *n == name) {
            let (_, text) = REFUSAL_TEXT_10570
                .iter()
                .find(|(n, _)| *n == name)
                .expect("a refusal text per listed row");
            let joined = why.join("\n");
            if !why.is_empty() && !joined.contains(text) {
                wrong_text.push(format!("{name}: want `{text}` in:\n{joined}"));
            }
            listed.push((name, why.is_empty()));
        } else {
            refused.extend(why);
        }
    }
    assert!(runs > 40, "seeded scripts judged: {runs}");
    assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
    let passing: Vec<_> = listed.iter().filter(|(_, ok)| *ok).collect();
    assert!(passing.is_empty(), "listed but passing: {passing:?}");
    assert_eq!(listed.len(), STILL_REFUSED_10570.len(), "{listed:?}");
    assert!(wrong_text.is_empty(), "{}", wrong_text.join("\n\n"));
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
