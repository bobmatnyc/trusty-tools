//! Attaching to a session on its own tmux server (#10101, ruling Dc3).
//!
//! Why: a fresh PM runs on its workspace's private server. `attach-session`
//! with no socket reaches only the default server, and `switch-client` cannot
//! move a client to another server. Ruling Dc3 (2026-10-20): from inside tmux,
//! a target on another server opens a NEW window in the caller's session that
//! runs the attach; no switch-client crosses servers.
//! What: [`target_socket`] finds the one server holding the session;
//! [`attach_argv_on`] is the outside-tmux argv (`-S <socket>` for a workspace
//! server, today's argv for the default one); [`on_other_server`] compares the
//! caller's `$TMUX` socket with the target's; [`new_window_attach_argv`] is the
//! cross-server window.
//! Test: the tests at the end of this file.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// The default server's socket name in tmux's socket directory.
const DEFAULT_SERVER: &str = "default";

/// The socket of the one tmux server that holds session `name`: the default
/// server's (`<socket dir>/default`) or a live workspace server's.
///
/// What: the default server is asked by name (`-L default`), so a caller inside
/// a workspace server's client still reaches the real default server.
///
/// # Errors
///
/// tmux cannot be found, a server cannot be listed, or two servers hold
/// `name`; no server is guessed.
pub(crate) fn target_socket(name: &str) -> anyhow::Result<PathBuf> {
    use trusty_mpm::daemon::services::tmux_service::holder_of;
    let driver = trusty_mpm::daemon::tmux::TmuxDriver::discover()
        .context("tmux is not available to attach with")?;
    let dir = trusty_mpm::core::tmux_server::ambient_socket_dir();
    let holder = holder_of(&driver.on_server(DEFAULT_SERVER), &dir, name)
        .map_err(|e| anyhow::anyhow!("cannot tell which tmux server holds '{name}': {e}"))?;
    let server = holder.server().unwrap_or(DEFAULT_SERVER);
    Ok(trusty_mpm::core::tmux_server::server_socket_path(
        &dir, server,
    ))
}

/// Whether `socket` is the default server's.
fn is_default(socket: &Path) -> bool {
    socket.file_name().is_some_and(|n| n == DEFAULT_SERVER)
}

/// The `-L` name of the workspace server holding session `name`, or `None`
/// for the default server (#10101).
///
/// Why: a by-name probe (`has-session`, `list-panes`) with no server reads
/// the default server, where a PM launched on its workspace server is absent.
/// What: [`target_socket`]'s file name, or `None` when that socket is the
/// default server's.
///
/// # Errors
///
/// As [`target_socket`].
///
/// Test: `the_picker_probes_read_the_workspace_server_10101`.
pub(crate) fn target_server(name: &str) -> anyhow::Result<Option<String>> {
    let socket = target_socket(name)?;
    Ok((!is_default(&socket))
        .then(|| socket.file_name().map(|n| n.to_string_lossy().into_owned()))
        .flatten())
}

/// A copy-pasteable command that attaches to session `name` on the server
/// holding it (#10101): `-S '<socket>'` for a workspace server; the plain
/// `shell_attach_command` for the default server or an unknown holder.
/// Test: `a_printed_attach_hint_names_the_workspace_socket_10101`.
pub(crate) fn shell_attach_command_on(name: &str) -> String {
    let attach = trusty_mpm::core::tmux::shell_attach_command(name);
    match target_socket(name) {
        Ok(socket) if !is_default(&socket) => attach.replacen(
            "tmux ",
            &format!("tmux -S {} ", quote(&socket.to_string_lossy())),
            1,
        ),
        _ => attach,
    }
}

/// The outside-tmux attach argv for `session` on the server at `socket`:
/// [`super::tmux_attach::attach_argv`], behind `-S <socket>` for a workspace
/// server, unchanged for the default server.
/// Test: `attach_argv_on_prefixes_only_a_workspace_socket`.
pub(crate) fn attach_argv_on(socket: &Path, session: &str) -> Vec<String> {
    let mut argv = Vec::new();
    if !is_default(socket) {
        argv.push("-S".to_owned());
        argv.push(socket.to_string_lossy().into_owned());
    }
    argv.extend(super::tmux_attach::attach_argv(session));
    argv
}

