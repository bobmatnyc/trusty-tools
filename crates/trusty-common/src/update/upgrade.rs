//! Upgrade primitives: `cargo install`, binary health-gate, and safe self-restart.
//!
//! Why: Extracted from `update/mod.rs` to keep every file under the 500-line cap
//! while centralising the full upgrade workflow so trusty-memory and trusty-search
//! share identical battle-tested logic.
//!
//! What: Three building blocks plus one orchestrating function:
//! - [`perform_upgrade`] — shell out to `cargo install <name> --locked`,
//!   inheriting stdio (for the standalone `upgrade` commands); the
//!   [`perform_upgrade_captured`] sibling captures stdio instead, for a
//!   caller (trusty-installer) that may be driving its own live terminal
//!   display concurrently (#3830).
//! - [`verify_installed_binary`] — health-gate via `<bin> --version`.
//! - [`is_launchd_supervised`] — authoritative launchd-supervision check
//!   (delegates to [`crate::supervision`]; issue #4469 replaced the env-var
//!   heuristic that let an unsupervised child self-report as supervised).
//! - [`upgrade_and_restart`] — compose the three above into the full workflow.
//!
//! Test: `cargo test -p trusty-common --features update-check`

/// How a shelled-out upgrade command's stdout/stderr should be handled.
///
/// Why (#3830): `perform_upgrade` is shared — trusty-memory's and
/// trusty-search's own `upgrade` commands intentionally want cargo's live
/// build output inherited straight through to the operator's terminal. But
/// trusty-installer's `install_all` calls into this same primitive from
/// INSIDE its per-component loop while an interactive `LiveChecklist`
/// (`indicatif::MultiProgress`) may still be actively steady-ticking OTHER
/// not-yet-terminal rows on a background thread — an inherited child writing
/// straight to that same terminal fd races indicatif's redraw and desyncs its
/// line-count tracking, producing the exact duplicate/interleaved-line
/// corruption #3830 reported (traced: `download::try_install_prebuilt`
/// returns `Outcome::Fallback` on ANY failure — network blip, 404,
/// rate-limit, SHA mismatch, not just an unsupported platform — so this path
/// is reachable on a Tier-1 machine too). A typed mode keeps that difference
/// explicit at each call site instead of a bare boolean.
/// What: [`Inherit`] passes the child's stdout/stderr straight through
/// (existing behaviour, unchanged for the standalone upgrade commands);
/// [`Capture`] captures both instead, folding stderr into the returned error
/// on failure so a captured diagnosis is never silently dropped.
/// Test: `run_with_mode_inherit_leaks_to_parent_stdio` (proves `Inherit`
/// really does inherit — the pre-fix behaviour, still correct for its
/// callers), `run_with_mode_capture_never_leaks_to_parent_stdio` (the #3830
/// regression proof for `Capture`).
///
/// [`Inherit`]: UpgradeOutput::Inherit
/// [`Capture`]: UpgradeOutput::Capture
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpgradeOutput {
    /// Inherit the parent's stdout/stderr (cargo's live build output is
    /// visible to the operator). Used by [`perform_upgrade`].
    Inherit,
    /// Capture stdout/stderr instead of inheriting. Used by
    /// [`perform_upgrade_captured`].
    Capture,
}

