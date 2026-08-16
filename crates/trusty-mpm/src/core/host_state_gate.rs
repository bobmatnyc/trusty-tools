//! Gate over host state that `$HOME` does not isolate — the tmux server and
//! the host process table (#5784).
//!
//! Why: a daemon started under a throwaway `$HOME` (a scratch port, a
//! disposable data dir, `TRUSTY_MPM_URL` pointed at it) is isolated from the
//! operator's `~/.trusty-mpm` state and nothing else. tmux is keyed to the OS
//! user, not to `$HOME`, so the scratch daemon's startup auto-discovery still
//! listed the operator's live panes, adopted them as its own sessions, and —
//! because an adopted record carries the real project's working directory —
//! refreshed `.claude/skills/` inside two real project checkouts, one in a
//! different repository entirely. That instance was harmless (the cache is
//! git-ignored and the refresh idempotent), but the reachability is the
//! defect: anyone running an isolated test daemon expects `$HOME` isolation to
//! hold, and the next write on that path need not be either.
//!
//! What: [`host_state_access`] answers "is this the operator's real
//! environment?" by comparing the process `$HOME` against the home the OS
//! password database records for this uid. [`classify`] is the pure decision
//! behind it. The verdict is consulted by
//! [`crate::daemon::tmux::TmuxDriver::discover`] — the crate's only
//! constructor for a tmux-backed driver, so holding a `TmuxDriver` is itself
//! proof the gate passed — and by
//! [`crate::daemon::discovery::discover_all`], which also scans the host
//! process table.
//!
//! Fail-closed, deliberately inverted from this crate's usual direction: an
//! environment this module cannot classify is treated as scratch and skipped.
//! A wrong skip costs a test daemon that does not adopt sessions; a wrong
//! proceed writes into somebody's live project directories.
//!
//! Test: `core::host_state_gate::tests` covers [`classify`] on every arm;
//! `scratch_home_daemon_does_not_spawn_tmux` proves
//! the startup path spawns no tmux process under a scratch `$HOME`.

use std::path::{Path, PathBuf};

/// Operator opt-in that lifts this gate.
///
/// Why: someone deliberately testing tmux adoption under a scratch `$HOME`
/// must still be able to. Named and explicit, in the style of the crate's
/// other operator hatches (`TRUSTY_MPM_DISABLE_HOOKS`,
/// `TRUSTY_MPM_PM_UNRESTRICTED`, `TRUSTY_MPM_ALLOW_MCP_SPAWN`).
/// What: any truthy token (`1`/`true`/`yes`/`on`, case-insensitive) allows
/// access regardless of how the environment classifies.
pub const ALLOW_HOST_STATE_ENV: &str = "TRUSTY_MPM_ALLOW_HOST_STATE";

/// Whether this process may reach host state `$HOME` does not isolate.
///
/// Why: the two allow arms and the two block arms are kept distinct rather
/// than collapsed to a `bool` so the skip log can name WHY it skipped — a
/// silent skip is its own trap, and "scratch `$HOME`" and "could not tell"
/// call for different operator responses.
/// What: [`Self::is_allowed`] is the decision; [`Self::skip_reason`] is the
/// operator-facing sentence for the block arms.
/// Test: `classify_*` in this module's tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStateAccess {
    /// `$HOME` matches the password-database home for this uid.
    RealEnvironment,
    /// [`ALLOW_HOST_STATE_ENV`] is truthy — an explicit operator opt-in.
    OptedIn,
    /// `$HOME` was reassigned away from this uid's real home.
    ScratchEnvironment {
        /// The process `$HOME`.
        effective_home: PathBuf,
        /// The home the OS password database records for this uid.
        passwd_home: PathBuf,
    },
    /// Neither home could be established, so the environment is unclassifiable.
    Indeterminate {
        /// What could not be determined.
        detail: String,
    },
}

impl HostStateAccess {
    /// True when the caller may reach tmux and the host process table.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::RealEnvironment | Self::OptedIn)
    }

    /// The operator-facing reason a block arm skipped, or `None` when allowed.
    ///
    /// Why: every gated call site logs this verbatim, so the sentence names
    /// the mismatch it found AND the hatch that lifts it in one place rather
    /// than each site inventing its own wording.
    /// What: `None` for both allow arms; a full sentence for both block arms.
    /// Test: `skip_reason_names_the_hatch`.
    pub fn skip_reason(&self) -> Option<String> {
        match self {
            Self::RealEnvironment | Self::OptedIn => None,
            Self::ScratchEnvironment {
                effective_home,
                passwd_home,
            } => Some(format!(
                "$HOME is {} but this user's real home is {} — tmux and the host process \
                 table are shared system state that $HOME does not isolate, so reaching them \
                 would touch the operator's real sessions and project directories (#5784). \
                 Set {ALLOW_HOST_STATE_ENV}=1 to allow it.",
                effective_home.display(),
                passwd_home.display()
            )),
            Self::Indeterminate { detail } => Some(format!(
                "cannot tell whether this is the operator's real environment ({detail}), so \
                 shared host state is left alone rather than risk touching real sessions and \
                 project directories (#5784). Set {ALLOW_HOST_STATE_ENV}=1 to allow it."
            )),
        }
    }
}

