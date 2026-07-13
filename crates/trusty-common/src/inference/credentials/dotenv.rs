//! The one shared `.env.local` loader (issue #2401).
//!
//! Why: `trusty-agents` (`runtime/startup.rs:114-119`) and `trusty-search`
//! (`main.rs:1155`) each call `dotenvy` ad hoc today, with slightly
//! different search strategies (cwd-only vs. cwd + detected project dir).
//! This module is the canonical replacement — designed so those two crates
//! can adopt it in their own migration tickets without re-deriving the
//! search logic — but this ticket does **not** modify either call site; it
//! only ships the shared loader.
//! What: [`load_env_local_once`] is the production entry point: idempotent
//! (a `OnceLock` — repeated calls across a process are free), searches from
//! the current working directory upward for a `.env.local` file (mirroring
//! Cargo/git's own upward-search convention for "workspace root" discovery),
//! and calls `dotenvy::from_path` on the first hit. Because `dotenvy` never
//! overrides an already-set environment variable, this naturally implements
//! "process env beats `.env.local`" — the resolver's tier check
//! (`std::env::var`) is what actually observes the precedence; this loader
//! just populates the process environment once, then gets out of the way.
//! [`find_workspace_env_local`] and [`load_env_from_path`] are the hermetic
//! cores used by the resolver's precedence tests, which must not depend on
//! `OnceLock`'s once-only semantics or the real repo's `.env.local`.
//! Test: `dotenv_tests` (sibling file).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Search `start` and each ancestor directory for a `.env.local` file.
///
/// Why: the hermetic core for [`load_env_local_once`]; also directly usable
/// by tests that want to point the search at a temp directory tree instead
/// of the real cwd.
/// What: returns the first `.env.local` found walking upward from `start`,
/// or `None` if no ancestor has one.
/// Test: `dotenv_tests::finds_env_local_in_ancestor`,
/// `dotenv_tests::absent_env_local_returns_none`.
pub fn find_workspace_env_local(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(d) = dir {
        let candidate = d.join(".env.local");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

/// Load a specific `.env.local` path into the process environment.
///
/// Why: the resolver's precedence tests need to simulate "the `.env.local`
/// tier fired" without touching [`load_env_local_once`]'s process-wide
/// `OnceLock` (which would make a second test's load a silent no-op) or the
/// real cwd search.
/// What: thin wrapper over `dotenvy::from_path`; returns `true` on success.
/// Like `dotenvy` generally, this never overrides a variable already present
/// in the process environment.
/// Test: `dotenv_tests::load_env_from_path_sets_new_var`,
/// `dotenv_tests::load_env_from_path_does_not_override_existing`.
pub fn load_env_from_path(path: &Path) -> bool {
    dotenvy::from_path(path).is_ok()
}

/// Idempotent, cwd-upward-search `.env.local` loader for production use.
///
/// Why: the resolver calls this once before checking `std::env::var` so a
/// developer's `.env.local` (at the repo root, or any ancestor of cwd) is
/// picked up regardless of which subdirectory a binary was launched from —
/// the same problem `trusty-agents` fixed ad hoc for itself in #250.
/// What: on first call, searches upward from the current working directory
/// via [`find_workspace_env_local`] and loads the first hit (if any) via
/// `dotenvy::from_path`. Subsequent calls are a no-op `OnceLock` check.
/// Errors (missing file, malformed file) are swallowed — a missing
/// `.env.local` is the common case (production/CI), not a failure.
/// Test: exercised indirectly via `resolver::resolve_key`'s production path;
/// the once-only + cwd-search behaviour itself is not independently unit
/// tested because `OnceLock` fires exactly once per test binary — see
/// module docs for why precedence tests use [`load_env_from_path`] instead.
pub fn load_env_local_once() {
    static LOADED: OnceLock<()> = OnceLock::new();
    LOADED.get_or_init(|| {
        if let Ok(cwd) = std::env::current_dir()
            && let Some(path) = find_workspace_env_local(&cwd)
        {
            let _ = dotenvy::from_path(&path);
        }
    });
}

/// Read a single variable's value from a specific `.env.local` file **without**
/// mutating the process environment.
///
/// Why: the `config keys list` verb reports which tier (process env >
/// `.env.local` > secure store) currently supplies each provider's key. To
/// distinguish "set in the shell env" from "loaded from `.env.local`" honestly,
/// the lister must inspect the file directly rather than call
/// [`load_env_local_once`] (which would fold the file's values into the process
/// env and erase the distinction). This hermetic, path-injectable reader is that
/// inspection primitive.
/// What: parses `path` with `dotenvy`'s non-mutating iterator, skipping malformed
/// entries, and returns the first non-empty value bound to `var`, or `None` when
/// the file is unreadable, has no such key, or binds it to an empty value.
/// Test: `dotenv_tests::read_var_from_env_local_finds_value`,
/// `dotenv_tests::read_var_from_env_local_absent_is_none`.
pub fn read_var_from_env_local(path: &Path, var: &str) -> Option<String> {
    let iter = dotenvy::from_path_iter(path).ok()?;
    iter.flatten()
        .find(|(k, _)| k == var)
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty())
}

/// Value of `var` in the nearest ancestor `.env.local`, without mutating the
/// process environment.
///
/// Why: the production `.env.local` tier check for `config keys list` — it must
/// use the same cwd-upward search [`load_env_local_once`] uses so the reported
/// tier matches what resolution would actually pick, but observe the file
/// read-only.
/// What: searches upward from the current working directory via
/// [`find_workspace_env_local`] and delegates to [`read_var_from_env_local`];
/// `None` when there is no `.env.local` or it does not bind `var`.
/// Test: covered structurally via `read_var_from_env_local` (the cwd-search wrap
/// is not independently unit tested — it depends on the real cwd, exactly like
/// [`load_env_local_once`]).
pub fn env_local_value(var: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let path = find_workspace_env_local(&cwd)?;
    read_var_from_env_local(&path, var)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: the upward search must find a `.env.local` in a parent of the
    /// starting directory, not just the exact directory.
    /// Test: itself.
    #[test]
    fn finds_env_local_in_ancestor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let child = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(tmp.path().join(".env.local"), "X=1\n").unwrap();
        let found = find_workspace_env_local(&child).unwrap();
        assert_eq!(found, tmp.path().join(".env.local"));
    }

    /// Why: absent `.env.local` anywhere in the ancestor chain must return
    /// `None`, not error.
    /// Test: itself.
    #[test]
    fn absent_env_local_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(find_workspace_env_local(tmp.path()), None);
    }

    /// Why: the `config keys list` tier check must read a var out of a
    /// `.env.local` file WITHOUT touching the process environment.
    /// Test: itself.
    #[test]
    fn read_var_from_env_local_finds_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".env.local");
        std::fs::write(&path, "OPENAI_API_KEY=from-dotenv\nEMPTY=\n").unwrap();
        assert_eq!(
            read_var_from_env_local(&path, "OPENAI_API_KEY"),
            Some("from-dotenv".to_string())
        );
        // The read is non-mutating: the process env is untouched.
        assert!(std::env::var("OPENAI_API_KEY").is_err());
    }

    /// Why: an absent key (or one bound to an empty value) must read back as
    /// `None`, not as a spurious empty-string "configured" signal.
    /// Test: itself.
    #[test]
    fn read_var_from_env_local_absent_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".env.local");
        std::fs::write(&path, "EMPTY=\n").unwrap();
        assert_eq!(read_var_from_env_local(&path, "EMPTY"), None);
        assert_eq!(read_var_from_env_local(&path, "MISSING"), None);
    }

    /// Why: `load_env_from_path` must set a var not already in the process
    /// environment.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn load_env_from_path_sets_new_var() {
        let var = "TRUSTY_TEST_DOTENV_NEW_VAR";
        // SAFETY: `#[serial(dotenv_credential_env)]` guarantees no other
        // test in this crate mutates process env concurrently with this one.
        unsafe {
            std::env::remove_var(var);
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, format!("{var}=from-dotenv\n")).unwrap();
        assert!(load_env_from_path(&env_path));
        assert_eq!(std::env::var(var).unwrap(), "from-dotenv");
        unsafe {
            std::env::remove_var(var);
        }
    }

    /// Why: `dotenvy` must never override a variable already set in the
    /// process environment — this is the mechanism that gives "process env
    /// beats `.env.local`" precedence for free.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn load_env_from_path_does_not_override_existing() {
        let var = "TRUSTY_TEST_DOTENV_EXISTING_VAR";
        // SAFETY: see `load_env_from_path_sets_new_var`.
        unsafe {
            std::env::set_var(var, "already-set");
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let env_path = tmp.path().join(".env.local");
        std::fs::write(&env_path, format!("{var}=from-dotenv\n")).unwrap();
        assert!(load_env_from_path(&env_path));
        assert_eq!(std::env::var(var).unwrap(), "already-set");
        unsafe {
            std::env::remove_var(var);
        }
    }
}
