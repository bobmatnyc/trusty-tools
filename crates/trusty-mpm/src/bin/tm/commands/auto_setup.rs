//! Install and register the trusty-browser and trusty-computer MCP servers.
//!
//! Why: G0b — `tm install` and `tm fleet init` set both servers up, so an
//! operator never runs `cargo install` and `tm mcp add` by hand.
//! What: [`ensure`] installs a missing binary with `cargo install <crate>
//! --locked` (run from a directory outside any workspace, ADR-0043), then
//! registers it at user scope through the core calls `tm mcp add` uses
//! (`mcp_config::build_stdio_entry` + `add_server`). An existing entry is never
//! replaced. [`doctor_rows`] reports each server. Nothing here probes a macOS
//! permission. #11587: on macOS [`sign_browser`] then runs `tctl sign
//! trusty-browser`, which reads the signing identity from the Keychain.
//! Test: `auto_setup_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};
use trusty_mpm::core::mcp_config;

/// Set to any value to skip [`run_and_print`]; the scratch-HOME integration
/// tests set it so a `tm fleet init` run never spawns a real `cargo install`.
pub(crate) const SKIP_ENV: &str = "TM_SKIP_MCP_AUTO_SETUP";

/// How long one `cargo install` may run before it is killed (10 minutes).
pub(crate) const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// How often the install child is polled for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    /// #11909: whether `krate` is on crates.io. An unpublished server is
    /// optional: `tm install` never runs `cargo install` for it, and the
    /// doctor reports its absence as an info line. Static, like tctl's
    /// `StableMember::internal` (#10130); no runtime crates.io lookup.
    pub published: bool,
}

/// #11909: how to get an unpublished server, named in the install and doctor lines.
pub(crate) const UNPUBLISHED_HOWTO: &str = "optional, not published to crates.io; build it from \
     a trusty-tools source checkout, then run `tm install` to register it";

/// The servers G0b sets up.
pub(crate) const SERVERS: [Server; 2] = [
    Server {
        krate: "trusty-browser",
        bin: "trusty-browser-mcp",
        grant: "Connect (approve the trusty-browser extension connection)",
        published: true,
    },
    Server {
        krate: "trusty-computer",
        bin: "trusty-computer-mcp",
        grant: "Accessibility and Screen Recording (System Settings > Privacy & Security)",
        // #11909: `publish = false` in its Cargo.toml; flip once it publishes.
        published: false,
    },
];

/// The paths and tools one run uses; tests point every field at a temp dir.
#[derive(Debug, Clone)]
pub(crate) struct Env {
    /// `$CARGO_HOME`; passed to `cargo install --root` so the binary lands in
    /// `cargo_bin`, the path registered in the config.
    pub cargo_home: PathBuf,
    /// Longest a `cargo install` may run before it is killed.
    pub install_timeout: Duration,
    /// `$CARGO_HOME/bin`, where `cargo install` puts binaries.
    pub cargo_bin: PathBuf,
    /// The `cargo` executable, or `None` when none is on `PATH`.
    pub cargo: Option<PathBuf>,
    /// The `.claude.json`-bearing config dir `tm mcp add` writes.
    pub config_dir: PathBuf,
    /// Working directory for `cargo install`, outside any workspace.
    pub install_cwd: PathBuf,
    /// #11587: the `tctl` that signs trusty-browser, or `None` when none is found.
    pub tctl: Option<PathBuf>,
}

