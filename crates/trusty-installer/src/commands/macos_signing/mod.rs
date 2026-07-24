//! macOS Developer-ID codesign + FDA/TCC guidance — single source of truth (#2558).
//!
//! Why: Before #2558 this logic was duplicated between this module (the
//! `tctl install` post-install hook) and `scripts/install-trusty-search-signed.sh`
//! (the documented manual "permanent fix"). The two implementations had already
//! drifted — the script used `--identifier com.trusty.trusty-search` /
//! `com.trusty.trusty-embedderd` (matching the real launchd label convention in
//! `plist_label.rs` and the release-workflow docs) plus `--options runtime
//! --timestamp` and post-sign verification, while this module used the shorter
//! `com.trusty.search` / `com.trusty.embedderd` identifiers with none of the
//! extra flags. Folding both into one module (and one set of tests) means the
//! identifier scheme, signing flags, and guidance text can never diverge again.
//! Owner-authorized scope extension (2026-07-14, issue #2558 comment): the same
//! stable-identity signing now also covers `trusty-mpm` — macOS's App Data TCC
//! category ("'trusty-mpm' would like to access data from other apps")
//! re-prompts on every ad-hoc-signed rebuild for the same cdhash reason FDA does,
//! because `tm` reads other apps' `$HOME` containers (Claude config dirs, tmux
//! state).
//!
//! PR #2657 code-critic review fixes (same day):
//! - The canonicalization above is a SILENT IDENTIFIER MIGRATION for any host
//!   previously signed by the pre-PR Rust hook's `com.trusty.search` — changing
//!   the designated requirement invalidates that host's existing FDA grant with
//!   no warning (reproducing #873's `indexes:2` symptom). [`current_identifier`]
//!   reads the binary's on-disk identifier before signing and
//!   [`identifier_migration_notice`] prints a loud, distinct callout when it is
//!   about to change.
//! - `--options runtime --timestamp` (Hardened Runtime) is now gated by
//!   [`use_hardened_runtime`] rather than always-on: trusty-search/embedderd
//!   load an ONNX runtime dylib at runtime, and Hardened Runtime's library
//!   validation has NOT been empirically verified safe for that yet, so the
//!   pre-PR automatic `tctl install` hook's behavior (no hardened runtime) is
//!   preserved for the SEARCH set there, while the pre-PR script's behavior
//!   (hardened runtime) is preserved for explicit signing
//!   (`tctl sign trusty-search` / the wrapper script). trusty-mpm loads no
//!   dylibs, so it gets hardened runtime unconditionally — see the follow-up
//!   issue tracked in the PR for lifting this split once ONNX-under-Hardened-
//!   Runtime is verified. TCC grant stability depends on `--identifier` + Team
//!   ID, not on `--options runtime`, so this split does not reintroduce the
//!   identifier drift above.
//! - #2951: `trusty-mpm-gui` (the Tauri desktop shell) joined [`MPM_SET`] as a
//!   third binary. It presented its own App-Data TCC prompt textually
//!   identical to `trusty-mpm`'s (`productName: "trusty-mpm"` in
//!   `tauri.conf.json`) and, because ad-hoc debug builds happen in every
//!   worktree, churned a fresh random identity per rebuild — the same class of
//!   bug this module already fixed for `trusty-mpm`/`tm`. The primary fix is
//!   Tauri's own `bundle.macOS.signingIdentity` (so `cargo tauri build`
//!   produces a Developer-ID-signed `.app` directly); this table entry covers
//!   the fallback case of a bare `cargo install --path crates/trusty-mpm-gui`.
//!
//! What: [`binaries_for_set`] and [`codesign_identifier`] are both derived from
//! the single [`SIGNABLE_BINARIES`] table (binary, set, identifier) so set
//! membership and identifier mapping can never drift apart again.
//! [`has_developer_id_cert`] probes for a cert (env var `TRUSTY_SIGN_IDENTITY` >
//! `security find-identity`). [`sign_binary`] signs with `--options runtime
//! --timestamp` only when `hardened` is `true` (see [`use_hardened_runtime`]),
//! and [`verify_signature`] checks with `codesign --verify --deep --strict`.
//! [`post_install_search`] / [`post_install_mpm`] are the fail-soft hooks
//! `commands::install` calls after each member lands; [`sign_set_strict`] is the
//! hard-failing primitive the standalone `tctl sign <target>` command
//! (`commands::sign`) uses, which `scripts/install-trusty-search-signed.sh` now
//! shells out to instead of duplicating codesign flags in bash.
//!
//! Test: `tests` covers `binaries_for_set`, `codesign_identifier`,
//! `use_hardened_runtime`, `identifier_migration_notice`,
//! `has_developer_id_cert` identity-probe logic, and all guidance-text
//! generation as pure functions; `codesign` / `security` are not invoked in
//! tests (side-effecting, macOS-only).

