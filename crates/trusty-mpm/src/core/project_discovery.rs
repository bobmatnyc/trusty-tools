//! Project discovery from the Claude Code `~/.claude/projects/` directory.
//!
//! Why: Claude Code records one subdirectory per project it has been launched
//! in, named with the project's absolute path encoded (each `/` becomes `-`).
//! trusty-mpm can mine that directory to offer the operator a ready-made list
//! of projects to register — no manual `project init` per repo.
//! What: [`ProjectDiscovery::discover`] enumerates `~/.claude/projects/`,
//! decodes each directory name back into a filesystem path, verifies that path
//! exists, counts the session transcripts (`.jsonl` files) inside, and reports
//! the most recent session time. Results sort newest-session-first.
//! Test: `cargo test -p trusty-mpm-core project_discovery` covers path decoding
//! (including the ambiguous-hyphen case) against a temp directory tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One project discovered under `~/.claude/projects/`.
///
/// Why: the discovery endpoint and the Telegram `/projects` command need a
/// structured row per project — its path, how recently it was used, and how
/// many recorded sessions it has.
/// What: the decoded absolute project path, the mtime of the most recent
/// `.jsonl` transcript (or `None` when the directory holds none), and the
/// transcript count.
/// Test: `discovers_and_counts_sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProject {
    /// Absolute path to the project's working directory.
    pub path: PathBuf,
    /// Modification time of the most recent `.jsonl` transcript, if any.
    pub last_session: Option<SystemTime>,
    /// Number of `.jsonl` session transcripts recorded for the project.
    pub session_count: usize,
}

/// Pure-ish discoverer of Claude Code projects.
///
/// Why: isolating the enumeration logic as a unit type keeps it testable —
/// [`ProjectDiscovery::discover_in`] takes an explicit base directory so tests
/// never touch the real `~/.claude/projects/`.
/// What: [`discover`](ProjectDiscovery::discover) scans the real directory;
/// [`discover_in`](ProjectDiscovery::discover_in) scans an arbitrary one.
/// Test: `discovers_and_counts_sessions`, `decodes_path_with_hyphens`.
pub struct ProjectDiscovery;

impl ProjectDiscovery {
    /// Discover projects under `~/.claude/projects/`.
    ///
    /// Why: production callers want the real Claude Code projects directory
    /// without resolving the home directory themselves.
    /// What: resolves `~/.claude/projects` and delegates to
    /// [`discover_in`](Self::discover_in); an unresolvable home yields an empty
    /// list rather than an error.
    /// Test: `discover_on_missing_dir_is_empty` covers the absent-directory path.
    pub fn discover() -> Vec<DiscoveredProject> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        Self::discover_in(&home.join(".claude").join("projects"))
    }

    /// Discover projects under an arbitrary Claude Code projects directory.
    ///
    /// Why: tests must exercise decoding and counting against a temp tree; a
    /// missing or unreadable directory must degrade to an empty list.
    /// What: reads each subdirectory entry, decodes its name into a path via
    /// [`decode_project_path`], keeps only paths that exist on disk, gathers the
    /// `.jsonl` transcript count and newest mtime, and returns the rows sorted
    /// newest-session-first.
    /// Test: `discovers_and_counts_sessions`, `discover_on_missing_dir_is_empty`.
    pub fn discover_in(projects_dir: &Path) -> Vec<DiscoveredProject> {
        let Ok(entries) = fs::read_dir(projects_dir) else {
            return Vec::new();
        };

        let mut projects: Vec<DiscoveredProject> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|entry| {
                let dir_name = entry.file_name();
                let dir_name = dir_name.to_str()?;
                let path = decode_project_path(dir_name)?;
                let (session_count, last_session) = scan_sessions(&entry.path());
                Some(DiscoveredProject {
                    path,
                    last_session,
                    session_count,
                })
            })
            .collect();

        // Newest session first; projects with no session sort last (None < Some
        // under Option's ordering, so reverse-compare on the option).
        projects.sort_by_key(|p| std::cmp::Reverse(p.last_session));
        projects
    }
}

/// Count `.jsonl` transcripts in a project directory and find the newest mtime.
///
/// Why: discovery reports both how many sessions a project has and how recently
/// it was used; computing them in one directory pass keeps it cheap.
/// What: walks the directory's immediate entries, counting files ending in
/// `.jsonl` and tracking the maximum modification time.
/// Test: covered by `discovers_and_counts_sessions`.
fn scan_sessions(dir: &Path) -> (usize, Option<SystemTime>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, None);
    };
    let mut count = 0;
    let mut newest: Option<SystemTime> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        count += 1;
        if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
            newest = Some(match newest {
                Some(current) if current >= mtime => current,
                _ => mtime,
            });
        }
    }
    (count, newest)
}

