//! `trusty-review` binary entry point.
//!
//! Why: provides the CLI surface for trusty-review. Stage 1 scaffolds only the
//! `--version` and `--help` flags so the binary compiles and the workspace
//! picks up the crate.  Full CLI subcommands (`run`, `serve`, `compare`, etc.)
//! land in Stage 3.
//!
//! What: parses `--version` / `--help` via a minimal clap parser and exits.
//! All work is deferred to the library crates.
//!
//! Test: basic smoke-test is `trusty-review --version`; full subcommand tests
//! land in Stage 3.

use anyhow::Result;

fn main() -> Result<()> {
    // Initialise tracing to stderr (never stdout — stdout is reserved for
    // future MCP JSON-RPC framing).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // Minimal CLI: only --version / --help at this stage.
    // Full clap dispatch (run, serve, compare, eval) lands in Stage 3.
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(|s| s.as_str()).unwrap_or("trusty-review");

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", prog, env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--help" || a == "-h") || args.len() <= 1 {
        eprintln!("{prog} {}", env!("CARGO_PKG_VERSION"));
        eprintln!("Fast local PR-review service for trusty-tools.");
        eprintln!();
        eprintln!("USAGE:");
        eprintln!("    {prog} [SUBCOMMAND]");
        eprintln!();
        eprintln!("Note: full CLI subcommands (run, serve, compare, eval) land in Stage 3.");
        return Ok(());
    }

    anyhow::bail!("unknown subcommand — full CLI lands in Stage 3")
}
