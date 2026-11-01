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
//! `tmux_kill_server::evaluate_tmux_kill_server`, then the scripts that body
//! runs, to `MAX_SCRIPT_DEPTH`. A body in another language that names a
//! whole-server kill refuses: the guard cannot read it as shell.
//! Residual: a script the guard cannot locate or read is the #8879 rule's
//! fail-closed refusal, not this one's.
//! Test: `a_kill_server_in_a_script_body_is_refused`,
//! `a_scratch_socket_kill_in_a_script_is_allowed`.

use std::path::{Path, PathBuf};

use super::walk::each_script;
use super::{
    Body, Family, MAX_SCRIPT_DEPTH, family_of, read_script, shebang_interpreter,
    without_comment_lines,
};
use crate::commands::pm_guard_bash::tmux_kill_server::{
    ServerEnv, evaluate_tmux_kill_server, names_whole_server_kill, refusal,
};
use crate::commands::pm_guard_secret_script_self::{ToplevelRisk, resolve_self_paths_with};

/// Refuse a Bash command that runs a script whose body runs a whole-server
/// tmux kill without an explicit scratch `-S`: `Some(reason)` denies.
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
    seen.push(key);
    let Ok(Body::Script(body)) = read_script(path) else {
        return None;
    };
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
    if let Some(why) = evaluate_tmux_kill_server(&without_comment_lines(&body), env) {
        return Some(why);
    }
    let risk = ToplevelRisk::scan(&body, false, interpreter.contains("zsh"));
    let nested = resolve_self_paths_with(&body, path, risk);
    each_script(&nested, bases, |_, found, inner, inner_bases| {
        judge(&found.ok()?, inner, inner_bases, depth + 1, env, seen)
    })
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
        let body = format!("tmux -S {} kill-server\necho done\n", sock.display());
        std::fs::write(&script, body).expect("write script");
        let command = format!("bash {}", script.display());
        assert_eq!(
            evaluate_script_body_tmux_kill(&command, dir.path(), &env()),
            None
        );
    }
}