/// Run `cmd` to completion per `mode`, returning `Ok(())` on a zero exit.
///
/// Why: extracted as a free function over an already-configured `Command`
/// (rather than inlined in `perform_upgrade`) so the inherit-vs-capture
/// distinction is directly unit-testable against ANY command — not just
/// `cargo install`, which needs the network and is `#[ignore]`-tagged in CI.
/// What: [`UpgradeOutput::Inherit`] uses `.status()` (inherits stdio, the
/// pre-#3830 behaviour); [`UpgradeOutput::Capture`] uses `.output()`
/// (captures stdio) and folds trimmed stderr into the error on a non-zero
/// exit. `label` is the human-readable command description used in error
/// text (e.g. `` `cargo install foo --locked` ``).
/// Test: `run_with_mode_inherit_leaks_to_parent_stdio`,
/// `run_with_mode_capture_never_leaks_to_parent_stdio`,
/// `run_with_mode_capture_folds_stderr_into_error`.
pub(crate) async fn run_with_mode(
    mut cmd: tokio::process::Command,
    mode: UpgradeOutput,
    label: &str,
) -> anyhow::Result<()> {
    match mode {
        UpgradeOutput::Inherit => {
            let status = cmd
                .status()
                .await
                .map_err(|e| anyhow::anyhow!("failed to spawn {label}: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("{label} exited with status {status}"))
            }
        }
        UpgradeOutput::Capture => {
            let output = cmd
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to spawn {label}: {e}"))?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                if stderr.is_empty() {
                    Err(anyhow::anyhow!(
                        "{label} exited with status {}",
                        output.status
                    ))
                } else {
                    Err(anyhow::anyhow!(
                        "{label} exited with status {}: {stderr}",
                        output.status
                    ))
                }
            }
        }
    }
}

fn cargo_install_cmd(crate_name: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("cargo");
    cmd.args(["install", crate_name, "--locked"]);
    cmd
}

/// Run `cargo install <crate_name> --locked` to upgrade the named crate.
///
/// Why: Centralises the upgrade invocation so both trusty-memory and
/// trusty-search call the same battle-tested helper rather than each
/// hand-rolling a `tokio::process::Command`. Keeping it here behind the
/// `update-check` feature ensures no new transitive dependencies are added
/// (tokio::process is already a workspace dep).
///
/// What: Spawns `cargo install <crate_name> --locked`, inheriting the current
/// environment so `CARGO_HOME`, `RUSTUP_TOOLCHAIN`, and PATH resolve normally.
/// Cargo's stdout/stderr are inherited ([`UpgradeOutput::Inherit`]) so the
/// operator can follow progress. Returns `Ok(())` on exit 0; returns `Err`
/// with a descriptive message on non-zero exit.
///
/// For a caller that may be driving its own live terminal display
/// concurrently (trusty-installer's `LiveChecklist`), see
/// [`perform_upgrade_captured`] instead — inheriting here would corrupt that
/// display (#3830).
///
/// Test: `perform_upgrade_fails_cleanly_on_nonexistent_crate` (ignore-tagged —
/// no real cargo-install in CI); manual validation via `trusty-memory upgrade --yes`.
pub async fn perform_upgrade(crate_name: &str) -> anyhow::Result<()> {
    tracing::info!(crate_name, "running: cargo install {} --locked", crate_name);
    let label = format!("`cargo install {crate_name} --locked`");
    let result = run_with_mode(
        cargo_install_cmd(crate_name),
        UpgradeOutput::Inherit,
        &label,
    )
    .await;
    if result.is_ok() {
        tracing::info!(crate_name, "cargo install succeeded");
    }
    result
}

/// Run `cargo install <crate_name> --locked`, CAPTURING stdout/stderr instead
/// of inheriting them (#3830).
///
/// Why: trusty-installer's `install_all` calls into the cargo-fallback path
/// (`download::try_install_prebuilt` returning `Outcome::Fallback`, which
/// fires on ANY prebuilt-download failure — not just an unsupported
/// platform) from inside its per-component loop, while an interactive
/// `LiveChecklist` may still be actively animating OTHER rows in the
/// background. [`perform_upgrade`]'s inherited stdio would write straight
/// into indicatif's owned terminal region and reproduce the #3830
/// duplicate/interleaved-line corruption; this variant never touches the
/// terminal directly.
///
/// What: Identical to [`perform_upgrade`] except it uses
/// [`UpgradeOutput::Capture`] — the child's stdout/stderr are captured, and a
/// non-zero exit's stderr is folded into the returned error (never silently
/// dropped, never leaked to the live terminal).
///
/// Test: `run_with_mode_capture_never_leaks_to_parent_stdio`,
/// `run_with_mode_capture_folds_stderr_into_error`; the `cargo install`
/// wiring itself mirrors `perform_upgrade`'s (untested beyond that — no real
/// network install in CI).
pub async fn perform_upgrade_captured(crate_name: &str) -> anyhow::Result<()> {
    tracing::info!(
        crate_name,
        "running (captured): cargo install {} --locked",
        crate_name
    );
    let label = format!("`cargo install {crate_name} --locked`");
    let result = run_with_mode(
        cargo_install_cmd(crate_name),
        UpgradeOutput::Capture,
        &label,
    )
    .await;
    if result.is_ok() {
        tracing::info!(crate_name, "cargo install succeeded");
    }
    result
}