/// Decode a Claude Code projects directory name back into a filesystem path.
///
/// Why: Claude Code encodes a project's absolute path by replacing every `/`
/// with `-`. Because real directory names may *also* contain `-`, a blind
/// "replace all `-` with `/`" can produce a path that does not exist; the
/// decoder must verify the result and fall back to a smarter search.
/// What: the directory name begins with `-` (the leading `/`). First tries the
/// simple decoding (all `-` → `/`) and returns it if that path exists. Otherwise
/// walks the segments greedily: starting from `/`, it joins segments one at a
/// time, but when the next candidate directory does not exist it re-attaches the
/// segment to the previous one with a literal `-` — reconstructing hyphenated
/// directory names. If the greedy walk resolves nothing on disk (no prefix
/// exists), it falls back to the simple all-`-`-as-`/` decoding so a
/// never-launched project still decodes deterministically. Returns `None` when
/// the name does not start with `-`.
/// Test: `decodes_simple_path`, `decodes_path_with_hyphens`, `rejects_bad_name`,
/// `unresolved_path_still_decoded_best_effort`.
pub fn decode_project_path(dir_name: &str) -> Option<PathBuf> {
    // The encoding always starts with `-` representing the leading `/`.
    let body = dir_name.strip_prefix('-')?;
    if body.is_empty() {
        return None;
    }

    // Fast path: treat every `-` as `/`. If that exact path exists, done.
    let simple = PathBuf::from(format!("/{}", body.replace('-', "/")));
    if simple.exists() {
        return Some(simple);
    }

    // Greedy reconstruction: build the path segment by segment, re-joining a
    // segment to the previous one with a literal `-` when the `/`-separated
    // candidate does not exist on disk.
    let segments: Vec<&str> = body.split('-').collect();
    let mut path = PathBuf::from("/");
    let mut pending = String::new();
    let mut any_resolved = false;

    for (idx, segment) in segments.iter().enumerate() {
        let candidate_name = if pending.is_empty() {
            (*segment).to_string()
        } else {
            format!("{pending}-{segment}")
        };
        let candidate = path.join(&candidate_name);
        let is_last = idx + 1 == segments.len();

        if candidate.exists() {
            // This component resolved — commit it and reset the pending buffer.
            path = candidate;
            pending.clear();
            any_resolved = true;
        } else if is_last {
            // Last segment never resolved on disk; commit the best-effort name
            // so callers still get a usable (if unverified) path.
            path = candidate;
            pending.clear();
        } else {
            // Ambiguous: the `-` was part of a directory name. Carry the
            // accumulated name forward and try to attach the next segment.
            pending = candidate_name;
        }
    }

    // If no prefix at all resolved on disk, the greedy walk had nothing to go
    // on — fall back to the deterministic all-`-`-as-`/` decoding.
    if any_resolved {
        Some(path)
    } else {
        Some(simple)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn decodes_simple_path() {
        // A directory whose decoded path exists is decoded by the fast path.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("Projects").join("app")).unwrap();
        // Encode `<base>/Projects/app` — base may itself contain separators.
        let encoded = format!("{}", base.join("Projects").join("app").display()).replace('/', "-");
        let decoded = decode_project_path(&encoded).expect("decodes");
        assert_eq!(decoded, base.join("Projects").join("app"));
    }

    #[test]
    fn decodes_path_with_hyphens() {
        // A real directory name containing a hyphen must be reconstructed.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let project = base.join("Projects").join("trusty-mpm");
        std::fs::create_dir_all(&project).unwrap();
        let encoded = format!("{}", project.display()).replace('/', "-");
        let decoded = decode_project_path(&encoded).expect("decodes");
        assert_eq!(decoded, project);
    }

    #[test]
    fn rejects_bad_name() {
        // A name that does not start with `-` is not a valid encoding.
        assert!(decode_project_path("Users-masa").is_none());
        // An empty body after the leading `-` is rejected.
        assert!(decode_project_path("-").is_none());
    }

    #[test]
    fn unresolved_path_still_decoded_best_effort() {
        // A path that does not exist on disk falls back to the simple decoding.
        let decoded = decode_project_path("-nonexistent-deep-path").expect("decodes");
        assert_eq!(decoded, PathBuf::from("/nonexistent/deep/path"));
    }

    #[test]
    fn discover_on_missing_dir_is_empty() {
        let missing = PathBuf::from("/no/such/projects/dir");
        assert!(ProjectDiscovery::discover_in(&missing).is_empty());
    }

    #[test]
    fn discovers_and_counts_sessions() {
        // Build a fake `~/.claude/projects` tree with one project holding two
        // `.jsonl` transcripts and verify discovery counts them.
        let home = tempfile::tempdir().unwrap();
        let projects_dir = home.path().join(".claude").join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        // A real project directory the encoded name must point back at.
        let project = home.path().join("work").join("demo");
        std::fs::create_dir_all(&project).unwrap();
        let encoded = format!("{}", project.display()).replace('/', "-");

        let claude_dir = projects_dir.join(&encoded);
        std::fs::create_dir_all(&claude_dir).unwrap();
        for name in ["a.jsonl", "b.jsonl"] {
            let mut f = std::fs::File::create(claude_dir.join(name)).unwrap();
            writeln!(f, "{{}}").unwrap();
        }
        // A non-transcript file must not be counted.
        std::fs::File::create(claude_dir.join("notes.txt")).unwrap();

        let found = ProjectDiscovery::discover_in(&projects_dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, project);
        assert_eq!(found[0].session_count, 2);
        assert!(found[0].last_session.is_some());
    }

    #[test]
    fn discover_sorts_newest_session_first() {
        // Two projects: the one whose transcript was written later sorts first.
        let home = tempfile::tempdir().unwrap();
        let projects_dir = home.path().join(".claude").join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let make = |name: &str| -> PathBuf {
            let project = home.path().join(name);
            std::fs::create_dir_all(&project).unwrap();
            let encoded = format!("{}", project.display()).replace('/', "-");
            let claude_dir = projects_dir.join(encoded);
            std::fs::create_dir_all(&claude_dir).unwrap();
            std::fs::File::create(claude_dir.join("s.jsonl")).unwrap();
            project
        };

        let older = make("older");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = make("newer");

        let found = ProjectDiscovery::discover_in(&projects_dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, newer);
        assert_eq!(found[1].path, older);
    }
}
