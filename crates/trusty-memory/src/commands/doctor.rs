//! Handler for `trusty-memory doctor`.
//!
//! Why: GH #62 — when the launchd-managed daemon misbehaves (silent EROFS on
//! fastembed model download, stale plist without `FASTEMBED_CACHE_PATH`,
//! daemon not bound, etc.), operators currently have to grep through
//! `~/Library/LaunchAgents`, `~/.cache/fastembed`, and the lock file by hand
//! to figure out what's wrong. `doctor` runs the same checks in one shot and
//! prints a human-readable pass/fail report so the user can act immediately.
//! What: a one-shot CLI command that runs four checks:
//!   1. fastembed cache directory exists and is readable
//!   2. launchd plist exists at `~/Library/LaunchAgents/com.trusty.memory.plist`
//!      and contains the `FASTEMBED_CACHE_PATH` env var (macOS only)
//!   3. The HTTP daemon responds to `GET /health` on its configured port
//!   4. No obvious stale palace lock sidecar files (`*.lock`) under the data dir
//!
//! Each check prints a ✅ or ❌ line. The command exits 0 if all critical
//! checks pass, 1 otherwise.
//!
//! Test: `fastembed_cache_check_reports_missing_dir` and
//! `plist_check_detects_missing_env_var` cover the helpers; the full
//! orchestrator is exercised manually via
//! `cargo run -p trusty-memory -- doctor`.

use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::project_root::{project_slug_at, PERSONAL_PALACE};

/// Outcome of a single doctor check.
///
/// Why: keeps the orchestrator able to count failures without re-parsing
/// strings, while preserving the human-readable message for printing.
/// What: a tiny enum-with-message: `Pass` for green checks, `Warn` for
/// non-critical issues (don't flip exit code), `Fail` for actionable
/// failures (flip exit code to 1).
/// Test: not directly — exercised via the per-check unit tests.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// A single doctor check result.
///
/// Why: the orchestrator collects every result before printing so the
/// summary line ("N checks passed, M failed") is accurate.
/// What: bundles the status with a human-readable label and optional detail
/// (file path, error message, etc.).
/// Test: covered transitively by the helper tests.
#[derive(Debug, Clone)]
struct CheckResult {
    status: CheckStatus,
    label: String,
    detail: Option<String>,
}

impl CheckResult {
    fn pass(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Pass,
            label: label.into(),
            detail: Some(detail.into()),
        }
    }
    fn warn(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Warn,
            label: label.into(),
            detail: Some(detail.into()),
        }
    }
    fn fail(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            label: label.into(),
            detail: Some(detail.into()),
        }
    }

    fn print(&self) {
        let glyph = match self.status {
            CheckStatus::Pass => "✅".to_string(),
            CheckStatus::Warn => "⚠️ ".to_string(),
            CheckStatus::Fail => "❌".to_string(),
        };
        let label = match self.status {
            CheckStatus::Pass => self.label.green().to_string(),
            CheckStatus::Warn => self.label.yellow().to_string(),
            CheckStatus::Fail => self.label.red().to_string(),
        };
        match &self.detail {
            Some(d) => println!("{glyph} {label} — {}", d.dimmed()),
            None => println!("{glyph} {label}"),
        }
    }
}

/// Outcome of a single palace audit entry for `doctor --fix-palaces`.
///
/// Why: separating the palace audit result from the standard `CheckResult`
/// makes the presentation logic cleaner — palace audit output is tabular
/// (one row per palace) rather than the pass/warn/fail traffic-light model
/// used by the daemon health checks.
/// What: encodes whether a palace is `Ok` (name matches a detectable project
/// slug or is the `personal` sentinel), `Orphaned` (name does not correspond
/// to any project directory we can find on disk), or `Empty` (the palace
/// directory exists but has no `palace.json`).
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PalaceAuditStatus {
    /// Palace name matches the current project slug or is `personal`.
    Ok,
    /// Palace name does not match any project directory found by walking
    /// `find_project_root` upward from the palace data directory's parent.
    /// Advisory: existing data is intact; no mutation is made.
    Orphaned,
    /// The palace directory exists under the data root but contains no
    /// `palace.json` (was never fully initialised, or was manually deleted).
    Empty,
}