/// Resolve the ordered list of directories a binary might have been
/// installed into, given an optional home directory and `CARGO_HOME` value.
///
/// Why: `cargo install` and the prebuilt-download installer path
/// (`trusty-installer::download::default_install_dir`, `~/.local/bin`) place
/// binaries in different directories, and the cargo path honours a
/// `$CARGO_HOME` override (issue #1771). Extracting the rule into a pure
/// function over explicit `home` / `cargo_home` inputs — rather than reading
/// `dirs::home_dir()` / `std::env::var` directly — keeps it testable without
/// mutating global process state for the common case.
///
/// What: Returns, in priority order: [`crate::bin_resolve::canonical_bin_dir_from`]
/// (`<cargo_home>/bin` when `cargo_home` is `Some` and non-empty, else
/// `<home>/.cargo/bin`); then `<home>/.local/bin` (the prebuilt installer's
/// default). Entries that require `home` are omitted when `home` is `None`.
///
/// #4964: the first entry's rule is no longer restated here — it is the shared
/// [`crate::bin_resolve::canonical_bin_dir_from`], so this list and every
/// installer write path resolve the cargo bin dir identically.
///
/// Test: `candidate_bin_dirs_prefers_cargo_home_override`,
/// `candidate_bin_dirs_falls_back_to_dot_cargo`,
/// `candidate_bin_dirs_includes_local_bin`,
/// `candidate_bin_dirs_empty_without_home_or_cargo_home`.
pub(crate) fn candidate_bin_dirs(
    home: Option<&std::path::Path>,
    cargo_home: Option<&str>,
) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    if let Some(canonical) = crate::bin_resolve::canonical_bin_dir_from(home, cargo_home) {
        dirs.push(canonical);
    }

    if let Some(home) = home {
        dirs.push(home.join(".local").join("bin"));
    }

    dirs
}

/// Probe the freshly-installed binary with `--version` as a health gate.
///
/// Why: Before the daemon self-exits to trigger launchd's KeepAlive respawn,
/// we must confirm the new binary is not corrupt or incompatible. If the
/// health gate fails we keep the old binary running and report the failure
/// clearly — we never exit into a broken binary. The binary may have landed
/// in `~/.cargo/bin` (cargo-install path), `~/.local/bin` (the prebuilt
/// installer's default — see `trusty-installer::download::default_install_dir`),
/// or a custom `$CARGO_HOME/bin`; checking only `~/.cargo/bin` produced false
/// "not installed" reports for prebuilt installs (issue #1771/#1992).
///
/// What: Checks, in order, `$CARGO_HOME/bin/<binary_name>` (or
/// `~/.cargo/bin/<binary_name>` when `CARGO_HOME` is unset), then
/// `~/.local/bin/<binary_name>`, then falls back to PATH via `which`. Spawns
/// `<bin> --version` with a 10-second timeout; returns `Ok(())` if the
/// process exits 0. Returns `Err` on failure, timeout, or missing binary.
///
/// Test: `verify_installed_binary_passes_for_cargo` (ignore-tagged integration);
/// `verify_installed_binary_fails_for_missing_binary`,
/// `verify_installed_binary_finds_binary_in_cargo_bin`,
/// `verify_installed_binary_finds_binary_in_local_bin`,
/// `verify_installed_binary_finds_binary_via_path`,
/// `verify_installed_binary_honours_cargo_home_override`.
pub async fn verify_installed_binary(binary_name: &str) -> anyhow::Result<()> {
    let home = dirs::home_dir();
    let cargo_home = std::env::var("CARGO_HOME").ok();
    let found = candidate_bin_dirs(home.as_deref(), cargo_home.as_deref())
        .into_iter()
        .map(|dir| dir.join(binary_name))
        .find(|p| p.exists());

    let bin_path = match found {
        Some(p) => p.to_string_lossy().into_owned(),
        None => {
            let which_out = tokio::process::Command::new("which")
                .arg(binary_name)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to run `which {binary_name}`: {e}"))?;
            if !which_out.status.success() {
                return Err(anyhow::anyhow!(
                    "binary `{binary_name}` not found in ~/.cargo/bin, ~/.local/bin, or PATH"
                ));
            }
            String::from_utf8_lossy(&which_out.stdout).trim().to_owned()
        }
    };

    tracing::debug!(
        binary_name,
        bin_path,
        "probing installed binary with --version"
    );

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(&bin_path)
            .arg("--version")
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let version_line = String::from_utf8_lossy(&output.stdout);
            tracing::info!(
                binary_name,
                version = version_line.trim(),
                "health gate passed"
            );
            Ok(())
        }
        Ok(Ok(output)) => Err(anyhow::anyhow!(
            "`{bin_path} --version` exited with status {} — new binary may be broken",
            output.status
        )),
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "failed to spawn `{bin_path} --version`: {e}"
        )),
        Err(_) => Err(anyhow::anyhow!(
            "`{bin_path} --version` timed out after 10 s — new binary may be hung"
        )),
    }
}

