//! Developer ID signing of the binary `install-service` / `install-host` register.
//!
//! Why: the Keychain item's access list trusts a code identity. An ad-hoc
//! `cargo install` binary gets a new identity on every rebuild, so the next
//! Keychain read prompts, and a prompt under launchd hangs the broker. A
//! binary signed with the team-`4JH68XUHC5` Developer ID and the fixed
//! identifier `com.trusty.browser` keeps one identity across rebuilds (spike
//! 828b63cadb; owner ruling "Sign with Developer ID").
//! What: this product's [`SPEC`] plus thin wrappers over
//! `trusty_daemon_common::codesign` (#11692), which checks the binary against
//! [`REQUIREMENT`], signs it in place when it fails, verifies again, and fails
//! closed before the caller writes a plist or manifest.
//! Test: `install_service_signs_an_unsigned_binary_before_writing` and the
//! other signing tests in `tests/install.rs`.

use std::io;
use std::path::{Path, PathBuf};

use trusty_daemon_common::codesign as shared;
pub use trusty_daemon_common::codesign::{Codesign, Signer, Signing, TEAM};

/// The fixed code-signing identifier.
pub const IDENTIFIER: &str = "com.trusty.browser";
/// Env var naming the signing identity when `--sign-identity` is absent.
pub const IDENTITY_ENV: &str = "TRUSTY_BROWSER_SIGN_IDENTITY";
/// The designated requirement a registered binary must satisfy; equals `SPEC.requirement()`.
pub const REQUIREMENT: &str = "identifier \"com.trusty.browser\" and anchor apple generic and certificate leaf[subject.OU] = \"4JH68XUHC5\"";
/// The stderr warning `--no-sign` prints.
pub const NO_SIGN_WARNING: &str = "warning: --no-sign: the binary is not Developer ID signed; every upgrade will bring back a Keychain prompt, and a prompt under launchd hangs the broker";
/// File name of the MCP binary `cargo install` puts beside `trusty-browser`.
pub const MCP_BINARY: &str = "trusty-browser-mcp";

/// trusty-browser's signing identity and install shape.
pub static SPEC: shared::SignSpec = shared::SignSpec {
    identifier: IDENTIFIER,
    team: TEAM,
    identity_env: IDENTITY_ENV,
    no_sign_warning: NO_SIGN_WARNING,
    mcp_binary: MCP_BINARY,
};

/// The real `/usr/bin/codesign` and `/usr/bin/security` for [`SPEC`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCodesign;

impl Codesign for SystemCodesign {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        shared::SystemCodesign { spec: &SPEC }.verify(path)
    }

    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>> {
        shared::SystemCodesign { spec: &SPEC }.sign(path, identity)
    }

    fn identities(&self) -> io::Result<String> {
        shared::SystemCodesign { spec: &SPEC }.identities()
    }
}

/// The one valid `Developer ID Application: … (4JH68XUHC5)` identity in `listing`.
///
/// Test: `auto_detect_needs_exactly_one_team_identity`.
pub fn detect_identity(listing: &str) -> Result<String, String> {
    SPEC.detect_identity(listing)
}

/// Make `exe` satisfy [`REQUIREMENT`] before it is registered; fail closed.
///
/// Why: a plist or manifest naming an unsigned binary brings the Keychain
/// prompt back, so registration must not happen unless signing held.
/// Test: `install_service_signs_an_unsigned_binary_before_writing`,
/// `install_service_writes_nothing_when_signing_fails`,
/// `install_service_writes_nothing_when_verify_fails_after_signing`,
/// `install_service_refuses_without_an_identity`, `install_host_writes_nothing_when_signing_fails`.
pub fn ensure_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    SPEC.ensure_signed(exe, signer)
}

/// The `trusty-browser-mcp` in the same directory as `exe`.
pub fn mcp_sibling(exe: &Path) -> PathBuf {
    SPEC.mcp_sibling(exe)
}

/// Sign `exe` and its [`mcp_sibling`] before an install registers anything.
///
/// Why: `trusty-browser-mcp` unseals a workflow ticket from the same Keychain
/// item at start-up, and its MCP client cannot answer a prompt (#11440).
/// Test: `install_service_signs_the_mcp_sibling`,
/// `install_service_writes_nothing_when_the_sibling_fails_to_sign`,
/// `install_service_refuses_a_missing_mcp_sibling`,
/// `install_host_signs_the_mcp_sibling_and_refuses_a_missing_one`.
pub fn ensure_install_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    SPEC.ensure_install_signed(exe, signer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_constant_matches_the_spec() {
        assert_eq!(REQUIREMENT, SPEC.requirement());
    }
}
