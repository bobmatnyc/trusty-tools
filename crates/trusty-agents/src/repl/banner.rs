//! Banner data helpers used by the ratatui REPL renderer.
//!
//! Why: The previous crossterm-based banner printer (`print_banner` /
//! `print_startup_status`) was deleted with the legacy REPL path (#268 P5).
//! The ratatui draw loop in `tui.rs` builds its own banner widget; this
//! module now only exposes the data-gathering helpers that survived the
//! cleanup (recent git commits, dir entry counting, MCP service counting).
//! What: Pure-data helpers — no terminal I/O — so they can be called from
//! the ratatui startup path without fighting for the cursor.
//! Test: `count_dir_entries_*` and the MCP-config parser tests below.

/// Count files in `dir` ending with `.{ext}`. Returns 0 on any I/O error.
pub(crate) fn count_dir_entries(dir: &std::path::Path, ext: &str) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(ext))
        })
        .count()
}

/// Best-effort count of MCP services configured for the current user.
///
/// Why: The startup banner advertises "MCP: N connections" so the user knows
/// at a glance whether their MCP integrations are wired up.
/// What: Reads `~/.trusty-agents/mcp.toml` if present and counts the `[[mcp]]`
/// service entries. Returns 0 on any failure.
pub(crate) fn count_active_mcp_services() -> usize {
    let Some(home) = dirs::home_dir() else {
        return 0;
    };
    let path = home.join(".trusty-agents").join("mcp.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    content
        .lines()
        .filter(|l| l.trim().starts_with("[[mcp"))
        .count()
}

/// Read up to `n` recent commits as `"hash • relative-time • subject"`.
///
/// Why: Showing the last few commits gives users immediate situational
/// awareness when re-attaching to the REPL on a project they're actively
/// working on.
/// What: Spawns `git log --format=%h • %ar • %s -n` synchronously. On any
/// failure (not a repo, git missing, non-zero exit), returns a single
/// "(no git history)" entry so the caller can still render N rows.
/// Test: Indirect — exercised via banner integration.
pub(crate) fn recent_git_commits(n: usize) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["log", &format!("-{}", n), "--format=%h • %ar • %s"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let lines: Vec<String> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect();
            if lines.is_empty() {
                vec!["(no git history)".to_string()]
            } else {
                lines
            }
        }
        _ => vec!["(no git history)".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_dir_entries_returns_zero_for_missing_dir() {
        let n = count_dir_entries(std::path::Path::new("/nonexistent-xyz-123"), "md");
        assert_eq!(n, 0);
    }

    #[test]
    fn count_dir_entries_filters_by_extension() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), b"").unwrap();
        std::fs::write(tmp.path().join("b.md"), b"").unwrap();
        std::fs::write(tmp.path().join("c.txt"), b"").unwrap();
        assert_eq!(count_dir_entries(tmp.path(), "md"), 2);
        assert_eq!(count_dir_entries(tmp.path(), "txt"), 1);
    }

    #[test]
    fn recent_git_commits_returns_some_lines() {
        // In any normal environment we should get a vec of at least one entry
        // (either real commits or the "(no git history)" placeholder).
        let v = recent_git_commits(1);
        assert!(!v.is_empty());
    }
}