/// Probe a binary at a KNOWN, CONCRETE path with `--version` as a health
/// gate (#3554).
///
/// Why: [`verify_installed_binary`] resolves a bare binary NAME through a
/// priority-ordered directory search that ends in a bare-name PATH `which`
/// fallback — the right shape when the caller genuinely does not know where
/// the binary landed (e.g. after a plain `cargo install`). But a caller that
/// just placed a binary at a SPECIFIC path (the prebuilt-download placement
/// destination, or a resolved `$CARGO_HOME/bin` join) already knows the exact
/// file to verify; re-deriving the path by name afterward is a needless,
/// shadowable extra hop — issue #3554: the web installer wrote 0.19.29 to
/// `~/.local/bin/tm`, then health-gated by NAME, which found a stale
/// 0.19.26 copy at `~/.cargo/bin/tm` first (both because
/// `candidate_bin_dirs` checks `~/.cargo/bin` before `~/.local/bin`, and
/// because a bare-name PATH lookup is inherently shadowable) and reported
/// the health gate "passed" against the WRONG binary. Probing the known
/// absolute path directly makes the health gate immune to any such
/// shadowing — there is no name resolution step for a stale binary to win.
///
/// What: Returns `Err` immediately if `bin_path` does not exist (no PATH
/// fallback — the caller already knows exactly where the binary should be).
/// Otherwise spawns `<bin_path> --version` with the same 10-second timeout as
/// [`verify_installed_binary`]; on a clean exit 0 returns `Ok(<trimmed stdout>)`
/// (the raw `--version` line, so the caller can parse/compare it against an
/// expected version). Returns `Err` on a non-zero exit, spawn failure, or
/// timeout.
///
/// Test: `verify_installed_binary_at_path_reads_exact_binary_despite_path_shadow`
/// (the #3554 regression shape), `verify_installed_binary_at_path_fails_for_missing_binary`.
pub async fn verify_installed_binary_at_path(bin_path: &std::path::Path) -> anyhow::Result<String> {
    if !bin_path.exists() {
        return Err(anyhow::anyhow!(
            "binary not found at expected install path {}",
            bin_path.display()
        ));
    }

    tracing::debug!(
        bin_path = %bin_path.display(),
        "probing installed binary with --version (concrete path, #3554)"
    );

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(bin_path)
            .arg("--version")
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let version_line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            tracing::info!(
                bin_path = %bin_path.display(),
                version = version_line,
                "health gate passed"
            );
            Ok(version_line)
        }
        Ok(Ok(output)) => Err(anyhow::anyhow!(
            "`{} --version` exited with status {} — new binary may be broken",
            bin_path.display(),
            output.status
        )),
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "failed to spawn `{} --version`: {e}",
            bin_path.display()
        )),
        Err(_) => Err(anyhow::anyhow!(
            "`{} --version` timed out after 10 s — new binary may be hung",
            bin_path.display()
        )),
    }
}

