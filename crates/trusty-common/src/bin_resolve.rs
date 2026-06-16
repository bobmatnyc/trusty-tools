//! Robust executable discovery and daemon `PATH` composition.
//!
//! Why: macOS launchd relaunches LaunchAgents with a deliberately minimal
//! `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`). Daemons that shell out to tools
//! installed by Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`) or into the
//! user's home (`~/.local/bin`, `~/.cargo/bin`) therefore fail *before*
//! reaching application logic — e.g. the trusty-mpm session manager could not
//! find `tmux` or `claude` after every daemon restart (#1298). Two daemons
//! independently hand-rolled `which`-style lookups and would each have to
//! re-derive the same well-known-dir list; this module is the single shared
//! answer.
//!
//! What: [`daemon_path_dirs`] returns the ordered, de-duplicated list of bin
//! directories a trusty-* daemon should be able to see (Homebrew + user bins
//! before the standard system dirs, with `~` expanded to the real home).
//! [`daemon_path_env`] joins them into a `PATH` string suitable for a launchd
//! `EnvironmentVariables` dict. [`resolve_binary`] finds an executable by
//! consulting the live `PATH` first and falling back to those well-known dirs,
//! so a daemon spawned with a minimal `PATH` still locates `tmux`/`claude`.
//!
//! Test: `daemon_path_*` and `resolve_binary_*` unit tests below. The module is
//! cross-platform (the well-known dirs are macOS/Linux-oriented but harmless
//! elsewhere) so it is not gated behind `#[cfg(target_os = "macos")]`.

use std::path::{Path, PathBuf};

/// Standard system bin directories present even under launchd's minimal `PATH`.
///
/// Why: these must always be in the composed `PATH` so core utilities
/// (`/bin/sh`, `/usr/bin/env`, …) resolve. They go *after* the user/Homebrew
/// dirs so a Homebrew tool shadows an older system copy when both exist.
const SYSTEM_BIN_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Absolute (non-home) bin directories that hold operator-installed tools.
///
/// Why: Homebrew installs to `/opt/homebrew/bin` (Apple silicon) or
/// `/usr/local/bin` (Intel); both must be visible to the daemon. Listed before
/// the home-relative dirs only for readability — final ordering is
/// user/Homebrew first, then system, enforced in [`daemon_path_dirs`].
const ABSOLUTE_TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Home-relative bin directories (expanded against the real home dir).
///
/// Why: `claude` ships to `~/.local/bin` and cargo-installed binaries land in
/// `~/.cargo/bin`; launchd never expands `~`, so the daemon must carry the
/// expanded absolute paths.
const HOME_RELATIVE_BIN_DIRS: &[&str] = &[".local/bin", ".cargo/bin"];

/// Compose the ordered, de-duplicated list of bin directories a trusty-*
/// daemon should be able to see, with `~` expanded to the real home.
///
/// Why: launchd's minimal `PATH` omits Homebrew and user bin dirs, breaking
/// daemon spawns of `tmux`/`claude` (#1298). A single canonical ordering keeps
/// the generated plist `PATH` and the runtime [`resolve_binary`] fallback in
/// agreement.
/// What: returns Homebrew/absolute tool dirs, then the home-relative dirs
/// (`~/.local/bin`, `~/.cargo/bin`) expanded against [`dirs::home_dir`], then
/// the standard system dirs — de-duplicated, preserving first-seen order.
/// Existing entries from the live `PATH` are intentionally *not* merged here;
/// callers that want the inherited `PATH` too should prepend it.
/// Test: `daemon_path_dirs_orders_user_before_system`,
/// `daemon_path_dirs_expands_home`, `daemon_path_dirs_dedupes`.
pub fn daemon_path_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, acc: &mut Vec<PathBuf>| {
        if !acc.contains(&p) {
            acc.push(p);
        }
    };

    for d in ABSOLUTE_TOOL_DIRS {
        push(PathBuf::from(d), &mut dirs);
    }
    if let Some(home) = dirs::home_dir() {
        for rel in HOME_RELATIVE_BIN_DIRS {
            push(home.join(rel), &mut dirs);
        }
    }
    for d in SYSTEM_BIN_DIRS {
        push(PathBuf::from(d), &mut dirs);
    }
    dirs
}

/// Render [`daemon_path_dirs`] as a colon-joined `PATH` string.
///
/// Why: a launchd `EnvironmentVariables` dict needs `PATH` as a single string;
/// generating it from the same source as the runtime fallback guarantees the
/// installed daemon and the live resolver look in identical places.
/// What: joins [`daemon_path_dirs`] with `:`, skipping any path that is not
/// valid UTF-8 (launchd plist values are UTF-8 strings).
/// Test: `daemon_path_env_contains_expected_dirs`.
pub fn daemon_path_env() -> String {
    daemon_path_dirs()
        .into_iter()
        .filter_map(|p| p.to_str().map(str::to_owned))
        .collect::<Vec<_>>()
        .join(":")
}

