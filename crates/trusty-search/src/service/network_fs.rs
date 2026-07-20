//! Detect whether a path lives on a network-mounted filesystem (issue #3408).
//!
//! Why: `notify`'s inotify (Linux) / FSEvents (macOS) backends are local-host
//! kernel notification mechanisms — they cannot observe writes made by a
//! *different* host onto a shared network mount (EFS/NFS/SMB/CIFS). This is an
//! OS-level limitation, not a bug in `notify` or in this crate's watcher: no
//! amount of retrying or reconfiguring the debouncer fixes it. Today the
//! watcher starts successfully on such a mount, the daemon reports healthy,
//! and the watcher simply never fires for cross-host changes — a silent no-op
//! that looks like correct behaviour. This module lets the watcher manager
//! detect that condition *before* spawning the watcher so it can degrade
//! loudly (see `service::watcher_manager`) instead of lying about liveness.
//!
//! What: [`classify_root`] resolves the filesystem type at a path via
//! `statfs` — on macOS through `f_fstypename` (via the already-vendored `nix`
//! crate's `filesystem_type_name`), on Linux through the `f_type` magic number
//! plus a `/proc/mounts` fallback — and classifies it as [`MountKind::Local`]
//! or [`MountKind::Network`].
//!
//! Detection is deliberately conservative (a false positive — refusing to
//! watch a local disk — is worse than a false negative): a filesystem is only
//! ever classified as `Network` when it matches a SPECIFIC, unambiguous
//! network filesystem identifier (`nfs`, `nfs4`, `cifs`, `smbfs`/`smb3`, the
//! NFS/CIFS/legacy-SMB `statfs` magic numbers, or an explicitly-named network
//! FUSE backend like `fuse.sshfs`/`fuse.s3fs`). A bare/generic `fuse` mount
//! (which covers many purely local use cases — encfs, unionfs, etc.) is never
//! classified as network on its own. Any error resolving the filesystem type
//! (path missing, permission denied, unsupported platform) fails open to
//! `Local` — we only degrade the watcher when we are positively confident the
//! root is network-backed.
//!
//! Test: [`classify_fstype_name`] and [`classify_magic`] are pure functions
//! taking the raw OS-reported type directly, so the matching logic is fully
//! unit-tested without requiring a real NFS/CIFS/SMB mount in CI. The
//! platform-specific `statfs`/`/proc/mounts` glue is exercised indirectly via
//! [`classify_root`] against real local paths (which must always resolve to
//! `Local` in a CI sandbox).

use std::path::Path;

/// Whether a path resolves to a local or network-backed filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// A local (or otherwise not positively-identified-as-network) filesystem.
    /// This is also the fail-open outcome on any detection error.
    Local,
    /// Positively identified as a network-backed mount (NFS/EFS/SMB/CIFS/a
    /// named network FUSE backend).
    Network,
}

impl MountKind {
    /// True when the mount was positively identified as network-backed.
    pub fn is_network(self) -> bool {
        matches!(self, MountKind::Network)
    }
}

/// Known-network filesystem type names, matched case-insensitively against
/// whatever the OS reports (macOS `f_fstypename`; Linux `/proc/mounts` field
/// 3). Deliberately a narrow, explicit allowlist rather than a denylist or a
/// substring match — see the module docs for the false-positive rationale.
///
/// `nfs`/`nfs4` covers plain NFS and AWS EFS (EFS mounts as NFSv4 on the
/// client). `cifs`/`smbfs`/`smb3` cover Windows-file-share mounts. `afpfs`
/// covers the legacy Apple Filing Protocol. The `fuse.*` entries are
/// EXPLICITLY named network FUSE backends — a bare `fuse` type is never
/// matched (see module docs).
const NETWORK_FSTYPE_NAMES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smbfs",
    "smb3",
    "afpfs",
    "webdav",
    "fuse.sshfs",
    "fuse.s3fs",
    "fuse.gcsfuse",
    "fuse.rclone",
];