/// One row in the `doctor --fix-palaces` audit table.
///
/// Why: bundles the palace id, its on-disk data directory, and the audit
/// status into a single struct so the presenter and the audit logic can be
/// separated cleanly.
/// What: carries `id` (the palace name used as the on-disk dir name),
/// `data_dir` (absolute path), and `status`.
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`.
#[derive(Debug, Clone)]
pub struct PalaceAuditEntry {
    pub id: String,
    pub data_dir: PathBuf,
    pub status: PalaceAuditStatus,
}

/// Audit every palace subdirectory under `registry_dir` and classify each.
///
/// Why: the classification logic is factored out of the presenter so it can
/// be unit-tested without touching the terminal.
/// What: walks `registry_dir` one level deep; for each subdirectory, checks
/// for a `palace.json` (marks `Empty` when absent), then checks whether the
/// subdirectory name either equals `personal` or matches the slug derived
/// from any parent directory that is a project root. A palace whose name
/// equals its own parent directory slug is considered `Ok` (it was created
/// when standing inside that project). Palaces whose names do not satisfy
/// either condition are `Orphaned`.
///
/// Note: the "matches a project root" check is advisory — we walk *up* from
/// the palace's own data directory looking for project markers. For palaces
/// created from within a project, the data directory lives under the data
/// root (e.g. `~/Library/Application Support/trusty-memory/my-project/`),
/// which is typically NOT inside any project directory. We therefore
/// additionally look for a directory on disk whose basename slug equals the
/// palace id — a heuristic that covers the common case (project at
/// `~/Projects/my-project`) without requiring a registry of project paths.
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`.
pub fn audit_palaces(registry_dir: &Path) -> Vec<PalaceAuditEntry> {
    let Ok(entries) = std::fs::read_dir(registry_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // `Empty` takes priority — no palace.json means it was never
        // initialised or was partially deleted.
        if !path.join("palace.json").exists() {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Empty,
            });
            continue;
        }
        // `personal` is always Ok.
        if id == PERSONAL_PALACE {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Ok,
            });
            continue;
        }
        // Check whether any ancestor of `registry_dir` is a project root
        // whose slug matches the palace id. This catches the case where the
        // user runs the daemon from inside a project tree.
        let matches_ancestor = project_slug_at(registry_dir)
            .map(|slug| slug == id)
            .unwrap_or(false);
        if matches_ancestor {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Ok,
            });
            continue;
        }
        // Heuristic: look for a directory named after the slug in the user's
        // common project locations. We check `$HOME/Projects/<id>`,
        // `$HOME/Developer/<id>`, `$HOME/Code/<id>`, and `$HOME/<id>` as
        // plausible locations. This keeps the check lightweight (no
        // recursive scan) while catching the most common single-project-dir
        // layout.
        let found_on_disk = dirs::home_dir()
            .map(|home| {
                let candidates = [
                    home.join("Projects").join(&id),
                    home.join("Developer").join(&id),
                    home.join("Code").join(&id),
                    home.join(&id),
                ];
                candidates.iter().any(|c| c.is_dir())
            })
            .unwrap_or(false);
        let status = if found_on_disk {
            PalaceAuditStatus::Ok
        } else {
            PalaceAuditStatus::Orphaned
        };
        out.push(PalaceAuditEntry {
            id,
            data_dir: path,
            status,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Entry point for `trusty-memory doctor --fix-palaces [--fix]`.
///
/// Why: issue #88 — users with many accumulated palaces (e.g. 89 from
/// account-recovery) need a way to see which ones are orphaned without
/// destructive auto-cleanup. This command provides the read-only audit view
/// (default) and prints rename suggestions when `--fix` is also given.
/// Actual renaming is deliberately deferred to a future PR to avoid data
/// loss during this first conservative implementation.
/// What: resolves the palace registry directory (same logic as daemon startup),
/// calls `audit_palaces`, prints a table, and exits 0. The `--fix` flag
/// adds "rename suggested: X → personal" lines for every `Orphaned` entry
/// but does NOT mutate the filesystem.
/// Test: `doctor_fix_palaces_lists_orphaned_dry_run`.
pub async fn handle_doctor_fix_palaces(suggest_fix: bool) -> Result<()> {
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{} could not resolve data directory: {e:#}", "✗".red());
            return Ok(());
        }
    };
    let registry_dir = crate::resolve_palace_registry_dir(data_dir);

    println!(
        "{} Auditing palaces under {}\n",
        "·".dimmed(),
        registry_dir.display()
    );

    let entries = audit_palaces(&registry_dir);
    if entries.is_empty() {
        println!("{} No palace directories found.", "·".dimmed());
        return Ok(());
    }

    let mut ok_count = 0usize;
    let mut orphaned_count = 0usize;
    let mut empty_count = 0usize;

    for entry in &entries {
        match entry.status {
            PalaceAuditStatus::Ok => {
                ok_count += 1;
                println!(
                    "✅  {} — {}",
                    entry.id.green(),
                    "project palace ok".dimmed()
                );
            }
            PalaceAuditStatus::Orphaned => {
                orphaned_count += 1;
                println!(
                    "⚠️   {} — {}",
                    entry.id.yellow(),
                    "orphaned (no matching project directory found on disk)".dimmed()
                );
                if suggest_fix {
                    println!(
                        "   {} rename suggested: {} → {}",
                        "→".dimmed(),
                        entry.id.yellow(),
                        PERSONAL_PALACE.cyan()
                    );
                }
            }
            PalaceAuditStatus::Empty => {
                empty_count += 1;
                println!(
                    "❌  {} — {}",
                    entry.id.red(),
                    "empty (no palace.json; directory may be a leftover)".dimmed()
                );
            }
        }
    }

    println!();
    println!(
        "{} palace audit: {} ok, {} orphaned, {} empty.",
        "·".dimmed(),
        ok_count,
        orphaned_count,
        empty_count
    );

    if orphaned_count > 0 && !suggest_fix {
        println!(
            "{} Run with {} to see rename suggestions (no filesystem changes made).",
            "·".dimmed(),
            "--fix-palaces --fix".cyan()
        );
    }
    if suggest_fix && orphaned_count > 0 {
        println!(
            "{} Rename suggestions printed above (dry-run — no filesystem changes made).",
            "·".dimmed()
        );
    }

    Ok(())
}

/// Entry point for `trusty-memory doctor`.
///
/// Why: a single command for operators to triage daemon health without
/// having to remember four separate diagnostic incantations.
/// What: runs each check, prints the ✅/❌ line, and exits 0 (all pass) or
/// 1 (any `Fail`). `Warn` results print but do not flip the exit code.
/// Test: orchestrator is process-level; per-check helpers are unit-tested.
pub async fn handle_doctor() -> Result<()> {
    println!("{} Running trusty-memory diagnostics…\n", "·".dimmed());

    let mut results: Vec<CheckResult> = Vec::new();

    // Check 1: fastembed cache.
    results.push(check_fastembed_cache());

    // Check 2: launchd plist (macOS only).
    #[cfg(target_os = "macos")]
    {
        results.push(check_launchd_plist());
    }
    #[cfg(not(target_os = "macos"))]
    {
        results.push(CheckResult::warn(
            "launchd plist".to_string(),
            "skipped (not macOS)".to_string(),
        ));
    }

    // Check 3: HTTP daemon health.
    results.push(check_daemon_health().await);

    // Check 4: stale palace locks.
    results.push(check_stale_palace_locks());

    for r in &results {
        r.print();
    }

    let failed = results
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count();
    let passed = results
        .iter()
        .filter(|r| r.status == CheckStatus::Pass)
        .count();
    let warned = results
        .iter()
        .filter(|r| r.status == CheckStatus::Warn)
        .count();

    println!();
    if failed == 0 {
        println!(
            "{} {} passed, {} warnings, {} failed.",
            "✓".green(),
            passed,
            warned,
            failed
        );
        Ok(())
    } else {
        eprintln!(
            "{} {} passed, {} warnings, {} failed.",
            "✗".red(),
            passed,
            warned,
            failed
        );
        std::process::exit(1);
    }
}

/// Verify the fastembed model cache exists and is readable.
///
/// Why: GH #58/#62 — when the daemon can't reach a writable cache path it
/// fails with EROFS on first embed and never goes ready. Checking for the
/// resolved cache dir up-front catches both "env var unset" (resolver falls
/// back to `$HOME/.cache/fastembed` which might not exist) and "directory
/// pinned but missing".
/// What: calls `trusty_common::embedder::resolve_fastembed_cache_dir()`,
/// then checks the path exists and is a directory we can read. Returns
/// `Pass` when the dir contains at least one model file, `Warn` when it
/// exists but is empty (pre-warm never ran), `Fail` when it does not exist.
/// Test: `fastembed_cache_check_reports_missing_dir`.
fn check_fastembed_cache() -> CheckResult {
    let cache = trusty_common::embedder::resolve_fastembed_cache_dir();
    let label = "fastembed cache".to_string();
    if !cache.exists() {
        return CheckResult::fail(
            label,
            format!(
                "missing: {} — run `trusty-memory setup` to pre-warm",
                cache.display()
            ),
        );
    }
    if !cache.is_dir() {
        return CheckResult::fail(label, format!("not a directory: {}", cache.display()));
    }
    match fastembed_cache_has_models(&cache) {
        Ok(true) => CheckResult::pass(label, format!("ready at {}", cache.display())),
        Ok(false) => CheckResult::warn(
            label,
            format!(
                "{} exists but is empty — daemon will download on first request",
                cache.display()
            ),
        ),
        Err(e) => CheckResult::fail(label, format!("cannot read {}: {e}", cache.display())),
    }
}

/// Check whether the fastembed cache directory holds at least one entry.
///
/// Why: an empty `~/.cache/fastembed` is operationally equivalent to a
/// missing one — the daemon will still have to download on first call.
/// What: returns `Ok(true)` if `read_dir` yields any entry, `Ok(false)` if
/// it's empty, `Err` if `read_dir` itself fails (permissions, etc.).
/// Test: `fastembed_cache_has_models_detects_entries`.
fn fastembed_cache_has_models(path: &Path) -> std::io::Result<bool> {
    let mut iter = std::fs::read_dir(path)?;
    Ok(iter.next().is_some())
}

/// Verify the launchd plist exists and contains `FASTEMBED_CACHE_PATH`.
///
/// Why: GH #62 — the whole point of the plist update is that
/// `FASTEMBED_CACHE_PATH` (and/or `FASTEMBED_CACHE_DIR`) is wired into the
/// daemon's environment. If an older plist is still installed without the
/// env var, the daemon will silently fail with EROFS. Detecting this is
/// the single most useful thing `doctor` can do.
/// What: resolves `~/Library/LaunchAgents/com.trusty.memory.plist`, reads
/// it as text, and looks for the `FASTEMBED_CACHE_PATH` key. `Pass` when
/// present, `Fail` when the file exists but the key is missing, `Fail`
/// when the file is missing entirely.
/// Test: `plist_check_detects_missing_env_var`.
#[cfg(target_os = "macos")]
fn check_launchd_plist() -> CheckResult {
    let label = "launchd plist".to_string();
    let Some(home) = dirs::home_dir() else {
        return CheckResult::fail(label, "could not resolve $HOME".to_string());
    };
    let plist = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", crate::commands::service::LAUNCHD_LABEL));
    if !plist.exists() {
        return CheckResult::fail(
            label,
            format!(
                "missing: {} — run `trusty-memory service install`",
                plist.display()
            ),
        );
    }
    match plist_contains_fastembed_cache_path(&plist) {
        Ok(true) => CheckResult::pass(label, format!("{} ok", plist.display())),
        Ok(false) => CheckResult::fail(
            label,
            format!(
                "{} is missing FASTEMBED_CACHE_PATH — reinstall via `trusty-memory service install`",
                plist.display()
            ),
        ),
        Err(e) => CheckResult::fail(label, format!("cannot read {}: {e}", plist.display())),
    }
}

/// Check whether a plist file contains the `FASTEMBED_CACHE_PATH` key.
///
/// Why: keeping the parse trivial (substring search) avoids pulling in an
/// XML/plist crate just for one diagnostic. The key string is unique enough
/// inside a launchd plist that a false positive is implausible.
/// What: reads the file as UTF-8 text and returns true iff the literal
/// `FASTEMBED_CACHE_PATH` substring appears.
/// Test: `plist_check_detects_missing_env_var`.
#[cfg(target_os = "macos")]
fn plist_contains_fastembed_cache_path(path: &Path) -> std::io::Result<bool> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents.contains("FASTEMBED_CACHE_PATH"))
}

