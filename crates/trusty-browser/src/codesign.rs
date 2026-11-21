//! Developer ID signing of the binary `install-service` / `install-host` register.
//!
//! Why: the Keychain item's access list trusts a code identity. An ad-hoc
//! `cargo install` binary gets a new identity on every rebuild, so the next
//! Keychain read prompts, and a prompt under launchd hangs the broker. A
//! binary signed with the team-`4JH68XUHC5` Developer ID and the fixed
//! identifier `com.trusty.browser` keeps one identity across rebuilds (spike
//! 828b63cadb; owner ruling "Sign with Developer ID").
//! What: [`ensure_signed`] checks the binary against [`REQUIREMENT`], signs it
//! in place when it fails, and verifies again; any failure is an error the
//! caller returns before it writes a plist or manifest. `codesign` and
//! `security` sit behind [`Codesign`] so tests never sign or read the Keychain.
//! Test: `install_service_signs_an_unsigned_binary_before_writing` and the
//! other signing tests in `tests/install.rs`.

use std::io;
use std::path::Path;
use std::process::Command;

/// The fixed code-signing identifier.
pub const IDENTIFIER: &str = "com.trusty.browser";
/// The Developer ID team the identity must belong to.
pub const TEAM: &str = "4JH68XUHC5";
/// Env var naming the signing identity when `--sign-identity` is absent.
pub const IDENTITY_ENV: &str = "TRUSTY_BROWSER_SIGN_IDENTITY";
/// The designated requirement a registered binary must satisfy.
pub const REQUIREMENT: &str = "identifier \"com.trusty.browser\" and anchor apple generic and certificate leaf[subject.OU] = \"4JH68XUHC5\"";
/// The stderr warning `--no-sign` prints.
pub const NO_SIGN_WARNING: &str = "warning: --no-sign: the binary is not Developer ID signed; every upgrade will bring back a Keychain prompt, and a prompt under launchd hangs the broker";

/// Runs `codesign` / `security`; a seam so tests never sign or read the Keychain.
pub trait Codesign {
    /// True when `path` satisfies [`REQUIREMENT`].
    fn verify(&self, path: &Path) -> io::Result<bool>;
    /// Sign `path` in place with `identity`; `Err` carries codesign's stderr.
    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>>;
    /// `security find-identity -v -p codesigning` stdout.
    fn identities(&self) -> io::Result<String>;
}

/// The real `/usr/bin/codesign` and `/usr/bin/security`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCodesign;

impl Codesign for SystemCodesign {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        let out = Command::new("/usr/bin/codesign")
            .arg("-v")
            .arg(format!("-R={REQUIREMENT}"))
            .arg(path)
            .output()?;
        Ok(out.status.success())
    }

    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>> {
        let out = Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--options",
                "runtime",
                "--identifier",
                IDENTIFIER,
            ])
            .args(["-s", identity])
            .arg(path)
            .output()?;
        Ok(if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        })
    }

    fn identities(&self) -> io::Result<String> {
        let out = Command::new("/usr/bin/security")
            .args(["find-identity", "-v", "-p", "codesigning"])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// How the install verbs treat signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signing {
    /// Sign when needed; `None` auto-detects the identity.
    Sign(Option<String>),
    /// `--no-sign`: register the binary as it is, with a warning.
    Skip,
}

/// The signing policy plus the tool that carries it out.
pub struct Signer<'a> {
    /// What to do.
    pub signing: Signing,
    /// How to do it.
    pub tool: &'a dyn Codesign,
}

/// The one valid `Developer ID Application: … (4JH68XUHC5)` identity in `listing`.
///
/// Why: auto-detection must never guess between two identities.
/// What: collects the quoted names of `security find-identity` output that
/// are Developer ID Application identities of [`TEAM`]; exactly one is `Ok`.
/// Test: `auto_detect_needs_exactly_one_team_identity`.
pub fn detect_identity(listing: &str) -> Result<String, String> {
    let suffix = format!("({TEAM})");
    let mut found: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split('"').nth(1))
        .filter(|n| n.starts_with("Developer ID Application: ") && n.ends_with(&suffix))
        .collect();
    found.dedup();
    match found.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(format!(
            "No Developer ID Application identity for team {TEAM} found; pass --sign-identity, set {IDENTITY_ENV}, or use --no-sign"
        )),
        _ => Err(format!(
            "More than one Developer ID Application identity for team {TEAM}; pass --sign-identity or set {IDENTITY_ENV}"
        )),
    }
}

/// Make `exe` satisfy [`REQUIREMENT`] before it is registered; fail closed.
///
/// Why: a plist or manifest naming an unsigned binary brings the Keychain
/// prompt back, so registration must not happen unless signing held.
/// What: `Skip` warns and returns. Otherwise an already-satisfying binary is
/// left alone; else the identity (given, or [`detect_identity`]) signs `exe`
/// in place and `exe` is verified again. Every failure is `Err`.
/// Test: `install_service_signs_an_unsigned_binary_before_writing`,
/// `install_service_writes_nothing_when_signing_fails`,
/// `install_service_writes_nothing_when_verify_fails_after_signing`,
/// `install_service_refuses_without_an_identity`, `install_host_writes_nothing_when_signing_fails`.
pub fn ensure_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    let identity = match &signer.signing {
        Signing::Skip => {
            eprintln!("{NO_SIGN_WARNING}");
            return Ok(());
        }
        Signing::Sign(identity) => identity,
    };
    let tool = signer.tool;
    if tool.verify(exe)? {
        return Ok(());
    }
    let identity = match identity {
        Some(i) => i.clone(),
        None => detect_identity(&tool.identities()?).map_err(io::Error::other)?,
    };
    if let Err(msg) = tool.sign(exe, &identity)? {
        return Err(io::Error::other(format!(
            "codesign failed for {}: {msg}",
            exe.display()
        )));
    }
    if !tool.verify(exe)? {
        return Err(io::Error::other(format!(
            "{} does not satisfy the signing requirement after signing with {identity:?}; nothing was registered",
            exe.display()
        )));
    }
    Ok(())
}

/// File name of the MCP binary `cargo install` puts beside `trusty-browser`.
pub const MCP_BINARY: &str = "trusty-browser-mcp";

/// The `trusty-browser-mcp` in the same directory as `exe`.
pub fn mcp_sibling(exe: &Path) -> std::path::PathBuf {
    exe.with_file_name(MCP_BINARY)
}

/// Sign `exe` and its [`mcp_sibling`] before an install registers anything.
///
/// Why: `trusty-browser-mcp` unseals a workflow ticket from the same Keychain
/// item at start-up, and its MCP client cannot answer a prompt (#11440).
/// What: `Skip` warns once and returns. Otherwise a missing sibling is `Err`;
/// then [`ensure_signed`] runs on `exe` and on the sibling with the same
/// identity and [`IDENTIFIER`], and the first failure is returned.
/// Test: `install_service_signs_the_mcp_sibling`,
/// `install_service_writes_nothing_when_the_sibling_fails_to_sign`,
/// `install_service_refuses_a_missing_mcp_sibling`,
/// `install_host_signs_the_mcp_sibling_and_refuses_a_missing_one`.
pub fn ensure_install_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    if signer.signing == Signing::Skip {
        return ensure_signed(exe, signer);
    }
    let sibling = mcp_sibling(exe);
    if !sibling.is_file() {
        return Err(io::Error::other(format!(
            "{} is missing; install both binaries from the same `cargo install`, or use --no-sign",
            sibling.display()
        )));
    }
    ensure_signed(exe, signer)?;
    ensure_signed(&sibling, signer)
}
