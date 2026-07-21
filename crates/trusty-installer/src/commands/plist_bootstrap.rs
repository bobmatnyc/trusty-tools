//! macOS launchd plist bootstrap for the trusty-mpm supervisor (#1206, Phase 7).
//!
//! Why: The supervisor plist must be bootstrapped after trusty-mpm is installed
//! so the daemon starts at login and restarts after a crash. Doing it in the
//! installer gives operators a zero-config path; they do not need to follow the
//! README sed recipe manually.
//!
//! What: Embeds the plist template from `deploy/supervisor/`, fills the
//! `__HOME__` and `__TM_BINARY_PATH__` placeholders at runtime, writes the plist
//! to `~/Library/LaunchAgents/`, and bootstraps it with `launchctl`. On non-macOS
//! platforms the function is a no-op and prints a hint toward the systemd unit.
//! Idempotent: if the label is already loaded it is booted out first.
//!
//! `#3527` hardening — this used to run UNCONDITIONALLY whenever trusty-mpm
//! installed, the one member exempt from the #2556 `plans_service_bootstrap`
//! opt-out, and with no protection against clobbering a newer live daemon with
//! an older one:
//! 1. The caller (`install.rs`) now gates the call itself behind the same
//!    `--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP` decision every other daemon
//!    honours (see `install::plans_mpm_supervisor_bootstrap`); when skipped,
//!    this module is never invoked and never touches launchd.
//! 2. [`decide_downgrade`] refuses to replace an already-registered supervisor
//!    with an older-or-equal version unless `force` is set — see
//!    [`install_mpm_supervisor`]'s `force` parameter.
//!
//! `#3551` hardening — the #3527 escape hatch above was two INDEPENDENT
//! process-global env vars: one redirected the resolved home directory, the
//! other separately skipped the `launchctl` subprocess calls. `resolve_uid()`
//! (the `gui/<uid>` domain) was not namespaced by either — it always shelled
//! out to the real `id -u`. A test (or a sandboxed E2E) that set only the
//! home override, believing that was "isolated", still bootstrapped into the
//! real, live `gui/<real-uid>` domain, because the domain and the launchctl
//! calls were never actually gated by the home override at all. That is
//! precisely what #3551 reported: `$HOME` isolation could not, by
//! construction, isolate the launchd bootstrap.
//!
//! The fix replaces both env vars with one injected parameter,
//! [`SupervisorTarget`], bundling the three things that must always move
//! together: `home` (plist/log paths), `domain` (the `launchctl` target,
//! `gui/<uid>` in production), and `launchctl` (a [`LaunchctlPort`] —
//! [`RealLaunchctl`] shells out for real, [`StubLaunchctl`] records calls and
//! never spawns a process). [`install_mpm_supervisor_for`] takes this
//! parameter explicitly, so a caller cannot accidentally isolate only one of
//! the three and leave the domain live — there is no env var to forget to
//! set, and no shared mutable process state a parallel test could race.
//! [`install_mpm_supervisor`] (the production entry point `install.rs` calls)
//! builds [`SupervisorTarget::production`] and is otherwise unchanged.
//!
//! Test: `tests` covers the placeholder replacement, the downgrade-guard
//! decision table, the registered-binary-path parser, and (via
//! [`StubLaunchctl`]) the full write path — all as pure functions with no
//! subprocess ever spawned in-test.

use std::path::{Path, PathBuf};

/// The plist label for the trusty-mpm supervisor.
///
/// Why: Used consistently for `launchctl list`/`bootstrap`/`bootout` so all
/// callers reference one constant rather than a magic string.
///
/// What: The label as it appears in the plist `<key>Label</key>` entry.
///
/// Test: `tests::label_constant_matches_plist`.
pub const PLIST_LABEL: &str = "com.trusty.mpm.supervisor";

