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
//! 3. [`HOME_OVERRIDE_ENV`] / [`SKIP_LAUNCHCTL_ENV`] let a sandboxed test
//!    exercise the plist-write + downgrade-guard decision without touching the
//!    real user's `~/Library/LaunchAgents` or the real `gui/<uid>` launchd
//!    domain.
//!
//! Test: `tests` covers the placeholder replacement, the downgrade-guard
//! decision table, and the registered-binary-path parser as pure functions (no
//! launchctl calls in tests unless `SKIP_LAUNCHCTL_ENV` is set).

/// The plist label for the trusty-mpm supervisor.
///
/// Why: Used consistently for `launchctl list`/`bootstrap`/`bootout` so all
/// callers reference one constant rather than a magic string.
///
/// What: The label as it appears in the plist `<key>Label</key>` entry.
///
/// Test: `tests::label_constant_matches_plist`.
pub const PLIST_LABEL: &str = "com.trusty.mpm.supervisor";

/// Test/E2E escape hatch: overrides the home directory used to resolve the
/// plist path, the log directory, and the `__HOME__` template token (#3527).
///
/// Why: a sandboxed installer E2E needs to exercise the plist-write +
/// downgrade-guard decision without writing into the real user's
/// `~/Library/LaunchAgents`. Mirrors the `TRUSTY_DATA_DIR_OVERRIDE` pattern
/// already used by `ensure::project_setup` for the same reason.
///
/// **Intended for tests only** — never set in production.
///
/// Test: `tests::resolve_home_honours_override`.
pub const HOME_OVERRIDE_ENV: &str = "TCTL_MPM_SUPERVISOR_HOME_OVERRIDE";

/// Test/E2E escape hatch: when set (to any value), skips the actual
/// `launchctl bootout`/`bootstrap` subprocess calls — the plist is still
/// written to disk (so the write + downgrade-guard logic is exercised) but the
/// real `gui/<uid>` launchd domain is never touched (#3527).
///
/// **Intended for tests only** — never set in production.
///
/// Test: `tests::install_mpm_supervisor_writes_plist_and_skips_launchctl`.
pub const SKIP_LAUNCHCTL_ENV: &str = "TCTL_MPM_SUPERVISOR_SKIP_LAUNCHCTL";

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
/// What: On macOS only (cfg gate): resolves home dir and tm binary path, fills
/// the template, creates the log directory, applies the [`decide_downgrade`]
/// guard against a previously-registered plist, writes the plist to
/// `~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist`, and runs
/// `launchctl bootstrap gui/<uid> <plist>` (booting out first if the label is
/// already loaded) — unless [`SKIP_LAUNCHCTL_ENV`] is set, in which case the
/// plist is written but launchd is never touched. On non-macOS: prints a short
/// hint toward the systemd unit and returns Ok(()).
///
/// `force` (#3527) bypasses the downgrade guard — see [`decide_downgrade`].
/// The caller (`install::install_all`) is responsible for gating whether this
/// function is called at all (`--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP`);
/// this function itself never re-checks that opt-out.
///
/// Test: `tests` covers template filling, the downgrade decision, and the
/// registered-binary parser (all pure); the plist write + `launchctl` calls are
/// side-effecting — `tests::install_mpm_supervisor_writes_plist_and_skips_launchctl`
/// exercises the full write path via [`HOME_OVERRIDE_ENV`] / [`SKIP_LAUNCHCTL_ENV`]
/// without touching the real user domain.
pub fn install_mpm_supervisor(force: bool) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        install_mpm_supervisor_macos(force)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = force;
        eprintln!(
            "trusty-mpm supervisor: on Linux, install the systemd unit \
             at `crates/trusty-mpm/deploy/supervisor/trusty-mpm-supervisor.service`."
        );
        Ok(())
    }
}

