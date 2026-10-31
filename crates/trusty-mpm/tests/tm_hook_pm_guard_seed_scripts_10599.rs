//! #10599: the fleet-init Architect's seeded scripts, judged by the real
//! `tm hook --pm-guard` under the supervisor profile.
//!
//! Why: the guard judged three seeded scripts as credential reads or
//! unresolvable runs, so a fresh Architect could not run its own tooling.
//! Hand-composed evaluators miss the profile, the hook cwd and the order the
//! hook applies its rules in; only the built binary proves the verdict.
//! What: each test seeds an Architect with `tm fleet init --no-launch` under a
//! scratch `$HOME` (which writes the `supervisor` profile and the allowlist
//! entry), then spawns `tm hook --pm-guard` with a `PreToolUse` Bash payload,
//! `TRUSTY_MPM_SESSION_PROFILE=supervisor`, `TRUSTY_MPM_PM_UNRESTRICTED`
//! unset and an unreachable daemon. Nothing a judged command names is run.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_seed_scripts_10599::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// A seeded Architect project and the scratch `$HOME` that grants it.
struct Arch {
    /// The Architect directory `tm fleet init` seeded.
    dir: PathBuf,
    /// The scratch `$HOME`.
    home: tempfile::TempDir,
    /// Holds a non-default Architect directory outside `$HOME`.
    _outer: tempfile::TempDir,
}

/// Seed an Architect: at the default directory under `$HOME`, or, when
/// `custom`, at a non-default directory outside `$HOME`.
fn seed(custom: bool) -> Arch {
    let home = tempfile::tempdir().expect("home");
    let outer = tempfile::tempdir().expect("outer");
    let tmux = tempfile::Builder::new()
        .prefix("tms")
        .tempdir_in("/tmp")
        .expect("tmux dir");
    common::write_disk_threshold(home.path(), 100);
    let mut cmd = common::tm_command_in(home.path());
    cmd.args(["fleet", "init", "--no-launch"]);
    if custom {
        cmd.arg("--dir").arg(outer.path().join("ops/arch-x"));
    }
    for key in trusty_mpm::core::claude_env_scrub::INHERITED_SESSION_MARKERS {
        cmd.env_remove(key);
    }
    let out = cmd
        .env("TMUX_TMPDIR", tmux.path())
        .env_remove("TMUX_SOCKET")
        .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
        .output()
        .expect("spawn `tm fleet init`");
    assert!(
        out.status.success(),
        "fleet init: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = if custom {
        outer.path().join("ops/arch-x")
    } else {
        home.path().join("trusty-mpm-projects/architect")
    };
    assert!(dir.join("scripts/self-clear.sh").is_file(), "seeded");
    Arch {
        dir,
        home,
        _outer: outer,
    }
}

