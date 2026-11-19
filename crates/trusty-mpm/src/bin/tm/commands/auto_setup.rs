//! Install and register the trusty-browser and trusty-computer MCP servers.
//!
//! Why: G0b — `tm install` and `tm fleet init` set both servers up, so an
//! operator never runs `cargo install` and `tm mcp add` by hand.
//! What: [`ensure`] installs a missing binary with `cargo install <crate>
//! --locked` (run from a directory outside any workspace, ADR-0043), then
//! registers it at user scope through the core calls `tm mcp add` uses
//! (`mcp_config::build_stdio_entry` + `add_server`). An existing entry is never
//! replaced. [`doctor_rows`] reports each server. Nothing here probes a macOS
//! permission, so no TCC dialog and no Keychain read can occur.
//! Test: `auto_setup_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};
use trusty_mpm::core::mcp_config;

/// The doctor check name.
pub(crate) const CHECK: &str = "auto_setup_mcp";

/// One server tm sets up.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Server {
    /// crates.io crate name.
    pub krate: &'static str,
    /// Binary name; also the `mcpServers` key.
    pub bin: &'static str,
    /// The permission grant the server needs, printed once.
    pub grant: &'static str,
}

/// The servers G0b sets up.
pub(crate) const SERVERS: [Server; 2] = [
    Server {
        krate: "trusty-browser",
        bin: "trusty-browser-mcp",
        grant: "Connect (approve the trusty-browser extension connection)",
    },
    Server {
        krate: "trusty-computer",
        bin: "trusty-computer-mcp",
        grant: "Accessibility and Screen Recording (System Settings > Privacy & Security)",
    },
];

/// The paths and tools one run uses; tests point every field at a temp dir.
#[derive(Debug, Clone)]
pub(crate) struct Env {
    /// `$CARGO_HOME/bin`, where `cargo install` puts binaries.
    pub cargo_bin: PathBuf,
    /// The `cargo` executable, or `None` when none is on `PATH`.
    pub cargo: Option<PathBuf>,
    /// The `.claude.json`-bearing config dir `tm mcp add` writes.
    pub config_dir: PathBuf,
    /// Working directory for `cargo install`, outside any workspace.
    pub install_cwd: PathBuf,
}

impl Env {
    /// The live environment: `CARGO_HOME` (else `~/.cargo`), `cargo` from
    /// there or `PATH`, and tm's managed config dir. `None` without a home.
    pub(crate) fn live() -> Option<Self> {
        let home = dirs::home_dir()?;
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        let cargo_bin = cargo_home.join("bin");
        let cargo = Some(cargo_bin.join("cargo"))
            .filter(|p| p.is_file())
            .or_else(|| trusty_common::bin_resolve::resolve_binary("cargo"));
        Some(Self {
            cargo,
            config_dir: trusty_mpm::core::trusty_tools_config::managed_claude_config_dir()?,
            install_cwd: std::env::temp_dir(),
            cargo_bin,
        })
    }
}

/// How the binary step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Install {
    /// Already in `cargo_bin`.
    Present,
    /// `cargo install` put it there.
    Installed,
    /// Not installed; the reason names why (no cargo, cargo failed).
    Missing(String),
}

/// How the registration step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Register {
    /// A new entry was written.
    Added,
    /// The same entry was already there.
    Unchanged,
    /// A different entry was there and was kept; holds its JSON.
    KeptExisting(String),
    /// Not registered because the binary is missing.
    Skipped,
    /// The config read or write failed.
    Failed(String),
}

/// One server's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// The server.
    pub bin: &'static str,
    /// Binary step.
    pub install: Install,
    /// Registration step.
    pub register: Register,
}

/// Install and register every server in [`SERVERS`].
///
/// Why: G0b — one call from `tm install` and `tm fleet init`.
/// What: per server, [`ensure_binary`] then [`register`]; a failure on one
/// server never stops the other, and nothing here returns an error.
/// Test: `a_second_run_changes_nothing`, `missing_cargo_is_reported_and_registration_skipped`.
pub(crate) fn ensure(env: &Env) -> Vec<Outcome> {
    SERVERS
        .iter()
        .map(|s| {
            let install = ensure_binary(env, s);
            let register = match install {
                Install::Missing(_) => Register::Skipped,
                _ => register(env, s),
            };
            Outcome {
                bin: s.bin,
                install,
                register,
            }
        })
        .collect()
}

