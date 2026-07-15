//! `tm services` workspace daemon inspection command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`ServicesAction`] — `list`/`status`/`port`/`url`/`health`/
//! `log`/`init`/`restart`.
//! Test: `cli_parses_services_*` in `tests.rs`.

use clap::Subcommand;

/// Subcommands for `tm services`.
///
/// Why: each subcommand answers exactly one agent question (port? url? healthy?)
/// so the output is scriptable without parsing a full status block.
/// What: eight variants covering list, status, port, url, health, log, init,
/// and restart. Exit codes follow the spec: 0=ok/running, 1=down/unhealthy,
/// 2=unknown service.
/// Test: `cli_parses_services_*` tests in the `#[cfg(test)]` block.
#[derive(Debug, Subcommand)]
pub(crate) enum ServicesAction {
    /// List all declared services with their current status.
    ///
    /// Exit code: always 0 (list never fails; individual services may be down).
    List {
        /// Output as JSON array instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Show detailed status for one service.
    ///
    /// Exit code: 0 if running, 1 if down, 2 if service name not in manifest.
    Status {
        /// Service name (e.g. trusty-search).
        name: String,
        /// Output as JSON object.
        #[arg(long)]
        json: bool,
    },

    /// Print the port number for a service (scriptable: PORT=$(tm services port X)).
    ///
    /// Prints just the port number on stdout. Exit code: 0 if port known,
    /// 1 if service is down or port unavailable, 2 if unknown service.
    Port {
        /// Service name.
        name: String,
    },

    /// Print the full base URL for a service (e.g. http://localhost:7878).
    ///
    /// Exit code: 0 if URL known, 1 if service is down, 2 if unknown service.
    Url {
        /// Service name.
        name: String,
    },

    /// Probe the health endpoint and print OK or FAIL.
    ///
    /// Prints "OK" on stdout when healthy; diagnostic detail on stderr when
    /// unhealthy. Exit code: 0 if healthy, 1 if unhealthy or down.
    Health {
        /// Service name.
        name: String,
    },

    /// Print the path to the most-recent log file.
    ///
    /// Scriptable: `tail -f $(tm services log trusty-search)`
    /// Exit code: 0 if log path known and file exists, 1 if not, 2 if unknown.
    Log {
        /// Service name.
        name: String,
    },

    /// Write the default manifest to ~/.claude-mpm/services.yaml.
    ///
    /// Non-destructive: errors if the file already exists. Use --force to
    /// overwrite an existing manifest.
    Init {
        /// Overwrite an existing manifest.
        #[arg(long)]
        force: bool,
    },

    /// Restart a service using its manifest `restart_cmd`.
    ///
    /// Exit code: 0 if restart_cmd succeeded, 1 if restart_cmd absent or failed,
    /// 2 if unknown service.
    Restart {
        /// Service name.
        name: String,
    },
}