/// The `trusty-search` signable set: `trusty-search` + its bundled `trusty-embedderd`.
pub const SEARCH_SET: &str = "trusty-search";

/// The `trusty-mpm` signable set: both binaries `cargo install --path
/// crates/trusty-mpm` produces — the `trusty-mpm` MCP-bridge binary AND its
/// `tm` CLI/daemon binary.
///
/// Why (#2721): `cargo install` installs `trusty-mpm` and `tm` side by side and
/// both are ad-hoc/linker-signed with a churning cdhash. Signing only
/// `trusty-mpm` left `tm` — the primary user-facing binary that also runs the
/// daemon (`tm daemon`/`tm supervisor`) — ad-hoc, so its App-Data TCC grant kept
/// being revoked on every reinstall and the "'trusty-mpm' would like to access
/// data from other apps" prompt still recurred. Both binaries must carry a
/// stable Developer-ID `--identifier` for the grant to survive.
pub const MPM_SET: &str = "trusty-mpm";

/// The master table: every signable binary → (its set, its codesign identifier).
///
/// Why: PR #2657 review (MEDIUM) — `binaries_for_set` and `codesign_identifier`
/// were two independently-maintained tables that could drift apart exactly the
/// way the script and the pre-#2558 hook already had. Collapsing set
/// membership and identifier mapping into one table makes that class of drift
/// structurally impossible: every signable binary is declared exactly once.
///
/// What: `(binary, set, identifier)` triples. [`binaries_for_set`] filters on
/// the middle field; [`codesign_identifier`] looks up the third field by the
/// first.
///
/// Test: `tests::every_set_member_has_a_real_identifier`.
const SIGNABLE_BINARIES: &[(&str, &str, &str)] = &[
    ("trusty-search", SEARCH_SET, "com.trusty.trusty-search"),
    (
        "trusty-embedderd",
        SEARCH_SET,
        "com.trusty.trusty-embedderd",
    ),
    ("trusty-mpm", MPM_SET, "com.trusty.trusty-mpm"),
    // `tm` is installed alongside `trusty-mpm` by the same `cargo install --path
    // crates/trusty-mpm` and is the primary user-facing/daemon binary. It MUST
    // be signed too or its App-Data TCC grant is revoked on every reinstall
    // (#2721). Ordered after `trusty-mpm` so `binaries_for_set(MPM_SET).first()`
    // — the binary whose path the guidance/prompt names — stays `trusty-mpm`.
    ("tm", MPM_SET, "com.trusty.tm"),
    // `trusty-mpm-gui` (#2951): the Tauri desktop shell. It is a separate
    // binary/crate from `trusty-mpm`/`tm` and is normally not present under
    // `install_dir` unless someone runs `cargo install --path
    // crates/trusty-mpm-gui`, so `post_install_signed_set`'s existing
    // "skip if the path doesn't exist" behavior makes this entry a no-op for
    // everyone who never installs the GUI that way. Sharing `MPM_SET` (rather
    // than a new set) means `tctl sign trusty-mpm` / the automatic
    // `tctl install` post-install hook picks it up for free with no new set
    // name to thread through `SignSetError`'s messages. Identifier matches the
    // `bundle.macOS.signingIdentity`-driven identifier in
    // `crates/trusty-mpm-gui/tauri.conf.json` so both signing paths (Tauri's
    // own bundler and this fallback for a bare `cargo install`ed binary) agree
    // on one designated requirement.
    ("trusty-mpm-gui", MPM_SET, "com.trusty.trusty-mpm.gui"),
];

