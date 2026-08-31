//! Incremental catch-up system for the DOC-28 cutover bridge (#1762).
//!
//! Why: when a native `tm` session starts, the operator needs a summary of
//! activity since the last session — paused sessions, git commits, and recent
//! memory palace drawers — so they can resume with full context. This module
//! orchestrates the three sources and renders one markdown digest.
//! What: [`generate_catchup_context`](crate::catchup::generate_catchup_context) computes the digest (watermark-aware,
//! fail-open on every source); [`run_catchup`](crate::catchup::run_catchup) wraps it and optionally advances
//! the watermark. [`CatchupOptions`](crate::catchup::CatchupOptions) controls which sources are active.
//! Test: `generate_catchup_context_renders_all_sections`,
//! `run_catchup_no_advance_does_not_panic`, `run_catchup_advance_writes_under_the_state_root`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

pub mod git;
pub mod json;
pub mod mpm_registry;
pub mod mpm_session;
pub mod palace;
pub mod pause;
pub mod resolve;
pub mod session_finder;
pub mod session_log;
pub mod state;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

pub use json::{
    CatchupJson, PausedSessionJson, RecentMemoryJson, generate_catchup_json,
    generate_catchup_json_in,
};

use self::{
    git::git_commits_since,
    palace::fetch_recent_palace_drawers,
    session_finder::{filter_sessions_since, find_paused_sessions, render_resume_context},
    state::{CatchupState, load_catchup_state_in, save_catchup_state_in},
};

/// Options controlling one catch-up run.
///
/// Why: callers (CLI command, auto-inject, tests) need a single value to thread
/// the relevant parameters without a long argument list.
/// What: groups project path, memory URL, source toggles, and limits.
/// Test: `generate_catchup_context_renders_all_sections`.
#[derive(Clone)]
pub struct CatchupOptions {
    /// The project directory to scan (git repo root).
    pub project_dir: PathBuf,
    /// Socket the trusty-memory daemon serves on (#6286 — it was a base URL
    /// until the daemon retired its HTTP listener).
    pub memory_socket: PathBuf,
    /// Whether to include git commit history.
    pub include_git: bool,
    /// Whether to include palace drawer inspection.
    pub include_palace: bool,
    /// Maximum number of git commits to include.
    pub git_limit: usize,
    /// Maximum number of palace drawers to include.
    pub drawer_limit: usize,
    /// When true, ignore the watermark and return full history.
    pub full: bool,
}

impl Default for CatchupOptions {
    fn default() -> Self {
        Self {
            project_dir: PathBuf::from("."),
            // #6286: the derived socket path, else a guaranteed-unreachable one
            // that fails fast — catch-up degrades on an absent daemon rather
            // than aborting.
            memory_socket: crate::memory_rpc::resolve_memory_socket_or_unreachable(),
            include_git: true,
            include_palace: true,
            git_limit: 50,
            drawer_limit: 15,
            full: false,
        }
    }
}

/// Resolve the palace_id for a project directory.
///
/// Why: the catch-up state is keyed by palace_id, so it must be the SAME id the
/// memory daemon writes under. It used to answer the shared literal
/// `"unknown-project"` on failure, and [`run_catchup`] then wrote a watermark to
/// `~/.trusty-mpm/projects/unknown-project/catchup-state.json`. Every other
/// project landing on that literal read the same watermark, so a second project
/// reported "no new activity" for commits, sessions and drawers that were all
/// genuinely new (#5811). Returning the error lets both callers decline instead:
/// no watermark is read, and none is written.
/// What: delegates to [`crate::palace_resolve::resolve_palace`] — env override,
/// then the committed pin, then the git `owner/repo` slug, then the `parent/dir`
/// slug of the main worktree root.
/// Test: `derive_palace_id_for_agrees_with_daemon_path_no_remote_no_env`,
/// `derive_palace_id_for_env_override_unchanged`,
/// `derive_palace_id_for_git_remote_unchanged`,
/// `malformed_pin_does_not_advance_the_shared_watermark`.
fn derive_palace_id_for(
    project_dir: &Path,
) -> Result<String, crate::palace_resolve::PalaceResolveError> {
    // #5811: this probed the remote itself and called the PURE three-level
    // core, so it answered without the committed pin — a catch-up digest read
    // a different palace than the daemon wrote to whenever a project was
    // pinned. Both now route through the same four-level entry point.
    crate::palace_resolve::resolve_palace(project_dir).map(|resolution| resolution.id)
}

