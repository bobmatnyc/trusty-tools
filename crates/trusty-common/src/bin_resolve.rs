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

/// Return `dir/name` when it is an existing, runnable file, else `None`.
///
/// Why: factoring the join+exists check keeps [`resolve_binary`] readable and
/// the "is this a runnable file" predicate in one place. A bare `is_file` check
/// is too loose: a non-executable regular file (e.g. a stray `claude.json` or a
/// data file that happens to share a name) would be returned and then fail at
/// spawn time. Requiring the execute bit on Unix means resolution only yields
/// paths the daemon can actually `exec`.
/// What: joins `dir` and `name`. On Unix the result is returned only when it is
/// a file *and* at least one execute bit (`0o111`) is set in its permissions; a
/// symlink to an executable file also satisfies this (metadata follows the
/// link). On non-Unix targets the historical [`Path::is_file`] behaviour is
/// preserved (no portable execute concept).
/// Test: `candidate_requires_execute_bit_on_unix`, plus the `resolve_binary_*`
/// tests.
fn candidate(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(name);
    if !p.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `metadata` follows symlinks, so a symlink to an executable resolves
        // correctly. A file with no execute bit is not a runnable binary.
        match std::fs::metadata(&p) {
            Ok(meta) if meta.permissions().mode() & 0o111 != 0 => Some(p),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        Some(p)
    }
}

/// Path segments that mark a binary as an ephemeral (non-installed) build.
///
/// Why: a `target/debug`/`target/release` binary and any binary living under a
/// git worktree directory are rebuilt/deleted on the next `cargo` invocation or
/// worktree cleanup. Baking such a path into a persisted, shared config
/// (managed hook commands, statusline command) leaves a stale absolute path
/// that 404s once the build artifact is gone (#2229).
/// What: the worktree layout markers this workspace uses
/// (`.claude/worktrees/`, `.base/.worktrees/`) plus the two Cargo build
/// profiles. Matched as substrings so `deps/`-nested and profile-suffixed
/// variants are all covered.
const EPHEMERAL_PATH_SEGMENTS: &[&str] = &[
    "target/debug",
    "target/release",
    ".claude/worktrees/",
    ".base/.worktrees/",
];

/// Whether `path` points inside an ephemeral build/worktree location that will
/// not survive a rebuild or worktree cleanup.
///
/// Why: `std::env::current_exe()` returns a `target/debug/deps/...` path when
/// `tm`/`trusty-mpm` runs from a worktree or a debug build. Persisting that
/// path into the SHARED global `settings.json` (hook commands, statusline)
/// breaks every managed session once the artifact is rebuilt away (#2229).
/// Callers use this to reject `current_exe()` and fall back to a stable,
/// PATH-resolved installed binary instead.
/// What: returns `true` when the path's string form contains any of
/// [`EPHEMERAL_PATH_SEGMENTS`]. Uses a lossy string comparison so non-UTF-8
/// path components degrade to "not ephemeral" rather than panicking.
/// Test: `is_ephemeral_build_path_flags_build_and_worktree_paths`,
/// `is_ephemeral_build_path_accepts_installed_paths`.
pub fn is_ephemeral_build_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    EPHEMERAL_PATH_SEGMENTS.iter().any(|seg| s.contains(seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_ephemeral_build_path_flags_build_and_worktree_paths() {
        for p in [
            "/Users/x/trusty-tools/target/debug/deps/trusty_mpm-abc123",
            "/Users/x/trusty-tools/target/release/tm",
            "/Users/x/repo/.claude/worktrees/fix-123/target/debug/tm",
            "/Users/x/repo/.base/.worktrees/abc/crates/trusty-mpm",
        ] {
            assert!(
                is_ephemeral_build_path(Path::new(p)),
                "{p} must be flagged as an ephemeral build path"
            );
        }
    }

    #[test]
    fn is_ephemeral_build_path_accepts_installed_paths() {
        for p in ["/Users/x/.cargo/bin/trusty-mpm", "/usr/local/bin/tm", "tm"] {
            assert!(
                !is_ephemeral_build_path(Path::new(p)),
                "{p} must NOT be flagged as an ephemeral build path"
            );
        }
    }

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

    /// Create a unique temp directory for a test, returning its path.
    ///
    /// Why: each test needs an isolated scratch dir; keying it on the test name
    /// plus the process id avoids collisions under the parallel harness without
    /// pulling in an extra dev-dependency.
    /// What: joins the system temp dir with `bin_resolve_<tag>_<pid>` and
    /// `create_dir_all`s it.
    /// Test: exercised by every test that calls it.
    fn make_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bin_resolve_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Write a file and, on Unix, mark it executable.
    ///
    /// Why: [`candidate`] now requires the execute bit on Unix, so test fixtures
    /// that stand in for binaries must be chmod'd `0o755` to be discoverable.
    /// What: writes a tiny shebang stub at `path` and sets mode `0o755` on Unix.
    /// Test: exercised by `candidate_*` and `resolve_binary_*` tests.
    fn write_executable(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\n").expect("write fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).expect("stat fixture").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod fixture");
        }
    }

    #[test]
    fn resolve_binary_finds_in_well_known_dir() {
        // Verify the "found in a directory" branch WITHOUT mutating the
        // process-global PATH (which races the parallel test harness). We
        // exercise candidate() directly against an explicit temp dir — the same
        // per-directory predicate resolve_binary() applies to each PATH and
        // well-known-dir entry — and confirm resolve_binary() accepts the
        // resulting explicit path verbatim.
        let tmp = make_temp_dir("well_known");
        let bin = tmp.join("fake-tool-xyz");
        write_executable(&bin);

        // candidate() finds the executable given its directory.
        let hit = candidate(&tmp, "fake-tool-xyz");
        assert_eq!(hit.as_deref(), Some(bin.as_path()));

        // resolve_binary() accepts the discovered path as an explicit path,
        // closing the loop from "found in a dir" to "usable result" — no PATH
        // mutation required.
        let explicit = bin.to_str().expect("utf8 temp path");
        assert_eq!(resolve_binary(explicit).as_deref(), Some(bin.as_path()));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn candidate_requires_execute_bit_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = make_temp_dir("exec_bit");

        // A non-executable regular file must NOT be returned.
        let data = tmp.join("not-a-binary");
        std::fs::write(&data, b"plain data\n").expect("write data file");
        let mut perms = std::fs::metadata(&data).expect("stat data").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&data, perms).expect("chmod data");
        assert_eq!(
            candidate(&tmp, "not-a-binary"),
            None,
            "a non-executable regular file must not resolve as a runnable binary"
        );

        // An executable file with the same parent dir IS returned.
        let exe = tmp.join("a-binary");
        write_executable(&exe);
        assert_eq!(
            candidate(&tmp, "a-binary").as_deref(),
            Some(exe.as_path()),
            "an executable file must resolve"
        );

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
