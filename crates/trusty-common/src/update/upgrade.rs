//! Upgrade primitives: `cargo install`, binary health-gate, and safe self-restart.
//!
//! Why: Extracted from `update/mod.rs` to keep every file under the 500-line cap
//! while centralising the full upgrade workflow so trusty-memory and trusty-search
//! share identical battle-tested logic.
//!
//! What: Three building blocks plus one orchestrating function:
//! - [`perform_upgrade`] — shell out to `cargo install <name> --locked`.
//! - [`verify_installed_binary`] — health-gate via `<bin> --version`.
//! - [`is_launchd_supervised`] — detect launchd supervision heuristically.
//! - [`upgrade_and_restart`] — compose the three above into the full workflow.
//!
//! Test: `cargo test -p trusty-common --features update-check`

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
/// Cargo's stdout/stderr are inherited so the operator can follow progress.
/// Returns `Ok(())` on exit 0; returns `Err` with a descriptive message on
/// non-zero exit.
///
/// Test: `perform_upgrade_fails_cleanly_on_nonexistent_crate` (ignore-tagged —
/// no real cargo-install in CI); manual validation via `trusty-memory upgrade --yes`.
pub async fn perform_upgrade(crate_name: &str) -> anyhow::Result<()> {
    tracing::info!(crate_name, "running: cargo install {} --locked", crate_name);

    let status = tokio::process::Command::new("cargo")
        .args(["install", crate_name, "--locked"])
        .status()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn `cargo install`: {e}"))?;

    if status.success() {
        tracing::info!(crate_name, "cargo install succeeded");
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "`cargo install {crate_name} --locked` exited with status {status}"
        ))
    }
}

/// Probe the freshly-installed binary with `--version` as a health gate.
///
/// Why: Before the daemon self-exits to trigger launchd's KeepAlive respawn,
/// we must confirm the new binary is not corrupt or incompatible. If the
/// health gate fails we keep the old binary running and report the failure
/// clearly — we never exit into a broken binary.
///
/// What: Locates the binary by checking `~/.cargo/bin/<binary_name>` first,
/// then falling back to PATH via `which`. Spawns `<bin> --version` with a
/// 10-second timeout; returns `Ok(())` if the process exits 0.
/// Returns `Err` on failure, timeout, or missing binary.
///
/// Test: `verify_installed_binary_passes_for_cargo` (ignore-tagged integration);
/// `verify_installed_binary_fails_for_missing_binary` tests the failure path.
pub async fn verify_installed_binary(binary_name: &str) -> anyhow::Result<()> {
    // Prefer the explicit ~/.cargo/bin/<name> path — that is where
    // `cargo install` drops the binary, and it may differ from PATH.
    let cargo_bin = dirs::home_dir().map(|h| h.join(".cargo").join("bin").join(binary_name));

    let bin_path = match cargo_bin {
        Some(p) if p.exists() => p.to_string_lossy().into_owned(),
        _ => {
            let which_out = tokio::process::Command::new("which")
                .arg(binary_name)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("failed to run `which {binary_name}`: {e}"))?;
            if !which_out.status.success() {
                return Err(anyhow::anyhow!(
                    "binary `{binary_name}` not found in ~/.cargo/bin or PATH"
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

/// Return `true` when the current process was started by launchd.
///
/// Why: The self-restart strategy (non-zero exit → launchd KeepAlive
/// respawn) only works when launchd is actually supervising the process.
/// If the daemon was started manually, a non-zero exit would simply terminate
/// with no respawn. Detecting the supervisor lets us choose the right
/// post-upgrade action: supervised → exit(1); unsupervised → print hint.
///
/// What: Heuristic with two prongs on macOS.
///
/// 1. `XPC_SERVICE_NAME` is injected by launchd into every managed job.
///    We guard against interactive terminals (which may inherit env vars)
///    by requiring `TERM_PROGRAM` to be absent.
/// 2. Fallback: PPID == 1 means launchd is the direct parent.
///
/// On non-macOS platforms always returns `false` (launchd is macOS-only).
///
/// Test: `is_launchd_supervised_returns_false_in_test_env` in `tests.rs` verifies
/// that the function returns false in a normal test/terminal environment.
pub fn is_launchd_supervised() -> bool {
    #[cfg(target_os = "macos")]
    {
        let has_xpc = std::env::var("XPC_SERVICE_NAME")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let in_terminal = std::env::var("TERM_PROGRAM")
            .map(|v| !v.is_empty())
            .unwrap_or(false);

        if has_xpc && !in_terminal {
            return true;
        }

        // Fallback: PPID == 1 on macOS means launchd is the direct parent.
        #[cfg(unix)]
        {
            let ppid = unsafe { libc::getppid() };
            if ppid == 1 {
                return true;
            }
        }
    }

    false
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