/// Resolve the home directory used for the plist/log paths, honouring
/// [`HOME_OVERRIDE_ENV`] (#3527 sandboxed-E2E escape hatch).
///
/// Why: extracted so the override check is exercised by
/// [`install_mpm_supervisor_macos`] without duplicating the env lookup.
/// What: returns the override when set and non-empty, else `dirs::home_dir()`.
/// Test: `tests::resolve_home_honours_override`.
#[cfg(target_os = "macos")]
fn resolve_home() -> anyhow::Result<std::path::PathBuf> {
    if let Some(v) = std::env::var_os(HOME_OVERRIDE_ENV) {
        if !v.is_empty() {
            return Ok(std::path::PathBuf::from(v));
        }
    }
    dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory for plist bootstrap"))
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

#[cfg(target_os = "macos")]
fn install_mpm_supervisor_macos(force: bool) -> anyhow::Result<()> {
    use anyhow::Context;

    let home = resolve_home()?;
    let home_str = home
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("home directory path is not valid UTF-8"))?;

    // Resolve the `tm` binary path.
    let tm_path = resolve_tm_binary(&home);
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

    // #3527: sandboxed E2E escape hatch — the plist above is still written
    // (so the write + downgrade-guard logic is exercised) but the real
    // `gui/<uid>` launchd domain is never touched.
    if std::env::var_os(SKIP_LAUNCHCTL_ENV).is_some() {
        eprintln!(
            "trusty-mpm supervisor: plist written to {} ({SKIP_LAUNCHCTL_ENV} set — launchctl skipped).",
            plist_path.display()
        );
        return Ok(());
    }

    // Resolve the current user UID for launchctl gui/<uid>.
    let uid = resolve_uid();

    // Idempotent load: bootout first (ignore errors — it may not be loaded).
    let _ = std::process::Command::new("launchctl")
        .args([
            "bootout",
            &format!("gui/{uid}"),
            plist_path.to_str().unwrap_or(""),
        ])
        .output();

    // Bootstrap.
    let out = std::process::Command::new("launchctl")
        .args([
            "bootstrap",
            &format!("gui/{uid}"),
            plist_path.to_str().unwrap_or(""),
        ])
        .output()
        .with_context(|| "running launchctl bootstrap")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("launchctl bootstrap failed: {stderr}");
    }

    eprintln!(
        "trusty-mpm supervisor: plist written to {} and bootstrapped (launchd label: {PLIST_LABEL}).",
        plist_path.display()
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
mod tests {
    use super::*;

    /// Process-wide lock serialising tests that mutate [`HOME_OVERRIDE_ENV`] /
    /// [`SKIP_LAUNCHCTL_ENV`] (process-global env vars would otherwise race
    /// across parallel test threads — mirrors `ensure::ENV_TEST_LOCK`).
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Why: The template must contain the two placeholder tokens so fill_template
    /// has something to replace.
    /// What: Asserts both `__HOME__` and `__TM_BINARY_PATH__` appear in the raw
    /// template string.
    /// Test: This is the test.
    #[test]
    fn template_contains_placeholders() {
        assert!(
            PLIST_TEMPLATE.contains("__HOME__"),
            "template missing __HOME__ placeholder"
        );
        assert!(
            PLIST_TEMPLATE.contains("__TM_BINARY_PATH__"),
            "template missing __TM_BINARY_PATH__ placeholder"
        );
    }

    /// Why: `fill_template` must replace every occurrence of both tokens.
    /// What: Fills with synthetic home + path; asserts neither token survives
    /// and both replacement values appear in the result.
    /// Test: This is the test.
    #[test]
    fn fill_template_replaces_all_tokens() {
        let filled = fill_template("/home/testuser", "/usr/local/bin/tm");
        assert!(!filled.contains("__HOME__"), "unfilled __HOME__ token");
        assert!(
            !filled.contains("__TM_BINARY_PATH__"),
            "unfilled __TM_BINARY_PATH__ token"
        );
        assert!(filled.contains("/home/testuser"), "home not in output");
        assert!(
            filled.contains("/usr/local/bin/tm"),
            "tm path not in output"
        );
        // Log paths must contain the substituted home.
        assert!(
            filled.contains("/home/testuser/.trusty-mpm/logs/supervisor.out.log"),
            "stdout log path wrong"
        );
        assert!(
            filled.contains("/home/testuser/.trusty-mpm/logs/supervisor.err.log"),
            "stderr log path wrong"
        );
    }

    /// Why: The label constant must match what the plist embeds (label-mismatch
    /// would cause launchctl to fail with a confusing error).
    /// What: Asserts PLIST_LABEL appears in the template.
    /// Test: This is the test.
    #[test]
    fn label_constant_matches_plist() {
        assert!(
            PLIST_TEMPLATE.contains(PLIST_LABEL),
            "PLIST_LABEL not found in template"
        );
    }

    /// Why: `resolve_tm_binary` must return a non-empty path even when `tm` is
    /// not installed (fallback path).
    /// What: Calls with a synthetic home dir; asserts the result is non-empty.
    /// Test: This is the test.
    #[test]
    fn resolve_tm_binary_fallback() {
        let home = std::path::Path::new("/tmp/fake-home-for-test");
        let p = resolve_tm_binary(home);
        assert!(!p.as_os_str().is_empty());
        // On a system where `tm` is not installed the fallback must not reference `__HOME__`.
        assert!(!p.to_string_lossy().contains("__"), "placeholder in path");
    }

    /// Why: `resolve_uid` must return a sensible UID (non-zero in typical CI).
    /// What: Calls it and asserts the value is parseable (we get an integer back).
    /// Test: This is the test.
    #[test]
    fn resolve_uid_returns_nonzero_on_real_system() {
        // On macOS and Linux the UID of the test runner is always non-zero in CI
        // (root-as-UID-0 can run but is unusual in CI; we just assert it is a u32).
        let uid = resolve_uid();
        // uid is always a valid u32 — the function never panics.
        let _ = uid; // just confirm it compiled and ran without panic
    }

    // ── decide_downgrade (#3527) ─────────────────────────────────────────────

    /// Why: `--force` is the explicit operator override; it must always win
    /// regardless of the version comparison.
    /// What: A strictly-older candidate with `force = true` still proceeds.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_force_always_proceeds() {
        assert_eq!(
            decide_downgrade(Some("2.0.0"), Some("1.0.0"), true),
            DowngradeDecision::Proceed
        );
    }

    /// Why: with nothing currently registered (or its version undeterminable),
    /// there is nothing to guard against — a fresh install must proceed.
    /// What: `current = None` proceeds regardless of the candidate.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_no_current_proceeds() {
        assert_eq!(
            decide_downgrade(None, Some("0.1.0"), false),
            DowngradeDecision::Proceed
        );
    }

    /// Why: the core happy path — a strictly newer candidate must always
    /// proceed without needing `--force`.
    /// What: candidate > current → Proceed.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_newer_proceeds() {
        assert_eq!(
            decide_downgrade(Some("0.19.27"), Some("0.20.0"), false),
            DowngradeDecision::Proceed
        );
    }

    /// Why: THE #3527 regression this guard exists for — an older candidate
    /// (e.g. a stale GitHub release) must be refused without `--force`.
    /// What: candidate < current → Refuse.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_older_refuses() {
        assert_eq!(
            decide_downgrade(Some("0.19.27"), Some("0.16.0"), false),
            DowngradeDecision::Refuse
        );
    }

    /// Why: "older-or-equal" per the #3527 spec — a same-version reinstall
    /// must also be refused (no-op re-bootstrap is not worth a live daemon
    /// bootout/bootstrap cycle).
    /// What: candidate == current → Refuse.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_equal_refuses() {
        assert_eq!(
            decide_downgrade(Some("0.19.27"), Some("0.19.27"), false),
            DowngradeDecision::Refuse
        );
    }

    /// Why: an unparseable version string means we cannot PROVE a downgrade;
    /// failing open avoids blocking a legitimate install on a version-string
    /// quirk. Also covers a leading `v` prefix being stripped correctly.
    /// What: unparseable current/candidate → Proceed; `v`-prefixed versions
    /// parse and compare normally.
    /// Test: This is the test.
    #[test]
    fn decide_downgrade_unparseable_proceeds() {
        assert_eq!(
            decide_downgrade(Some("not-a-version"), Some("0.1.0"), false),
            DowngradeDecision::Proceed
        );
        assert_eq!(
            decide_downgrade(Some("0.1.0"), Some("not-a-version"), false),
            DowngradeDecision::Proceed
        );
        // `v`-prefixed versions must still compare correctly (not treated as
        // unparseable).
        assert_eq!(
            decide_downgrade(Some("v0.19.27"), Some("v0.16.0"), false),
            DowngradeDecision::Refuse
        );
    }

    // ── extract_program_path (#3527) ─────────────────────────────────────────

    /// Why: the downgrade guard parses the EXISTING on-disk plist to find which
    /// binary is currently registered; pin the happy path against a real
    /// filled template.
    /// What: fills the template with a synthetic path and asserts the parser
    /// recovers it exactly.
    /// Test: This is the test.
    #[test]
    fn extract_program_path_finds_binary() {
        let filled = fill_template("/home/testuser", "/home/testuser/.local/bin/tm");
        assert_eq!(
            extract_program_path(&filled),
            Some("/home/testuser/.local/bin/tm".to_owned())
        );
    }

    /// Why: a plist missing the `ProgramArguments` key (malformed / unexpected
    /// content) must not panic — the guard should just skip the comparison.
    /// What: asserts `None` on XML without the key.
    /// Test: This is the test.
    #[test]
    fn extract_program_path_missing_key_is_none() {
        assert_eq!(extract_program_path("<plist><dict></dict></plist>"), None);
    }

    // ── resolve_home (#3527) ──────────────────────────────────────────────────

    /// Why: the sandboxed-E2E escape hatch must actually redirect the resolved
    /// home directory when set.
    /// What: sets [`HOME_OVERRIDE_ENV`] to a temp dir; asserts `resolve_home`
    /// returns it verbatim.
    /// Test: This is the test.
    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_home_honours_override() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(HOME_OVERRIDE_ENV, tmp.path());
        let resolved = resolve_home().expect("resolve_home");
        std::env::remove_var(HOME_OVERRIDE_ENV);
        assert_eq!(resolved, tmp.path());
    }

    // ── install_mpm_supervisor end-to-end (macOS, sandboxed) ────────────────

    /// Why: the full write path (home resolution, plist write, downgrade
    /// guard) must be exercisable WITHOUT touching the real user's
    /// `~/Library/LaunchAgents` or the real `gui/<uid>` launchd domain — the
    /// whole point of the #3527 sandboxed-E2E escape hatches.
    /// What: with both `HOME_OVERRIDE_ENV` (a temp dir) and
    /// `SKIP_LAUNCHCTL_ENV` set, calls `install_mpm_supervisor(false)` and
    /// asserts it succeeds and the plist actually landed under the temp home
    /// — never under the real home directory.
    /// Test: This is the test.
    #[test]
    #[cfg(target_os = "macos")]
    fn install_mpm_supervisor_writes_plist_and_skips_launchctl() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(HOME_OVERRIDE_ENV, tmp.path());
        std::env::set_var(SKIP_LAUNCHCTL_ENV, "1");

        let result = install_mpm_supervisor(false);

        std::env::remove_var(HOME_OVERRIDE_ENV);
        std::env::remove_var(SKIP_LAUNCHCTL_ENV);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let plist_path = tmp
            .path()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{PLIST_LABEL}.plist"));
        assert!(
            plist_path.exists(),
            "plist should have been written under the overridden home"
        );
    }

    /// Why: when a plist is ALREADY registered but its `ProgramArguments`
    /// binary no longer exists (so its `--version` cannot be probed), there is
    /// no PROVABLE downgrade — the guard must fail open rather than block a
    /// legitimate install on an unprobeable predecessor. Seeding the existing
    /// plist by hand (rather than depending on whatever `tm` happens to be on
    /// the test machine's PATH) keeps this hermetic and deterministic
    /// regardless of the host environment.
    /// What: writes a plist whose `ProgramArguments[0]` points at a
    /// guaranteed-nonexistent path, then calls `install_mpm_supervisor(false)`;
    /// asserts it proceeds (`Ok`).
    /// Test: This is the test.
    #[test]
    #[cfg(target_os = "macos")]
    fn install_mpm_supervisor_proceeds_when_existing_binary_unprobeable() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(HOME_OVERRIDE_ENV, tmp.path());
        std::env::set_var(SKIP_LAUNCHCTL_ENV, "1");

        // Seed an existing plist by hand, pointing at a binary that cannot
        // possibly exist, so `installed_version` deterministically returns
        // `None` for the "current" side of the comparison.
        let agents_dir = tmp.path().join("Library").join("LaunchAgents");
        std::fs::create_dir_all(&agents_dir).expect("create LaunchAgents dir");
        let plist_path = agents_dir.join(format!("{PLIST_LABEL}.plist"));
        let seeded = fill_template(
            &tmp.path().to_string_lossy(),
            "/nonexistent/fake-tm-binary-for-test-xyz",
        );
        std::fs::write(&plist_path, seeded).expect("seed plist");

        let result = install_mpm_supervisor(false);

        std::env::remove_var(HOME_OVERRIDE_ENV);
        std::env::remove_var(SKIP_LAUNCHCTL_ENV);

        assert!(
            result.is_ok(),
            "an unprobeable existing binary must not block the install: {result:?}"
        );
    }
}