/// Resolve the binaries that make up a named Developer-ID-signable set.
///
/// Why: `trusty-search` ships two binaries that must be signed together
/// (`trusty-search` + the bundled `trusty-embedderd`); `trusty-mpm` is signed
/// alone. Centralising the set membership means every caller (the fail-soft
/// install hooks and the strict `tctl sign` command) agrees on exactly which
/// binaries a target name covers.
///
/// What: Returns the binary names whose [`SIGNABLE_BINARIES`] entry matches
/// `set`, in table order. An empty `Vec` for any other input (the caller
/// treats that as "unknown set").
///
/// Test: `tests::binaries_for_set_covers_search_and_mpm`,
/// `tests::binaries_for_set_unknown_is_empty`.
pub fn binaries_for_set(set: &str) -> Vec<&'static str> {
    SIGNABLE_BINARIES
        .iter()
        .filter(|(_, s, _)| *s == set)
        .map(|(binary, _, _)| *binary)
        .collect()
}

/// The codesign identifier for a signable binary.
///
/// Why: A fixed `--identifier` anchors the designated requirement (DR) to a
/// stable value rather than the binary hash, so the FDA / App-Data TCC grant
/// survives a recompile even without a Developer ID cert. The scheme
/// (`com.trusty.<binary>` with the binary's full name, e.g.
/// `com.trusty.trusty-search`) matches the launchd label convention in
/// `plist_label.rs` and `docs/reference/release-workflow.md` — this is the
/// canonical scheme; the pre-#2558 `com.trusty.search` (short form) used here
/// was the drift this issue fixes. Migrating an already-signed host from the
/// old identifier is handled separately by [`identifier_migration_notice`].
///
/// What: Looks up `binary` in [`SIGNABLE_BINARIES`]; falls back to
/// `"com.trusty.unknown"` for anything not in the table.
///
/// Test: `tests::identifier_map_covers_all_signable_binaries`,
/// `tests::every_set_member_has_a_real_identifier`.
pub fn codesign_identifier(binary: &str) -> &'static str {
    SIGNABLE_BINARIES
        .iter()
        .find(|(b, _, _)| *b == binary)
        .map(|(_, _, identifier)| *identifier)
        .unwrap_or("com.trusty.unknown")
}

/// Whether to enable Hardened Runtime (`--options runtime --timestamp`) for a
/// given signing context.
///
/// Why: PM decision, PR #2657 review (HIGH) — preserve BOTH pre-PR behaviors
/// exactly rather than flipping a default. The pre-PR script (an explicit
/// operator action) always signed with `--options runtime`; the pre-PR
/// `tctl install` post-install hook (automatic, fail-soft, runs on every
/// install with a cert present) never did. trusty-search/trusty-embedderd load
/// an ONNX runtime dylib, and Hardened Runtime's library-validation
/// restriction has not been empirically verified safe for that load path —
/// flipping the automatic hook to hardened-by-default risks silently breaking
/// `tctl install trusty-search` on every machine with a Developer ID cert
/// already installed. trusty-mpm loads no dylibs, so Hardened Runtime is
/// low-risk there in both contexts. Note: TCC grant stability (the actual
/// bug #873/#2558 fix) depends on `--identifier` + Team ID, NOT on
/// `--options runtime`, so this split does not reintroduce the identifier-drift
/// problem — it only affects notarization eligibility and dylib-validation
/// strictness.
///
/// What: `explicit` (the operator directly ran `tctl sign <target>`, or the
/// wrapper script which shells out to it) → always `true`. Automatic
/// (`tctl install`'s fail-soft hook, `explicit = false`) → `true` only for
/// [`MPM_SET`]; `false` for [`SEARCH_SET`] until ONNX-under-Hardened-Runtime
/// is verified (see the tracking issue referenced in PR #2657).
///
/// Test: `tests::hardened_runtime_policy`.
pub fn use_hardened_runtime(set: &str, explicit: bool) -> bool {
    explicit || set == MPM_SET
}