/// Classify a raw filesystem type NAME (as reported by the OS) as local or
/// network. Pure and case-insensitive so it is directly unit-testable without
/// mounting anything.
///
/// Test: `classify_fstype_name_matches_known_network_types`,
/// `classify_fstype_name_is_case_insensitive`,
/// `classify_fstype_name_does_not_flag_generic_fuse`.
pub fn classify_fstype_name(name: &str) -> MountKind {
    let lower = name.trim().to_ascii_lowercase();
    if NETWORK_FSTYPE_NAMES.iter().any(|known| *known == lower) {
        MountKind::Network
    } else {
        MountKind::Local
    }
}

/// NFS magic number (`statfs.f_type` on Linux) — covers NFSv3/NFSv4 and thus
/// AWS EFS, which mounts as NFSv4 on the client.
const NFS_SUPER_MAGIC: i64 = 0x6969;
/// Legacy `smbfs` magic number.
const SMB_SUPER_MAGIC: i64 = 0x517b;
/// CIFS magic number (kernel `fs/cifs/cifsfs.h` `CIFS_MAGIC_NUMBER`). Not
/// exposed as a named constant by `libc`/`nix`, so declared directly here.
const CIFS_MAGIC_NUMBER: i64 = 0xFF534D42u32 as i64;

/// Classify a raw Linux `statfs.f_type` magic number as local or network.
/// Pure so it is directly unit-testable without a real mount.
///
/// Test: `classify_magic_matches_known_network_magics`,
/// `classify_magic_does_not_flag_local_magics`.
pub fn classify_magic(magic: i64) -> MountKind {
    match magic {
        NFS_SUPER_MAGIC | SMB_SUPER_MAGIC | CIFS_MAGIC_NUMBER => MountKind::Network,
        _ => MountKind::Local,
    }
}

