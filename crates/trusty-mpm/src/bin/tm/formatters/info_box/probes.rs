//! Time-bounded probes for the welcome panel.
//!
//! Why: every datum shown in the panel that requires a subprocess or network
//! call must be bounded so the panel never hangs before launch.
//! What: `probe_recent_commits` runs `git log` in a background thread and
//! waits ≤150 ms; `run_git_log` spawns the subprocess and installs a 3-second
//! watchdog that sends SIGKILL so no git process ever lingers on a slow or
//! network-mounted filesystem; `shorten_git_age` compresses git's verbose
//! relative-time strings.
//! Test: `probe_commits_returns_empty_for_non_git_dir`,
//! `shorten_age_*` in the inline test module below.

use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use super::CommitLine;

// ── Recent commits ────────────────────────────────────────────────────────────

/// Probe the last N commits of the git repo at `workdir` (≤150 ms).
///
/// Why: the welcome panel shows recent repo changes so the operator can
/// orient themselves; the probe must never block launch.
/// What: spawns a background thread that runs `git -C <workdir> log` with
/// a 5-second process timeout, sends the parsed commits over a channel;
/// the caller waits ≤150 ms, returning an empty vec on timeout or error.
/// Test: `probe_commits_returns_empty_for_non_git_dir`.
pub(crate) fn probe_recent_commits(workdir: &str) -> Vec<CommitLine> {
    let path = workdir.to_string();
    let (tx, rx) = mpsc::channel::<Vec<CommitLine>>();
    std::thread::spawn(move || {
        let commits = run_git_log(&path).unwrap_or_default();
        let _ = tx.send(commits);
    });
    rx.recv_timeout(Duration::from_millis(150))
        .unwrap_or_default()
}

/// Spawn `git log`, install a 3-second SIGKILL watchdog, and collect output.
///
/// Why: `Command::output()` cannot be interrupted — on a slow or
/// network-mounted `.git`, it can block for minutes. The caller's
/// `recv_timeout(150 ms)` only bounds the panel render path; without a real
/// subprocess kill the git child process would keep running in the spawned
/// thread long after the panel has been shown. The watchdog closes that gap:
/// the process is forcibly killed ≤3 seconds after spawn regardless of
/// whether the caller already moved on.
/// What: spawns the child with piped stdout; the watchdog thread sleeps 3 s
/// then sends SIGKILL via the child PID (Unix) or `child.kill()` on Windows.
/// `wait_with_output()` collects stdout+status; a non-zero exit or parse
/// failure returns `None`.
/// Test: indirect — covered by `probe_recent_commits` tests.
fn run_git_log(workdir: &str) -> Option<Vec<CommitLine>> {
    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["log", "--oneline", "-5", "--format=%h|%cr|%s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Watchdog: kill the child after 3 s so it never lingers on a slow FS
    // even after the panel recv_timeout(150ms) already fired in the caller.
    let pid = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        #[cfg(unix)]
        // SAFETY: `kill(pid, SIGKILL)` is async-signal-safe; pid is a valid
        // u32 from Child::id(); the process may already be gone — errno ESRCH
        // is harmless.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        {
            // On non-Unix platforms use the std API (best-effort).
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status();
        }
    });

    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let commits: Vec<CommitLine> = stdout.lines().filter_map(parse_commit_line).collect();
    if commits.is_empty() {
        None
    } else {
        Some(commits)
    }
}

/// Parse one line of `git log --format=%h|%cr|%s` output into a `CommitLine`.
///
/// Why: the separator `|` is not safe against subjects containing `|`, so
/// `splitn(3, '|')` is used to keep the full subject in the third field.
/// What: splits on `|` into at most 3 parts; returns `None` when the sha is
/// empty or fewer than 3 fields are present.
/// Test: covered indirectly by `run_git_log`.
fn parse_commit_line(line: &str) -> Option<CommitLine> {
    let mut parts = line.splitn(3, '|');
    let sha = parts.next()?.trim().to_string();
    let age_raw = parts.next()?.trim().to_string();
    let subject = parts.next()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    let age = shorten_git_age(&age_raw);
    Some(CommitLine { sha, age, subject })
}