/// The whole digest when the project's palace cannot be resolved (#5811).
///
/// Why: the operator has to be told, because every section below the palace is
/// keyed by it. Rendering the usual three sections without a palace would report
/// "no new activity" from a watermark belonging to a different project.
/// What: one section naming the failure verbatim.
/// Test: `malformed_pin_renders_a_resolution_failure_section`.
fn palace_resolution_failed_section(e: &crate::palace_resolve::PalaceResolveError) -> String {
    format!(
        "## Catch-Up Unavailable\n\npalace resolution failed: {e}\n\n\
         Catch-up is keyed by palace, so no watermark was read and none was \
         advanced. Fix the pin (or set `TRUSTY_MEMORY_PALACE`) and re-run.\n\n"
    )
}

/// Probe the HEAD git SHA of a repo (for watermark advancement).
///
/// Why: storing the HEAD SHA lets future enhancements do SHA-bounded git ranges.
/// What: runs `git -C <repo> rev-parse HEAD`; returns None on any error.
/// Test: covered indirectly by `run_catchup_advance_writes_under_the_state_root`.
fn probe_head_sha(project_dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Render the whole Paused Sessions section body from a filter outcome.
///
/// Why: markdown is the surface that ADVANCES the watermark — `run_catchup`
/// calls `save_catchup_state` right after building this string — so a session
/// withheld here leaves every future window permanently. The receipt therefore
/// has to appear whether or not anything else survived: reporting it only in
/// the empty case hid it in exactly the run where the operator is least likely
/// to look, because a populated digest reads as complete. Split out as its own
/// function because an undatable session is unreachable through the filesystem
/// once both [`session_finder::PausedSession`] arms fall back to mtime — this
/// is the seam that keeps both the branch and its wiring to
/// [`session_finder::FilteredSessions::dropped_undatable`] testable.
/// What: the kept sessions rendered by [`render_resume_context`], or the plain
/// "nothing since last catch-up" notice when none survived; then, whenever
/// `dropped_undatable` is non-zero, a notice naming the count and pointing at
/// `full`.
/// Test: `sessions_section_reports_withheld_alongside_kept`,
/// `sessions_section_distinguishes_empty_from_withheld`.
fn render_sessions_section(filtered: &session_finder::FilteredSessions) -> String {
    let mut out = if filtered.kept.is_empty() {
        "No paused sessions since last catch-up.\n\n".to_string()
    } else {
        format!("{}\n", render_resume_context(&filtered.kept))
    };
    // #5072: unconditional — NOT an `else` on the empty case.
    if filtered.dropped_undatable > 0 {
        let n = filtered.dropped_undatable;
        let (verb, object) = if n == 1 {
            ("was", "it")
        } else {
            ("were", "them")
        };
        out.push_str(&format!(
            "{n} further paused session(s) could not be dated and {verb} withheld \
             — re-run catch-up with `full` to see {object}.\n\n"
        ));
    }
    out
}

/// Generate a markdown catch-up digest for the given options.
///
/// Why: a single entry point for assembling all three activity sources into one
/// operator-readable markdown block, respecting the watermark for incremental runs.
/// What: derives palace_id, loads watermark (None when full=true or absent),
/// collects paused sessions / git commits / palace drawers since watermark,
/// renders one markdown digest with three clearly labelled sections.
/// Test: `generate_catchup_context_renders_all_sections`,
/// `malformed_pin_renders_a_resolution_failure_section`.
pub async fn generate_catchup_context(opts: &CatchupOptions) -> String {
    generate_catchup_context_in(opts, None).await
}

/// [`generate_catchup_context`] with the framework state root supplied by the
/// caller.
///
/// Why (#4323): the watermark READ is keyed by palace id under
/// `~/.trusty-mpm/projects/`, so a test running against a temp project still
/// consulted the operator's real state dir — and a stale watermark there
/// silently changed what the digest reported. Taking the root explicitly is the
/// same seam [`run_catchup_in`] uses for the write.
/// What: identical to [`generate_catchup_context`] except that `state_root`
/// replaces the `.trusty-mpm` framework root; `None` is the production
/// home-relative default.
/// Test: `run_catchup_advance_writes_under_the_state_root`.
pub async fn generate_catchup_context_in(
    opts: &CatchupOptions,
    state_root: Option<&Path>,
) -> String {
    // #5811: no palace, no watermark — see `palace_resolution_failed_section`.
    let palace_id = match derive_palace_id_for(&opts.project_dir) {
        Ok(id) => id,
        Err(e) => return palace_resolution_failed_section(&e),
    };

    // Load watermark (None → full history; also None when opts.full).
    let watermark: Option<DateTime<Utc>> = if opts.full {
        None
    } else {
        load_catchup_state_in(&palace_id, state_root).map(|s| s.last_catchup_at)
    };

    let mut out = String::new();

    // ── Section 1: Paused Sessions ──────────────────────────────────────────
    out.push_str("## Paused Sessions\n\n");
    match find_paused_sessions(&opts.project_dir) {
        Ok(sessions) => {
            // #5072: shared fail-closed predicate — see `filter_sessions_since`.
            out.push_str(&render_sessions_section(&filter_sessions_since(
                sessions, watermark,
            )));
        }
        Err(e) => {
            eprintln!("catchup: warning: could not scan paused sessions: {e}");
            out.push_str("(session scan unavailable)\n\n");
        }
    }

    // ── Section 2: Recent Commits ───────────────────────────────────────────
    if opts.include_git {
        out.push_str("## Recent Commits\n\n");
        let commits = git_commits_since(&opts.project_dir, watermark);
        let commits: Vec<_> = commits.into_iter().take(opts.git_limit).collect();
        if commits.is_empty() {
            out.push_str("No new commits since last catch-up.\n\n");
        } else {
            for c in &commits {
                let ts_str =
                    c.ts.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_default();
                out.push_str(&format!(
                    "- `{}` {} — {} ({})\n",
                    &c.sha[..8.min(c.sha.len())],
                    c.msg,
                    c.author,
                    ts_str
                ));
            }
            out.push('\n');
        }
    }

    // ── Section 3: Recent Memory ────────────────────────────────────────────
    if opts.include_palace {
        out.push_str("## Recent Memory\n\n");
        // `None` means trusty-memory was unreachable; `Some(vec![])` means the
        // daemon answered but had nothing new. Issue #2030 (item 5): these two
        // outcomes must render distinct messages rather than the same
        // "No recent palace activity" line.
        match fetch_recent_palace_drawers(
            &opts.memory_socket,
            &palace_id,
            opts.drawer_limit,
            watermark,
        )
        .await
        {
            None => {
                out.push_str("trusty-memory unreachable — catch-up skipped for this section.\n\n");
            }
            Some(drawers) if drawers.is_empty() => {
                out.push_str("No recent palace activity since last catch-up.\n\n");
            }
            Some(drawers) => {
                for d in &drawers {
                    let tags = if d.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", d.tags.join(", "))
                    };
                    out.push_str(&format!("- {}{}\n", d.title, tags));
                }
                out.push('\n');
            }
        }
    }

    out
}