/// Classify an environment from its two homes and the opt-in flag.
///
/// Why: the whole decision is pure and exhaustively testable here, so the
/// call sites carry only `if !access.is_allowed() { skip }` and no test needs
/// to manufacture a password-database entry.
/// What: the opt-in short-circuits. Otherwise both homes must be present and
/// equal (see [`same_home`]) for [`HostStateAccess::RealEnvironment`]; present
/// and different is [`HostStateAccess::ScratchEnvironment`]; either one
/// missing is [`HostStateAccess::Indeterminate`], which blocks.
/// Test: `classify_opt_in_allows`, `classify_matching_homes_allow`,
/// `classify_mismatched_homes_block`, `classify_missing_home_blocks`,
/// `classify_missing_passwd_home_blocks`,
/// `classify_trailing_separator_still_matches`.
pub fn classify(
    effective_home: Option<&Path>,
    passwd_home: Option<&Path>,
    opt_in: bool,
) -> HostStateAccess {
    if opt_in {
        return HostStateAccess::OptedIn;
    }
    match (effective_home, passwd_home) {
        (Some(effective), Some(passwd)) if same_home(effective, passwd) => {
            HostStateAccess::RealEnvironment
        }
        (Some(effective), Some(passwd)) => HostStateAccess::ScratchEnvironment {
            effective_home: effective.to_path_buf(),
            passwd_home: passwd.to_path_buf(),
        },
        (None, _) => HostStateAccess::Indeterminate {
            detail: "$HOME is unset or empty".to_string(),
        },
        (_, None) => HostStateAccess::Indeterminate {
            detail: "the OS password database has no home for this uid".to_string(),
        },
    }
}

/// True when two paths name the same home directory.
///
/// Why: `$HOME` and the password-database entry routinely differ in spelling
/// for the same directory — a trailing separator, or a symlinked mount point
/// (`/home/x` behind `/System/Volumes/Data/home/x`). Comparing raw strings
/// would report a scratch environment on a perfectly real one.
/// What: canonicalizes both and compares; when either canonicalization fails
/// (the directory may not exist, which is itself normal for a scratch home),
/// compares the paths with trailing separators trimmed. Never guesses in the
/// direction of equality on a failed comparison.
/// Test: `classify_trailing_separator_still_matches`,
/// `same_home_rejects_siblings`.
fn same_home(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    trim_trailing_separators(a) == trim_trailing_separators(b)
}

/// Strip trailing path separators so `/Users/x/` and `/Users/x` compare equal,
/// keeping a bare root (`/`) intact.
fn trim_trailing_separators(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let trimmed = raw.trim_end_matches(std::path::MAIN_SEPARATOR);
    if trimmed.is_empty() {
        path.to_path_buf()
    } else {
        PathBuf::from(trimmed)
    }
}

/// The verdict for THIS process, read from its environment.
///
/// Why: the single entry point every gated call site uses, so the signal
/// cannot drift between them.
/// What: reads `$HOME`, the password-database home for the current uid, and
/// [`ALLOW_HOST_STATE_ENV`], then defers to [`classify`]. Recomputed per call
/// rather than cached — the inputs are two syscalls next to a `fork`+`exec`,
/// and a cache would be exactly the global state this crate's conventions
/// forbid.
/// Test: covered through [`classify`]; the end-to-end effect is proven by
/// `scratch_home_daemon_does_not_spawn_tmux`.
pub fn host_state_access() -> HostStateAccess {
    #[cfg(not(unix))]
    {
        return host_state_access_non_unix();
    }
    #[cfg(unix)]
    classify(
        effective_home().as_deref(),
        passwd_home().as_deref(),
        opt_in_enabled(),
    )
}