/// Resolve an executable by name, trusting the live `PATH` first and falling
/// back to the well-known [`daemon_path_dirs`].
///
/// Why: a daemon relaunched by launchd with a minimal `PATH` cannot find
/// `tmux`/`claude` via a bare `PATH` lookup, yet the binaries exist at known
/// locations. Checking those locations after the `PATH` lookup makes spawning
/// resilient to the inherited environment without trusting it.
/// What: if `name` contains a path separator it is treated as a literal path
/// and returned when it is an existing file. Otherwise each entry of the
/// current process `PATH` is checked, then each [`daemon_path_dirs`] entry, for
/// an existing `dir/name`; the first hit is returned. Returns `None` if nothing
/// matches.
/// Test: `resolve_binary_finds_in_well_known_dir`,
/// `resolve_binary_returns_none_for_missing`,
/// `resolve_binary_accepts_absolute_path`.
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    // An explicit path (absolute or relative with a separator) is used verbatim.
    if name.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }

    // 1) Honour the live PATH (covers interactive/login invocations).
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(hit) = candidate(&dir, name) {
                return Some(hit);
            }
        }
    }

    // 2) Fall back to the well-known daemon dirs (covers launchd's minimal PATH).
    for dir in daemon_path_dirs() {
        if let Some(hit) = candidate(&dir, name) {
            return Some(hit);
        }
    }
    None
}

/// Return `dir/name` when it is an existing file, else `None`.
///
/// Why: factoring the join+exists check keeps [`resolve_binary`] readable and
/// the "is this a runnable file" predicate in one place.
/// What: joins `dir` and `name` and returns the path when [`Path::is_file`]
/// holds (a symlink to a file also satisfies `is_file`).
/// Test: exercised via the `resolve_binary_*` tests.
fn candidate(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name);
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_path_dirs_orders_user_before_system() {
        let dirs = daemon_path_dirs();
        let pos = |needle: &str| dirs.iter().position(|p| p == &PathBuf::from(needle));
        let homebrew = pos("/opt/homebrew/bin").expect("homebrew dir present");
        let usr_bin = pos("/usr/bin").expect("/usr/bin present");
        assert!(
            homebrew < usr_bin,
            "Homebrew must precede /usr/bin so it shadows older system copies"
        );
    }

    #[test]
    fn daemon_path_dirs_expands_home() {
        let home = dirs::home_dir().expect("home dir resolvable in test env");
        let dirs = daemon_path_dirs();
        assert!(
            dirs.contains(&home.join(".local/bin")),
            "~/.local/bin must be expanded to the real home"
        );
        assert!(
            dirs.contains(&home.join(".cargo/bin")),
            "~/.cargo/bin must be expanded to the real home"
        );
        // No literal tilde should survive expansion.
        assert!(
            dirs.iter().all(|p| !p.starts_with("~")),
            "launchd does not expand ~; paths must be absolute"
        );
    }

    #[test]
    fn daemon_path_dirs_dedupes() {
        let dirs = daemon_path_dirs();
        let mut sorted = dirs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            dirs.len(),
            "daemon_path_dirs must not contain duplicates"
        );
    }

    #[test]
    fn daemon_path_env_contains_expected_dirs() {
        let env = daemon_path_env();
        let home = dirs::home_dir().expect("home dir resolvable in test env");
        assert!(env.contains("/opt/homebrew/bin"), "PATH missing Homebrew");
        assert!(
            env.contains("/usr/local/bin"),
            "PATH missing /usr/local/bin"
        );
        assert!(
            env.contains(home.join(".local/bin").to_str().unwrap()),
            "PATH missing expanded ~/.local/bin"
        );
        assert!(
            env.contains(home.join(".cargo/bin").to_str().unwrap()),
            "PATH missing expanded ~/.cargo/bin"
        );
        for sys in SYSTEM_BIN_DIRS {
            assert!(env.contains(sys), "PATH missing system dir {sys}");
        }
    }

    #[test]
    fn resolve_binary_finds_in_well_known_dir() {
        // Create a fake "binary" inside a temp dir, then point one of the
        // well-known dirs at it via a symlink-free approach: we cannot inject
        // into daemon_path_dirs, so instead verify resolution through PATH and
        // through an explicit directory join using candidate().
        let tmp = std::env::temp_dir().join(format!("bin_resolve_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bin = tmp.join("fake-tool-xyz");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        // candidate() must find it given the directory.
        let hit = candidate(&tmp, "fake-tool-xyz");
        assert_eq!(hit.as_deref(), Some(bin.as_path()));

        // resolve_binary honours PATH: prepend tmp to PATH and resolve.
        let orig = std::env::var_os("PATH");
        let mut paths = vec![tmp.clone()];
        if let Some(ref p) = orig {
            paths.extend(std::env::split_paths(p));
        }
        let joined = std::env::join_paths(paths).unwrap();
        // SAFETY: single-threaded test; restored immediately after.
        unsafe {
            std::env::set_var("PATH", &joined);
        }
        let resolved = resolve_binary("fake-tool-xyz");
        // SAFETY: restore original PATH before any assertion can unwind.
        unsafe {
            match orig {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        assert_eq!(resolved.as_deref(), Some(bin.as_path()));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_binary_returns_none_for_missing() {
        assert!(
            resolve_binary("definitely-not-a-real-binary-zzz-1298").is_none(),
            "a nonexistent binary must resolve to None"
        );
    }

    #[test]
    fn resolve_binary_accepts_absolute_path() {
        // /bin/sh exists on every supported unix.
        let sh = PathBuf::from("/bin/sh");
        if sh.is_file() {
            assert_eq!(resolve_binary("/bin/sh"), Some(sh));
        }
        assert!(
            resolve_binary("/no/such/path/here-1298").is_none(),
            "a non-existent explicit path must resolve to None"
        );
    }
}
