//! `services` command handler.
//!
//! Why: service discovery is a self-contained subsystem — YAML manifest,
//! pgrep probing, health checks, log path lookup — and benefits from its own
//! file.
//! What: `services` dispatcher, `claude_mpm_dir` helper; display helpers are
//! in `formatters/services.rs`.
//! Test: `cli_parses_services_*` in `tests.rs`; live probing is covered by
//! `tests/services_integration.rs`.

use crate::cli::ServicesAction;
use crate::formatters::services::{print_service_status_block, print_services_table};

/// `services` subcommand — inspect and probe workspace service daemons.
///
/// Why: agents need a canonical, scriptable interface to replace ad-hoc `lsof`/
/// `curl`/`ps` service discovery. `tm services` reads (or embeds) a YAML manifest
/// and probes each declared service with pgrep + optional HTTP health checks.
/// What: dispatches all `ServicesAction` variants; exit codes follow the spec
/// (0=running/healthy, 1=down/unhealthy, 2=unknown service). List always exits 0.
/// Test: `cli_parses_services_*` unit tests cover argument parsing; the
/// integration test in `tests/services_integration.rs` covers live probing.
pub(crate) fn services(action: ServicesAction) -> anyhow::Result<()> {
    use trusty_mpm::services::{Discoverer, HealthState, ServicesManifest, load_user_manifest};

    // Resolve the manifest: user file if present, otherwise embedded default.
    // #9543: a retired trusty-search entry is mapped on read, with one WARN on
    // stderr; the user's file is never written.
    let services_yaml_path = claude_mpm_dir().join("services.yaml");
    let manifest = load_user_manifest(&services_yaml_path, &mut std::io::stderr())?;

    let mut discoverer = Discoverer::new(manifest.clone());

    match action {
        ServicesAction::List { json } => {
            let statuses = discoverer.list();
            if json {
                println!("{}", serde_json::to_string_pretty(&statuses)?);
            } else {
                print_services_table(&statuses);
            }
            // list always exits 0.
        }

        ServicesAction::Status { name, json } => match discoverer.status(&name) {
            None => {
                eprintln!("unknown service: {name}");
                std::process::exit(2);
            }
            Some(status) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    print_service_status_block(&status);
                }
                if !status.running {
                    std::process::exit(1);
                }
            }
        },

        // #9543: a socket-only service has no port or URL; the error names
        // its socket.
        ServicesAction::Port { name } => match discoverer.port(&name) {
            None => {
                eprintln!("unknown service: {name}");
                std::process::exit(2);
            }
            Some(Ok(port)) => print!("{port}"),
            Some(Err(reason)) => {
                eprintln!("{reason}");
                std::process::exit(1);
            }
        },

        ServicesAction::Url { name } => match discoverer.url(&name) {
            None => {
                eprintln!("unknown service: {name}");
                std::process::exit(2);
            }
            Some(Ok(url)) => print!("{url}"),
            Some(Err(reason)) => {
                eprintln!("{reason}");
                std::process::exit(1);
            }
        },

        ServicesAction::Health { name } => match discoverer.health(&name) {
            None => {
                eprintln!("unknown service: {name}");
                std::process::exit(2);
            }
            Some(result) => match result.state {
                HealthState::Ok => {
                    println!("OK");
                }
                HealthState::Unknown => {
                    println!("OK (no health endpoint; process running)");
                }
                HealthState::Fail { ref detail } => {
                    eprintln!("FAIL: {detail}");
                    std::process::exit(1);
                }
            },
        },

        ServicesAction::Log { name } => match discoverer.status(&name) {
            None => {
                eprintln!("unknown service: {name}");
                std::process::exit(2);
            }
            Some(status) => match status.log_path {
                Some(path) => print!("{}", path.display()),
                None => {
                    eprintln!("{name}: log path unavailable or file does not exist");
                    std::process::exit(1);
                }
            },
        },

        ServicesAction::Init { force } => {
            let path = services_yaml_path;
            if path.exists() && !force {
                anyhow::bail!(
                    "{} already exists. Use --force to overwrite.",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Serialize the embedded default manifest back to YAML.
            let default = ServicesManifest::default_manifest();
            let yaml = serde_yaml::to_string(&default)?;
            std::fs::write(&path, yaml)?;
            println!("wrote {}", path.display());
        }

        ServicesAction::Restart { name } => {
            // Look up the service in the manifest directly (no probe needed for
            // restart). #9543: the manifest already loaded above, not a re-read.
            match manifest.services.get(&name) {
                None => {
                    eprintln!("unknown service: {name}");
                    std::process::exit(2);
                }
                Some(decl) => match &decl.restart_cmd {
                    None => {
                        eprintln!("{name}: no restart_cmd defined in manifest");
                        std::process::exit(1);
                    }
                    Some(cmd) => {
                        let status = std::process::Command::new("sh")
                            .args(["-c", cmd])
                            .status()?;
                        if status.success() {
                            println!("{name}: restarted");
                        } else {
                            eprintln!("{name}: restart command failed (exit {status})");
                            std::process::exit(1);
                        }
                    }
                },
            }
        }
    }

    Ok(())
}

/// Resolve `~/.claude-mpm/` directory (not expanded by the shell in library code).
///
/// Why: `tm services init` and the manifest loader both need the canonical
/// `~/.claude-mpm/` path without relying on shell tilde expansion.
/// What: joins `dirs::home_dir()` with `.claude-mpm`. Falls back to the
/// process cwd when the home directory is unavailable (rare in practice).
/// Test: indirect — exercised by `cli_parses_services_init`.
pub(crate) fn claude_mpm_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude-mpm")
}