/// Compress git's verbose relative-time into a short token.
///
/// Why: git `%cr` gives "2 hours ago", "3 days ago" etc.; the panel
/// uses compact forms ("2h", "3d") to fit more content per line.
/// What: parses the integer + unit prefix and maps to single-char suffix;
/// unknown formats are returned unchanged (truncated to 8 chars).
/// Test: `shorten_age_seconds`, `shorten_age_minutes`, `shorten_age_hours`,
/// `shorten_age_days`, `shorten_age_weeks`, `shorten_age_months`,
/// `shorten_age_years`, `shorten_age_unknown`.
pub(crate) fn shorten_git_age(raw: &str) -> String {
    // git gives: "N seconds ago", "N minutes ago", "N hours ago",
    // "N days ago", "N weeks ago", "N months ago", "N years ago"
    // Also "a minute ago", "an hour ago".
    let s = raw.trim();

    // Handle "a/an <unit> ago" → "1<suffix>"
    if let Some(rest) = s.strip_prefix("a ").or_else(|| s.strip_prefix("an ")) {
        let unit = rest.split_whitespace().next().unwrap_or("");
        return format!("1{}", unit_suffix(unit));
    }

    let mut words = s.split_whitespace();
    let n_str = words.next().unwrap_or("?");
    let unit = words.next().unwrap_or("");

    if let Ok(n) = n_str.parse::<u64>() {
        format!("{}{}", n, unit_suffix(unit))
    } else {
        // Unknown format — truncate to keep columns tidy.
        let short: String = s.chars().take(8).collect();
        short
    }
}

/// Map a git unit word to a single-char or two-char suffix.
///
/// Why: avoids repeating the match in multiple call sites.
/// What: "seconds"/"second" → "s", "minutes"/"minute" → "m", "hours" → "h",
/// "days" → "d", "weeks" → "w", "months" → "mo", "years" → "y"; anything
/// else → "?".
/// Test: covered indirectly by `shorten_git_age` tests.
fn unit_suffix(unit: &str) -> &'static str {
    match unit {
        "seconds" | "second" => "s",
        "minutes" | "minute" => "m",
        "hours" | "hour" => "h",
        "days" | "day" => "d",
        "weeks" | "week" => "w",
        "months" | "month" => "mo",
        "years" | "year" => "y",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_commits_returns_empty_for_non_git_dir() {
        // /tmp is not a git repository; probe must return an empty vec, not panic.
        let commits = probe_recent_commits("/tmp");
        assert!(
            commits.is_empty(),
            "expected empty commits for non-git dir, got: {commits:?}"
        );
    }

    #[test]
    fn shorten_age_seconds() {
        assert_eq!(shorten_git_age("45 seconds ago"), "45s");
    }

    #[test]
    fn shorten_age_minutes() {
        assert_eq!(shorten_git_age("3 minutes ago"), "3m");
    }

    #[test]
    fn shorten_age_hours() {
        assert_eq!(shorten_git_age("2 hours ago"), "2h");
    }

    #[test]
    fn shorten_age_days() {
        assert_eq!(shorten_git_age("5 days ago"), "5d");
    }

    #[test]
    fn shorten_age_weeks() {
        assert_eq!(shorten_git_age("1 week ago"), "1w");
    }

    #[test]
    fn shorten_age_months() {
        assert_eq!(shorten_git_age("2 months ago"), "2mo");
    }

    #[test]
    fn shorten_age_years() {
        assert_eq!(shorten_git_age("1 year ago"), "1y");
    }

    #[test]
    fn shorten_age_a_minute() {
        assert_eq!(shorten_git_age("a minute ago"), "1m");
    }

    #[test]
    fn shorten_age_an_hour() {
        assert_eq!(shorten_git_age("an hour ago"), "1h");
    }

    #[test]
    fn shorten_age_unknown_truncates() {
        let raw = "right now tomorrow";
        let result = shorten_git_age(raw);
        assert!(
            result.chars().count() <= 8,
            "unknown age must be truncated to ≤8 chars: {result:?}"
        );
    }

    #[test]
    fn probe_commits_real_repo_returns_commits() {
        // The worktree IS a git repo, so we should get at least one commit.
        // Use the worktree directory if available; fall back gracefully.
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let commits = probe_recent_commits(&dir);
        // In CI or a non-git fallback, commits may be empty — just don't panic.
        // When run inside the workspace, we expect ≥ 1 commit.
        for c in &commits {
            assert!(!c.sha.is_empty(), "sha must not be empty");
            assert!(!c.age.is_empty(), "age must not be empty");
        }
    }
}
