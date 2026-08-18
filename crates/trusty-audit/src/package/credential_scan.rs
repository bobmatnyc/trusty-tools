//! Scanning a packaged member for the engagement's own secrets, one member at
//! a time and without holding it in memory.
//!
//! Why: split out of `package.rs` when adding [`crate::error::AuditError::MissingExtractDatabase`]
//! pushed that file one line past the 500-SLOC production cap (#5862) — the
//! same split pattern documented in the project's file-size-cap policy. The
//! scan itself is unchanged; only its home moved. The needle set grew once
//! more the same way: the `gh`-derived GitHub token (#5980 CRITICAL 4) joined
//! `openrouter_key` and the two board secrets as a value the deliverable must
//! never carry, so `secret_needles` and `copy_member` both take it as an
//! explicit parameter rather than reaching for a type this crate does not own.
//!
//! What: [`copy_member`] streams one file into the archive through
//! [`CredentialScan`], a byte-oriented sliding window that has no notion of
//! file format — it scans a SQLite database exactly as it scans a `.md`
//! report, as an opaque byte stream searched for every configured secret's
//! exact bytes. Nothing about `extract/<stem>.db` exempts it: this is the same
//! scan every member goes through, database or not.
//! Test: `super::super::package_tests`, `credential_scan_tests`.

use std::io::{Read as _, Write as _};
use std::path::Path;

use crate::config::EngagementConfig;
use crate::error::AuditError;

use super::{Archive, start};

/// How much of a member is read at a time while copying and scanning it.
const CHUNK_BYTES: usize = 64 * 1024;

/// Every secret this engagement holds, as the bytes a member may not carry.
///
/// Why: the guard used to scan for `openrouter_key` alone, and #5857 put two
/// more secrets on the same trust boundary — the JIRA token and the Linear API
/// key reach a `tga audit` child through its environment, and the files that
/// child writes are exactly the files this function's caller packages and sends
/// off the recipient's network. One list, so a credential added to
/// [`EngagementConfig`] is refused here the moment it is configured.
///
/// `github_token` extends the same guard to the `gh`-derived credential
/// (#5980 CRITICAL 4): before this parameter existed, a token echoed into a
/// `tga`-written file under `out/`/`extract/` — the same shape #5857 already
/// covers for JIRA and Linear — reached the deliverable unscanned, because
/// [`EngagementConfig::configured_secrets`] has no way to name a credential
/// that never lived in the engagement TOML.
/// What: [`EngagementConfig::configured_secrets`] as byte slices, plus
/// `github_token` when one was read — the same pair `crate::run`'s
/// child-log scrubber uses as its needles, so the two guards cannot come to
/// disagree about what a secret is. A provider with no entry contributes no
/// needle.
/// Test: `super::super::package_tests::a_member_carrying_the_jira_token_is_refused_and_leaves_no_zip`,
/// `super::super::package_tests::a_member_carrying_the_linear_api_key_is_refused_and_leaves_no_zip`,
/// `super::super::package_tests::a_member_carrying_several_secrets_is_refused_on_the_first_pass`,
/// `super::super::package_tests::a_member_carrying_the_github_token_is_refused_and_leaves_no_zip`.
fn secret_needles<'a>(
    config: &'a EngagementConfig,
    github_token: Option<&'a str>,
) -> Vec<&'a [u8]> {
    let mut needles: Vec<&[u8]> = config
        .configured_secrets()
        .into_iter()
        .map(str::as_bytes)
        .collect();
    if let Some(token) = github_token {
        needles.push(token.as_bytes());
    }
    needles
}