impl Env {
    /// The live environment: `CARGO_HOME` (else `~/.cargo`), `cargo` from
    /// there or `PATH`, and tm's managed config dir. `None` without a home.
    pub(crate) fn live() -> Option<Self> {
        let home = dirs::home_dir()?;
        // A relative `CARGO_HOME` would resolve against `install_cwd` (the temp
        // dir) for `--root`, so make it absolute here.
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        let cargo_home = std::path::absolute(&cargo_home).unwrap_or(cargo_home);
        let cargo_bin = cargo_home.join("bin");
        let cargo = Some(cargo_bin.join("cargo"))
            .filter(|p| p.is_file())
            .or_else(|| trusty_common::bin_resolve::resolve_binary("cargo"));
        // #11587: the tctl beside the binaries it signs, else the one on PATH.
        let tctl = Some(cargo_bin.join("tctl"))
            .filter(|p| p.is_file())
            .or_else(|| trusty_common::bin_resolve::resolve_binary("tctl"));
        Some(Self {
            tctl,
            cargo_home,
            install_timeout: INSTALL_TIMEOUT,
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
/// What: stdin is null; the child runs in `install_cwd` with `--root
/// <cargo_home>`. It is polled against `install_timeout`; on expiry it is
/// killed and reaped. A missing cargo, a spawn error, a timeout, a non-zero
/// exit, or an exit 0 that left no binary is `Missing`. #11909: an absent
/// unpublished server is `Missing` without spawning cargo.
/// Test: `cargo_install_failure_is_reported`, `fake_cargo_install_then_register`,
/// `a_hung_cargo_install_is_killed_at_the_deadline`,
/// `install_never_runs_cargo_for_an_unpublished_server`.
fn ensure_binary(env: &Env, s: &Server) -> Install {
    let path = env.cargo_bin.join(s.bin);
    if path.is_file() {
        return Install::Present;
    }
    // #11909: never `cargo install` a crate crates.io does not have.
    if !s.published {
        return Install::Missing(UNPUBLISHED_HOWTO.into());
    }
    let Some(cargo) = &env.cargo else {
        return Install::Missing("no `cargo` on PATH".into());
    };
    println!(
        "  installing {} (cargo install {} --locked)",
        s.bin, s.krate
    );
    let mut cmd = Command::new(cargo);
    cmd.args(["install", s.krate, "--locked", "--root"])
        .arg(&env.cargo_home)
        .current_dir(&env.install_cwd)
        .stdin(Stdio::null());
    // Own process group, so a timeout can kill rustc and build scripts too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd.spawn();
    let status = child.and_then(|c| wait_bounded(c, env.install_timeout));
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Install::Missing(format!(
            "cargo install timed out after {} s",
            env.install_timeout.as_secs()
        )),
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

/// Wait for `child` until `limit`; past it, kill and reap it.
///
/// What: polls `try_wait`; on expiry, or on a `try_wait` error, SIGKILLs the
/// child's whole process group (it leads one) and reaps the child. Expiry
/// returns a `TimedOut` error.
/// Test: `a_hung_cargo_install_is_killed_at_the_deadline`,
/// `a_timeout_kills_the_whole_process_group`.
fn wait_bounded(
    mut child: std::process::Child,
    limit: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st),
            Ok(None) => {}
            Err(e) => {
                kill_group_and_reap(&mut child);
                return Err(e);
            }
        }
        if Instant::now() >= deadline {
            kill_group_and_reap(&mut child);
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// SIGKILL `child`'s process group, then reap `child`.
///
/// Why: `child.kill()` signals only cargo; rustc and build scripts survive it.
/// What: the child is its own group leader (`process_group(0)`), so its pid is
/// the pgid. Off unix it falls back to `Child::kill`.
/// Test: `a_timeout_kills_the_whole_process_group`.
fn kill_group_and_reap(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        let _ = killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            Signal::SIGKILL,
        );
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
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
/// Why: G0b; a server failure is printed, never returned, so it aborts
/// nothing. #11587: a failed trusty-browser sign is returned, so the command
/// exits non-zero (code-critic on PR #12137: it exited 0).
/// Test: [`run_with`].
pub(crate) fn run_and_print() -> anyhow::Result<()> {
    if std::env::var_os(SKIP_ENV).is_some() {
        return Ok(());
    }
    println!("Setting up trusty-browser and trusty-computer MCP servers");
    let Some(env) = Env::live() else {
        println!("  skipped: no home directory");
        return Ok(());
    };
    // #11587: codesign and the Keychain are macOS-only.
    run_with(&env, cfg!(target_os = "macos"))
}

/// Run [`ensure`], print its lines, then, with `sign`, [`sign_browser`].
///
/// What: `Err` only when [`sign_browser`] reports a failure; every line is
/// printed first.
/// Test: `a_failed_browser_sign_fails_the_run`,
/// `a_present_browser_is_not_signed_again`.
pub(crate) fn run_with(env: &Env, sign: bool) -> anyhow::Result<()> {
    let outcomes = ensure(env);
    for line in report_lines(&outcomes) {
        println!("  {line}");
    }
    if !sign {
        return Ok(());
    }
    let signing = sign_browser(env, &outcomes);
    for line in &signing.lines {
        println!("  {line}");
    }
    anyhow::ensure!(
        !signing.failed,
        "trusty-browser was installed but not signed; run `tctl sign trusty-browser`"
    );
    Ok(())
}

/// The binaries `cargo install trusty-browser` places, both Keychain readers.
pub(crate) const BROWSER_BINS: [&str; 2] = ["trusty-browser", "trusty-browser-mcp"];

/// What [`sign_browser`] did: one line per present binary, and whether the
/// run must exit non-zero over it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BrowserSigning {
    /// One report line per present browser binary.
    pub lines: Vec<String>,
    /// tctl ran with an identity configured and the sign itself failed.
    pub failed: bool,
}

/// Why the browser binaries are not signed, and whether that fails the run.
struct NotSigned {
    why: String,
    /// `false` when signing could not be attempted: no tctl, a tctl too old
    /// to sign the browser (exit 2), or no identity configured (exit 3).
    fails_run: bool,
}

impl NotSigned {
    fn failure(why: String) -> Self {
        Self {
            why,
            fails_run: true,
        }
    }

    fn warning(why: String) -> Self {
        Self {
            why,
            fails_run: false,
        }
    }
}

/// Sign the trusty-browser binaries this run installed, with `tctl sign
/// trusty-browser`.
///
/// Why (#11587: sign at install only): `tm install` is the
/// path that installs trusty-browser, and before this the binaries stayed
/// ad-hoc until `install-service` / `install-host` signed them. tctl owns
/// every codesign flag and identifier and attests the result fail-closed, so
/// tm duplicates none of it.
/// What: nothing unless this run's `cargo install` put the browser in place
/// (`Install::Installed`): a `Present` binary did not change, so it is not
/// signed again and the identity is not looked up (code-critic on PR
/// #12137). Otherwise runs `<tctl> sign trusty-browser --dir <cargo_bin>`
/// (stdin null, bounded by `install_timeout`), one line per present binary.
/// Only exit 0 reads as signed. No tctl, exit 2 (this tctl predates the
/// target) and exit 3 ("no identity") are warnings; a timeout, a spawn error
/// and any other exit set `failed`.
/// Test: `browser_signing_reports_each_binary_by_tctl_exit_code`,
/// `browser_signing_never_claims_signed_without_tctl_or_a_binary`,
/// `a_present_browser_is_not_signed_again`.
pub(crate) fn sign_browser(env: &Env, outcomes: &[Outcome]) -> BrowserSigning {
    let installed = outcomes
        .iter()
        .any(|o| o.bin == "trusty-browser-mcp" && o.install == Install::Installed);
    let present: Vec<&str> = BROWSER_BINS
        .into_iter()
        .filter(|b| env.cargo_bin.join(b).is_file())
        .collect();
    if !installed || present.is_empty() {
        return BrowserSigning::default();
    }
    let verdict = match &env.tctl {
        // #11587: a clean-config `tm install` exits 0; no tctl only warns.
        None => Err(NotSigned::warning(
            "no `tctl` found; install trusty-installer".to_string(),
        )),
        Some(tctl) => run_tctl_sign(env, tctl),
    };
    let lines = present
        .into_iter()
        .map(|b| match &verdict {
            Ok(()) => format!("{b}: signed with Developer ID and attested (tctl sign)"),
            Err(n) => format!(
                "{b}: NOT signed: {}; then run `tctl sign trusty-browser`",
                n.why
            ),
        })
        .collect();
    BrowserSigning {
        lines,
        failed: verdict.as_ref().is_err_and(|n| n.fails_run),
    }
}

/// Run `tctl sign trusty-browser --dir <cargo_bin>`; `Err` says why it failed.
fn run_tctl_sign(env: &Env, tctl: &Path) -> Result<(), NotSigned> {
    let mut cmd = Command::new(tctl);
    cmd.args(["sign", "trusty-browser", "--dir"])
        .arg(&env.cargo_bin)
        .stdin(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let status = cmd
        .spawn()
        .and_then(|c| wait_bounded(c, env.install_timeout));
    match status {
        Ok(st) if st.success() => Ok(()),
        Ok(st) if st.code() == Some(3) => Err(NotSigned::warning(
            "no Developer ID Application identity is configured (install the \
             certificate or set TRUSTY_SIGN_IDENTITY)"
                .to_string(),
        )),
        // #11587: a tctl too old to sign the browser only warns, like no tctl.
        Ok(st) if st.code() == Some(2) => Err(NotSigned::warning(
            "this tctl has no `trusty-browser` sign target; upgrade trusty-installer".to_string(),
        )),
        Ok(st) => Err(NotSigned::failure(format!(
            "`tctl sign trusty-browser` failed ({st})"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(NotSigned::failure(format!(
            "`tctl sign trusty-browser` timed out after {} s",
            env.install_timeout.as_secs()
        ))),
        Err(e) => Err(NotSigned::failure(format!("cannot run tctl: {e}"))),
    }
}

/// Read one `mcpServers` entry without ever writing or renaming the config.
///
/// Why: the doctor must be read-only; `mcp_config::get_server` quarantines a
/// malformed `.claude.json`. What: a missing file is `Ok(None)`; a read error,
/// a parse failure or a non-object root is `Err`.
/// Test: `doctor_rows_leave_a_malformed_config_untouched`.
fn read_entry_readonly(config_dir: &Path, name: &str) -> Result<Option<Value>, String> {
    let path = config_dir.join(".claude.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let root: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    if !root.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok(root.get("mcpServers").and_then(|m| m.get(name)).cloned())
}

/// The doctor rows: per server, installed, registered, command path exists.
///
/// Why: G0b item 3. What: `Ok` when all three hold; `Warn` naming what is
/// missing; `Unknown` when the config cannot be read. #11909: an unpublished
/// server with no binary and no entry is an info line (`Ok`, message starting
/// `info:`) naming how to get it. Read-only.
/// Test: `doctor_rows_report_each_fact`,
/// `doctor_lists_an_absent_unpublished_server_as_optional`.
pub(crate) fn doctor_rows(env: &Env) -> Vec<DoctorCheck> {
    SERVERS
        .iter()
        .map(|s| {
            let installed = env.cargo_bin.join(s.bin).is_file();
            let entry = match read_entry_readonly(&env.config_dir, s.bin) {
                Ok(e) => e,
                Err(e) => {
                    let msg = format!("{}: config unreadable: {e}", s.bin);
                    return DoctorCheck::new(CHECK, CheckStatus::Unknown, msg);
                }
            };
            let command = entry
                .as_ref()
                .and_then(|e| e.get("command"))
                .and_then(Value::as_str);
            let cmd_exists = command.is_some_and(|c| Path::new(c).is_file());
            // #11909: an optional server the host never set up is not a fault.
            if !s.published && !installed && entry.is_none() {
                let msg = format!("info: {}: not installed ({UNPUBLISHED_HOWTO})", s.bin);
                return DoctorCheck::new(CHECK, CheckStatus::Ok, msg);
            }
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
