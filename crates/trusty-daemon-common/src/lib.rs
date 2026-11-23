//! Shared plumbing for the trusty-browser and trusty-computer broker daemons.
//!
//! Why: trusty-computer began as a copy of trusty-browser's peer check,
//! owner-only file helpers, Developer ID signing, launchd plist writer and
//! audit-log rotation (issue #11692). One copy keeps a security fix in one place.
//! What: [`private`] (0700/0600 checks), [`peer`] (the kernel peer uid),
//! [`codesign`] (sign and verify against a per-product [`codesign::SignSpec`]),
//! [`launchd`] (plist, `launchctl` seam, owned-file writes) and [`audit_file`]
//! (append one private line, rotate at a size limit).
//! Test: the unit tests in each module, plus `tests/install.rs` and the audit
//! tests of both consumer crates.

pub mod audit_file;
pub mod codesign;
pub mod launchd;
pub mod peer;
pub mod private;