/// Parse the Developer ID Application identity out of `security
/// find-identity -v -p codesigning` stdout.
///
/// Why: Extracted as a pure function (no system calls) so the parsing logic —
/// the #2937/#2939 root cause — is regression-tested against a realistic
/// multi-line fixture without invoking `security`. The prior implementation
/// used `line.split(") ").nth(1)`, which only strips the leading index token
/// (`  1) `) and leaves the SHA1 fingerprint and surrounding quotes attached
/// (e.g. `F03283D9...FB362 "Developer ID Application: Bob Matsuoka
/// (4JH68XUHC5)"`); that garbage string was then passed straight to `codesign
/// --sign`, which fails with "no identity found" even though the cert is
/// valid and `security find-identity` lists it. Mirrors the working
/// `detect_identity()` in `scripts/install-trusty-mpm-signed.sh`.
///
/// What: Scans `stdout` line by line for the first line containing
/// "Developer ID Application"; on that line, extracts the substring between
/// the first and last `"` (the quoted common name, e.g. `Developer ID
/// Application: Bob Matsuoka (4JH68XUHC5)`). A matching line with fewer than
/// two quotes is skipped (scan continues to the next line) rather than
/// aborting the whole probe. Returns `None` when no well-formed match is
/// found in the entire output.
///
/// Test: `tests::parse_developer_id_identity_strips_fingerprint_and_quotes`,
/// `tests::parse_developer_id_identity_no_match`,
/// `tests::parse_developer_id_identity_skips_malformed_line`.
pub fn parse_developer_id_identity(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if !line.contains("Developer ID Application") {
            continue;
        }
        let Some(first) = line.find('"') else {
            continue;
        };
        let Some(last) = line.rfind('"') else {
            continue;
        };
        if last > first {
            return Some(line[first + 1..last].to_owned());
        }
    }
    None
}

/// Check whether a Developer ID Application certificate is available.
///
/// Why: Signing is only possible when a suitable cert is present in the keychain;
/// we check before attempting `codesign` to give a clear early message.
///
/// What: Thin wrapper over [`has_developer_id_cert_verbose`] with `verbose =
/// false` — the default, silent probe used by the fail-soft `tctl install`
/// post-install hooks.
///
/// Test: `tests::has_developer_id_cert_respects_env` (macOS only).
#[cfg(target_os = "macos")]
pub fn has_developer_id_cert() -> Option<String> {
    has_developer_id_cert_verbose(false)
}

/// Check whether a Developer ID Application certificate is available,
/// optionally narrating the probe to stderr (#2939).
///
/// Why: `tctl sign --verbose` needs to show the raw `security find-identity`
/// output and which identity was selected, so an operator hitting "no
/// identity found" despite a valid cert (#2937/#2939) can see exactly what
/// the probe saw instead of guessing.
///
/// What: Checks `TRUSTY_SIGN_IDENTITY` env first (user override); if absent,
/// runs `security find-identity -v -p codesigning` and parses the result via
/// [`parse_developer_id_identity`]. When `verbose` is `true`, prints the env
/// override (if used), the raw probe stdout, and the selected identity (or a
/// "not found" note) to stderr. Returns `Some(identity)` when found, `None`
/// when no cert is available.
///
/// Test: `tests::has_developer_id_cert_respects_env` (macOS only, exercises
/// `verbose = false` via [`has_developer_id_cert`]); the verbose narration
/// itself is side-effecting (stderr) and not asserted directly — its content
/// is composed entirely from the independently-tested
/// [`parse_developer_id_identity`].
#[cfg(target_os = "macos")]
pub fn has_developer_id_cert_verbose(verbose: bool) -> Option<String> {
    // Environment override takes priority.
    if let Ok(identity) = std::env::var("TRUSTY_SIGN_IDENTITY") {
        if !identity.is_empty() {
            if verbose {
                eprintln!("tctl sign --verbose: using TRUSTY_SIGN_IDENTITY override: {identity}");
            }
            return Some(identity);
        }
    }
    // Probe the keychain.
    let output = std::process::Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if verbose {
        eprintln!(
            "tctl sign --verbose: raw `security find-identity -v -p codesigning` output:\n{stdout}"
        );
    }
    let identity = parse_developer_id_identity(&stdout);
    if verbose {
        match &identity {
            Some(id) => eprintln!("tctl sign --verbose: selected identity: {id}"),
            None => eprintln!(
                "tctl sign --verbose: no 'Developer ID Application' identity found in probe output"
            ),
        }
    }
    identity
}

/// Read a binary's CURRENT on-disk codesign identifier, if any.
///
/// Why: PR #2657 review (HIGH) — before (re-)signing with the canonical
/// identifier, we must know whether the binary was already signed with a
/// DIFFERENT identifier (e.g. the pre-#2558 `com.trusty.search` short form).
/// Silently changing the DR invalidates any TCC grant keyed to the old value
/// with no warning — exactly #873's `indexes:2` symptom, reintroduced by this
/// PR's own canonicalization if left unchecked.
///
/// What: Runs `codesign -d <path>` (display mode) and parses the
/// `Identifier=<value>` line from its stderr output (that is where `codesign
/// -d` writes its human-readable summary). Returns `None` when the path does
/// not exist, the binary is unsigned (no `Identifier=` line, non-zero exit),
/// or `codesign` cannot be spawned.
///
/// Test: Not invoked in tests (side-effecting, requires a real signed
/// binary); the comparison logic that consumes its output is tested via
/// `identifier_migration_notice`.
#[cfg(target_os = "macos")]
pub fn current_identifier(path: &std::path::Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let out = std::process::Command::new("codesign")
        .args(["-d"])
        .arg(path)
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr.lines().find_map(|line| {
        line.strip_prefix("Identifier=")
            .map(|s| s.trim().to_owned())
    })
}

