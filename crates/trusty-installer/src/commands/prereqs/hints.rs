//! Install-hint selection: package-manager detection and OS-specific commands.
//!
//! Why: Install guidance printed when a prereq is missing must be actionable and
//! OS-specific. Selecting the right package manager and command up front lets the
//! phase runner offer an actual, paste-able install command rather than a generic
//! URL that leaves the operator to work out the details themselves.
//!
//! What: `PlatformInfo` bundles the current OS and detected package manager;
//! `detect_platform` inspects the compile-time target and probes `PATH` for
//! known package managers; `install_hint_for` is a **pure** function that maps
//! `(binary, PlatformInfo)` to an `InstallHint` (auto-install command + manual
//! fallback note). The pure design makes `install_hint_for` testable with no
//! side effects.
//!
//! Test: `tests::hint_*` construct `PlatformInfo` directly and assert the
//! returned `InstallHint` fields without touching the real filesystem.

/// The host OS category used for install-hint selection.
///
/// Why: macOS and Linux have different package-manager ecosystems. An explicit
/// `Unknown` variant ensures any future platform gets a safe manual-only hint.
///
/// What: Three-variant enum; derived from `cfg!(target_os)` at runtime.
///
/// Test: Constructed directly in `tests::hint_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Os {
    /// Apple macOS.
    MacOs,
    /// Any Linux distribution.
    Linux,
    /// Any other platform (Windows, BSD, …).
    Unknown,
}

/// The detected system package manager.
///
/// Why: On macOS Homebrew dominates; on Linux distributions vary widely
/// (Debian/Ubuntu → `apt-get`, Fedora/RHEL → `dnf`, Arch → `pacman`).
/// Detecting upfront produces a ready-to-paste command rather than a generic
/// suggestion.
///
/// What: Each variant maps to a known package-manager binary and install idiom.
///
/// Test: Used as the `pkg_mgr` field of `PlatformInfo` in `tests::hint_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgMgr {
    /// Homebrew (`brew`) — the standard macOS package manager.
    Brew,
    /// Debian/Ubuntu APT (`apt-get`).
    Apt,
    /// Fedora/RHEL DNF (`dnf`).
    Dnf,
    /// Arch Linux Pacman (`pacman`).
    Pacman,
}

/// Platform context passed to `install_hint_for`.
///
/// Why: Keeping detection (side-effecting) separate from hint selection (pure)
/// makes `install_hint_for` unit-testable without mocking OS calls.
///
/// What: `os` + optional `pkg_mgr` + `npm_available` are all probed once by
/// `detect_platform` and then injected as a plain struct.
///
/// Test: `tests::hint_*` construct this directly.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    /// Host OS category.
    pub os: Os,
    /// Detected system package manager (None when none is found on PATH).
    pub pkg_mgr: Option<PkgMgr>,
    /// Whether `npm` is on PATH (drives the Claude Code install route).
    pub npm_available: bool,
}

/// Detect the current platform and available package manager.
///
/// Why: The phase runner calls this once per `tctl install` invocation;
/// injecting the result into `install_hint_for` keeps that function pure.
///
/// What: Detects the OS from the compile-time `cfg!(target_os)` macro, then
/// probes PATH for `brew`, `apt-get`, `dnf`, and `pacman` (in that priority
/// order) and for `npm`. Returns a `PlatformInfo` with all findings.
///
/// Test: Side-effecting (reads PATH); unit-tested indirectly via
/// `install_hint_for` which takes a `PlatformInfo` as input.
pub fn detect_platform() -> PlatformInfo {
    let os = if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else {
        Os::Unknown
    };

    let npm_available = which::which("npm").is_ok();

    let pkg_mgr = if which::which("brew").is_ok() {
        Some(PkgMgr::Brew)
    } else if which::which("apt-get").is_ok() {
        Some(PkgMgr::Apt)
    } else if which::which("dnf").is_ok() {
        Some(PkgMgr::Dnf)
    } else if which::which("pacman").is_ok() {
        Some(PkgMgr::Pacman)
    } else {
        None
    };

    PlatformInfo {
        os,
        pkg_mgr,
        npm_available,
    }
}

/// An actionable install hint for a specific binary on a specific platform.
///
/// Why: The phase runner needs both an automated command to run when the user
/// consents AND a human-readable fallback for the non-interactive path.
///
/// What: `auto_cmd` is the shell command to run on TTY consent (`None` when no
/// known auto-install exists for the platform); `manual_note` is always set and
/// is printed when auto-install is unavailable or declined.
///
/// Test: Returned by `install_hint_for` and asserted in `tests::hint_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallHint {
    /// Shell command to execute on user consent. `None` when no known
    /// package-manager path exists for this binary + platform.
    pub auto_cmd: Option<String>,
    /// Human-readable manual-install instruction, always available.
    pub manual_note: String,
}

