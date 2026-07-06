//! Project-identity + branch-label segment for `tm statusline`.
//!
//! Why: split out of `mod.rs` (#2031 follow-up) once the file approached the
//! 500-SLOC production cap — this module owns everything needed to resolve
//! the `<project> ⎇ <branch>` half of the statusline: the GitHub `owner/repo`
//! label, the tmux-session-name-over-git-branch selection, and the managed-
//! session branch elision.
//! What: exposes [`project_segment`] as the single entry point; all other
//! items are private helpers plus their bounded-thread probes.
//! Test: see the `tests` module below.

/// Build the project-identity + branch-label segment from `cwd`.
///
/// Why (#1913-followup): a managed tm session runs in `.worktrees/<uuid>/`, so the
/// cwd basename is just the session UUID — useless as a project label and a
/// duplicate of the `session/<uuid>` branch. The leading field should instead be
/// the GitHub `owner/repo` slug (the codebase's canonical `source_id`), and the
/// redundant `session/<…>` branch is dropped so the operator can actually tell
/// which project and branch they are on.
/// What: derives `owner/repo` from the git remote (reusing
/// [`trusty_common::github_path::derive_github_path`] — the same parser
/// `derive_source_id_for_record` uses; handles https and scp-style remotes),
/// falling back to the cwd basename only when there is no parseable GitHub remote.
/// Appends ` ⎇ <branch>` for meaningful branches, but omits it for the auto-created
/// `session/<…>` managed-session branch and for detached `HEAD`.
/// Test: `project_segment_basename_fallback_non_git`, `git_owner_repo_reads_remote`,
/// `render_project_segment_*`, `render_statusline_minimal_input` (empty cwd → None).
pub(crate) fn project_segment(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    // Prefer the GitHub owner/repo slug from the git remote; fall back to the cwd
    // basename only when there is no parseable remote (non-GitHub / non-git dir).
    let project = git_owner_repo(cwd).unwrap_or_else(|| {
        std::path::Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let branch = select_branch_label(tmux_session_name(), || git_branch(cwd));
    render_project_segment(project, branch.as_deref())
}

/// Choose the branch-position label: prefer the tmux session name, falling
/// back to the git branch when no session name is available.
///
/// Why (#2031): a managed tm session runs in `.worktrees/<uuid>/` on a bare
/// session-UUID branch (not the elided `session/<uuid>` form the old check
/// covered), so [`git_branch`] alone leaked the UUID into the status bar. The
/// tmux session name (e.g. `tm-trusty-tools-01`) is the human-meaningful label
/// tm actually assigns to the session, so it takes priority whenever present.
/// Keeping the git-branch source lazy (`FnOnce`) means the fallback probe's
/// subprocess call is skipped entirely on the common managed-session path
/// where a tmux session name is already available.
/// What: returns `tmux_session` when `Some` and non-empty; otherwise invokes
/// `fallback` and returns its result. Pure aside from the lazy call.
/// Test: `select_branch_label_selection_contract`.
fn select_branch_label(
    tmux_session: Option<String>,
    fallback: impl FnOnce() -> Option<String>,
) -> Option<String> {
    tmux_session.filter(|s| !s.is_empty()).or_else(fallback)
}

/// Assemble the project segment string from an already-resolved label and branch.
///
/// Why: keeping the label/branch combination pure (no git I/O) makes the
/// branch-omission logic unit-testable without spawning subprocesses.
/// What: returns `None` for an empty label; otherwise `"<project> ⎇ <branch>"`
/// (single space, per the #2011 layout) for a meaningful branch, or just
/// `"<project>"` when the branch is empty, detached `HEAD`, or a managed
/// `session/<…>` branch (see [`is_managed_session_branch`]).
/// Test: `render_project_segment_omits_managed_session_branch`,
/// `render_project_segment_keeps_real_branch`, `render_project_segment_empty_label`.
fn render_project_segment(project: String, branch: Option<&str>) -> Option<String> {
    if project.is_empty() {
        return None;
    }
    Some(match branch {
        Some(b) if !b.is_empty() && b != "HEAD" && !is_managed_session_branch(b) => {
            format!("{project} \u{2387} {b}")
        }
        _ => project,
    })
}

/// Report whether `branch` is an auto-created managed tm session branch.
///
/// Why: tm provisions each managed session on a `session/<uuid>` branch; showing
/// it in the status bar just repeats the session id and crowds out the real
/// project identity, so it is elided.
/// What: matches the literal `session` branch and any `session/<…>` prefix.
/// Test: `is_managed_session_branch_matches_session_prefix`.
fn is_managed_session_branch(branch: &str) -> bool {
    branch == "session" || branch.starts_with("session/")
}

/// Run `git rev-parse --abbrev-ref HEAD` in `cwd` with a hard 100 ms wall-clock
/// timeout and return the branch name.
///
/// Why: `statusline` is on Claude Code's hot render path; a stuck credential
/// helper, network filesystem, or git hook would otherwise block every render
/// cycle. The bounded thread + `recv_timeout` pattern matches
/// [`super::compaction::compaction_segment`].
/// What: spawns a detached thread that calls `git rev-parse`, sends the result
/// over an mpsc channel; the caller waits ≤100 ms, returns `None` on timeout.
/// Test: `render_statusline_full_payload` (in `mod.rs`) exercises the detected
/// branch path; covered here via [`select_branch_label`]'s fallback tests.
fn git_branch(cwd: &str) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let cwd = cwd.to_string();
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let result = (|| -> Option<String> {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if branch.is_empty() {
                None
            } else {
                Some(branch)
            }
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

/// Run `tmux display-message -p '#S'` with a hard 100 ms wall-clock timeout to
/// resolve the current tmux session's name.
///
/// Why (#2031): managed tm sessions run in `.worktrees/<uuid>/` and Claude Code
/// inherits the tmux session's `$TMUX` env var from the shell that launched it;
/// probing the CURRENT session (no `-t` target) recovers tm's actual
/// human-meaningful session label (e.g. `tm-trusty-tools-01`) without any extra
/// plumbing. The bounded-thread pattern matches [`git_branch`].
/// What: returns `None` immediately when `$TMUX` is unset (not inside tmux);
/// otherwise spawns a detached thread that runs `tmux display-message -p '#S'`,
/// trims the output, and returns `None` on missing binary, non-zero exit, empty
/// output, or a >100 ms stall.
/// Test: environment-dependent (requires a live tmux session), so covered
/// indirectly via the pure [`select_branch_label`] selection tests and
/// `project_segment_basename_fallback_non_git`, which asserts against this
/// probe's own live result rather than assuming an environment.
fn tmux_session_name() -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    std::env::var_os("TMUX")?;

    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let result = (|| -> Option<String> {
            let out = std::process::Command::new("tmux")
                .args(["display-message", "-p", "#S"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

/// Derive the GitHub `owner/repo` slug for `cwd` with a 100 ms wall-clock timeout.
///
/// Why: the leading status-bar field must identify the project, not the worktree
/// UUID; the canonical identity is the repo's `owner/repo` (the `source_id`). The
/// bounded-thread pattern (matching [`git_branch`]) keeps a stuck git config /
/// filesystem from blocking Claude Code's hot render path.
/// What: spawns a detached thread that calls the shared parser
/// [`trusty_common::github_path::derive_github_path`] (which shells to
/// `git -C <cwd> config --get remote.origin.url` — no network — and parses both
/// https and scp-style remotes), formats `owner/repo`, and sends it over an mpsc
/// channel; the caller waits ≤100 ms and returns `None` on timeout, no remote, or
/// an unparseable / non-GitHub URL.
/// Test: `git_owner_repo_reads_remote` (temp repo with a github origin);
/// `git_owner_repo_none_outside_repo` (bare temp dir → None).
fn git_owner_repo(cwd: &str) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let cwd = cwd.to_string();
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let result = trusty_common::github_path::derive_github_path(std::path::Path::new(&cwd))
            .map(|gh| format!("{}/{}", gh.owner, gh.repo));
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a managed tm session runs on a `session/<uuid>` branch that just
    /// repeats the session id; it must be elided while real branches stay.
    /// Test: itself.
    #[test]
    fn is_managed_session_branch_matches_session_prefix() {
        assert!(is_managed_session_branch("session/2dfb23d1-579f-4168-9d28"));
        assert!(is_managed_session_branch("session/anything"));
        assert!(is_managed_session_branch("session"));
        assert!(!is_managed_session_branch("main"));
        assert!(!is_managed_session_branch("fix/foo"));
        // Not the managed prefix (note the trailing `s`).
        assert!(!is_managed_session_branch("sessions/foo"));
    }

    /// Why: a managed session must render `owner/repo` with NO branch (and hence
    /// no duplicated UUID) even though a `session/<uuid>` branch is checked out.
    /// Test: itself.
    #[test]
    fn render_project_segment_omits_managed_session_branch() {
        let seg = render_project_segment(
            "bobmatnyc/trusty-tools".to_string(),
            Some("session/2dfb23d1-579f-4168-9d28"),
        );
        assert_eq!(seg.as_deref(), Some("bobmatnyc/trusty-tools"));
        // No branch marker and no UUID leaked in.
        let s = seg.unwrap();
        assert!(!s.contains('\u{2387}'), "managed branch must be omitted");
        assert!(!s.contains("2dfb23d1"), "session UUID must not appear");
    }

    /// Why: a normal checkout on a meaningful branch must keep `⎇ <branch>`.
    /// Test: itself.
    #[test]
    fn render_project_segment_keeps_real_branch() {
        let seg = render_project_segment("bobmatnyc/trusty-tools".to_string(), Some("main"));
        assert_eq!(seg.as_deref(), Some("bobmatnyc/trusty-tools \u{2387} main"));

        // Detached HEAD and empty branch collapse to just the label.
        assert_eq!(
            render_project_segment("owner/repo".to_string(), Some("HEAD")).as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            render_project_segment("owner/repo".to_string(), None).as_deref(),
            Some("owner/repo")
        );
    }

    /// Why: an empty label (basename resolution failed) must omit the segment
    /// entirely rather than emit a stray `⎇`.
    /// Test: itself.
    #[test]
    fn render_project_segment_empty_label() {
        assert_eq!(render_project_segment(String::new(), Some("main")), None);
    }

    /// Why (#2031): pins the full selection contract in one place — tmux wins
    /// when present and non-empty; the git-branch fallback is used only when
    /// it's absent or empty; both absent yields no branch. The tmux-present
    /// case passes a panicking fallback to prove it is never invoked.
    /// Test: itself.
    #[test]
    fn select_branch_label_selection_contract() {
        let panics = || -> Option<String> {
            panic!("fallback must not run when tmux session name is present")
        };
        assert_eq!(
            select_branch_label(Some("tm-trusty-tools-01".to_string()), panics).as_deref(),
            Some("tm-trusty-tools-01")
        );
        assert_eq!(
            select_branch_label(None, || Some("main".to_string())).as_deref(),
            Some("main")
        );
        assert_eq!(
            select_branch_label(Some(String::new()), || Some("main".to_string())).as_deref(),
            Some("main")
        );
        assert_eq!(select_branch_label(None, || None), None);
    }

    /// Why: a bare directory with no git repo must fall back to the cwd basename
    /// and never panic. (#2031: the branch position now prefers a live tmux
    /// session name over the git branch, so this test computes its expectation
    /// from the SAME [`tmux_session_name`] probe the production code uses,
    /// rather than assuming "no tmux" — it stays deterministic whether or not
    /// the test runner itself is inside a tmux session.)
    /// Test: itself.
    #[test]
    fn project_segment_basename_fallback_non_git() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        let seg = project_segment(&cwd).expect("basename segment");
        let basename = tmp
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match tmux_session_name() {
            Some(session) => assert_eq!(
                seg,
                format!("{basename} \u{2387} {session}"),
                "inside tmux, non-git cwd → basename + tmux session label"
            ),
            None => {
                assert_eq!(seg, basename, "non-git cwd → basename, no branch");
                assert!(!seg.contains('\u{2387}'), "no branch marker for a bare dir");
            }
        }
    }

    /// Why: the leading field must resolve to the GitHub `owner/repo` slug parsed
    /// from the git remote (both scp-style and https), matching `source_id`.
    /// Test: itself (temp git repo; skipped when git is unavailable on the runner).
    #[test]
    fn git_owner_repo_reads_remote() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return; // no git on runner
        }
        assert!(git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:bobmatnyc/trusty-tools.git",
        ]));
        let cwd = dir.to_string_lossy().to_string();
        assert_eq!(
            git_owner_repo(&cwd).as_deref(),
            Some("bobmatnyc/trusty-tools"),
            "scp-style remote → owner/repo slug"
        );
    }

    /// Why: outside any git repo `git_owner_repo` must yield `None` so callers
    /// fall back to the basename.
    /// Test: itself.
    #[test]
    fn git_owner_repo_none_outside_repo() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(git_owner_repo(&cwd), None);
    }
}