/// Build the identifier-migration warning, if `old_identifier` differs from
/// the canonical identifier for `binary`.
///
/// Why: Extracted as a pure function (no system calls) so the exact wording
/// and the "only warn on an actual change" decision are unit-tested directly.
///
/// What: Returns `None` when `old_identifier` is empty (never signed before)
/// or already matches [`codesign_identifier`]; otherwise returns a loud,
/// distinct notice explaining that existing FDA / App-Data grants keyed to
/// the old identifier will stop matching.
///
/// Test: `tests::identifier_migration_notice_warns_on_change`,
/// `tests::identifier_migration_notice_silent_when_unchanged_or_absent`.
pub fn identifier_migration_notice(binary: &str, old_identifier: &str) -> Option<String> {
    let canonical = codesign_identifier(binary);
    if old_identifier.is_empty() || old_identifier == canonical {
        return None;
    }
    Some(format!(
        "\u{26A0} IDENTIFIER CHANGE for {binary}: {old_identifier} -> {canonical}. Existing \
         macOS Full Disk Access / App Data grants keyed to the old identifier will STOP \
         MATCHING after this signing — re-grant/re-approve once after this install."
    ))
}

/// Print [`identifier_migration_notice`] to stderr when `path`'s current
/// on-disk identifier differs from the canonical one for `binary`.
///
/// Why: Shared by both signing call sites ([`sign_set_strict`] and the
/// fail-soft [`post_install_signed_set`]) so the warning fires regardless of
/// which path re-signs an already-signed binary. Always printed (not gated by
/// `json`) — this is safety-critical information about an existing TCC grant
/// breaking, not routine narration.
///
/// What: Reads `path`'s current identifier via [`current_identifier`]; if a
/// migration notice applies, prints it to stderr.
///
/// Test: Not invoked in tests (side-effecting); composes two independently
/// tested pure/probe functions.
#[cfg(target_os = "macos")]
fn warn_on_identifier_change(path: &std::path::Path, binary: &str) {
    if let Some(old) = current_identifier(path) {
        if let Some(notice) = identifier_migration_notice(binary, &old) {
            eprintln!("trusty-installer: {notice}");
        }
    }
}