/// Select the install hint for a binary on the given platform.
///
/// Why: A pure table-driven function means adding a new binary or package
/// manager requires only a new match arm — no control-flow changes.
///
/// What:
/// - `tmux`: macOS + Homebrew → `brew install tmux`; Linux + apt →
///   `sudo apt-get install -y tmux`; Linux + dnf → `sudo dnf install -y tmux`;
///   Linux + pacman → `sudo pacman -S tmux`; otherwise `auto_cmd = None` with
///   platform-appropriate manual note.
/// - `claude`: npm on PATH → `npm install -g @anthropic-ai/claude-code`;
///   macOS/Linux without npm → `curl -fsSL https://claude.ai/install.sh | sh`;
///   unknown platform without npm → `auto_cmd = None`. Manual note always
///   includes `https://claude.ai/download`.
/// - Other binaries: `auto_cmd = None`, generic manual note.
///
/// Test: `tests::hint_tmux_brew`, `tests::hint_tmux_apt`, `tests::hint_tmux_dnf`,
/// `tests::hint_tmux_pacman`, `tests::hint_tmux_no_pkg_mgr`,
/// `tests::hint_claude_npm`, `tests::hint_claude_native_installer`,
/// `tests::hint_claude_unknown_platform`.
pub fn install_hint_for(binary: &str, platform: &PlatformInfo) -> InstallHint {
    match binary {
        "tmux" => hint_tmux(platform),
        "claude" => hint_claude(platform),
        other => InstallHint {
            auto_cmd: None,
            manual_note: format!(
                "Install `{other}` via your system package manager or its official documentation."
            ),
        },
    }
}

fn hint_tmux(platform: &PlatformInfo) -> InstallHint {
    let auto_cmd = match &platform.pkg_mgr {
        Some(PkgMgr::Brew) => Some("brew install tmux".to_owned()),
        Some(PkgMgr::Apt) => Some("sudo apt-get install -y tmux".to_owned()),
        Some(PkgMgr::Dnf) => Some("sudo dnf install -y tmux".to_owned()),
        Some(PkgMgr::Pacman) => Some("sudo pacman -S tmux".to_owned()),
        None => None,
    };
    let manual_note = match &platform.os {
        Os::MacOs => "Install tmux: `brew install tmux` \
             (Homebrew required — https://brew.sh)"
            .to_owned(),
        Os::Linux => "Install tmux via your package manager: \
             `apt-get install tmux`, `dnf install tmux`, or `pacman -S tmux`"
            .to_owned(),
        Os::Unknown => "Install tmux: see https://github.com/tmux/tmux/wiki/Installing".to_owned(),
    };
    InstallHint {
        auto_cmd,
        manual_note,
    }
}