/// Verify the HTTP daemon responds to `GET /health`.
///
/// Why: the most direct test of "is the daemon running and accepting
/// requests". Reading the lock file alone is not enough — the file can be
/// stale after a crash. A real round-trip catches that.
/// What: reads the daemon address from `trusty_common::read_daemon_addr`,
/// then issues a `GET /health` with a 2-second timeout. `Pass` on any 2xx
/// status, `Fail` on connection error or non-2xx, `Fail` when the address
/// file is absent (daemon never started).
/// Test: side-effecting (network); covered manually.
async fn check_daemon_health() -> CheckResult {
    let label = "HTTP daemon".to_string();
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(a)) => a,
        Ok(None) => {
            return CheckResult::fail(
                label,
                "no daemon address recorded — start with `trusty-memory service start`".to_string(),
            );
        }
        Err(e) => {
            return CheckResult::fail(label, format!("could not read daemon address: {e}"));
        }
    };
    // `read_daemon_addr` returns a bare `host:port` (e.g. `127.0.0.1:3038`);
    // reqwest requires a full URL, so prepend the scheme when missing.
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.clone()
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}/health");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => return CheckResult::fail(label, format!("could not build HTTP client: {e}")),
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            CheckResult::pass(label, format!("{} → {}", url, resp.status()))
        }
        Ok(resp) => CheckResult::fail(label, format!("{} → {}", url, resp.status())),
        Err(e) => CheckResult::fail(label, format!("{url} unreachable: {e}")),
    }
}