/// Return `true` only when launchd AUTHORITATIVELY reports it runs this
/// process.
///
/// Why: The self-restart strategy (non-zero exit → launchd KeepAlive respawn)
/// only works when launchd is actually supervising the process; started
/// manually, a non-zero exit just terminates with no respawn. Until #4469 this
/// was an environment-variable heuristic (`XPC_SERVICE_NAME` set and
/// `TERM_PROGRAM` absent, falling back to `getppid() == 1`) and BOTH prongs were
/// satisfiable by a process launchd had never heard of — env vars are inherited
/// by every child, and an orphan whose parent exited is reparented to PID 1. A
/// `tm daemon` child could therefore self-report supervised, pass the #4230
/// callee-side guard, and publish `supervised: true` on `/health`, making the
/// documented `curl /health | jq '.supervised'` verification unsound in exactly
/// the case it exists to catch.
///
/// What: delegates to [`crate::supervision::launchd_supervision`], which asks
/// launchd itself. Maps the three-state answer down to a bool by treating
/// `Unknown` as NOT supervised — the conservative direction here, because the
/// only consequence is whether [`upgrade_and_restart`] exits expecting a
/// respawn or prints a manual-restart hint, and printing a hint after a
/// successful upgrade is harmless while exiting into no respawn is not. Callers
/// that must DISTINGUISH "not supervised" from "could not tell" (the daemon's
/// `/health` flag, `tm doctor`) call `launchd_supervision` directly.
///
/// Test: `is_launchd_supervised_is_not_fooled_by_inherited_env` in `tests.rs`
/// (the #4469 regression proof) and the exhaustive parser/state coverage in
/// `crate::supervision::tests`.
pub fn is_launchd_supervised() -> bool {
    crate::supervision::launchd_supervision().is_supervised()
}

/// Install the new version, health-gate it, then restart or print a hint.
///
/// Why: Concentrates the full upgrade workflow so trusty-memory and
/// trusty-search share identical logic. The function diverges at the final
/// step depending on whether launchd supervision is detected.
///
/// What:
///
/// 1. `perform_upgrade(crate_name)` — run `cargo install <name> --locked`.
/// 2. `verify_installed_binary(binary_name)` — health gate.
/// 3. If supervised by launchd: log to stderr and call
///    `std::process::exit(1)` so launchd respawns the new binary. This
///    path never returns.
/// 4. If not supervised: return `Ok(Some(<hint>))` telling the operator
///    to restart manually.
///
/// Any error short-circuits and returns `Err`; the old binary keeps running.
///
/// `crate_name` is the crates.io package name; `binary_name` is the
/// installed binary name (for trusty-memory and trusty-search they match).
///
/// Test: individual steps (`perform_upgrade`, `verify_installed_binary`,
/// `is_launchd_supervised`) are tested independently in `tests.rs`.
/// The full path is exercised by the CLI upgrade handler and MCP tool.
pub async fn upgrade_and_restart(
    crate_name: &str,
    binary_name: &str,
) -> anyhow::Result<Option<String>> {
    perform_upgrade(crate_name).await?;

    verify_installed_binary(binary_name).await.map_err(|e| {
        anyhow::anyhow!(
            "health gate failed after installing {crate_name}: {e}\n\
             The old binary is still running — do not restart manually until resolved."
        )
    })?;

    if is_launchd_supervised() {
        tracing::info!(
            crate_name,
            binary_name,
            "upgrade succeeded; restarting under launchd supervision"
        );
        eprintln!("[trusty-common] {binary_name} upgraded successfully — restarting under launchd");
        // Non-zero exit → launchd KeepAlive::OnSuccess respawn.
        std::process::exit(1);
    }

    let hint = format!(
        "{binary_name} upgraded successfully. \
         No supervisor detected — please restart the daemon to apply the new binary."
    );
    tracing::info!(hint, "no launchd supervision detected; restart required");
    Ok(Some(hint))
}