/// Sign a binary with a Developer ID Application certificate.
///
/// Why: Signing with `--identifier` anchors the DR so the TCC FDA / App-Data
/// grant persists across reinstalls (no re-grant needed after `cargo install`).
/// `hardened` gates `--options runtime` (Hardened Runtime, required for
/// notarization) + `--timestamp` (secure timestamp from Apple's servers) — see
/// [`use_hardened_runtime`] for why this is no longer unconditional.
///
/// What: Runs `codesign --force [--options runtime --timestamp] --identifier
/// <id> --sign <identity> <path>`. Returns `Ok(())` on success, `Err` with the
/// `codesign` stderr on failure.
///
/// Test: Not invoked in tests (side-effecting); the identifier mapping is
/// tested via `codesign_identifier`, the hardened-runtime gating via
/// `use_hardened_runtime`.
#[cfg(target_os = "macos")]
pub fn sign_binary(path: &std::path::Path, identity: &str, hardened: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    let binary_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let identifier = codesign_identifier(binary_name);

    let mut args: Vec<&str> = vec!["--force"];
    if hardened {
        args.extend(["--options", "runtime", "--timestamp"]);
    }
    args.extend(["--identifier", identifier, "--sign", identity]);

    let out = std::process::Command::new("codesign")
        .args(&args)
        .arg(path)
        .output()
        .with_context(|| format!("running codesign on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("codesign failed for {}: {stderr}", path.display());
    }
    Ok(())
}

/// Verify a binary's signature immediately after signing.
///
/// Why: `set -e`-equivalent belt-and-suspenders — `sign_binary` already fails
/// on a non-zero `codesign` exit, but a strict `--verify --deep --strict` pass
/// catches subtler corruption (e.g. a truncated write) that the sign step alone
/// might not (matches the script's post-sign check, issue #2322).
///
/// What: Runs `codesign --verify --deep --strict <path>`; `Ok(())` on a clean
/// verification, `Err` with the verifier's output otherwise.
///
/// Test: Not invoked in tests (side-effecting, requires a real signed binary).
#[cfg(target_os = "macos")]
pub fn verify_signature(path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let out = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .output()
        .with_context(|| format!("running codesign --verify on {}", path.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "post-sign verification failed for {}: {stderr}",
            path.display()
        );
    }
    Ok(())
}

/// Typed error for [`sign_set_strict`].
///
/// Why: PR #2657 review (MEDIUM) — `commands::sign::run_macos` previously
/// distinguished "no cert" from "signing failed" by substring-matching the
/// error message, which is brittle (any future wording change silently breaks
/// the branch that shows cert-setup guidance). A typed variant lets the caller
/// match on the actual failure kind.
///
/// What: `UnknownSet` — `set` did not resolve to any binaries.
/// `NoCertificate` — no Developer ID Application cert is available.
/// `NoBinariesFound` — the set resolved, but none of its binaries exist under
/// the install directory. `Sign` — signing or post-sign verification failed
/// for a specific binary (wraps the underlying `anyhow::Error`).
///
/// Test: `tests::sign_set_error_display_is_distinct_per_variant`.
#[derive(Debug)]
pub enum SignSetError {
    /// `set` did not resolve to any binaries via [`binaries_for_set`].
    UnknownSet(String),
    /// No Developer ID Application certificate is available (env override or keychain).
    NoCertificate,
    /// No binary from the resolved set exists under the install directory.
    NoBinariesFound {
        /// The signable-set name that was requested.
        set: String,
        /// The install directory searched.
        dir: String,
    },
    /// Signing or post-sign verification failed for a specific binary.
    Sign(anyhow::Error),
}

impl std::fmt::Display for SignSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignSetError::UnknownSet(set) => write!(
                f,
                "unknown signable set '{set}' (expected 'trusty-search' or 'trusty-mpm')"
            ),
            SignSetError::NoCertificate => {
                write!(f, "no Developer ID Application certificate found")
            }
            SignSetError::NoBinariesFound { set, dir } => {
                write!(f, "no binaries from set '{set}' found in {dir}")
            }
            SignSetError::Sign(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SignSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SignSetError::Sign(e) => e.source(),
            _ => None,
        }
    }
}

/// Sign and verify every binary in a named set, hard-failing on any problem.
///
/// Why: The fail-soft `post_install_*` hooks below must never abort a `tctl
/// install` run over a signing hiccup, but the standalone `tctl sign <target>`
/// command (used directly, or shelled out to by
/// `scripts/install-trusty-search-signed.sh`) is the operator explicitly asking
/// for signing to happen — a silent partial failure there would be worse than a
/// loud one, exactly like the pre-#2558 script's `set -euo pipefail` behaviour.
///
/// What: Resolves the Developer ID identity (erroring with
/// [`SignSetError::NoCertificate`] if none is found), warns on any
/// identifier migration via [`warn_on_identifier_change`], then for every
/// binary in `binaries_for_set(set)` that exists under `install_dir`, signs it
/// (always with Hardened Runtime — `explicit = true` per
/// [`use_hardened_runtime`]) and verifies the signature — bailing out on the
/// first failure. Returns the paths that were signed. Errors on an unknown
/// set name or when no binary in the set was found on disk. `verbose` (#2939)
/// is forwarded to [`has_developer_id_cert_verbose`] so `tctl sign --verbose`
/// can show the raw identity-probe output and which identity was selected.
///
/// Test: Not invoked in tests (side-effecting); the set/identifier resolution
/// it composes is tested independently via `binaries_for_set` /
/// `codesign_identifier` / `use_hardened_runtime`.
#[cfg(target_os = "macos")]
pub fn sign_set_strict(
    install_dir: &std::path::Path,
    set: &str,
    verbose: bool,
) -> Result<Vec<std::path::PathBuf>, SignSetError> {
    let binaries = binaries_for_set(set);
    if binaries.is_empty() {
        return Err(SignSetError::UnknownSet(set.to_owned()));
    }
    let identity = has_developer_id_cert_verbose(verbose).ok_or(SignSetError::NoCertificate)?;
    let hardened = use_hardened_runtime(set, true);

    let mut signed = Vec::new();
    for name in binaries {
        let path = install_dir.join(name);
        if !path.exists() {
            continue;
        }
        warn_on_identifier_change(&path, name);
        sign_binary(&path, &identity, hardened).map_err(SignSetError::Sign)?;
        verify_signature(&path).map_err(SignSetError::Sign)?;
        signed.push(path);
    }
    if signed.is_empty() {
        return Err(SignSetError::NoBinariesFound {
            set: set.to_owned(),
            dir: install_dir.display().to_string(),
        });
    }
    Ok(signed)
}

