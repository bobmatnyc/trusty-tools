//! The #10665 whole-server tmux kill rule applied to the BODY of a script a
//! Bash command runs.
//!
//! Why: the 2026-11-01 kill ran from line 38 of a scratch script; the Bash
//! command the hook saw was only `bash <script>`. The #8879 script walk
//! (with the #10566 substitution reach) already locates every script a
//! command runs, so this rule reuses it rather than a second walker.
//! What: [`evaluate_script_body_tmux_kill`] locates each script with
//! `walk::each_script`, reads it with `read_script`, and judges a shell body
//! (comment lines blanked, continuations joined) with
//! `kill_server::evaluate_tmux_kill_server`, then the scripts that body
//! runs, to `MAX_SCRIPT_DEPTH`. A body in another language that names a
//! whole-server kill refuses: the guard cannot read it as shell.
//! Owner ruling Dac (2026-11-02): this rule is log-only. Its finding is a
//! would-refuse the hook records; the call runs. Script-body bypasses are
//! #10706's scope. #10706 C2: a trusty-tools `scripts/sandbox_*.sh` whose
//! bytes match `HEAD` is trusted unread (`trusted_sandbox_script`).
//! Residual: a script the guard cannot locate or read is the #8879 rule's
//! fail-closed refusal, not this one's.
//! Test: `a_kill_server_in_a_script_body_is_refused`,
//! `a_scratch_socket_kill_in_a_script_is_allowed`,
//! `sandbox_daemon_script_allowed_by_repo_and_content`.

use std::path::{Path, PathBuf};

use super::walk::each_script;
use super::{
    Body, Family, MAX_SCRIPT_DEPTH, family_of, read_script, shebang_interpreter,
    without_comment_lines,
};
use crate::commands::pm_guard_bash::kill_server::{
    KillVerdict, ServerEnv, evaluate_tmux_kill_server, names_whole_server_kill, refusal,
};
use crate::commands::pm_guard_secret_script_self::{ToplevelRisk, resolve_self_paths_with};

/// The would-refuse reason for a Bash command that runs a script whose body
/// runs a whole-server tmux kill without a scratch server, or `None`.
/// #10665 (ruling Dac): the caller logs it and never refuses.
pub(crate) fn evaluate_script_body_tmux_kill(
    command: &str,
    cwd: &Path,
    env: &ServerEnv,
) -> Option<String> {
    let bases = [cwd.to_path_buf()];
    let mut seen = Vec::new();
    each_script(command, &bases, |word, found, interpreter, seg_bases| {
        let path = found.ok()?;
        let why = judge(&path, interpreter, seg_bases, 0, env, &mut seen)?;
        Some(format!(
            "{why} Found in the script `{word}` this command runs."
        ))
    })
}

/// Judge one script body, then the scripts a shell body runs.
fn judge(
    path: &Path,
    interpreter: Option<&str>,
    bases: &[PathBuf],
    depth: usize,
    env: &ServerEnv,
    seen: &mut Vec<PathBuf>,
) -> Option<String> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&key) {
        return None;
    }
    seen.push(key.clone());
    let Ok(Body::Script(body)) = read_script(path) else {
        return None;
    };
    if trusted_sandbox_script(&key, &body) {
        return None;
    }
    if depth >= MAX_SCRIPT_DEPTH {
        return None;
    }
    let interpreter = interpreter
        .map(str::to_string)
        .unwrap_or_else(|| shebang_interpreter(&body));
    if family_of(&interpreter) != Some(Family::Shell) {
        return names_whole_server_kill(&body)
            .then(|| refusal("a script body the guard cannot read as shell names one"));
    }
    // #10665 (ruling Dac): either verdict is only a would-refuse here.
    if let Some(KillVerdict::Refuse(why) | KillVerdict::LogOnly(why)) =
        evaluate_tmux_kill_server(&without_comment_lines(&body), env)
    {
        return Some(why);
    }
    let risk = ToplevelRisk::scan(&body, false, interpreter.contains("zsh"));
    let nested = resolve_self_paths_with(&body, path, risk);
    each_script(&nested, bases, |_, found, inner, inner_bases| {
        judge(&found.ok()?, inner, inner_bases, depth + 1, env, seen)
    })
}

