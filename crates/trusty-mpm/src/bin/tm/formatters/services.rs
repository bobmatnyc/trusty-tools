//! Output formatters for the `tm services` subcommand.
//!
//! Why: the services list and status-block printers are display-only helpers
//! that should live outside the handler to keep handler files under the
//! 500-line cap.
//! What: `print_services_table` and `print_service_status_block` render
//! `ServiceStatus` values into human-readable terminal output.
//! Test: exercised indirectly by manual smoke tests; the output shape is
//! verified via `cli_parses_services_*` parse tests in `tests.rs`.

/// Render the `tm services list` human-readable table.
///
/// Why: a fixed-width table is more readable than JSON in an interactive terminal
/// while still being parseable by humans scanning for red/green status indicators.
/// What: prints a header row then one row per `ServiceStatus` using fixed-width
/// columns. Status is colorized green/red/yellow via the `colored` crate.
/// Test: indirect — verified by manual smoke test.
pub(crate) fn print_services_table(statuses: &[trusty_mpm::services::ServiceStatus]) {
    use colored::Colorize;
    use trusty_mpm::services::HealthState;

    const W_NAME: usize = 24;
    const W_STATUS: usize = 10;
    const W_PORT: usize = 8;
    const W_VERSION: usize = 12;

    println!(
        "{:<W_NAME$} {:<W_STATUS$} {:<W_PORT$} {:<W_VERSION$} HEALTH",
        "NAME", "STATUS", "PORT", "VERSION"
    );

    for s in statuses {
        let status_str = if s.running { "running" } else { "down" };
        let status_colored = if s.running {
            status_str.green().to_string()
        } else {
            status_str.red().to_string()
        };

        let port_str = s
            .port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "\u{2014}".to_string()); // —

        let version_str = s.version.clone().unwrap_or_else(|| "\u{2014}".to_string());

        let health_str = match &s.health {
            HealthState::Ok => "ok".green().to_string(),
            HealthState::Unknown => "\u{2014}".normal().to_string(),
            HealthState::Fail { .. } => {
                if s.running {
                    "fail".yellow().to_string()
                } else {
                    "\u{2014}".normal().to_string()
                }
            }
        };

        println!(
            "{:<W_NAME$} {:<W_STATUS$} {:<W_PORT$} {:<W_VERSION$} {}",
            s.name, status_colored, port_str, version_str, health_str
        );
    }
}

/// Render the `tm services status <name>` detail block.
///
/// Why: a labeled block is more readable than a JSON object for interactive use,
/// making it easy to scan all fields at a glance.
/// What: prints one label-colon-value line per `ServiceStatus` field, with
/// tilde-contracted paths for readability.
/// Test: indirect — verified by manual smoke test.
pub(crate) fn print_service_status_block(s: &trusty_mpm::services::ServiceStatus) {
    use trusty_mpm::services::HealthState;

    println!("{}", s.name);
    println!("  Status:   {}", if s.running { "running" } else { "down" });
    println!(
        "  PID:      {}",
        s.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "\u{2014}".to_string())
    );
    println!(
        "  Port:     {}",
        s.port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "\u{2014}".to_string())
    );
    println!("  URL:      {}", s.url.as_deref().unwrap_or("\u{2014}"));
    println!("  Version:  {}", s.version.as_deref().unwrap_or("\u{2014}"));
    let health_label = match &s.health {
        HealthState::Ok => "ok".to_string(),
        HealthState::Unknown => "unknown (no health endpoint)".to_string(),
        HealthState::Fail { detail } => format!("fail ({detail})"),
    };
    println!("  Health:   {health_label}");
    println!(
        "  Log:      {}",
        s.log_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "\u{2014}".to_string())
    );
    if let Some(secs) = s.uptime_secs {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        println!("  Uptime:   {hours}h {mins}m");
    } else {
        println!("  Uptime:   \u{2014}");
    }
}