/// Generate the catch-up digest and optionally advance the watermark.
///
/// Why: the CLI `tm session catchup` command needs to produce the digest
/// without advancing the watermark (manual peek), while the auto-inject on
/// session start should advance it.
/// What: calls [`generate_catchup_context`] to produce the context string,
/// then — when `advance_watermark` is true — calls [`save_catchup_state_in`] with
/// the current timestamp and the HEAD git SHA. Returns the digest string.
/// Test: `run_catchup_no_advance_does_not_panic`, `run_catchup_advance_writes_under_the_state_root`.
pub async fn run_catchup(opts: &CatchupOptions, advance_watermark: bool) -> String {
    run_catchup_in(opts, advance_watermark, None).await
}

/// [`run_catchup`] with the framework state root supplied by the caller.
///
/// Why (#4323): `run_catchup(&opts, true)` is the ONE catch-up call that writes,
/// and it resolved `~/.trusty-mpm/projects/<palace-id>/` unconditionally. Every
/// test exercising the advancing path therefore created a directory in the
/// operator's real state dir named after its own tempdir — the `t-tmpXXXX`
/// entries that grew to 39,749 of 39,910. A temp `$HOME` did not help, because
/// this path resolves the home directory itself rather than taking a base.
/// What: identical to [`run_catchup`] except that `state_root` replaces the
/// `.trusty-mpm` framework root for both the watermark read and the write;
/// `None` is the production home-relative default.
/// Test: `run_catchup_advance_writes_under_the_state_root`,
/// `run_catchup_advance_leaves_the_home_state_dir_alone`.
pub async fn run_catchup_in(
    opts: &CatchupOptions,
    advance_watermark: bool,
    state_root: Option<&Path>,
) -> String {
    let context = generate_catchup_context_in(opts, state_root).await;
    if advance_watermark {
        // #5811: the watermark file is named by the palace id, so an
        // unresolvable palace must not write one — the shared placeholder made
        // one project's watermark suppress another project's activity.
        let palace_id = match derive_palace_id_for(&opts.project_dir) {
            Ok(id) => id,
            Err(e) => {
                eprintln!(
                    "catchup: warning: palace resolution failed ({e}); watermark not advanced"
                );
                return context;
            }
        };
        let sha = probe_head_sha(&opts.project_dir);
        let state = CatchupState {
            last_catchup_at: Utc::now(),
            palace_id: palace_id.clone(),
            last_git_sha: sha,
        };
        if let Err(e) = save_catchup_state_in(&palace_id, &state, state_root) {
            eprintln!("catchup: warning: could not save watermark state: {e}");
        }
    }
    context
}

