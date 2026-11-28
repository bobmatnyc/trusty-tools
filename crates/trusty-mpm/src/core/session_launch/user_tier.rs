//! The user-tier Claude config a launch writes into (#9846).
//!
//! Why: with `CLAUDE_CONFIG_DIR` set, Claude Code reads its user tier from that
//! directory — `settings.json` and `.claude.json` both (see
//! `managed_config::prepare_interactive_config_dir`, which seeds trust into
//! `<config_dir>/.claude.json` for the same reason). The launch pipeline joined
//! both files onto the home instead, so `tm validate --repair` wrote a
//! statusLine and a trust seed no session reads, and never stripped legacy hooks
//! from the file sessions do read. One resolver keeps the statusLine seed, the
//! trust seed and the legacy-hook strip on one directory, and that directory is
//! the one `tm doctor` reads (`session_record::claude_config_dir_override`).
//! What: [`resolve`] (pure) and [`resolve_for_write`] (declines an unusable
//! location), both over a caller-supplied home and config dir.
//! Test: `user_tier_follows_the_config_dir_when_one_is_named`,
//! `repair_writes_the_user_tier_under_claude_config_dir`.

use std::path::{Path, PathBuf};

/// Where the user-tier Claude config lives for one launch.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct UserTier {
    /// The directory holding `settings.json`: the config dir, else `<home>/.claude`.
    pub(super) dir: PathBuf,
    /// The trust/onboarding file: `<config_dir>/.claude.json`, else `<home>/.claude.json`.
    pub(super) claude_json: PathBuf,
}

impl UserTier {
    /// The user-tier `settings.json`.
    pub(super) fn settings_json(&self) -> PathBuf {
        self.dir.join("settings.json")
    }
}

/// The user tier for `home`, relocated to `config_dir` when one is named.
///
/// Why: see the module doc. `config_dir` is an argument, never an env read, so
/// a test names it without a process-global `CLAUDE_CONFIG_DIR` write.
/// What: `config_dir` holds both files when `Some`; otherwise `<home>/.claude`
/// and `<home>/.claude.json`, the pre-#9846 layout. `None` when neither is given.
/// Test: `user_tier_follows_the_config_dir_when_one_is_named`.
pub(super) fn resolve(home: Option<&Path>, config_dir: Option<&Path>) -> Option<UserTier> {
    match config_dir {
        Some(dir) => Some(UserTier {
            dir: dir.to_path_buf(),
            claude_json: dir.join(".claude.json"),
        }),
        None => home.map(|home| UserTier {
            dir: crate::core::session_record::default_claude_config_dir_under(home),
            claude_json: home.join(".claude.json"),
        }),
    }
}

/// The user tier's config dir for a caller whose home a test may pin (#9999).
///
/// Why: the daemon and the connect client take their home from a test pin
/// when one is set (#8545). `CLAUDE_CONFIG_DIR` is set inside every managed
/// session, so reading it under a pinned home would send a test's user-tier
/// writes into the operator's real config dir.
/// What: `pinned_config_dir` when `pinned_home` is `Some`; otherwise
/// [`crate::core::session_record::claude_config_dir_override`], the dir #9846
/// resolves for `tm validate --repair`. Feed the answer to [`resolve_for_write`].
/// Test: `a_pinned_home_never_reads_the_ambient_config_dir`.
pub fn user_tier_config_dir(
    pinned_home: Option<&Path>,
    pinned_config_dir: Option<&Path>,
) -> Option<PathBuf> {
    match pinned_home {
        Some(_) => pinned_config_dir.map(Path::to_path_buf),
        None => crate::core::session_record::claude_config_dir_override(),
    }
}

/// The user tier's config dir for a CLI launch under `home` (#10025).
///
/// Why: a CLI launch names its home, and production names the process home.
/// `CLAUDE_CONFIG_DIR` belongs to that home only, so a sandbox or test home
/// must never read it, the case [`user_tier_config_dir`] guards for the daemon.
/// What: [`user_tier_config_dir`], with `home` pinned when it is not
/// `dirs::home_dir()`; the process home (or none) reads `CLAUDE_CONFIG_DIR`.
/// Test: `a_cli_home_reads_the_config_dir_only_as_the_process_home_10025`.
pub(super) fn config_dir_for_home(home: Option<&Path>) -> Option<PathBuf> {
    let process_home = dirs::home_dir();
    let ambient = crate::core::session_record::claude_config_dir_override();
    config_dir_for_home_in(home, process_home.as_deref(), ambient)
}