/// The process `$HOME`, or `None` when unset or empty.
///
/// Read directly rather than through `dirs::home_dir` so the value being
/// compared is unambiguously the one the operator reassigned.
fn effective_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The home the OS password database records for the current uid.
///
/// Why: this is the signal that actually distinguishes a scratch environment,
/// because the process cannot reassign it the way it reassigns `$HOME` — and
/// it is the same OS user identity the tmux server is keyed to.
/// What: `getpwuid(getuid())`'s home directory via `nix`. A lookup failure or
/// a passwd entry with an empty home yields `None`, which
/// [`classify`] treats as indeterminate and therefore blocks.
/// Test: `classify_missing_passwd_home_blocks` covers the `None` arm.
#[cfg(unix)]
fn passwd_home() -> Option<PathBuf> {
    match nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
        Ok(Some(user)) if !user.dir.as_os_str().is_empty() => Some(user.dir),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("#5784: password-database lookup for the current uid failed: {e}");
            None
        }
    }
}

/// Non-unix hosts have no password database and no tmux, so there is nothing
/// for this gate to isolate — it always allows.
///
/// Returning `effective_home()` here would have blocked instead: Windows
/// normally leaves `$HOME` unset, so `classify` would hit its `(None, _)` arm
/// and refuse every tmux path on a platform that has no tmux to refuse.
#[cfg(not(unix))]
fn host_state_access_non_unix() -> HostStateAccess {
    HostStateAccess::RealEnvironment
}

/// True when [`ALLOW_HOST_STATE_ENV`] holds a truthy token.
fn opt_in_enabled() -> bool {
    std::env::var(ALLOW_HOST_STATE_ENV)
        .ok()
        .map(|v| crate::core::auto_resume::parse_truthy(v.trim()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_opt_in_allows() {
        // The opt-in must win even on an environment that would otherwise be
        // classified scratch — that is the whole point of the hatch.
        let access = classify(
            Some(Path::new("/tmp/scratch-home")),
            Some(Path::new("/Users/real")),
            true,
        );
        assert_eq!(access, HostStateAccess::OptedIn);
        assert!(access.is_allowed());
        assert!(access.skip_reason().is_none());
    }

    #[test]
    fn classify_matching_homes_allow() {
        let access = classify(
            Some(Path::new("/Users/real")),
            Some(Path::new("/Users/real")),
            false,
        );
        assert_eq!(access, HostStateAccess::RealEnvironment);
        assert!(access.is_allowed());
    }

    #[test]
    fn classify_mismatched_homes_block() {
        let access = classify(
            Some(Path::new("/tmp/scratch-home")),
            Some(Path::new("/Users/real")),
            false,
        );
        assert!(!access.is_allowed(), "a reassigned $HOME must block");
        assert!(matches!(access, HostStateAccess::ScratchEnvironment { .. }));
    }

    #[test]
    fn classify_missing_home_blocks() {
        // Fail-closed: an environment with no $HOME at all is unclassifiable,
        // and the safe answer is to leave shared host state alone.
        let access = classify(None, Some(Path::new("/Users/real")), false);
        assert!(!access.is_allowed());
        assert!(matches!(access, HostStateAccess::Indeterminate { .. }));
    }

    #[test]
    fn classify_missing_passwd_home_blocks() {
        let access = classify(Some(Path::new("/Users/real")), None, false);
        assert!(!access.is_allowed());
        assert!(matches!(access, HostStateAccess::Indeterminate { .. }));
    }

    #[test]
    fn classify_trailing_separator_still_matches() {
        // `$HOME=/Users/real/` and a passwd entry of `/Users/real` are the
        // same directory; reporting scratch here would gate a real operator.
        let access = classify(
            Some(Path::new("/Users/real/")),
            Some(Path::new("/Users/real")),
            false,
        );
        assert_eq!(access, HostStateAccess::RealEnvironment);
    }

    #[test]
    fn same_home_rejects_siblings() {
        assert!(!same_home(
            Path::new("/Users/real"),
            Path::new("/Users/rea")
        ));
        assert!(!same_home(
            Path::new("/Users/real"),
            Path::new("/Users/real2")
        ));
    }

    #[test]
    fn skip_reason_names_the_hatch() {
        // A skip an operator cannot lift is a dead end; both block arms must
        // name the variable that lifts them.
        for access in [
            classify(
                Some(Path::new("/tmp/scratch-home")),
                Some(Path::new("/Users/real")),
                false,
            ),
            classify(None, None, false),
        ] {
            let reason = access
                .skip_reason()
                .expect("a block arm must explain itself");
            assert!(
                reason.contains(ALLOW_HOST_STATE_ENV),
                "skip reason must name the opt-in: {reason}"
            );
        }
    }
}