/// Run catch-up in a blocking context by spawning a dedicated tokio runtime
/// on a separate thread.
///
/// Why: `prepare_session_inner` is synchronous but catch-up uses async HTTP;
/// calling `block_on` from inside an existing tokio runtime would deadlock.
/// Spawning a thread with its own `current_thread` runtime avoids that.
/// What: clones `opts`, spawns a thread, builds a `current_thread` tokio runtime,
/// runs [`run_catchup`] on it, and joins. On any error returns an empty string
/// (fail-open — catch-up must never abort session start).
/// Test: covered indirectly by the session-launch tests (session still starts
/// even when catch-up cannot complete).
pub fn run_catchup_blocking(opts: CatchupOptions, advance_watermark: bool) -> String {
    match std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match rt {
            Ok(rt) => rt.block_on(run_catchup(&opts, advance_watermark)),
            Err(e) => {
                eprintln!("catchup: could not build tokio runtime: {e}");
                String::new()
            }
        }
    })
    .join()
    {
        Ok(ctx) => ctx,
        Err(_) => {
            eprintln!("catchup: catch-up thread panicked; skipping");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_git_repo(tmp: &TempDir) {
        let p = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["init"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.email", "t@t.com"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.name", "T"])
            .output()
            .unwrap();
        fs::write(p.join("README.md"), b"test").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn generate_catchup_context_renders_all_sections() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            // Use a port nobody listens on → fail-open for palace section.
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: true,
            include_palace: true,
            git_limit: 50,
            drawer_limit: 15,
            full: true,
        };

        let context = generate_catchup_context(&opts).await;

        assert!(
            context.contains("## Paused Sessions"),
            "must have paused sessions section"
        );
        assert!(
            context.contains("## Recent Commits"),
            "must have commits section"
        );
        assert!(
            context.contains("## Recent Memory"),
            "must have memory section"
        );
        // Should contain the commit we made.
        assert!(context.contains("init"), "commit message should appear");
        // Palace section should gracefully note the daemon is unreachable
        // (memory_socket points at a path nothing serves) rather than
        // conflating it with a genuinely empty result (issue #2030, item 5).
        assert!(
            context.contains("trusty-memory unreachable"),
            "palace section should distinguish unreachable from empty"
        );
    }

    #[tokio::test]
    async fn run_catchup_no_advance_does_not_panic() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: true,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: true,
        };

        // Run without advancing — must not panic or error.
        let ctx = run_catchup(&opts, false).await;
        assert!(!ctx.is_empty(), "context should not be empty");
    }

    /// Why: #4323 — this test used to call `run_catchup(&opts, true)`, whose
    /// write resolved `~/.trusty-mpm/projects/<palace-id>/` from the real home
    /// directory. The palace id derives from the tempdir, so every run left one
    /// more `t-tmpXXXX` directory behind: 39,749 of the operator's 39,910.
    /// What: runs the advancing path against a temp state root and asserts the
    /// watermark landed THERE. The assertion is what makes the seam load-free
    /// to verify — the old test asserted only that the context was non-empty,
    /// which stayed true no matter where the file went.
    /// Test: itself.
    #[tokio::test]
    async fn run_catchup_advance_writes_under_the_state_root() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);
        let state_root = TempDir::new().unwrap();

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: true,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: true,
        };

        let ctx = run_catchup_in(&opts, true, Some(state_root.path())).await;
        assert!(
            !ctx.is_empty(),
            "context should not be empty when advancing"
        );

        let palace_id = derive_palace_id_for(tmp.path()).expect("a temp dir resolves to a palace");
        assert!(
            state_root
                .path()
                .join("projects")
                .join(&palace_id)
                .join("catchup-state.json")
                .is_file(),
            "the advanced watermark must land under the supplied state root"
        );
    }

    /// #4323: the companion assertion — advancing against a state root must
    /// leave the home-relative state dir untouched. Reads the home path
    /// directly rather than trusting the write above, so a regression that
    /// ignored `state_root` and wrote to BOTH would still fail here.
    #[tokio::test]
    async fn run_catchup_advance_leaves_the_home_state_dir_alone() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);
        let state_root = TempDir::new().unwrap();

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: false,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: true,
        };

        let palace_id = derive_palace_id_for(tmp.path()).expect("a temp dir resolves to a palace");
        let Some(home) = dirs::home_dir() else {
            return; // No home dir resolvable: nothing to assert about.
        };
        let home_dir = home.join(".trusty-mpm").join("projects").join(&palace_id);
        assert!(
            !home_dir.exists(),
            "precondition: a fresh tempdir's palace must not already have a \
             watermark dir at {}",
            home_dir.display()
        );

        let _ = run_catchup_in(&opts, true, Some(state_root.path())).await;

        assert!(
            !home_dir.exists(),
            "advancing against a state root must not touch {}",
            home_dir.display()
        );
    }

    #[test]
    fn run_catchup_blocking_succeeds() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: true,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: true,
        };

        let ctx = run_catchup_blocking(opts, false);
        assert!(
            !ctx.is_empty(),
            "blocking wrapper should return non-empty context"
        );
    }

    fn filtered(kept: usize, dropped: usize) -> session_finder::FilteredSessions {
        session_finder::FilteredSessions {
            kept: (0..kept)
                .map(|_| session_finder::PausedSession::ClaudeMpm {
                    session: crate::catchup::mpm_session::ClaudeMpmSession {
                        resume_instructions: Some("work".to_string()),
                        ..Default::default()
                    },
                })
                .collect(),
            dropped_undatable: dropped,
        }
    }

    /// Why: #5072 — the withheld case used to render the byte-identical
    /// "No paused sessions since last catch-up." line as the genuinely-empty
    /// case, so the operator got no signal that sessions existed and no reason
    /// to re-run with `full`. The watermark advances past them regardless.
    /// What: the two causes produce different text, and the withheld text names
    /// the count and the recovery.
    /// Test: itself.
    #[test]
    fn sessions_section_distinguishes_empty_from_withheld() {
        let empty = render_sessions_section(&filtered(0, 0));
        let withheld = render_sessions_section(&filtered(0, 3));
        assert_ne!(
            empty, withheld,
            "withheld sessions must not read as a genuinely empty digest"
        );
        assert!(!empty.contains("withheld"), "{empty}");
        assert!(withheld.contains('3'), "must name the count: {withheld}");
        assert!(
            withheld.contains("full"),
            "must name the recovery: {withheld}"
        );
        assert!(render_sessions_section(&filtered(0, 1)).contains("was withheld"));
    }

    /// Why: markdown is the surface that advances the watermark, so a withheld
    /// session leaves every future window permanently. Reporting the receipt
    /// only when NOTHING survived hid it in the one run where the operator is
    /// least likely to look — a populated digest reads as complete (#5072).
    /// What: with one session kept and two withheld, the section renders the
    /// kept session AND the withheld notice.
    /// Test: itself.
    #[test]
    fn sessions_section_reports_withheld_alongside_kept() {
        let out = render_sessions_section(&filtered(1, 2));
        assert!(
            out.contains("Paused Session Catch-Up"),
            "kept sessions still render: {out}"
        );
        assert!(
            out.contains("withheld") && out.contains('2'),
            "the receipt must appear even when other sessions survived: {out}"
        );
    }

    // -----------------------------------------------------------------------
    // derive_palace_id_for — agreement with the shared derivation core (#1772)
    // -----------------------------------------------------------------------

    /// Why: issue #1772 — before this fix, `derive_palace_id_for` (the
    /// catch-up path) re-implemented its own divergent 4th fallback (a raw,
    /// unslugified `file_name()` basename) whenever `derive_palace_id`
    /// returned `None`, which could disagree with the memory daemon's
    /// `cwd_palace_slug_at` (trusty-memory) — the only other caller that
    /// reaches the "no override, no git remote" branch. Both now call
    /// `crate::derive_palace_id` with identical inputs and no caller-local
    /// re-derivation, so for a project with no git remote and no
    /// `TRUSTY_MEMORY_PALACE` override, the catch-up-path value MUST equal
    /// the value `derive_palace_id` itself produces for the exact same
    /// inputs — the same call the daemon path makes at its own step 3.
    /// What: a plain (non-git) temp directory has no remote to probe, and the
    /// env override is left unset, so `derive_palace_id_for` falls straight
    /// through to `derive_palace_id(project_dir, None, None)`. Asserts the
    /// two calls agree byte-for-byte.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn derive_palace_id_for_agrees_with_daemon_path_no_remote_no_env() {
        // SAFETY: #[serial] serialises this against the other TRUSTY_MEMORY_PALACE
        // mutating test in this module; ensure a clean slate first.
        unsafe {
            std::env::remove_var(crate::PALACE_OVERRIDE_ENV);
        }
        let tmp = TempDir::new().unwrap();

        let catchup_value =
            derive_palace_id_for(tmp.path()).expect("a temp dir resolves to a palace");

        // The "daemon path": trusty-memory's `cwd_palace_slug_at` step 3 calls
        // `crate::derive_palace_id(project_root, git_remote, None)` directly.
        // A plain temp dir has no git remote, mirroring that exact call.
        let daemon_value = crate::derive_palace_id(tmp.path(), None, None)
            .expect("a real temp dir always has a usable parent/dir slug");

        assert_eq!(
            catchup_value, daemon_value,
            "catch-up and daemon derivations must agree with no remote and no env override"
        );
    }

    /// Why: regression guard — the `TRUSTY_MEMORY_PALACE` override must still
    /// win unconditionally (precedence level 1), unaffected by removing the
    /// divergent fallback.
    /// What: sets the env override, asserts `derive_palace_id_for` returns the
    /// slugified override value regardless of the (non-git) directory.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn derive_palace_id_for_env_override_unchanged() {
        let tmp = TempDir::new().unwrap();
        // SAFETY: #[serial] serialises env access against the other
        // TRUSTY_MEMORY_PALACE-mutating test in this module.
        unsafe {
            std::env::set_var(crate::PALACE_OVERRIDE_ENV, "My Override");
        }
        let got = derive_palace_id_for(tmp.path()).expect("a temp dir resolves to a palace");
        unsafe {
            std::env::remove_var(crate::PALACE_OVERRIDE_ENV);
        }
        assert_eq!(got, "my-override");
    }

    /// Why: regression guard — a valid git remote must still win over the
    /// parent/dir fallback (precedence level 2), unaffected by removing the
    /// divergent fallback. Uses the real `bobmatnyc/trusty-tools` shape so the
    /// expected slug matches this repo's own palace id.
    /// What: inits a temp git repo, adds an `origin` remote, asserts
    /// `derive_palace_id_for` returns the owner-repo slug.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn derive_palace_id_for_git_remote_unchanged() {
        // SAFETY: #[serial] serialises env access against the other
        // TRUSTY_MEMORY_PALACE-mutating tests in this module.
        unsafe {
            std::env::remove_var(crate::PALACE_OVERRIDE_ENV);
        }
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ])
            .output()
            .unwrap();

        let got = derive_palace_id_for(tmp.path()).expect("a temp dir resolves to a palace");
        assert_eq!(got, "bobmatnyc-trusty-tools");
    }

    // -----------------------------------------------------------------------
    // Unresolvable palace — the shared-placeholder watermark (#5811)
    // -----------------------------------------------------------------------

    /// A project root whose committed pin exists but does not parse.
    ///
    /// Why: only a real file on disk reaches the pin-trust failures.
    /// `.trusty-tools/` is itself a project marker, so the returned directory IS
    /// the root `find_project_root` stops at.
    /// What: a `TempDir` holding a `.trusty-tools/trusty-memory.yaml` that is not
    /// pin YAML. Keep the handle alive for the test's duration.
    fn project_root_with_malformed_pin() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".trusty-tools");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("trusty-memory.yaml"),
            "palace: [unclosed\n\t bad: :",
        )
        .unwrap();
        tmp
    }

    fn opts_for(dir: &Path) -> CatchupOptions {
        CatchupOptions {
            project_dir: dir.to_path_buf(),
            memory_socket: PathBuf::from("/nonexistent/catchup-test.sock"),
            include_git: true,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: false,
        }
    }

    /// The digest says the palace could not be resolved, instead of reporting
    /// another project's activity window as this project's (#5811).
    ///
    /// Why: every section is keyed by the palace id, and the old code answered
    /// the shared literal `"unknown-project"` — so a second project with a broken
    /// pin read the FIRST one's watermark and rendered "no new activity" over
    /// commits, sessions and drawers that were all genuinely new.
    /// Test: itself.
    #[tokio::test]
    async fn malformed_pin_renders_a_resolution_failure_section() {
        let tmp = project_root_with_malformed_pin();

        let digest = generate_catchup_context(&opts_for(tmp.path())).await;

        assert!(
            digest.contains("palace resolution failed:"),
            "digest must name the resolution failure, got: {digest}"
        );
        assert!(
            !digest.contains("No new commits since last catch-up."),
            "digest must not report a watermark-filtered result with no watermark, got: {digest}"
        );
    }

    /// Advancing the watermark writes nothing when the palace is unresolvable
    /// (#5811).
    ///
    /// Why: `save_catchup_state` names the file by palace id, so the shared
    /// placeholder wrote every unresolvable project's watermark to the single
    /// `~/.trusty-mpm/projects/unknown-project/catchup-state.json`. The next
    /// project to land there read it and went silent about real activity.
    /// What: snapshots that exact path before and after `run_catchup(.., true)`
    /// and asserts it is untouched — correct whether or not a previous run of the
    /// old code left one behind. No other test in this crate uses that id.
    /// Test: itself.
    #[tokio::test]
    async fn malformed_pin_does_not_advance_the_shared_watermark() {
        let placeholder = dirs::home_dir()
            .expect("home dir")
            .join(".trusty-mpm/projects/unknown-project/catchup-state.json");
        let before = fs::read(&placeholder).ok();

        let tmp = project_root_with_malformed_pin();
        let _ = run_catchup(&opts_for(tmp.path()), true).await;

        assert_eq!(
            fs::read(&placeholder).ok(),
            before,
            "an unresolvable palace must not write the shared placeholder watermark at {}",
            placeholder.display()
        );
    }
}
