//! `trusty-browser-mcp`: the stdio MCP server; all logic is in
//! [`trusty_browser::mcp_cli`].

fn main() -> std::process::ExitCode {
    trusty_browser::mcp_cli::main()
}