/// The Developer ID certificate setup guidance (no cert found, strict path).
///
/// Why: Extracted as a pure function so the exact guidance text is unit-tested
/// without any system calls; mirrors the script's `print_cert_setup_and_exit`.
///
/// What: Returns the 5-step "enroll → issue cert → install → verify → re-run"
/// instructions for `tctl sign <target>`.
///
/// Test: `tests::cert_setup_guidance_contains_steps`.
pub fn cert_setup_guidance(target: &str) -> String {
    format!(
        "No 'Developer ID Application' certificate found in the login keychain.\n\
         \n\
         To enable persistent signing (one-time setup):\n\
         \n\
         1. Enroll in the Apple Developer Program:\n\
         \x20\x20\x20\x20https://developer.apple.com/programs/enroll/\n\
         2. Issue a \"Developer ID Application\" certificate (Xcode -> Settings ->\n\
         \x20\x20\x20\x20Accounts -> Manage Certificates -> \"+\", or\n\
         \x20\x20\x20\x20https://developer.apple.com/account/resources/certificates/list).\n\
         3. Download and double-click the .cer file to install it in your login\n\
         \x20\x20\x20\x20keychain.\n\
         4. Verify: security find-identity -v -p codesigning | grep 'Developer ID Application'\n\
         5. Re-run: tctl sign {target}\n"
    )
}

/// The 4-step FDA re-grant guidance (used when no Developer ID cert is found).
///
/// Why: Extracted as a pure function so the exact guidance text can be unit-tested
/// without any system calls.
///
/// What: Returns the formatted 4-step manual FDA re-grant instructions, with
/// the actual binary path interpolated.
///
/// Test: `tests::fda_guidance_contains_steps`.
pub fn fda_guidance(binary_path: &str) -> String {
    format!(
        "trusty-search FDA guidance (macOS):\n\
         Every `cargo install` replaces the binary with a new cdhash, which invalidates\n\
         the macOS TCC Full Disk Access grant. Re-grant it now:\n\
         \n\
         1. Open System Settings \u{2192} Privacy & Security \u{2192} Full Disk Access.\n\
         2. Remove `{binary_path}` from the list (click the entry, then click -).\n\
         3. Re-add it (click +, navigate to `{binary_path}`).\n\
         4. Restart the daemon:\n\
            launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-search.plist\n\
            launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.trusty-search.plist\n\
         \n\
         Tip: install a Developer ID Application certificate and set TRUSTY_SIGN_IDENTITY\n\
         to make this grant persist across all future reinstalls."
    )
}

/// The App-Data TCC guidance for `trusty-mpm` (used when no Developer ID cert
/// is found).
///
/// Why: `tm` re-prompts "'trusty-mpm' would like to access data from other
/// apps" after every ad-hoc-signed rebuild, for the same cdhash-identity reason
/// FDA does for trusty-search — but it is a different TCC category (App Data,
/// not Full Disk Access) with no `System Settings` list entry to remove/re-add,
/// so the guidance differs from [`fda_guidance`].
///
/// What: Returns guidance explaining that the next prompt must be approved once
/// and that a Developer ID cert makes that approval persist.
///
/// Test: `tests::app_data_guidance_mentions_prompt`.
pub fn app_data_guidance(binary_path: &str) -> String {
    format!(
        "trusty-mpm App Data TCC guidance (macOS):\n\
         Every `cargo install` replaces the binary with a new cdhash, so macOS re-prompts\n\
         \"'trusty-mpm' would like to access data from other apps\" (it reads other apps'\n\
         $HOME containers — Claude config dirs, tmux state) after every reinstall.\n\
         \n\
         Approve the next prompt for `{binary_path}` when it appears — no manual\n\
         System Settings step is required for this TCC category.\n\
         \n\
         Tip: install a Developer ID Application certificate and set TRUSTY_SIGN_IDENTITY\n\
         to make that approval persist across all future reinstalls (run `tctl sign trusty-mpm`)."
    )
}