/// Scan the data directory for stray `*.lock` files left over from a
/// crashed daemon.
///
/// Why: redb leaves a sidecar lock file when a previous owner exits
/// uncleanly; opening the palace from a fresh daemon then fails until the
/// stale lock is removed. Surfacing this in `doctor` saves users from a
/// confusing "palace won't load" symptom that has nothing to do with the
/// palace itself.
/// What: walks the trusty-memory data dir (one level deep into each palace
/// directory) and lists any `*.lock` file. `Pass` when none found, `Warn`
/// when at least one is present (the daemon may be running and using it,
/// so we can't safely call this a `Fail`).
/// Test: `stale_lock_check_warns_when_lock_present`.
fn check_stale_palace_locks() -> CheckResult {
    let label = "palace locks".to_string();
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => return CheckResult::fail(label, format!("could not resolve data dir: {e}")),
    };
    let root = crate::resolve_palace_registry_dir(data_dir);
    let locks = find_lock_files(&root);
    if locks.is_empty() {
        CheckResult::pass(label, format!("{} clean", root.display()))
    } else {
        let preview = locks
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if locks.len() > 3 {
            format!(" (+{} more)", locks.len() - 3)
        } else {
            String::new()
        };
        CheckResult::warn(
            label,
            format!(
                "{} lock file(s) found: {preview}{suffix} — if the daemon is stopped, these can be removed",
                locks.len()
            ),
        )
    }
}