/// Determine whether `path` lives on a network-mounted filesystem.
///
/// Why: called by `service::watcher_manager` before spawning a watcher
/// (issue #3408) so a network-mounted index root degrades loudly instead of
/// silently starting a watcher that will never fire for cross-host writes.
/// What: platform-specific `statfs` glue that resolves the OS-reported
/// filesystem identifier and delegates the actual classification decision to
/// the pure [`classify_fstype_name`] / [`classify_magic`] helpers above. Any
/// error (`statfs` failing, unsupported platform) fails open to
/// [`MountKind::Local`] — see module docs on the false-positive trade-off.
/// Test: exercised against real local paths (must resolve `Local`) by
/// `classify_root_local_tempdir_is_local` below; the network path is covered
/// by the pure helpers since CI has no NFS/CIFS mount available.
pub fn classify_root(path: &Path) -> MountKind {
    imp::classify_root(path)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{classify_fstype_name, MountKind};
    use std::path::Path;

    pub(super) fn classify_root(path: &Path) -> MountKind {
        match nix::sys::statfs::statfs(path) {
            Ok(stat) => classify_fstype_name(stat.filesystem_type_name()),
            Err(err) => {
                tracing::debug!(
                    ?path,
                    %err,
                    "network_fs: statfs failed — treating as local (fail-open, issue #3408)"
                );
                MountKind::Local
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{classify_fstype_name, classify_magic, MountKind};
    use std::path::Path;

    pub(super) fn classify_root(path: &Path) -> MountKind {
        // Primary signal: the statfs magic number — a single syscall, no
        // /proc parsing, and unambiguous for NFS/legacy-SMB/CIFS.
        if let Ok(stat) = nix::sys::statfs::statfs(path) {
            if classify_magic(stat.filesystem_type().0 as i64).is_network() {
                return MountKind::Network;
            }
        } else {
            tracing::debug!(
                ?path,
                "network_fs: statfs failed — falling back to /proc/mounts (issue #3408)"
            );
        }

        // Secondary signal: /proc/mounts fstype string. Needed for
        // explicitly-named network FUSE backends (`fuse.sshfs`, `fuse.s3fs`,
        // …), which all share the same generic FUSE magic number at the
        // statfs level and so cannot be distinguished from a purely local
        // FUSE mount by magic number alone.
        match mount_fstype_for_path(path) {
            Some(fstype) => classify_fstype_name(&fstype),
            None => MountKind::Local,
        }
    }

    /// Resolve the filesystem type of the longest-matching mount point for
    /// `path` by scanning `/proc/mounts`. Returns `None` on any read failure
    /// or if no mount point matches (fail-open).
    fn mount_fstype_for_path(path: &Path) -> Option<String> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let contents = std::fs::read_to_string("/proc/mounts").ok()?;
        let mut best: Option<(usize, String)> = None;
        for line in contents.lines() {
            let mut fields = line.split_whitespace();
            let Some(_device) = fields.next() else {
                continue;
            };
            let Some(mount_point) = fields.next() else {
                continue;
            };
            let Some(fstype) = fields.next() else {
                continue;
            };
            // Longest-prefix match: the deepest mount point that contains
            // `path` wins (mirrors how the kernel resolves nested mounts).
            if canonical.starts_with(mount_point) {
                let len = mount_point.len();
                if best.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
                    best = Some((len, fstype.to_string()));
                }
            }
        }
        best.map(|(_, fstype)| fstype)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::MountKind;
    use std::path::Path;

    /// No detection support on this platform — fail open to `Local`.
    pub(super) fn classify_root(_path: &Path) -> MountKind {
        MountKind::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure name-matching tests (no mount required) ────────────────────────

    #[test]
    fn classify_fstype_name_matches_known_network_types() {
        for name in [
            "nfs",
            "nfs4",
            "cifs",
            "smbfs",
            "smb3",
            "afpfs",
            "webdav",
            "fuse.sshfs",
            "fuse.s3fs",
        ] {
            assert_eq!(
                classify_fstype_name(name),
                MountKind::Network,
                "{name:?} should classify as Network"
            );
        }
    }

    #[test]
    fn classify_fstype_name_is_case_insensitive() {
        assert_eq!(classify_fstype_name("NFS"), MountKind::Network);
        assert_eq!(classify_fstype_name("CIFS"), MountKind::Network);
        assert_eq!(classify_fstype_name("Fuse.SSHFS"), MountKind::Network);
    }

    #[test]
    fn classify_fstype_name_leaves_local_types_alone() {
        for name in ["apfs", "ext4", "xfs", "btrfs", "tmpfs", "overlay", "hfs"] {
            assert_eq!(
                classify_fstype_name(name),
                MountKind::Local,
                "{name:?} should classify as Local"
            );
        }
    }

    /// Why: a bare/generic `fuse` mount covers many purely local use cases
    /// (encfs, unionfs, various sync tools); only explicitly-named network
    /// FUSE backends should be flagged — see module docs.
    #[test]
    fn classify_fstype_name_does_not_flag_generic_fuse() {
        assert_eq!(classify_fstype_name("fuse"), MountKind::Local);
        assert_eq!(classify_fstype_name("fuseblk"), MountKind::Local);
    }

    #[test]
    fn classify_magic_matches_known_network_magics() {
        assert_eq!(classify_magic(0x6969), MountKind::Network); // NFS_SUPER_MAGIC
        assert_eq!(classify_magic(0x517b), MountKind::Network); // SMB_SUPER_MAGIC
        assert_eq!(
            classify_magic(0xFF534D42u32 as i64),
            MountKind::Network // CIFS_MAGIC_NUMBER
        );
    }

    #[test]
    fn classify_magic_does_not_flag_local_magics() {
        assert_eq!(classify_magic(0xEF53), MountKind::Local); // ext4
        assert_eq!(classify_magic(0x9123683e), MountKind::Local); // btrfs
        assert_eq!(classify_magic(0), MountKind::Local);
    }

    // ── Real-filesystem smoke test (must never false-positive in CI) ───────

    /// Why: the single most important false-positive guard — a plain tempdir
    /// on the CI sandbox's local disk must never be classified as network.
    /// Exercises the real platform `statfs` glue (not just the pure helpers).
    #[test]
    fn classify_root_local_tempdir_is_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            classify_root(dir.path()),
            MountKind::Local,
            "a local tempdir must never be classified as a network mount"
        );
    }

    /// Why: a nonexistent path must fail open rather than panic or crash the
    /// caller (`statfs` returns an error; `/proc/mounts` lookup finds no
    /// canonicalizable path).
    #[test]
    fn classify_root_missing_path_fails_open_to_local() {
        let missing = std::path::Path::new("/definitely/does/not/exist/3408");
        assert_eq!(classify_root(missing), MountKind::Local);
    }
}
