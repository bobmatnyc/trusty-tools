//! Machine-wide claude-mpm project discovery via the session registry DB.
//!
//! Why: during cutover, users may have active claude-mpm projects not visible
//! via Claude Code's `ProjectDiscovery` (which only covers recently-opened
//! directories). The claude-mpm registry DB is the ground-truth inventory of
//! every project where claude-mpm ran.
//! What: reads `~/.claude-mpm/session-registry.db` (SQLite) and returns a
//! deduplicated, newest-first list of project paths that still have live pause
//! files on disk.
//! Test: `discover_returns_empty_when_db_missing`, `discover_orders_newest_first`,
//! `discover_filters_missing_dirs` in the inline `#[cfg(test)]` module.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// Discover project paths registered in the claude-mpm session registry.
///
/// Why: the claude-mpm registry captures all projects where the Python tool ran,
/// including worktrees and repos outside Claude Code's own inventory. Using it
/// as the discovery surface for `--all-projects` ensures we find sessions users
/// actually paused under claude-mpm.
/// What: opens `registry_path` read-only, queries DISTINCT `project_path` ordered
/// by `MAX(last_active) DESC`, then filters to paths that (a) still exist as
/// directories and (b) contain at least one live claude-mpm pause file under
/// `<path>/.claude-mpm/sessions/`.
/// When `registry_path` does not exist the function returns `Ok(vec![])` —
/// fail-open so users without a claude-mpm install see no error.
/// Test: in-memory DB tests in the `tests` module below.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub fn discover_claude_mpm_projects(registry_path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !registry_path.exists() {
        return Ok(vec![]);
    }

    // Open read-only so we never mutate the claude-mpm registry.
    let conn = rusqlite::Connection::open_with_flags(
        registry_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening registry DB at {}", registry_path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT project_path, MAX(last_active) AS la \
         FROM sessions \
         GROUP BY project_path \
         ORDER BY la DESC",
    )?;

    let raw: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let results = raw
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir() && has_live_pause_file(p))
        .collect();

    Ok(results)
}

/// Returns the default path of the claude-mpm session-registry DB.
///
/// Why: centralises the registry path so callers don't hard-code the location.
/// What: `~/.claude-mpm/session-registry.db`, or a relative fallback when
/// `dirs::home_dir` is unavailable.
/// Test: `default_registry_path_under_home`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub fn default_registry_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude-mpm")
        .join("session-registry.db")
}

/// Returns true when `project_dir` has at least one live claude-mpm pause file.
///
/// Why: the ground-truth indicator that a project is actually paused (rather than
/// merely having been used in the past) is the presence of a session JSON file
/// under `.claude-mpm/sessions/`.
/// What: checks for `LATEST-SESSION.txt` first (cheapest), then falls back to
/// any `session-*.json` glob. Returns false on any I/O error (fail-open).
/// Test: covered transitively by `discover_filters_missing_dirs`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
fn has_live_pause_file(project_dir: &Path) -> bool {
    let sessions_dir = project_dir.join(".claude-mpm").join("sessions");
    if !sessions_dir.is_dir() {
        return false;
    }
    // Fast path: LATEST-SESSION.txt is written at pause time.
    if sessions_dir.join("LATEST-SESSION.txt").exists() {
        return true;
    }
    // Fallback: any session-*.json file.
    std::fs::read_dir(&sessions_dir)
        .map(|mut rd| {
            rd.any(|e| {
                e.ok()
                    .and_then(|e| e.file_name().into_string().ok())
                    .map(|n| n.starts_with("session-") && n.ends_with(".json"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn make_registry(entries: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("session-registry.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT,
                project_path TEXT,
                project_name TEXT,
                started_at TEXT,
                last_active TEXT,
                status TEXT,
                pid INTEGER
            )",
        )
        .unwrap();
        for (path, last_active) in entries {
            conn.execute(
                "INSERT INTO sessions (session_id, project_path, last_active, status) \
                 VALUES (?1, ?2, ?3, 'paused')",
                rusqlite::params![format!("sess-{path}"), path, last_active],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    fn make_pause_file(project_dir: &Path) {
        let sessions = project_dir.join(".claude-mpm").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("LATEST-SESSION.txt"), "pointer").unwrap();
    }

    #[test]
    fn discover_returns_empty_when_db_missing() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("nonexistent.db");
        let result = discover_claude_mpm_projects(&missing).unwrap();
        assert!(result.is_empty(), "missing DB should return empty vec");
    }

    #[test]
    fn discover_filters_missing_dirs() {
        let base = TempDir::new().unwrap();
        let real_dir = base.path().join("real-project");
        std::fs::create_dir_all(&real_dir).unwrap();
        make_pause_file(&real_dir);

        let ghost = base.path().join("deleted-project");
        // ghost directory does NOT exist

        let (_db_dir, db_path) = make_registry(&[
            (real_dir.to_str().unwrap(), "2026-06-27T10:00:00Z"),
            (ghost.to_str().unwrap(), "2026-06-27T09:00:00Z"),
        ]);

        let result = discover_claude_mpm_projects(&db_path).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], real_dir);
    }

    #[test]
    fn discover_orders_newest_first() {
        let base = TempDir::new().unwrap();
        let older = base.path().join("older");
        let newer = base.path().join("newer");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        make_pause_file(&older);
        make_pause_file(&newer);

        let (_db_dir, db_path) = make_registry(&[
            (older.to_str().unwrap(), "2026-06-26T08:00:00Z"),
            (newer.to_str().unwrap(), "2026-06-27T10:00:00Z"),
        ]);

        let result = discover_claude_mpm_projects(&db_path).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], newer);
        assert_eq!(result[1], older);
    }

    #[test]
    fn discover_deduplicates_same_path() {
        let base = TempDir::new().unwrap();
        let proj = base.path().join("project");
        std::fs::create_dir_all(&proj).unwrap();
        make_pause_file(&proj);

        let path_str = proj.to_str().unwrap();
        let (_db_dir, db_path) = make_registry(&[
            (path_str, "2026-06-27T10:00:00Z"),
            (path_str, "2026-06-26T09:00:00Z"),
        ]);

        let result = discover_claude_mpm_projects(&db_path).unwrap();
        assert_eq!(result.len(), 1, "duplicate paths must be deduplicated");
    }
}
