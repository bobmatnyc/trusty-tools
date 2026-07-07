//! Nested-tmux-safe attach helper shared by `tm launch`, `tm connect`, and the
//! guided picker's resume/restart paths.
//!
//! Why: tmux refuses `attach-session` when invoked from inside an existing
//! tmux client (i.e. `$TMUX` is set), to avoid nesting one tmux client inside
//! another. Every call site that hands the terminal over to a tmux session hit
//! this failure identically ("sessions should be nested with care, unset
//! $TMUX to force" / `Error: tmux attach-session exited with failure").
//! What: [`attach_argv`] is the pure decision function (testable without
//! spawning tmux) that picks `switch-client` vs `attach-session` argv based on
//! whether the caller is already inside tmux; [`inside_tmux`] reads `$TMUX`;
//! [`tmux_attach`] ties them together — it shells out to the resulting tmux
//! subcommand and translates a `switch-client` failure into an actionable
//! hint.
//! Test: `attach_argv_inside_tmux_uses_switch_client`,
//! `attach_argv_outside_tmux_uses_attach_session` cover the pure decision
//! logic directly; `tmux_attach` itself is exercised indirectly by the
//! picker/launch/connect flows (it spawns a real `tmux` process).

use anyhow::Context as _;

/// Build the tmux argv for attaching to `session`, adapting to nesting.
///
/// Why: `tmux attach-session` errors out when `$TMUX` is already set (a
/// nested client), but `switch-client -t <session>` is the documented
/// nested-safe idiom — it moves the *current* client to the target session
/// instead of trying to spawn a new client inside the existing one.
/// What: returns `["switch-client", "-t", session]` when `inside_tmux` is
/// `true`, else `["attach-session", "-t", session]`.
/// Test: `attach_argv_inside_tmux_uses_switch_client`,
/// `attach_argv_outside_tmux_uses_attach_session`.
pub(crate) fn attach_argv(session: &str, inside_tmux: bool) -> Vec<String> {
    let verb = if inside_tmux {
        "switch-client"
    } else {
        "attach-session"
    };
    vec![verb.to_string(), "-t".to_string(), session.to_string()]
}

/// Detect whether the current process is already running inside a tmux client.
///
/// Why: centralizes the `$TMUX` check so every call site uses identical
/// semantics — an empty value (which tmux never sets, but a shell script
/// could export) is treated the same as unset.
/// What: returns `true` when the `TMUX` environment variable is set to a
/// non-empty string.
/// Test: environment-dependent, so it is exercised indirectly; `attach_argv`
/// takes the resulting bool directly so its tests stay hermetic.
pub(crate) fn inside_tmux() -> bool {
    std::env::var("TMUX")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Resolve the tmux session name of the CURRENT client's pane (#2157 item 4).
///
/// Why: the nested-session guard in `guided.rs` needs to know which tmux
/// session bare `tm` is running inside, so it can check whether that session
/// already maps to a managed record BEFORE offering to launch a brand-new
/// (nested) one. `$TMUX`'s own value encodes a socket path and a numeric
/// window/pane index, not the session's NAME, so the name has to come from
/// tmux itself.
/// What: returns `None` immediately when [`inside_tmux`] is `false` (no
/// session to query). Otherwise runs `tmux display-message -p '#S'` and
/// returns the trimmed, non-empty stdout, or `None` on any I/O failure,
/// non-zero exit, or empty output.
/// Test: I/O path, not unit-tested (requires a live tmux server); the guard's
/// decision logic that CONSUMES this value is pure and tested separately
/// (`nested_managed_match_*` in `tests_behavior_c_tests.rs`).
pub(crate) fn current_tmux_session_name() -> Option<String> {
    if !inside_tmux() {
        return None;
    }
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Attach (or switch) the current terminal into the tmux session `name`.
///
/// Why: single choke point for the nested-tmux-safe attach behavior so
/// `guided.rs` and `launch.rs` don't each re-implement the `$TMUX` branch —
/// and so a `switch-client` failure gets a more actionable error than a bare
/// "exited with failure".
/// What: shells out to `tmux <attach_argv(name, inside_tmux())>` and waits;
/// returns `Err` if tmux exits with a non-zero status. When the nested-client
/// `switch-client` path fails, the error mentions that the target session may
/// live on a different tmux server.
/// Test: exercised indirectly by the picker/launch/connect flows; the argv
/// selection itself is unit-tested via `attach_argv`.
pub(crate) fn tmux_attach(name: &str) -> anyhow::Result<()> {
    let inside = inside_tmux();
    let argv = attach_argv(name, inside);
    let verb = argv[0].clone();
    if inside {
        eprintln!("tm: switching client to session '{name}'");
    } else {
        eprintln!("tm: attaching to session '{name}'");
    }
    let status = std::process::Command::new("tmux")
        .args(&argv)
        .status()
        .context("failed to invoke tmux")?;
    if !status.success() {
        if inside {
            anyhow::bail!(
                "tmux {verb} exited with failure — the target session '{name}' may be \
                 attached on a different tmux server; try `tmux -L <socket> switch-client -t \
                 {name}` or detach the other client first"
            );
        }
        anyhow::bail!("tmux {verb} exited with failure");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inside an existing tmux client, attach must use `switch-client` — this
    /// is the nested-safe idiom that moves the current client instead of
    /// spawning a nested one (which tmux refuses).
    ///
    /// Test: this function IS the test.
    #[test]
    fn attach_argv_inside_tmux_uses_switch_client() {
        assert_eq!(
            attach_argv("tmpm-mcp-a-protocol-00f6f5ef", true),
            vec!["switch-client", "-t", "tmpm-mcp-a-protocol-00f6f5ef"]
        );
    }

    /// Outside tmux, attach must keep using `attach-session` — today's
    /// behavior, unchanged for the non-nested case.
    ///
    /// Test: this function IS the test.
    #[test]
    fn attach_argv_outside_tmux_uses_attach_session() {
        assert_eq!(
            attach_argv("my-session", false),
            vec!["attach-session", "-t", "my-session"]
        );
    }
}