/// Make sure `cargo_bin/<bin>` exists, running `cargo install` when it does not.
///
/// What: stdin is null; the child runs in `install_cwd`. A missing cargo, a
/// spawn error, a non-zero exit, or an exit 0 that left no binary is `Missing`.
/// Test: `cargo_install_failure_is_reported`, `fake_cargo_install_then_register`.
fn ensure_binary(env: &Env, s: &Server) -> Install {
    let path = env.cargo_bin.join(s.bin);
    if path.is_file() {
        return Install::Present;
    }
    let Some(cargo) = &env.cargo else {
        return Install::Missing("no `cargo` on PATH".into());
    };
    println!(
        "  installing {} (cargo install {} --locked)",
        s.bin, s.krate
    );
    let status = Command::new(cargo)
        .args(["install", s.krate, "--locked"])
        .current_dir(&env.install_cwd)
        .stdin(Stdio::null())
        .status();
    match status {
        Ok(st) if st.success() && path.is_file() => Install::Installed,
        Ok(st) if st.success() => Install::Missing(format!(
            "`cargo install {}` succeeded but {} is absent",
            s.krate,
            path.display()
        )),
        Ok(st) => Install::Missing(format!("`cargo install {}` failed ({st})", s.krate)),
        Err(e) => Install::Missing(format!("cannot run cargo: {e}")),
    }
}

/// The entry `tm mcp add <bin> <abs path>` writes.
fn entry(env: &Env, s: &Server) -> Value {
    let cmd = env.cargo_bin.join(s.bin);
    mcp_config::build_stdio_entry(&cmd.to_string_lossy(), &[], &Map::new())
}

/// Register one server unless an entry already exists under its name.
///
/// What: an equal entry is `Unchanged`, a different one `KeptExisting`;
/// otherwise `mcp_config::add_server` writes it. Every error is `Failed`.
/// Test: `a_user_edited_entry_is_kept_and_reported`,
/// `an_unwritable_config_is_failed_not_added`.
fn register(env: &Env, s: &Server) -> Register {
    let want = entry(env, s);
    match mcp_config::get_server(&env.config_dir, s.bin) {
        Err(e) => return Register::Failed(format!("{e:#}")),
        Ok(Some(have)) if have == want => return Register::Unchanged,
        Ok(Some(have)) => return Register::KeptExisting(have.to_string()),
        Ok(None) => {}
    }
    match mcp_config::add_server(&env.config_dir, s.bin, want) {
        Ok(_) => Register::Added,
        Err(e) => Register::Failed(format!("{e:#}")),
    }
}

/// One report line per outcome, plus the grant line for a registered server.
///
/// Test: `report_lines_never_call_a_failure_registered`.
pub(crate) fn report_lines(outcomes: &[Outcome]) -> Vec<String> {
    let mut lines = Vec::new();
    for (o, s) in outcomes.iter().zip(SERVERS.iter()) {
        let install = match &o.install {
            Install::Present => "installed".to_string(),
            Install::Installed => "installed now".to_string(),
            Install::Missing(why) => format!("skipped: {why}"),
        };
        let register = match &o.register {
            Register::Added => "registered".to_string(),
            Register::Unchanged => "already registered".to_string(),
            Register::KeptExisting(have) => {
                format!("NOT changed: an existing entry differs and was kept: {have}")
            }
            Register::Skipped => "not registered (binary missing)".to_string(),
            Register::Failed(e) => format!("registration FAILED: {e}"),
        };
        lines.push(format!("{}: {install}; {register}", o.bin));
        if matches!(o.register, Register::Added | Register::Unchanged) {
            lines.push(format!("{}: needs the {} grant", o.bin, s.grant));
        }
    }
    lines
}

/// `tm install` / `tm fleet init` step: run [`ensure`] and print its lines.
///
/// Why: G0b; a failure is printed, never returned, so it aborts nothing.
/// Test: covered through [`ensure`] and [`report_lines`].
pub(crate) fn run_and_print() {
    println!("Setting up trusty-browser and trusty-computer MCP servers");
    let Some(env) = Env::live() else {
        println!("  skipped: no home directory");
        return;
    };
    for line in report_lines(&ensure(&env)) {
        println!("  {line}");
    }
}

/// The doctor rows: per server, installed, registered, command path exists.
///
/// Why: G0b item 3. What: `Ok` when all three hold; `Warn` naming what is
/// missing; `Unknown` when the config cannot be read. Read-only.
/// Test: `doctor_rows_report_each_fact`.
pub(crate) fn doctor_rows(env: &Env) -> Vec<DoctorCheck> {
    SERVERS
        .iter()
        .map(|s| {
            let installed = env.cargo_bin.join(s.bin).is_file();
            let entry = match mcp_config::get_server(&env.config_dir, s.bin) {
                Ok(e) => e,
                Err(e) => {
                    let msg = format!("{}: config unreadable: {e:#}", s.bin);
                    return DoctorCheck::new(CHECK, CheckStatus::Unknown, msg);
                }
            };
            let command = entry
                .as_ref()
                .and_then(|e| e.get("command"))
                .and_then(Value::as_str);
            let cmd_exists = command.is_some_and(|c| Path::new(c).is_file());
            let msg = format!(
                "{}: installed={installed} registered={} command-path-exists={cmd_exists}",
                s.bin,
                entry.is_some()
            );
            let ok = installed && entry.is_some() && cmd_exists;
            let status = if ok {
                CheckStatus::Ok
            } else {
                CheckStatus::Warn
            };
            DoctorCheck::new(CHECK, status, msg)
        })
        .collect()
}

#[cfg(test)]
#[path = "auto_setup_tests.rs"]
mod tests;