/// Whether the caller's tmux client (its `$TMUX`, `<socket>,<pid>,<index>`)
/// is known to be on another server than the one at `target`. An unreadable
/// `$TMUX` is not known to be another server, so the caller keeps today's
/// guarded switch, which fails closed on its own.
/// Test: `on_other_server_reads_the_socket_field_of_tmux`.
pub(crate) fn on_other_server(caller_tmux: Option<&str>, target: &Path) -> bool {
    let Some(socket) = caller_socket(caller_tmux) else {
        return false;
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(socket) != canon(target)
}

/// The socket field of the caller's `$TMUX`; `None` when unset or empty.
/// Test: `on_other_server_reads_the_socket_field_of_tmux`.
pub(crate) fn caller_socket(caller_tmux: Option<&str>) -> Option<&Path> {
    caller_tmux
        .and_then(|v| v.split(',').next())
        .filter(|s| !s.is_empty())
        .map(Path::new)
}

/// POSIX single-quoting for one shell word.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The `@N` id of the window holding pane `pane`, read on the caller's server;
/// `None` when tmux prints no window id.
///
/// # Errors
///
/// tmux cannot be run, or exits non-zero.
///
/// Test: `a_cross_server_focus_opens_the_attach_window`,
/// `a_caller_pane_with_no_window_moves_nothing`.
pub(crate) fn window_of_pane(tmux_bin: &str, pane: &str) -> anyhow::Result<Option<String>> {
    let target = trusty_common::tmux::exact_window_target(pane);
    let output = std::process::Command::new(tmux_bin)
        .args(["display-message", "-p", "-t", &target, "#{window_id}"])
        .output()
        .context("failed to invoke tmux")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(id.starts_with('@').then_some(id))
}

/// Whether the tmux server at `socket` holds session `name` (`has-session`).
///
/// Why: [`target_socket`] answers the default server when no server holds
/// `name`; a window opened there runs an attach that fails and closes, and the
/// caller would report a move that never happened (#10101, Dc3).
/// What: `false` on any non-zero exit, including a server that is not running.
///
/// # Errors
///
/// tmux cannot be run.
///
/// Test: `focus_on_a_session_no_server_holds_moves_nothing`.
pub(crate) fn server_holds(tmux_bin: &str, socket: &Path, name: &str) -> anyhow::Result<bool> {
    let target = trusty_common::tmux::exact_session_target(name);
    let status = std::process::Command::new(tmux_bin)
        .arg("-S")
        .arg(socket)
        .args(["has-session", "-t", &target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to invoke tmux")?;
    Ok(status.success())
}

/// `new-window -a -t <caller_window> -n <session> <command>` on the caller's
/// server, where the command attaches to `session` on the server at `socket`
/// with `$TMUX`/`$TMUX_PANE` unset (tmux refuses a nested attach otherwise).
/// `caller_window` is a window id (`@N`): tmux 3.6 refuses a `%N` pane id here.
/// Test: `a_cross_server_attach_opens_a_window_running_the_attach`,
/// `a_cross_server_window_target_is_exact`.
pub(crate) fn new_window_attach_argv(
    caller_window: &str,
    tmux_bin: &str,
    socket: &Path,
    session: &str,
) -> Vec<String> {
    let attach = format!(
        "env -u TMUX -u TMUX_PANE {} -S {} attach-session -t {}",
        quote(tmux_bin),
        quote(&socket.to_string_lossy()),
        trusty_common::tmux::shell_exact_session_target(session)
    );
    vec![
        "new-window".to_owned(),
        "-a".to_owned(),
        "-t".to_owned(),
        // #8443: an @N id passes through; anything else is pinned exact.
        trusty_common::tmux::exact_window_target(caller_window),
        "-n".to_owned(),
        session.to_owned(),
        attach,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_argv_on_prefixes_only_a_workspace_socket() {
        assert_eq!(
            attach_argv_on(Path::new("/tmp/tmux-501/default"), "tm-a"),
            vec!["attach-session", "-t", "=tm-a"]
        );
        assert_eq!(
            attach_argv_on(Path::new("/tmp/tmux-501/tm-ws-x"), "tm-a"),
            vec![
                "-S",
                "/tmp/tmux-501/tm-ws-x",
                "attach-session",
                "-t",
                "=tm-a"
            ]
        );
    }

    #[test]
    fn on_other_server_reads_the_socket_field_of_tmux() {
        let target = Path::new("/nonexistent/tmux-501/tm-ws-x");
        assert!(!on_other_server(
            Some("/nonexistent/tmux-501/tm-ws-x,123,0"),
            target
        ));
        assert!(on_other_server(
            Some("/nonexistent/tmux-501/default,123,0"),
            target
        ));
        // Unknown is not "another server": today's guarded switch decides.
        assert!(!on_other_server(None, target));
        assert!(!on_other_server(Some(""), target));
        assert_eq!(
            caller_socket(Some("/s/tm-ws-x,1,0")),
            Some(Path::new("/s/tm-ws-x"))
        );
        assert_eq!(caller_socket(Some(",1,0")), None);
    }

    #[test]
    fn a_cross_server_attach_opens_a_window_running_the_attach() {
        let argv = new_window_attach_argv(
            "@7",
            "/opt/homebrew/bin/tmux",
            Path::new("/tmp/tmux-501/tm-ws-x"),
            "tm-a",
        );
        assert_eq!(&argv[..6], ["new-window", "-a", "-t", "@7", "-n", "tm-a"]);
        assert_eq!(
            argv[6],
            "env -u TMUX -u TMUX_PANE '/opt/homebrew/bin/tmux' -S '/tmp/tmux-501/tm-ws-x' \
             attach-session -t '=tm-a'"
        );
        // Never a switch-client across servers.
        assert!(argv.iter().all(|w| !w.contains("switch-client")));
    }

    #[test]
    fn a_cross_server_window_target_is_exact() {
        let socket = Path::new("/tmp/tmux-501/tm-ws-x");
        let by_name = new_window_attach_argv("main", "tmux", socket, "tm-a");
        assert_eq!(by_name[3], "=main:");
        let by_id = new_window_attach_argv("@12", "tmux", socket, "tm-a");
        assert_eq!(by_id[3], "@12");
    }
}