/// Abstraction over the two `launchctl` subcommands this module drives, so
/// the supervisor bootstrap can be exercised in tests against a stub that
/// never spawns the real `launchctl` binary and never touches the live
/// `gui/<uid>` domain (#3551).
///
/// Why: a parameter-injected trait removes the env-mutation hazard the old
/// `SKIP_LAUNCHCTL_ENV` had — a test simply constructs a [`StubLaunchctl`]
/// and passes it via [`SupervisorTarget`]; there is no process-global flag to
/// remember to set (or unset, or race with another test over).
///
/// What: [`bootout`](LaunchctlPort::bootout) mirrors `launchctl bootout
/// <domain> <plist_path>` (idempotent unload — the caller always ignores the
/// result, since it may legitimately fail when nothing is loaded yet).
/// [`bootstrap`](LaunchctlPort::bootstrap) mirrors `launchctl bootstrap
/// <domain> <plist_path>`, returning `Err(stderr)` on a nonzero exit or spawn
/// failure.
///
/// Test: exercised by every `tests::install_mpm_supervisor_for_*` test via
/// [`StubLaunchctl`]; [`RealLaunchctl`] is exercised only by a real install.
pub trait LaunchctlPort {
    /// `launchctl bootout <domain> <plist_path>`.
    fn bootout(&self, domain: &str, plist_path: &Path);
    /// `launchctl bootstrap <domain> <plist_path>`.
    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), String>;
}

/// Production [`LaunchctlPort`]: shells out to the real `launchctl` binary.
///
/// Test: not exercised directly by tests (it IS the thing #3551 forbids
/// tests from calling) — every test uses [`StubLaunchctl`] instead.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealLaunchctl;

impl LaunchctlPort for RealLaunchctl {
    fn bootout(&self, domain: &str, plist_path: &Path) {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", domain, plist_path.to_str().unwrap_or("")])
            .output();
    }

    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), String> {
        let out = std::process::Command::new("launchctl")
            .args(["bootstrap", domain, plist_path.to_str().unwrap_or("")])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
}

/// Test-only [`LaunchctlPort`]: records every call it receives and never
/// spawns a process (#3551).
///
/// Why: proves — by construction, not by inference from an absence of
/// crashes or from diffing the live supervisor before/after — that a test
/// run can never reach the real launchd domain: this type has no code path
/// that can invoke `Command::new("launchctl")` at all.
///
/// What: `calls()` returns every `"bootout <domain> <path>"` /
/// `"bootstrap <domain> <path>"` invocation in order, for tests that assert
/// on exactly what would have been sent to launchd. `fail_bootstrap`, when
/// set, makes `bootstrap` return that error instead of `Ok(())` (for
/// exercising the bootstrap-failure narration path in `install.rs`).
///
/// Test: `tests::install_mpm_supervisor_for_writes_plist_and_never_touches_real_launchctl`
/// and the other `install_mpm_supervisor_for_*` tests.
#[derive(Debug, Default)]
pub struct StubLaunchctl {
    calls: std::sync::Mutex<Vec<String>>,
    /// When `Some`, `bootstrap` returns this as `Err` instead of `Ok(())`.
    pub fail_bootstrap: Option<String>,
}

impl StubLaunchctl {
    /// A stub whose `bootstrap` always succeeds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call recorded so far, in order, as `"bootout <domain> <path>"` /
    /// `"bootstrap <domain> <path>"` strings.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl LaunchctlPort for StubLaunchctl {
    fn bootout(&self, domain: &str, plist_path: &Path) {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("bootout {domain} {}", plist_path.display()));
    }

    fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("bootstrap {domain} {}", plist_path.display()));
        match &self.fail_bootstrap {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

/// The full injected target for [`install_mpm_supervisor_for`]: where the
/// plist/log files land (`home`), which launchd domain to bootstrap into
/// (`domain`), and how to actually talk to launchd (`launchctl`) (#3551).
///
/// Why bundle all three: `home` and `domain` must always be overridden
/// together for isolation to hold — the #3551 bug was exactly a home
/// override that was NOT paired with a domain/launchctl override, so the
/// (correctly) redirected plist was still bootstrapped into the real,
/// live `gui/<real-uid>` domain. One struct makes "override one, forget the
/// other" impossible to express.
///
/// What: [`SupervisorTarget::production`] resolves the real values: the real
/// home directory, `gui/<real uid>`, and [`RealLaunchctl`]. Tests construct
/// one directly with a tempdir `home`, a synthetic `domain` string, and a
/// [`StubLaunchctl`].
///
/// Test: `tests::install_mpm_supervisor_for_*`.
pub struct SupervisorTarget<'a> {
    /// Home directory used to resolve the plist path, the log directory, and
    /// the `__HOME__` template token.
    pub home: PathBuf,
    /// The `launchctl` domain, e.g. `gui/501` in production.
    pub domain: String,
    /// How to actually talk to launchd.
    pub launchctl: &'a dyn LaunchctlPort,
}