/// Run the fail-soft post-install signing hook for a named set.
///
/// Why: Shared by [`post_install_search`] and [`post_install_mpm`] so the
/// probe/sign/guidance sequence is written once.
///
/// What: On macOS: probes for a Developer ID cert; if found, warns on any
/// identifier migration ([`warn_on_identifier_change`]), then signs + verifies
/// every present binary in `set` — with Hardened Runtime gated per
/// [`use_hardened_runtime`] (`explicit = false`: on for [`MPM_SET`], off for
/// [`SEARCH_SET`]) — and returns a success note; on cert-probe failure,
/// returns the set-appropriate guidance (FDA for search, App Data TCC for mpm)
/// using the set's first binary's path. Never aborts the caller. Returns
/// `None` under `json` (never any output in machine-readable mode) or when
/// there is nothing to report; callers own WHEN the returned text is printed
/// (demo-critical fix: this used to `eprintln!` directly, interleaving noisy
/// multi-line guidance into the middle of the live per-component checklist —
/// it is now data the caller can defer to a post-install summary instead).
#[cfg(target_os = "macos")]
fn post_install_signed_set(install_dir: &std::path::Path, set: &str, json: bool) -> Option<String> {
    if json {
        return None;
    }
    let binaries = binaries_for_set(set);
    let &primary = binaries.first()?;
    let primary_path = install_dir.join(primary);
    let hardened = use_hardened_runtime(set, false);

    match has_developer_id_cert() {
        Some(identity) => {
            let mut signed_ok = true;
            let mut warnings = Vec::new();
            for &name in &binaries {
                let path = install_dir.join(name);
                if !path.exists() {
                    // A bundled binary (e.g. embedderd) may not be present; skip gracefully.
                    continue;
                }
                warn_on_identifier_change(&path, name);
                if let Err(e) = sign_binary(&path, &identity, hardened) {
                    warnings.push(format!(
                        "trusty-installer: warning: codesign failed for {}: {e}",
                        path.display()
                    ));
                    signed_ok = false;
                }
            }
            if signed_ok {
                Some(format!(
                    "{set}: signed with Developer ID. The macOS grant will persist across \
                     all future reinstalls."
                ))
            } else {
                Some(warnings.join("\n"))
            }
        }
        None => {
            let path_str = primary_path.to_string_lossy();
            let guidance = if set == MPM_SET {
                app_data_guidance(&path_str)
            } else {
                fda_guidance(&path_str)
            };
            Some(guidance)
        }
    }
}

/// Run post-install codesign and FDA guidance for trusty-search.
///
/// Why: After trusty-search (and trusty-embedderd) are installed the installer
/// should immediately sign them (if a cert is available) or report the FDA
/// re-grant guidance so the operator knows what to do next.
///
/// What: On macOS delegates to [`post_install_signed_set`] for [`SEARCH_SET`],
/// returning its note/guidance text instead of printing directly (see that
/// function's doc). On non-macOS: `None`.
///
/// Test: `tests::fda_guidance_contains_steps` (pure); signing itself is not
/// invoked in tests (side-effecting).
#[must_use]
pub fn post_install_search(install_dir: &std::path::Path, json: bool) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        post_install_signed_set(install_dir, SEARCH_SET, json)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (install_dir, json); // suppress unused warnings on non-macOS
        None
    }
}

/// Run post-install codesign and App-Data-TCC guidance for trusty-mpm.
///
/// Why: Owner-authorized scope extension (#2558, 2026-07-14) — `tm` needs the
/// same stable-identity signing as trusty-search so the App Data TCC prompt
/// does not re-fire on every `cargo install` rebuild.
///
/// What: On macOS delegates to [`post_install_signed_set`] for [`MPM_SET`],
/// returning its note/guidance text instead of printing directly (see that
/// function's doc). On non-macOS: `None`.
///
/// Test: `tests::app_data_guidance_mentions_prompt` (pure); signing itself is
/// not invoked in tests (side-effecting).
#[must_use]
pub fn post_install_mpm(install_dir: &std::path::Path, json: bool) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        post_install_signed_set(install_dir, MPM_SET, json)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (install_dir, json); // suppress unused warnings on non-macOS
        None
    }
}

#[cfg(test)]
mod tests;