/// The guard's verdict on a Bash `command` run from the Architect directory:
/// `None` for ALLOW, else the refusal reason.
fn judge(arch: &Arch, command: &str) -> Option<String> {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-10599",
        "cwd": arch.dir.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = common::tm_command_in(arch.home.path())
        .args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&arch.dir)
        .env("CLAUDE_PROJECT_DIR", &arch.dir)
        .env("TRUSTY_MPM_SESSION_PROFILE", "supervisor")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --pm-guard`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the guard exits 0: {out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    common::assert_pm_guard_refusals_prefixed(&stdout);
    stdout.lines().find_map(|line| {
        let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
        let output = &parsed["hookSpecificOutput"];
        (output["permissionDecision"] == "deny").then(|| {
            output["permissionDecisionReason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
    })
}

/// `reason` with the Architect directory written as `<ARCH>`, so one text
/// fits every seed location.
fn normalized(arch: &Arch, reason: &str) -> String {
    let mut text = reason.to_string();
    if let Ok(real) = std::fs::canonicalize(&arch.dir) {
        text = text.replace(&real.display().to_string(), "<ARCH>");
    }
    text.replace(&arch.dir.display().to_string(), "<ARCH>")
}

/// The command that runs seeded script `name` at `path`: a Python file
/// through `python3`, a shell script by its path, as the seeded docs run them.
fn run_command(name: &str, path: &str) -> String {
    if name.ends_with(".py") {
        format!("python3 {path}")
    } else {
        path.to_string()
    }
}

/// Build one exact script refusal: the #8879 prefix naming the command's
/// script word as `<SCRIPT>`, `$why`, and the shared tail.
macro_rules! refusal {
    ($why:expr) => {
        concat!(
            "tm pm-guard: `tm hook --pm-guard` refused running the script `<SCRIPT>` (#8879): ",
            $why,
            ". A script gets the same credential-read refusal as the command written inline \
             (owner rulings 263 and 268), so moving a refused read into a script does not \
             change the answer. Stop and report this refusal to the Architect; do not retry \
             the read in another form."
        )
    };
}

/// The body runs a credential CLI word in order (`op` … `read`).
const CRED: &str = refusal!(
    "its body, or a script it runs, runs a credential-reading command (a Keychain, token or \
     secret CLI; #8596, #8248)"
);
/// The body names a secret-bearing file.
const FILE: &str = refusal!("its body, or a script it runs, names a secret-bearing file (#7266)");
/// A Python body judged as shell reads as a nested read or an env dump.
const NESTED: &str = refusal!(
    "its body, or a script it runs, reads a credential through a nested command or an \
     environment dump (#8756)"
);
/// The script runs the seeded `datg.py`, which [`CRED`] refuses.
const RUNS_DATG: &str = refusal!(
    "it runs the script `<ARCH>/scripts/datg.py`: its body, or a script it runs, runs a \
     credential-reading command (a Keychain, token or secret CLI; #8596, #8248)"
);
/// A comment line in `pm-notify.sh` reads as a script run.
const PM_NOTIFY: &str = refusal!(
    "it runs the script `ack/progress/fyi`, which does not exist and nothing else in the \
     command names it, so the guard cannot judge its body and fails closed"
);

/// Owner of the dashboard and Intent Ledger seed files, which this fix may not edit.
const FIXED_IN_10571: &str = "fixed in #10571";
/// The guard judging a Python body as shell, or a `;` inside a comment.
const GUARD_10598: &str = "#10598";

/// 🔴 REGRESSION (#10599): `self-clear.sh` resolves its root from its own
/// path, so the guard follows the scripts it runs instead of refusing
/// `$ROOT/...` as computed from `ARCHITECT_ROOT`. Relative from the default
/// Architect, absolute in a non-default one, run by path and through `bash`.
/// The one refusal left is the seeded `datg.py` it runs ([`RUNS_DATG`]),
/// which #10571 fixes; the ratchet below fails once that passes.
#[test]
fn self_clear_resolves_its_own_root_at_both_installed_paths_10599() {
    let default = seed(false);
    let custom = seed(true);
    let abs = custom
        .dir
        .join("scripts/self-clear.sh")
        .display()
        .to_string();
    let rows = [
        (&default, "scripts/self-clear.sh".to_string()),
        (&default, "bash scripts/self-clear.sh".to_string()),
        (&custom, abs.clone()),
        (&custom, format!("bash {abs}")),
    ];
    for (arch, command) in rows {
        let word = command.strip_prefix("bash ").unwrap_or(&command);
        let got = judge(arch, &command)
            .map(|r| normalized(arch, &r.replace(&format!("`{word}`"), "`<SCRIPT>`")));
        assert!(
            got.is_none() || got.as_deref() == Some(RUNS_DATG),
            "`{command}` must allow, or refuse only on the seeded datg.py: {got:?}"
        );
    }
}

/// 🔴 CONTROL (#10599): real credential reads from the seeded Architect
/// still refuse, so this fix changed no guard rule.
#[test]
fn real_credential_reads_still_deny_in_the_architect_10599() {
    let arch = seed(true);
    for command in [
        "op read op://fake-vault/x/y",
        "security find-generic-password -s fake-svc -w",
        "cat local/token.json",
    ] {
        assert!(judge(&arch, command).is_some(), "`{command}` must deny");
    }
}

/// A seeded script the guard still refuses for a cause another issue owns:
/// the script, its exact refusal with the command's script word as
/// `<SCRIPT>` and the Architect directory as `<ARCH>`, and the issue.
struct Residual {
    script: &'static str,
    reason: &'static str,
    issue: &'static str,
}

/// Shorthand for one [`RESIDUALS`] row.
const fn residual(script: &'static str, reason: &'static str, issue: &'static str) -> Residual {
    Residual {
        script,
        reason,
        issue,
    }
}

/// #10599: the seeded scripts that still refuse, each for a cause outside
/// this fix. A new refusal, a changed text, or a listed script that starts
/// passing fails `every_seed_script_passes_pm_guard_or_is_a_listed_residual_10599`.
const RESIDUALS: &[Residual] = &[
    residual("dashboard-db.py", CRED, FIXED_IN_10571),
    residual("dashboard-serve.py", FILE, FIXED_IN_10571),
    residual("dashboard_docs.py", FILE, FIXED_IN_10571),
    residual("dashboard_harness.py", FILE, FIXED_IN_10571),
    residual("dashboard_media.py", CRED, FIXED_IN_10571),
    residual("dashboard_store.py", CRED, FIXED_IN_10571),
    residual("dashboard_viz.py", FILE, FIXED_IN_10571),
    residual("datg-hook.sh", RUNS_DATG, FIXED_IN_10571),
    residual("datg.py", CRED, FIXED_IN_10571),
    residual("datg_ops.py", CRED, FIXED_IN_10571),
    residual("datg_store.py", FILE, FIXED_IN_10571),
    residual("fleet-poll.py", NESTED, GUARD_10598),
    residual("pm-notify.sh", PM_NOTIFY, GUARD_10598),
    residual("self-clear.sh", RUNS_DATG, FIXED_IN_10571),
];

/// 🔴 RATCHET (#10599): every seeded script, run relative from the Architect
/// directory and by its absolute path in a non-default Architect, either
/// passes the guard or refuses with exactly its [`RESIDUALS`] text.
#[test]
fn every_seed_script_passes_pm_guard_or_is_a_listed_residual_10599() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/architect/scripts");
    let mut names: Vec<String> = std::fs::read_dir(&assets)
        .expect("read seed scripts")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".py") || n.ends_with(".sh"))
        .collect();
    names.sort();
    assert!(names.len() > 40, "every seed script is found: {names:?}");
    for r in RESIDUALS {
        assert!(
            names.iter().any(|n| n == r.script),
            "{} is seeded",
            r.script
        );
    }
    let arch = seed(true);
    let mut wrong = Vec::new();
    for name in &names {
        let rel = format!("scripts/{name}");
        let abs = arch.dir.join(&rel).display().to_string();
        let listed = RESIDUALS.iter().find(|r| r.script == name);
        for word in [rel, abs] {
            let got = judge(&arch, &run_command(name, &word))
                .map(|r| normalized(&arch, &r.replace(&format!("`{word}`"), "`<SCRIPT>`")));
            if got.as_deref() != listed.map(|r| r.reason) {
                wrong.push(format!(
                    "{name} as `{word}`: got {got:?}, listed {:?}",
                    listed.map(|r| (r.reason, r.issue))
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "verdicts differ from RESIDUALS:\n{}",
        wrong.join("\n")
    );
}