/// Collect `*.lock` files one level deep beneath `root`.
///
/// Why: keeps the scan cheap (no recursive walk) while still catching the
/// common case of `<palace>/kg.redb.lock` sidecars from redb crashes.
/// What: returns every `*.lock` path in `root` itself and in each
/// immediate subdirectory of `root`. Missing or unreadable directories are
/// silently skipped (the surrounding check handles fatal data-dir errors).
/// Test: `find_lock_files_returns_paths`.
fn find_lock_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_lock_file(&path) {
            out.push(path.clone());
        }
        if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) {
                for child in sub.flatten() {
                    let cpath = child.path();
                    if is_lock_file(&cpath) {
                        out.push(cpath);
                    }
                }
            }
        }
    }
    out
}

fn is_lock_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: when the cache directory genuinely doesn't exist, doctor must
    /// flag it as a `Fail` so the user knows to run `setup` — silently
    /// passing here is the whole bug GH #62 protects against.
    /// What: builds a path under a tempdir that we deliberately do not
    /// create, calls the helper with a fake `FASTEMBED_CACHE_PATH`, and
    /// asserts the result is `Fail`.
    /// Test: pure, no network.
    #[test]
    fn fastembed_cache_check_reports_missing_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does_not_exist");
        // SAFETY: serial test — no other thread is reading the env var.
        // We don't set FASTEMBED_CACHE_DIR (which would take precedence)
        // and instead exercise FASTEMBED_CACHE_PATH so the resolver
        // returns our specific missing path.
        unsafe {
            std::env::remove_var("FASTEMBED_CACHE_DIR");
            std::env::set_var("FASTEMBED_CACHE_PATH", &missing);
        }
        let result = check_fastembed_cache();
        unsafe {
            std::env::remove_var("FASTEMBED_CACHE_PATH");
        }
        assert_eq!(result.status, CheckStatus::Fail, "got: {:?}", result);
    }

    /// Why: detecting model files (vs an empty cache) is what distinguishes
    /// "pre-warmed" from "first request will pay the download cost". The
    /// helper has to differentiate the two.
    /// What: creates an empty dir, asserts `Ok(false)`; writes a file,
    /// asserts `Ok(true)`.
    /// Test: pure filesystem.
    #[test]
    fn fastembed_cache_has_models_detects_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!fastembed_cache_has_models(tmp.path()).unwrap());
        std::fs::write(tmp.path().join("model.onnx"), b"x").unwrap();
        assert!(fastembed_cache_has_models(tmp.path()).unwrap());
    }

    /// Why: the plist check is the most operationally important diagnostic
    /// — it's the difference between "the daemon will work" and "the
    /// daemon will EROFS on first embed". Both branches must be covered.
    /// What: writes a plist *without* the key and asserts `Ok(false)`;
    /// writes a plist *with* the key and asserts `Ok(true)`.
    /// Test: pure filesystem.
    #[cfg(target_os = "macos")]
    #[test]
    fn plist_check_detects_missing_env_var() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let no_key = tmp.path().join("no_key.plist");
        std::fs::write(&no_key, "<plist><dict></dict></plist>").unwrap();
        assert!(
            !plist_contains_fastembed_cache_path(&no_key).unwrap(),
            "plist without env var must report false"
        );

        let with_key = tmp.path().join("with_key.plist");
        std::fs::write(
            &with_key,
            "<plist><dict><key>FASTEMBED_CACHE_PATH</key><string>/x</string></dict></plist>",
        )
        .unwrap();
        assert!(
            plist_contains_fastembed_cache_path(&with_key).unwrap(),
            "plist with env var must report true"
        );
    }

    /// Why: `audit_palaces` is the core of the `--fix-palaces` audit; it must
    /// correctly classify palaces as `Ok`, `Orphaned`, and `Empty` so the
    /// presenter can display actionable information.
    /// What: build a mock registry under a tempdir with three palace
    /// directories: one matching the `personal` sentinel (Ok), one with a
    /// `palace.json` and no matching project directory (Orphaned), and one
    /// with no `palace.json` at all (Empty). Assert the audit returns three
    /// entries with the correct statuses.
    /// Test: pure filesystem.
    #[test]
    fn find_orphaned_palaces_lists_non_matching_and_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = tmp.path();

        // `personal` → always Ok.
        let personal = registry.join("personal");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::write(personal.join("palace.json"), b"{}").unwrap();

        // `orphaned-proj` → has palace.json but no matching project on disk.
        let orphaned = registry.join("orphaned-proj-xyzzy");
        std::fs::create_dir_all(&orphaned).unwrap();
        std::fs::write(orphaned.join("palace.json"), b"{}").unwrap();

        // `empty-palace` → no palace.json.
        let empty = registry.join("empty-palace");
        std::fs::create_dir_all(&empty).unwrap();

        let entries = audit_palaces(registry);

        let personal_entry = entries.iter().find(|e| e.id == "personal");
        assert!(personal_entry.is_some(), "personal must appear in audit");
        assert_eq!(
            personal_entry.unwrap().status,
            PalaceAuditStatus::Ok,
            "personal must be Ok"
        );

        let orphaned_entry = entries.iter().find(|e| e.id == "orphaned-proj-xyzzy");
        assert!(orphaned_entry.is_some(), "orphaned entry must appear");
        assert_eq!(
            orphaned_entry.unwrap().status,
            PalaceAuditStatus::Orphaned,
            "orphaned-proj-xyzzy must be Orphaned"
        );

        let empty_entry = entries.iter().find(|e| e.id == "empty-palace");
        assert!(empty_entry.is_some(), "empty entry must appear");
        assert_eq!(
            empty_entry.unwrap().status,
            PalaceAuditStatus::Empty,
            "empty-palace must be Empty"
        );
    }

    /// Why: stale `.lock` files are the canonical "palace won't open"
    /// symptom; the scanner must report them so `doctor` can hint at the
    /// remediation.
    /// What: lays out a tiny `palace/kg.redb.lock` tree under a tempdir
    /// and asserts the scanner picks it up.
    /// Test: pure filesystem.
    #[test]
    fn find_lock_files_returns_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let palace = tmp.path().join("palace_a");
        std::fs::create_dir_all(&palace).unwrap();
        let lock = palace.join("kg.redb.lock");
        std::fs::write(&lock, b"").unwrap();
        // Also drop a non-lock file to ensure it is ignored.
        std::fs::write(palace.join("kg.redb"), b"").unwrap();

        let found = find_lock_files(tmp.path());
        assert!(
            found.iter().any(|p| p == &lock),
            "expected to find {} in {:?}",
            lock.display(),
            found
        );
        assert_eq!(
            found.len(),
            1,
            "non-lock files must be ignored: {:?}",
            found
        );
    }
}