impl<'a> SupervisorTarget<'a> {
    /// The real, live production target: the real home directory,
    /// `gui/<real uid>`, and [`RealLaunchctl`].
    ///
    /// Test: not exercised directly (it IS the live target #3551 exists to
    /// keep tests away from) — covered indirectly by
    /// `tests::resolve_uid_returns_nonzero_on_real_system`.
    pub fn production(launchctl: &'a RealLaunchctl) -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow::anyhow!("cannot determine home directory for plist bootstrap")
        })?;
        Ok(Self {
            home,
            domain: format!("gui/{}", resolve_uid()),
            launchctl,
        })
    }
}

/// The embedded plist template with `__HOME__` and `__TM_BINARY_PATH__` tokens.
///
/// Why: The installer binary has no access to the workspace at runtime; we must
/// embed the template at compile time. This const is populated from the plist
/// file in the repository (read at coding time, embedded verbatim).
///
/// What: A `&str` holding the full plist XML with the two placeholder tokens
/// that `fill_template` will replace at runtime.
///
/// Test: `tests::template_contains_placeholders`.
pub const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.trusty.mpm.supervisor</string>

    <key>ProgramArguments</key>
    <array>
        <string>__TM_BINARY_PATH__</string>
        <string>supervisor</string>
    </array>

    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:__HOME__/.local/bin:__HOME__/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>TRUSTY_MPM_SUPERVISOR_INTERVAL</key>
        <string>30</string>
        <key>TRUSTY_MPM_SUPERVISOR_ADDR</key>
        <string>127.0.0.1:7881</string>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>

    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>

    <key>StandardOutPath</key>
    <string>__HOME__/.trusty-mpm/logs/supervisor.out.log</string>
    <key>StandardErrorPath</key>
    <string>__HOME__/.trusty-mpm/logs/supervisor.err.log</string>

    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>"#;

/// Fill the plist template with the given home directory and `tm` binary path.
///
/// Why: Extracting the fill step as a pure function makes it trivially testable
/// without touching the filesystem or running launchctl.
///
/// What: Replaces all occurrences of `__HOME__` with `home` and
/// `__TM_BINARY_PATH__` with `tm_path`. Asserts (in debug) that no placeholder
/// tokens remain after the substitution.
///
/// Test: `tests::fill_template_replaces_all_tokens`.
pub fn fill_template(home: &str, tm_path: &str) -> String {
    let filled = PLIST_TEMPLATE
        .replace("__HOME__", home)
        .replace("__TM_BINARY_PATH__", tm_path);
    debug_assert!(
        !filled.contains("__HOME__") && !filled.contains("__TM_BINARY_PATH__"),
        "unfilled placeholder tokens remain after fill_template"
    );
    filled
}

