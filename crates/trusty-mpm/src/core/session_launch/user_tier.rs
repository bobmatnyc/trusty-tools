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
}
