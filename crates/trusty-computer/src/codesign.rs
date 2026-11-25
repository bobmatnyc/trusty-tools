//! Developer ID signing of both binaries `install-service` registers.
//!
//! Why: an ad-hoc `cargo install` binary gets a new code identity on every
//! rebuild. The launchd job and the MCP client config name these binaries, so
//! they carry the team-`4JH68XUHC5` Developer ID and the fixed identifier
//! `com.trusty.computer` (plan section 7, issue #11460). TCC attribution stays
//! with `CuaDriver.app`.
//! What: this product's [`SPEC`] plus thin wrappers over
//! `trusty_daemon_common::codesign` (#11692), which signs, verifies and fails
//! closed before the caller writes the plist.
//! Test: `install_service_signs_an_unsigned_binary_before_writing` and the
//! other signing tests in `tests/install.rs`.

use std::io;
use std::path::{Path, PathBuf};

use trusty_daemon_common::codesign as shared;
pub use trusty_daemon_common::codesign::{Codesign, Signer, Signing, TEAM};

/// The fixed code-signing identifier.
pub const IDENTIFIER: &str = trusty_common::launchd_labels::COMPUTER;
/// Env var naming the signing identity when `--sign-identity` is absent.
pub const IDENTITY_ENV: &str = "TRUSTY_COMPUTER_SIGN_IDENTITY";
/// The stderr warning `--no-sign` prints.
pub const NO_SIGN_WARNING: &str = "warning: --no-sign: the binary is not Developer ID signed; its code identity changes on every rebuild";
/// File name of the MCP binary `cargo install` puts beside `trusty-computer`.
pub const MCP_BINARY: &str = "trusty-computer-mcp";

/// trusty-computer's signing identity and install shape.
pub static SPEC: shared::SignSpec = shared::SignSpec {
    identifier: IDENTIFIER,
    team: TEAM,
    identity_env: IDENTITY_ENV,
    no_sign_warning: NO_SIGN_WARNING,
    mcp_binary: MCP_BINARY,
};

/// The designated requirement a registered binary must satisfy.
pub fn requirement() -> String {
    SPEC.requirement()
}

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

/// The one valid team Developer ID Application identity in `listing`.
///
/// Test: `auto_detect_needs_exactly_one_team_identity`.
pub fn detect_identity(listing: &str) -> Result<String, String> {
    SPEC.detect_identity(listing)
}

/// Make `exe` satisfy [`requirement()`] before it is registered; fail closed.
///
/// Test: `install_service_signs_an_unsigned_binary_before_writing`,
/// `install_service_writes_nothing_when_signing_fails`,
/// `install_service_writes_nothing_when_verify_fails_after_signing`,
/// `install_service_refuses_without_an_identity`.
pub fn ensure_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    SPEC.ensure_signed(exe, signer)
}

/// The `trusty-computer-mcp` in the same directory as `exe`.
pub fn mcp_sibling(exe: &Path) -> PathBuf {
    SPEC.mcp_sibling(exe)
}

/// Sign `exe` and its [`mcp_sibling`] before an install registers anything.
///
/// Test: `install_service_signs_the_mcp_sibling`,
/// `install_service_writes_nothing_when_the_sibling_fails_to_sign`,
/// `install_service_refuses_a_missing_mcp_sibling`.
pub fn ensure_install_signed(exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
    SPEC.ensure_install_signed(exe, signer)
}