/// [`config_dir_for_home`] with the process home and `CLAUDE_CONFIG_DIR` passed in.
///
/// Why (#10025): `tm session start` resolves a sandbox home in the binary, and
/// a test must name the "real" config dir without a process-global env write.
/// What: `None` when `home` is `Some` and not `process_home`; otherwise `ambient`.
/// Test: `a_sandboxed_session_start_writes_nothing_to_the_real_config_dir_10025`.
pub fn config_dir_for_home_in(
    home: Option<&Path>,
    process_home: Option<&Path>,
    ambient: Option<PathBuf>,
) -> Option<PathBuf> {
    match home {
        Some(home) if process_home != Some(home) => None,
        _ => ambient,
    }
}

/// [`resolve`], declined unless the location is absolute.
///
/// Why (#5544): a relative or absent base would send a user-global write beside
/// the process's working directory. Declining is correct: both callers are
/// best-effort side effects.
/// What: [`resolve`], logging at `warn!` and returning `None` when nothing
/// resolves or the resolved directory is relative.
/// Test: `home_writes_are_skipped_when_the_home_is_unusable`.
pub(super) fn resolve_for_write(
    home: Option<&Path>,
    config_dir: Option<&Path>,
) -> Option<UserTier> {
    match resolve(home, config_dir) {
        Some(tier) if tier.dir.is_absolute() => Some(tier),
        Some(tier) => {
            tracing::warn!(
                dir = %tier.dir.display(),
                "skipping user-global session writes: the user-tier directory is not absolute"
            );
            None
        }
        None => {
            tracing::warn!("skipping user-global session writes: no home directory resolved");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #9846: a named config dir holds both files; without one the home layout
    /// is unchanged.
    #[test]
    fn user_tier_follows_the_config_dir_when_one_is_named() {
        let home = Path::new("/h");
        let cfg = Path::new("/cfg");
        assert_eq!(
            resolve(Some(home), Some(cfg)),
            Some(UserTier {
                dir: PathBuf::from("/cfg"),
                claude_json: PathBuf::from("/cfg/.claude.json"),
            })
        );
        let unset = resolve(Some(home), None).expect("a home resolves");
        assert_eq!(unset.settings_json(), Path::new("/h/.claude/settings.json"));
        assert_eq!(unset.claude_json, Path::new("/h/.claude.json"));
        assert_eq!(resolve(None, None), None);
        assert_eq!(resolve_for_write(Some(home), Some(Path::new("rel"))), None);
    }

    /// #9999: a pinned home takes only the pinned config dir, never the
    /// ambient `CLAUDE_CONFIG_DIR` this test process may carry.
    #[test]
    fn a_pinned_home_never_reads_the_ambient_config_dir() {
        let home = Path::new("/h");
        let cfg = Path::new("/cfg");
        assert_eq!(user_tier_config_dir(Some(home), None), None);
        assert_eq!(
            user_tier_config_dir(Some(home), Some(cfg)),
            Some(PathBuf::from("/cfg"))
        );
        assert_eq!(
            user_tier_config_dir(None, Some(cfg)),
            crate::core::session_record::claude_config_dir_override()
        );
    }

    /// #10025: a sandbox or test home never reads the ambient
    /// `CLAUDE_CONFIG_DIR`; the process home reads it, as the daemon does.
    #[test]
    #[serial_test::serial]
    fn a_cli_home_reads_the_config_dir_only_as_the_process_home_10025() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(config_dir_for_home(Some(temp.path())), None);
        let ambient = crate::core::session_record::claude_config_dir_override();
        assert_eq!(config_dir_for_home(dirs::home_dir().as_deref()), ambient);
        assert_eq!(config_dir_for_home(None), ambient);
    }
}