/// Install the trusty-mpm supervisor plist and bootstrap it with launchd.
///
/// Why: After trusty-mpm is installed the supervisor plist must be loaded so
/// the daemon starts at login and restarts on exit. The installer performs this
/// step so the operator does not need to follow the README manually.
///
/// What: On macOS only (cfg gate): builds [`SupervisorTarget::production`]
/// and delegates to [`install_mpm_supervisor_for`], which fills the
/// template, creates the log directory, applies the [`decide_downgrade`]
/// guard against a previously-registered plist, writes the plist to
/// `~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist`, and runs
/// `launchctl bootstrap gui/<uid> <plist>` (booting out first if the label is
/// already loaded). On non-macOS: prints a short hint toward the systemd unit
/// and returns Ok(()).
///
/// `force` (#3527) bypasses the downgrade guard — see [`decide_downgrade`].
/// The caller (`install::install_all`) is responsible for gating whether this
/// function is called at all (`--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP`);
/// this function itself never re-checks that opt-out.
///
/// `tm_path` (#3554) is the CONCRETE, already-known path of the `tm` binary
/// this install just placed on disk (`install::install_one`'s return value).
/// It is used verbatim as the CANDIDATE side of the [`decide_downgrade`]
/// version comparison and embedded in the plist's `ProgramArguments`.
/// Previously this function re-derived the path itself via
/// [`resolve_tm_binary`], which tries a bare-name `which tm` FIRST — exactly
/// the #3554 mechanism: on a host with an earlier-PATH `~/.cargo/bin/tm`,
/// that lookup silently returned the STALE binary as the install's own
/// "candidate" version, so the downgrade guard compared the old version
/// against itself and refused to update the supervisor. Taking the path as a
/// parameter instead of a lookup makes this call site immune to PATH order.
///
/// Test: not exercised directly (it IS the live production path #3551 exists
/// to keep tests away from) — [`install_mpm_supervisor_for`], which holds
/// every bit of this function's logic, is covered by
/// `tests::install_mpm_supervisor_for_*` via [`StubLaunchctl`].
pub fn install_mpm_supervisor(force: bool, tm_path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let launchctl = RealLaunchctl;
        let target = SupervisorTarget::production(&launchctl)?;
        install_mpm_supervisor_for(&target, force, tm_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (force, tm_path);
        eprintln!(
            "trusty-mpm supervisor: on Linux, install the systemd unit \
             at `crates/trusty-mpm/deploy/supervisor/trusty-mpm-supervisor.service`."
        );
        Ok(())
    }
}

/// The downgrade-guard verdict (#3527).
///
/// Why: a pure enum keeps [`decide_downgrade`] trivially unit-testable
/// independent of the filesystem/subprocess calls that gather its inputs.
/// What: [`DowngradeDecision::Proceed`] — safe to write the new plist and
/// (re-)bootstrap; [`DowngradeDecision::Refuse`] — leave the existing
/// registration untouched.
/// Test: `tests::decide_downgrade_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DowngradeDecision {
    /// Safe to replace the registered supervisor.
    Proceed,
    /// The candidate is not newer than what is currently registered — refuse.
    Refuse,
}

/// Decide whether replacing a registered supervisor with `candidate` is safe.
///
/// Why: THE #3527 safety property — `tctl install` must never silently
/// downgrade a live trusty-mpm supervisor (e.g. a stale GitHub release
/// clobbering a newer crates.io install). Encoding the comparison as one pure
/// function makes it exhaustively testable without a real plist or subprocess.
///
/// What:
/// - `force` → always [`DowngradeDecision::Proceed`] (explicit operator
///   override).
/// - `current` is `None` (nothing registered yet, or its version could not be
///   determined) → [`DowngradeDecision::Proceed`] — there is nothing to guard
///   against.
/// - Both `current` and `candidate` parse as [`semver::Version`] and
///   `candidate <= current` → [`DowngradeDecision::Refuse`] ("older-or-equal").
/// - Otherwise (candidate is strictly newer, or either string fails to parse
///   as semver and no comparison is possible) → [`DowngradeDecision::Proceed`].
///   An unparseable version means we cannot prove this IS a downgrade, so we
///   fail open rather than block a legitimate install on a version-string
///   quirk — the guard exists to catch the *provable* downgrade case.
///
/// Test: `tests::decide_downgrade_force_always_proceeds`,
/// `tests::decide_downgrade_no_current_proceeds`,
/// `tests::decide_downgrade_newer_proceeds`,
/// `tests::decide_downgrade_older_refuses`,
/// `tests::decide_downgrade_equal_refuses`,
/// `tests::decide_downgrade_unparseable_proceeds`.
pub fn decide_downgrade(
    current: Option<&str>,
    candidate: Option<&str>,
    force: bool,
) -> DowngradeDecision {
    if force {
        return DowngradeDecision::Proceed;
    }
    let (Some(current), Some(candidate)) = (current, candidate) else {
        return DowngradeDecision::Proceed;
    };
    let parsed = (
        semver::Version::parse(current.trim_start_matches('v')),
        semver::Version::parse(candidate.trim_start_matches('v')),
    );
    match parsed {
        (Ok(cur), Ok(cand)) if cand <= cur => DowngradeDecision::Refuse,
        _ => DowngradeDecision::Proceed,
    }
}

