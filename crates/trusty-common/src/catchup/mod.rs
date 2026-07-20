//! Incremental catch-up system for the DOC-28 cutover bridge (#1762).
//!
//! Why: when a native `tm` session starts, the operator needs a summary of
//! activity since the last session — paused sessions, git commits, and recent
//! memory palace drawers — so they can resume with full context. This module
//! orchestrates the three sources and renders one markdown digest.
//! What: [`generate_catchup_context`] computes the digest (watermark-aware,
//! fail-open on every source); [`run_catchup`] wraps it and optionally advances
//! the watermark. [`CatchupOptions`] controls which sources are active.
//! Test: `generate_catchup_context_renders_all_sections`,
//! `run_catchup_no_advance_does_not_panic`, `run_catchup_advance_writes_state`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

pub mod git;
pub mod mpm_registry;
pub mod mpm_session;
pub mod palace;
pub mod session_finder;
pub mod session_log;
pub mod state;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use self::{
    git::git_commits_since,
    palace::fetch_recent_palace_drawers,
    session_finder::{find_paused_sessions, render_resume_context},
    state::{CatchupState, load_catchup_state, save_catchup_state},
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
    /// Base URL of the trusty-memory daemon.
    pub memory_url: String,
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
            // Discovery-first (issue #2030): resolves TRUSTY_MEMORY_URL when
            // set, else the daemon's actual discovered bound address, else a
            // guaranteed-unreachable placeholder that fails fast rather than
            // guessing a fixed (and likely wrong) port.
            memory_url: crate::mcp::memory_rpc::resolve_memory_base_url_or_unreachable(),
            include_git: true,
            include_palace: true,
            git_limit: 50,
            drawer_limit: 15,
            full: false,
        }
    }
}

/// Derive the palace_id for a project directory (fail-open).
///
/// Why: the catch-up state is keyed by palace_id; deriving it from the project
/// directory keeps it consistent with the session-launch palace pinning AND
/// with the memory daemon's own `cwd_palace_slug_at`
/// (`trusty-memory::messaging::operations`), which resolves the identical
/// three-level precedence through the same `crate::derive_palace_id` core.
/// Previously this function re-implemented its own 4th fallback — the RAW,
/// unslugified `file_name()` basename — whenever `derive_palace_id` returned
/// `None` (e.g. a leaf directory name that slugifies to empty). That
/// caller-local fallback could both (a) diverge from the daemon, which has no
/// such fallback and errors instead, and (b) leak a storage-unsafe,
/// unslugified token as a palace id (see issue #1772). `derive_palace_id` is
/// now the single source of truth for every precedence level, so this
/// function no longer re-derives anything from `project_dir` on failure — it
/// falls back to a fixed literal, never a directory-derived value — meaning
/// it can never disagree with the daemon on a real project.
/// What: probes git remote origin from the project dir, then calls
/// `crate::derive_palace_id`. Falls back to the fixed literal
/// `"unknown-project"` only when derivation yields `None` for every
/// precedence level.
/// Test: `derive_palace_id_for_agrees_with_daemon_path_no_remote_no_env`,
/// `derive_palace_id_for_env_override_unchanged`,
/// `derive_palace_id_for_git_remote_unchanged`; also covered indirectly by
/// `generate_catchup_context_renders_all_sections`.
fn derive_palace_id_for(project_dir: &Path) -> String {
    let remote = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let override_val = crate::palace_override_from_env();
    crate::derive_palace_id(project_dir, remote.as_deref(), override_val.as_deref())
        .unwrap_or_else(|| "unknown-project".to_string())
}

/// Probe the HEAD git SHA of a repo (for watermark advancement).
///
/// Why: storing the HEAD SHA lets future enhancements do SHA-bounded git ranges.
/// What: runs `git -C <repo> rev-parse HEAD`; returns None on any error.
/// Test: covered indirectly by `run_catchup_advance_writes_state`.
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

/// Generate a markdown catch-up digest for the given options.
///
/// Why: a single entry point for assembling all three activity sources into one
/// operator-readable markdown block, respecting the watermark for incremental runs.
/// What: derives palace_id, loads watermark (None when full=true or absent),
/// collects paused sessions / git commits / palace drawers since watermark,
/// renders one markdown digest with three clearly labelled sections.
/// Test: `generate_catchup_context_renders_all_sections`.
pub async fn generate_catchup_context(opts: &CatchupOptions) -> String {
    let palace_id = derive_palace_id_for(&opts.project_dir);

    // Load watermark (None → full history; also None when opts.full).
    let watermark: Option<DateTime<Utc>> = if opts.full {
        None
    } else {
        load_catchup_state(&palace_id).map(|s| s.last_catchup_at)
    };

    let mut out = String::new();

    // ── Section 1: Paused Sessions ──────────────────────────────────────────
    out.push_str("## Paused Sessions\n\n");
    match find_paused_sessions(&opts.project_dir) {
        Ok(sessions) => {
            // Filter by watermark when available.
            let sessions: Vec<_> = if let Some(wm) = watermark {
                sessions
                    .into_iter()
                    .filter(|s| s.sort_key().is_none_or(|ts| ts > wm))
                    .collect()
            } else {
                sessions
            };
            if sessions.is_empty() {
                out.push_str("No paused sessions since last catch-up.\n\n");
            } else {
                out.push_str(&render_resume_context(&sessions));
                out.push('\n');
            }
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
            &opts.memory_url,
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
/// then — when `advance_watermark` is true — calls [`save_catchup_state`] with
/// the current timestamp and the HEAD git SHA. Returns the digest string.
/// Test: `run_catchup_no_advance_does_not_panic`, `run_catchup_advance_writes_state`.
pub async fn run_catchup(opts: &CatchupOptions, advance_watermark: bool) -> String {
    let context = generate_catchup_context(opts).await;
    if advance_watermark {
        let palace_id = derive_palace_id_for(&opts.project_dir);
        let sha = probe_head_sha(&opts.project_dir);
        let state = CatchupState {
            last_catchup_at: Utc::now(),
            palace_id: palace_id.clone(),
            last_git_sha: sha,
        };
        if let Err(e) = save_catchup_state(&palace_id, &state) {
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
            memory_url: "http://127.0.0.1:19999".to_string(),
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
        // (memory_url points at a port nobody listens on) rather than
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
            memory_url: "http://127.0.0.1:19999".to_string(),
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

    #[tokio::test]
    async fn run_catchup_advance_writes_state() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_url: "http://127.0.0.1:19999".to_string(),
            include_git: true,
            include_palace: false,
            git_limit: 10,
            drawer_limit: 5,
            full: true,
        };

        // Advance=true triggers save_catchup_state (writes to real ~/.trusty-mpm).
        // Just verify no panic and context is non-empty.
        let ctx = run_catchup(&opts, true).await;
        assert!(
            !ctx.is_empty(),
            "context should not be empty when advancing"
        );
    }

    #[test]
    fn run_catchup_blocking_succeeds() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_url: "http://127.0.0.1:19999".to_string(),
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

        let catchup_value = derive_palace_id_for(tmp.path());

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
        let got = derive_palace_id_for(tmp.path());
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

        let got = derive_palace_id_for(tmp.path());
        assert_eq!(got, "bobmatnyc-trusty-tools");
    }
}
