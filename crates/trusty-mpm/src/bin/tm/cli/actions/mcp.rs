//! `tm mcp` user-scope MCP-server registry command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap — see `cli/mod.rs` for the split
//! rationale shared by every `cli/actions/*.rs` module.
//! What: [`McpCmd`] (the `tm mcp add|remove|list|get|test` verbs) and
//! [`McpTransportArg`] (its `-t/--transport` value enum).
//! Test: `cli_parses_mcp_*` in `tests.rs`.

use clap::Subcommand;

/// Verbs for the `tm mcp` user-scope MCP-server registry command group.
///
/// Why: mirrors the `claude mcp add|remove|list|get` UX so operators moving
/// between the stock CLI and tm get the same surface, while pointing writes at
/// tm's managed config dir instead of `~/.claude.json`.
/// What: `Add` upserts a stdio/http/sse server, `Remove` drops one, `List`
/// enumerates them, `Get` shows one. Every verb accepts `--root` to switch from
/// the daemon-managed dir to a standalone root.
/// Test: `cli_parses_mcp_add`, `cli_parses_mcp_add_http`, `cli_parses_mcp_remove`,
/// `cli_parses_mcp_list`, `cli_parses_mcp_get` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum McpCmd {
    /// Add (or replace) a user-scope MCP server.
    ///
    /// stdio (default): `tm mcp add <name> [-e KEY=VAL]... <command> [-- <args>...]`
    /// http/sse:        `tm mcp add <name> -t http [-H "K: V"]... <url>`
    Add {
        /// Server name (the `mcpServers` key).
        name: String,
        /// Transport: `stdio` (local subprocess), `http`, or `sse` (remote).
        #[arg(short = 't', long = "transport", value_enum, default_value_t = McpTransportArg::Stdio)]
        transport: McpTransportArg,
        /// Environment variable for a stdio server (repeatable): `KEY=VALUE`.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// HTTP header for an http/sse server (repeatable): `Name: Value`.
        #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
        header: Vec<String>,
        /// The command/URL followed by any stdio subprocess args, mirroring
        /// `claude mcp add`. The FIRST token is the command (stdio) or URL
        /// (http/sse); the rest are subprocess args (stdio only). A leading `--`
        /// stops flag parsing so hyphen-led args pass through
        /// (e.g. `-- npx -y some-pkg`). All `-t`/`-e`/`-H`/`--root` options must
        /// precede this token.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "COMMAND_OR_URL [ARGS...]"
        )]
        command_and_args: Vec<String>,
        /// Override the managed root (switches to the standalone config dir).
        ///
        /// Note: do NOT use `env = "TRUSTY_MPM_ROOT"` here — see `Register`.
        #[arg(long)]
        root: Option<String>,
    },
    /// Remove a user-scope MCP server by name.
    Remove {
        /// Server name to remove.
        name: String,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// List all user-scope MCP servers in the tm config dir.
    List {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// Show one user-scope MCP server's definition.
    Get {
        /// Server name to show.
        name: String,
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
    /// Verify MCP servers by running a real handshake against each.
    ///
    /// Bare (`tm mcp test`): sweeps every user-scope server unioned with the
    /// three framework built-ins. `tm mcp test <name>`: tests just that server.
    /// stdio servers get a full `initialize` → `tools/list` handshake (reporting
    /// the tool count); http/sse servers get an HTTP reachability check. Exits
    /// non-zero if ANY tested server fails, so it is CI-usable.
    Test {
        /// Optional server name; omit to sweep all servers + built-ins.
        name: Option<String>,
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Override the managed root (switches to the standalone config dir).
        #[arg(long)]
        root: Option<String>,
    },
}

/// Transport choice for `tm mcp add` (clap `ValueEnum` mirror of
/// [`trusty_mpm::core::mcp_config::McpTransport`]).
///
/// Why: clap needs a `ValueEnum` for `-t/--transport`; keeping it a thin mirror
/// of the core enum avoids leaking clap into the library crate.
/// What: `Stdio` | `Http` | `Sse`.
/// Test: `cli_parses_mcp_add_http` exercises the non-default value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum McpTransportArg {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP endpoint.
    Http,
    /// Remote SSE endpoint.
    Sse,
}
