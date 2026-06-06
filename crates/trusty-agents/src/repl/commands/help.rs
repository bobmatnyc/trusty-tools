//! Slash-command help text, split from `commands.rs` (#357).

use std::fmt::Write as _;

/// Print the slash-command reference.
pub(crate) fn write_help(out: &mut String) {
    let _ = writeln!(
        out,
        "trusty-agents REPL — slash commands

  Navigation
    /connect <path> <adapter> [name]
                         Create-or-reuse a TM project config and spawn a
                         `<name>-<adapter>-<serial>` tmux session.
                         adapters: claude-mpm, claude-code, codex, augment,
                         gemini, trusty-agents, shell
    /cd <path>           Switch the REPL's project context (.trusty-agents root)
                         without spawning a tmux session
    /projects            Show current project + how to switch

  Information
    /version             Build version and number
    /status              Controller liveness
    /session             Session ID, socket, project path
    /agent [<name>]      Switch persona, or list assistant agents
    /switch [<name>]     Switch front-end voice (ctrl | Izzie | CTO Assistant)
    /agents              List available agents
    /skills              List available skills
    /memories [query]    Search the memory store
    /history [N]         Show last N REPL input history entries (default 10)

  Actions
    /run <file>          Forward task from a file
    /log [N]             Tail last N lines of perf log (default 20)
    /logs                Tail last 20 chat-log entries (today)
    /telegram [cmd]      Telegram bot gateway (start|stop|status|pair)
                         `pair` issues a one-time code shown only in the REPL
                         to authorize a Telegram chat (#334).
    /slack [cmd]         Slack bot gateway (start|stop|status|pair)
                         `pair` issues a one-time code shown only in the REPL
                         to authorize a Slack channel (#452).
    /tm <subcmd>         Tmux session manager (try `/tm help`)
    /service [start|stop|status]  Manage persistent --serve daemon (#343)
    /clear               Clear terminal and reset conversation history
    /update              Check GitHub for a newer release and upgrade in place (#368)
    /exit | /quit | /disconnect  Quit  (Ctrl-D also works)
                         `/disconnect` is preferred when attached to a server session —
                         same effect, clearer intent (server keeps running).

  Session Management (run from terminal, not REPL)
    om start             Start the API server daemon
    om stop              Stop the API server daemon
    om status            Show server status (port, PID, uptime)
    om connect <path>    Register a project with the running server
    om session new       --project <path> --name <name> [--agent <agent>] [--worktree]
    om session list      [<project-path>]
    om session attach    <session-id>
    om session kill      <session-id>

  Routing (session-scoped overrides)
    /provider [<name>]   Show or set credential routing (openrouter|claude-code|bedrock|local|reset)
                         `local` probes a running ollama (OLLAMA_HOST or http://localhost:11434)
    /model [<id>]        Show or set model id for this session (or reset)
    /local [on|off|test] Local Ollama fast-path status / control (#319)

Type any other text to send it as a task to the PM controller."
    );
}