fn hint_claude(platform: &PlatformInfo) -> InstallHint {
    let auto_cmd = if platform.npm_available {
        Some("npm install -g @anthropic-ai/claude-code".to_owned())
    } else {
        match &platform.os {
            Os::MacOs | Os::Linux => {
                Some("curl -fsSL https://claude.ai/install.sh | sh".to_owned())
            }
            Os::Unknown => None,
        }
    };
    InstallHint {
        auto_cmd,
        manual_note: "Install Claude Code: https://claude.ai/download \
                      (or `npm install -g @anthropic-ai/claude-code`)"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macos_brew() -> PlatformInfo {
        PlatformInfo {
            os: Os::MacOs,
            pkg_mgr: Some(PkgMgr::Brew),
            npm_available: false,
        }
    }

    fn linux_apt() -> PlatformInfo {
        PlatformInfo {
            os: Os::Linux,
            pkg_mgr: Some(PkgMgr::Apt),
            npm_available: false,
        }
    }

    fn linux_dnf() -> PlatformInfo {
        PlatformInfo {
            os: Os::Linux,
            pkg_mgr: Some(PkgMgr::Dnf),
            npm_available: false,
        }
    }

    fn linux_pacman() -> PlatformInfo {
        PlatformInfo {
            os: Os::Linux,
            pkg_mgr: Some(PkgMgr::Pacman),
            npm_available: false,
        }
    }

    fn macos_no_pkg_mgr() -> PlatformInfo {
        PlatformInfo {
            os: Os::MacOs,
            pkg_mgr: None,
            npm_available: false,
        }
    }

    fn macos_with_npm() -> PlatformInfo {
        PlatformInfo {
            os: Os::MacOs,
            pkg_mgr: Some(PkgMgr::Brew),
            npm_available: true,
        }
    }

    fn linux_no_npm() -> PlatformInfo {
        PlatformInfo {
            os: Os::Linux,
            pkg_mgr: None,
            npm_available: false,
        }
    }

    fn unknown_no_npm() -> PlatformInfo {
        PlatformInfo {
            os: Os::Unknown,
            pkg_mgr: None,
            npm_available: false,
        }
    }

    /// Why: macOS + Homebrew should produce a `brew install tmux` auto_cmd.
    /// What: Asserts auto_cmd == Some("brew install tmux").
    /// Test: This is the test.
    #[test]
    fn hint_tmux_brew() {
        let hint = install_hint_for("tmux", &macos_brew());
        assert_eq!(hint.auto_cmd, Some("brew install tmux".to_owned()));
        assert!(hint.manual_note.contains("brew.sh"));
    }

    /// Why: Linux + apt should produce a `sudo apt-get install -y tmux` auto_cmd.
    /// What: Asserts auto_cmd contains "apt-get".
    /// Test: This is the test.
    #[test]
    fn hint_tmux_apt() {
        let hint = install_hint_for("tmux", &linux_apt());
        assert_eq!(
            hint.auto_cmd,
            Some("sudo apt-get install -y tmux".to_owned())
        );
    }

    /// Why: Linux + dnf should produce a `sudo dnf install -y tmux` auto_cmd.
    /// What: Asserts auto_cmd contains "dnf".
    /// Test: This is the test.
    #[test]
    fn hint_tmux_dnf() {
        let hint = install_hint_for("tmux", &linux_dnf());
        assert_eq!(hint.auto_cmd, Some("sudo dnf install -y tmux".to_owned()));
    }

    /// Why: Linux + pacman should produce a `sudo pacman -S tmux` auto_cmd.
    /// What: Asserts auto_cmd == Some("sudo pacman -S tmux").
    /// Test: This is the test.
    #[test]
    fn hint_tmux_pacman() {
        let hint = install_hint_for("tmux", &linux_pacman());
        assert_eq!(hint.auto_cmd, Some("sudo pacman -S tmux".to_owned()));
    }

    /// Why: macOS without a detected package manager → no auto_cmd but the
    /// manual note must reference Homebrew.
    /// What: Asserts auto_cmd is None and manual_note mentions Homebrew.
    /// Test: This is the test.
    #[test]
    fn hint_tmux_no_pkg_mgr() {
        let hint = install_hint_for("tmux", &macos_no_pkg_mgr());
        assert!(hint.auto_cmd.is_none());
        assert!(hint.manual_note.contains("brew"));
    }

    /// Why: npm on PATH → claude should be installed via npm for reliability.
    /// What: Asserts auto_cmd == Some("npm install -g @anthropic-ai/claude-code").
    /// Test: This is the test.
    #[test]
    fn hint_claude_npm() {
        let hint = install_hint_for("claude", &macos_with_npm());
        assert_eq!(
            hint.auto_cmd,
            Some("npm install -g @anthropic-ai/claude-code".to_owned())
        );
        assert!(hint.manual_note.contains("claude.ai/download"));
    }

    /// Why: No npm + macOS/Linux → fall back to the native curl installer.
    /// What: Asserts auto_cmd contains "claude.ai/install.sh".
    /// Test: This is the test.
    #[test]
    fn hint_claude_native_installer() {
        let hint = install_hint_for("claude", &linux_no_npm());
        assert!(hint
            .auto_cmd
            .as_deref()
            .unwrap_or("")
            .contains("claude.ai/install.sh"));
        assert!(hint.manual_note.contains("claude.ai/download"));
    }

    /// Why: Unknown platform without npm → no auto_cmd is safe (we can't guess
    /// the right install mechanism).
    /// What: Asserts auto_cmd is None; manual_note still references the docs.
    /// Test: This is the test.
    #[test]
    fn hint_claude_unknown_platform() {
        let hint = install_hint_for("claude", &unknown_no_npm());
        assert!(hint.auto_cmd.is_none());
        assert!(hint.manual_note.contains("claude.ai/download"));
    }

    /// Why: An unknown binary must return a safe default with no auto_cmd.
    /// What: Asserts auto_cmd is None for a fictional binary name.
    /// Test: This is the test.
    #[test]
    fn hint_unknown_binary() {
        let hint = install_hint_for("nonexistent-tool-xyz", &macos_brew());
        assert!(hint.auto_cmd.is_none());
    }
}