/// Copy one file into the archive, refusing it if it carries any credential.
pub(super) fn copy_member(
    zip: &mut Archive,
    entry: &str,
    source: &Path,
    config: &EngagementConfig,
    temporary: &Path,
    github_token: Option<&str>,
) -> Result<u64, AuditError> {
    let mut input = std::fs::File::open(source).map_err(|e| AuditError::Package {
        path: source.to_path_buf(),
        source: e,
    })?;
    let bytes = input.metadata().map(|m| m.len()).unwrap_or(0);
    start(zip, entry, bytes, temporary)?;

    let needles = secret_needles(config, github_token);
    let mut scan = CredentialScan::over(&needles);
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut written = 0_u64;
    loop {
        let read = input.read(&mut buffer).map_err(|e| AuditError::Package {
            path: source.to_path_buf(),
            source: e,
        })?;
        if read == 0 {
            break;
        }
        if scan.feed(&buffer[..read]) {
            return Err(AuditError::CredentialInPackage {
                path: source.to_path_buf(),
            });
        }
        zip.write_all(&buffer[..read])
            .map_err(|source| AuditError::Package {
                path: temporary.to_path_buf(),
                source,
            })?;
        written += read as u64;
    }
    Ok(written)
}

/// A multi-needle substring search across a stream, without holding the stream
/// in memory.
///
/// Why: the credential check has to cover an extract database that can run to
/// hundreds of megabytes, and a match that straddles two reads is exactly the
/// case a naive per-chunk search misses. Keeping the last `len - 1` bytes as the
/// next window's prefix is what closes that gap.
///
/// The carried tail is sized to the LONGEST needle (#5857): one buffer serves
/// every needle, and a tail cut to the shortest would let a longer secret
/// straddle two reads undetected — the precise failure a per-needle tail exists
/// to prevent. One shared window also means one copy per chunk rather than one
/// per needle per chunk, which matters at extract-database size.
/// Test: `credential_scan_tests::a_credential_split_across_two_reads_is_caught`.
struct CredentialScan<'a> {
    /// Every secret to refuse. Empty needles are dropped at construction — an
    /// unset credential must not match every file.
    needles: Vec<&'a [u8]>,
    /// Bytes to carry into the next window: `longest needle - 1`.
    keep: usize,
    tail: Vec<u8>,
}

impl<'a> CredentialScan<'a> {
    fn over(needles: &[&'a [u8]]) -> Self {
        let needles: Vec<&'a [u8]> = needles.iter().copied().filter(|n| !n.is_empty()).collect();
        let keep = needles.iter().map(|n| n.len()).max().unwrap_or(1) - 1;
        Self {
            needles,
            keep,
            tail: Vec::new(),
        }
    }

    /// Whether any needle appears in the stream up to and including `chunk`.
    fn feed(&mut self, chunk: &[u8]) -> bool {
        if self.needles.is_empty() {
            return false;
        }
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        let found = self.needles.iter().any(|needle| {
            window.len() >= needle.len()
                && window
                    .windows(needle.len())
                    .any(|candidate| candidate == *needle)
        });
        let keep = self.keep.min(window.len());
        self.tail = window[window.len() - keep..].to_vec();
        found
    }
}

#[cfg(test)]
mod credential_scan_tests {
    use super::*;

    /// A needle straddling two reads is the case the chunked scan exists for,
    /// and with several needles the tail has to be sized to the LONGEST — a tail
    /// cut to the shortest would let the longest one straddle undetected.
    #[test]
    fn a_credential_split_across_two_reads_is_caught() {
        let mut scan = CredentialScan::over(&[b"sk-or-v1-secret".as_slice()]);
        assert!(!scan.feed(b"noise sk-or-v1-"));
        assert!(scan.feed(b"secret more noise"));

        // The LONGEST needle straddles while a shorter one is also registered —
        // a tail sized to the shortest would miss this.
        let mut several =
            CredentialScan::over(&[b"lin_api".as_slice(), b"jira-token-that-is-long".as_slice()]);
        assert!(!several.feed(b"noise jira-token-"));
        assert!(several.feed(b"that-is-long more noise"));

        // And an empty key never matches everything.
        let mut blank = CredentialScan::over(&[b"".as_slice()]);
        assert!(!blank.feed(b"anything at all"));
        let mut nothing = CredentialScan::over(&[]);
        assert!(!nothing.feed(b"anything at all"));
    }
}