/// Extract the registered `tm` binary path from an existing plist's
/// `ProgramArguments` array.
///
/// Why: the downgrade guard needs to probe the CURRENTLY-registered binary's
/// `--version` before overwriting the plist. Parsing the already-on-disk plist
/// (rather than shelling out to `launchctl print`) keeps the guard
/// self-contained and unit-testable as pure text parsing.
/// What: finds the `<key>ProgramArguments</key>` marker, then returns the
/// content of the first `<string>…</string>` element after it (the `tm` binary
/// path — `supervisor` is the second array entry). Returns `None` if either
/// marker is missing.
/// Test: `tests::extract_program_path_finds_binary`,
/// `tests::extract_program_path_missing_key_is_none`.
pub fn extract_program_path(plist_xml: &str) -> Option<String> {
    let idx = plist_xml.find("<key>ProgramArguments</key>")?;
    let rest = &plist_xml[idx..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].to_owned())
}

/// The injectable core of the supervisor bootstrap (#3551): every bit of
/// logic [`install_mpm_supervisor`] runs, parameterised over WHERE it writes
/// and HOW (or whether) it talks to launchd via `target`.
///
/// Why not cfg-gated to macOS: nothing in this function's body is actually
/// macOS-specific — the `~/Library/LaunchAgents` path is just a path, and the
/// `launchctl` calls go through the injected [`LaunchctlPort`] rather than
/// shelling out directly. Leaving it portable lets
/// `tests::install_mpm_supervisor_for_*` run on every CI runner (`ci.yml`'s
/// `test` job is `ubuntu-latest`), not only on a macOS development machine —
/// the previous `#[cfg(target_os = "macos")]` gate on this logic meant the
/// #3527/#3554 regression tests silently never ran in CI at all. Only
/// [`install_mpm_supervisor`] (the production entry point, which decides
/// WHETHER to call this at all) stays macOS-gated, matching real launchd's
/// actual platform restriction.
///
/// What: fills the template, creates the log directory, applies the
/// [`decide_downgrade`] guard against a previously-registered plist, writes
/// the plist under `target.home`, then calls `target.launchctl.bootout` /
/// `.bootstrap` against `target.domain` — idempotent bootout first (errors
/// ignored — it may not be loaded), hard error on a failed bootstrap.
///
/// Test: `tests::install_mpm_supervisor_for_writes_plist_and_never_touches_real_launchctl`,
/// `tests::install_mpm_supervisor_for_proceeds_when_existing_binary_unprobeable`,
/// `tests::install_mpm_supervisor_for_candidate_version_ignores_path_shadow`,
/// `tests::install_mpm_supervisor_for_refuses_downgrade_without_force`,
/// `tests::install_mpm_supervisor_for_surfaces_bootstrap_failure`.
pub fn install_mpm_supervisor_for(
    target: &SupervisorTarget<'_>,
    force: bool,
    tm_path: &Path,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let home = &target.home;
    let home_str = home
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("home directory path is not valid UTF-8"))?;

    // #3554: `tm_path` is the CONCRETE path the caller just installed — never
    // re-derived here via a PATH lookup (see this function's doc / the
    // `install_mpm_supervisor` doc for why that was the #3554 bug).
    let tm_path_str = tm_path.to_str().unwrap_or("tm");

    // Fill the plist template.
    let plist_content = fill_template(home_str, tm_path_str);

    // Create the log directory.
    let log_dir = home.join(".trusty-mpm").join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log directory {}", log_dir.display()))?;

    // Write the plist.
    let agents_dir = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("creating LaunchAgents dir {}", agents_dir.display()))?;
    let plist_path = agents_dir.join(format!("{PLIST_LABEL}.plist"));

    // #3527: downgrade guard — refuse to replace an already-registered
    // supervisor with an older-or-equal version unless `force`.
    if let Ok(existing) = std::fs::read_to_string(&plist_path) {
        if let Some(old_binary) = extract_program_path(&existing) {
            let current_version = super::update_engine::installed_version(&old_binary);
            let candidate_version = super::update_engine::installed_version(tm_path_str);
            if decide_downgrade(
                current_version.as_deref(),
                candidate_version.as_deref(),
                force,
            ) == DowngradeDecision::Refuse
            {
                anyhow::bail!(
                    "refusing to replace trusty-mpm supervisor: registered version {} is not \
                     older than the candidate version {} being installed; pass --force to \
                     override (this refusal leaves the currently-running supervisor untouched)",
                    current_version.as_deref().unwrap_or("<unknown>"),
                    candidate_version.as_deref().unwrap_or("<unknown>"),
                );
            }
        }
    }

    std::fs::write(&plist_path, &plist_content)
        .with_context(|| format!("writing plist to {}", plist_path.display()))?;

    // #3551: idempotent load, bootout first (ignore errors — it may not be
    // loaded), then bootstrap — both via the INJECTED `target.launchctl` /
    // `target.domain`, never a hardcoded `RealLaunchctl` / `id -u` lookup.
    // In production this is `RealLaunchctl` against `gui/<real uid>`; in
    // tests it is a `StubLaunchctl` that never spawns a process at all.
    target.launchctl.bootout(&target.domain, &plist_path);
    target
        .launchctl
        .bootstrap(&target.domain, &plist_path)
        .map_err(|stderr| anyhow::anyhow!("launchctl bootstrap failed: {stderr}"))?;

    eprintln!(
        "trusty-mpm supervisor: plist written to {} and bootstrapped (launchd label: {PLIST_LABEL}, domain: {}).",
        plist_path.display(),
        target.domain
    );
    Ok(())
}