/// Whether `path` is a trusty-tools sandbox launcher the guard trusts unread.
///
/// Why: #10706 C2 (Architect direction). The sandbox scripts stop their own
/// server with `env -i PATH="$PATH" tmux -S "$SOCK" kill-server`, and with
/// the `env -i` fallback gone the floor cannot resolve `$SOCK`. They are
/// trusted by where they live and by their bytes instead.
/// What: `true` when `path` (canonical) is `<root>/scripts/sandbox_*.sh`,
/// `<root>` is the top of a git work tree whose `origin` is
/// bobmatnyc/trusty-tools, and `body` hashes to the blob `HEAD` holds at that
/// path — an uncommitted edit, or a copy in another repo, is judged as usual.
/// Test: `sandbox_daemon_script_allowed_by_repo_and_content`.
fn trusted_sandbox_script(path: &Path, body: &str) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let scripts = path
        .parent()
        .filter(|d| d.file_name() == Some("scripts".as_ref()));
    let Some(root) = scripts.and_then(Path::parent) else {
        return false;
    };
    if !(name.starts_with("sandbox_") && name.ends_with(".sh")) {
        return false;
    }
    let head = format!("HEAD:scripts/{name}");
    let Some(out) = git(root, &["rev-parse", "--show-toplevel", &head], None) else {
        return false;
    };
    let mut lines = out.lines();
    let (Some(top), Some(blob)) = (lines.next(), lines.next()) else {
        return false;
    };
    // Both sides canonical: a symlinked workspace path must still match.
    let at_top = std::fs::canonicalize(top)
        .ok()
        .zip(std::fs::canonicalize(root).ok())
        .is_some_and(|(t, r)| t == r);
    let origin = git(root, &["config", "--get", "remote.origin.url"], None);
    let ours = origin.is_some_and(|url| {
        let url = url.trim().trim_end_matches('/').to_ascii_lowercase();
        let url = url.strip_suffix(".git").unwrap_or(&url);
        url.ends_with("github.com/bobmatnyc/trusty-tools")
            || url.ends_with("github.com:bobmatnyc/trusty-tools")
    });
    let hashed = || {
        git(
            root,
            &["hash-object", "--no-filters", "--stdin"],
            Some(body),
        )
    };
    at_top && ours && hashed().is_some_and(|h| h.trim() == blob.trim())
}

/// `git -C <dir> <args>` with `input` on stdin: its stdout, or `None` when it
/// fails. The hook's `GIT_DIR`-family variables are dropped so `-C` decides.
fn git(dir: &Path, args: &[&str], input: Option<&str>) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Reap the child before acting on a failed write, so it never lingers.
    let wrote = match (input, child.stdin.take()) {
        (Some(input), Some(mut stdin)) => stdin.write_all(input.as_bytes()).is_ok(),
        (Some(_), None) => false,
        (None, _) => true,
    };
    let out = child.wait_with_output().ok()?;
    wrote.then_some(())?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ServerEnv {
        ServerEnv {
            tmux: Some("/tmp/tmux-501/default,4242,0".into()),
            tmux_tmpdir: None,
            uid: 501,
        }
    }

    #[test]
    fn a_kill_server_in_a_script_body_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = dir.path().display().to_string();
        let script = dir.path().join("exp2.sh");
        let body = format!("#!/bin/bash\nset -e\nT={t}\nTMUX_TMPDIR=\"$T\" tmux kill-server\n");
        std::fs::write(&script, body).expect("write script");
        let inner = dir.path().join("outer.sh");
        std::fs::write(&inner, format!("bash {}\n", script.display())).expect("write outer");
        let python = dir.path().join("k.py");
        std::fs::write(&python, "import os\nos.system('tmux kill-server')\n").expect("write py");
        for command in [
            format!("bash {}", script.display()),
            "bash exp2.sh".to_string(),
            format!("X=$(sh {})", script.display()),
            format!("bash {}", inner.display()),
            format!("python3 {}", python.display()),
        ] {
            let why = evaluate_script_body_tmux_kill(&command, dir.path(), &env());
            assert!(why.is_some_and(|w| w.contains("#10665")), "{command}");
        }
    }

    #[test]
    fn a_scratch_socket_kill_in_a_script_is_allowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("clean.sh");
        let sock = dir.path().join("s.sock");
        let sock = sock.display();
        // #10665 r2: the standing rule's `env -u TMUX` form, in a body too.
        for body in [
            format!("tmux -S {sock} kill-server\necho done\n"),
            format!("env -u TMUX tmux -S {sock} kill-server\n"),
            format!("env --unset=TMUX tmux -S {sock} kill-server\n"),
        ] {
            std::fs::write(&script, &body).expect("write script");
            let command = format!("bash {}", script.display());
            assert_eq!(
                evaluate_script_body_tmux_kill(&command, dir.path(), &env()),
                None,
                "{body}"
            );
        }
    }
}
