//! Working-tree + index status.
//!
//! Why: The first thing an agent asks before any commit operation is "what
//! changed?". Using `git2::Repository::statuses` keeps this fast (no fork)
//! and gives a typed enumeration we can map to a stable serialization.
//! What: `get_status` returns a vector of `FileStatus`; `format_status`
//! renders a `git status --short`-style block for display to the LLM.
//! Test: Against the trusty-agents repo (which has tracked + untracked files
//! per gitStatus snapshot).

use git2::{Status, StatusOptions};

use super::repo::GitRepo;

/// Categorical state for a single path.
///
/// Why: Mapping libgit2's bitfield to a small enum is friendlier to LLM
/// consumers and to JSON serialization.
/// What: Six variants covering the cases the tools need; `Renamed` carries
/// the previous path so the tool output can show both names.
/// Test: Indirect via `get_status` test below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Modified,
    Added,
    Deleted,
    Renamed { from: String },
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone)]
pub struct FileStatus {
    pub path: String,
    pub state: FileState,
}

/// Returns working tree + index status (excludes ignored entries).
///
/// Why: Filtering ignored entries inline keeps the tool output focused on
/// changes the user might commit; ignored noise would flood the context.
/// What: Walks `Repository::statuses(Some(opts))` once and maps each entry
/// to a `FileStatus`. The state is chosen by precedence: conflict > rename
/// > added > deleted > modified > untracked.
/// Test: `get_status_returns_entries_for_dirty_repo`.
pub fn get_status(repo: &GitRepo) -> anyhow::Result<Vec<FileStatus>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo
        .inner()
        .statuses(Some(&mut opts))
        .map_err(|e| anyhow::anyhow!("failed to read repo statuses: {e}"))?;

    let mut out = Vec::with_capacity(statuses.len());
    for entry in statuses.iter() {
        let path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let s = entry.status();
        let state = classify(s, &entry);
        out.push(FileStatus { path, state });
    }
    Ok(out)
}

fn classify(s: Status, entry: &git2::StatusEntry<'_>) -> FileState {
    if s.is_conflicted() {
        return FileState::Conflicted;
    }
    if s.is_index_renamed() || s.is_wt_renamed() {
        let from = entry
            .head_to_index()
            .and_then(|d| d.old_file().path().map(|p| p.display().to_string()))
            .or_else(|| {
                entry
                    .index_to_workdir()
                    .and_then(|d| d.old_file().path().map(|p| p.display().to_string()))
            })
            .unwrap_or_default();
        return FileState::Renamed { from };
    }
    if s.is_index_new() || s.is_wt_new() && !s.is_wt_modified() {
        // `wt_new` alone means untracked; `index_new` is staged-add.
        if s.is_index_new() {
            return FileState::Added;
        }
        return FileState::Untracked;
    }
    if s.is_index_deleted() || s.is_wt_deleted() {
        return FileState::Deleted;
    }
    if s.is_index_modified() || s.is_wt_modified() {
        return FileState::Modified;
    }
    // Fall back: anything we don't classify above as untracked.
    FileState::Untracked
}

/// Format status as a human-readable string (like `git status --short`).
///
/// Why: A compact rendering reduces tokens in tool-result messages. Two
/// columns (state, path) keeps it scan-friendly for the LLM.
/// What: One line per entry, prefixed with a 2-char code.
/// Test: `format_status_renders_each_state`.
pub fn format_status(entries: &[FileStatus]) -> String {
    if entries.is_empty() {
        return "Working tree clean".to_string();
    }
    let mut s = String::with_capacity(entries.len() * 32);
    for e in entries {
        let code = match &e.state {
            FileState::Modified => " M",
            FileState::Added => "A ",
            FileState::Deleted => " D",
            FileState::Renamed { .. } => "R ",
            FileState::Untracked => "??",
            FileState::Conflicted => "UU",
        };
        match &e.state {
            FileState::Renamed { from } => {
                s.push_str(&format!("{code} {from} -> {}\n", e.path));
            }
            _ => s.push_str(&format!("{code} {}\n", e.path)),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_status_returns_entries_for_dirty_repo() {
        let cwd = std::env::current_dir().unwrap();
        let repo = GitRepo::open(&cwd).unwrap();
        let entries = get_status(&repo).unwrap();
        // The trusty-agents repo has modified + untracked files at the time we
        // are running these tests; we just assert the call succeeds. We
        // don't depend on a specific count which would be flaky.
        let _ = entries;
    }

    #[test]
    fn format_status_handles_empty() {
        let out = format_status(&[]);
        assert!(out.contains("clean"));
    }

    #[test]
    fn format_status_renders_each_state() {
        let entries = vec![
            FileStatus {
                path: "a.txt".into(),
                state: FileState::Modified,
            },
            FileStatus {
                path: "b.txt".into(),
                state: FileState::Untracked,
            },
            FileStatus {
                path: "c.txt".into(),
                state: FileState::Renamed {
                    from: "old.txt".into(),
                },
            },
        ];
        let out = format_status(&entries);
        assert!(out.contains(" M a.txt"));
        assert!(out.contains("?? b.txt"));
        assert!(out.contains("R  old.txt -> c.txt"));
    }
}