/// Resolve the `tm` binary path: `which tm`, then `~/.local/bin/tm`, then `~/.cargo/bin/tm`.
///
/// Why: The plist must embed an absolute path because launchd does not expand $PATH
/// (or expand ~ / $HOME). We probe in the most-likely locations in order.
///
/// What: Returns the first existing absolute path for the `tm` binary.
///
/// Test: `tests::resolve_tm_binary_fallback`.
pub fn resolve_tm_binary(home: &std::path::Path) -> std::path::PathBuf {
    if let Ok(p) = which::which("tm") {
        return p;
    }
    let local = home.join(".local").join("bin").join("tm");
    if local.exists() {
        return local;
    }
    let cargo = home.join(".cargo").join("bin").join("tm");
    if cargo.exists() {
        return cargo;
    }
    // Fall back to the default install dir + "tm".
    crate::download::default_install_dir()
        .map(|d| d.join("tm"))
        .unwrap_or_else(|| home.join(".cargo").join("bin").join("tm"))
}

/// Resolve the current user's UID via `id -u`.
///
/// Why: `launchctl bootstrap gui/<uid>` requires the numeric UID; using `id -u`
/// avoids adding a `libc` dependency just for `getuid()`.
///
/// What: Runs `id -u`, parses the trimmed output as u32; falls back to 501 on
/// parse failure (typical first non-root macOS user) rather than panicking.
///
/// Test: `tests::resolve_uid_returns_nonzero` (CI always has a real UID).
pub fn resolve_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(501)
}

#[cfg(test)]
#[path = "plist_bootstrap_tests.rs"]
mod tests;
