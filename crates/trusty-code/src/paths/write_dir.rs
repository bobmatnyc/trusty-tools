//! Pinned, symlink-refusing write handles beneath `<project>/.trusty-code/`
//! (#7779).
//!
//! Why: [`check_native_write_target`] answers "is this a legal write target?"
//! against the filesystem as it looked when it was called. Every caller then
//! wrote through the same path STRING, so a directory swapped for a symlink at
//! any component between the two was followed and the write landed outside the
//! project — measured at 6 of 40 real `tcode` deploys against a 4 ms
//! `skill-refs -> victim` swap loop (#7779, PR #7773 review). A check a rename
//! can overtake is not a boundary.
//!
//! What: [`NativeWriteDir`] opens the target directory ONCE, descending from the
//! canonical project root one component at a time with `O_NOFOLLOW`, and keeps
//! the resulting directory descriptor. Every later read, write, rename and lock
//! is `openat`-relative to that descriptor, so the kernel resolves it against
//! the pinned inode instead of re-walking the path — a symlink swapped in
//! afterwards cannot redirect it. Each write then re-checks the handle's
//! identity against the path ([`NativeWriteDir::verify_pinned`]) so a swap
//! surfaces as a typed [`WriteTargetError::Unpinned`] instead of a deploy that
//! silently wrote into a directory nothing will ever read.
//!
//! The write-boundary contract itself is unchanged: ADR-0044 restricts writes to
//! the product's own configuration directory, and [`super::anchor_native_root`]
//! still decides membership. It decides it ONCE, and the path it returns is the
//! path pinned — a writer never re-resolves `.trusty-code`, so there is no
//! second answer for a rename to substitute (#7779 round 2).
//!
//! ## Symlinks beneath the config root are refused, never followed
//!
//! Every entry this handle touches is opened `O_NOFOLLOW`, READS included. A
//! symlinked `agents/pm.md`, `skill-refs/<skill>/SKILL.md` or ledger is a typed
//! [`WriteTargetError::Unpinned`] that fails the whole deploy, where the
//! pre-#7779 code followed it and treated the target's bytes as the deployed
//! file's. This is deliberate, and it narrows `deploy`'s "a hand-edited deployed
//! file is authoritative" policy to REAL files: a link's bytes cannot be bounded
//! by the handle that is supposed to bound them, and publishing over it with
//! `renameat` would silently replace the link rather than write where it points
//! — so the two would disagree about what the deploy just did. Point the link
//! the other way (keep the real file under `.trusty-code/` and link to it from
//! elsewhere) or drop it. PR #7773's review asked for this on the skill-refs
//! tree; it holds uniformly rather than per-directory.
//!
//! Unix only, like the rest of this crate's daemon transport — `openat`,
//! `renameat` and `O_NOFOLLOW` are what close the window and have no portable
//! equivalent.
//!
//! Test: `paths::write_dir_tests::*`.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use super::{
    AnchoredRoot, TRUSTY_CODE_DIRNAME, WriteTargetError, anchor_native_root, native_config_dir,
};

/// A directory beneath `<project>/.trusty-code/`, pinned to an open descriptor.
///
/// Why: see the module docs — the handle IS the guarantee. Holding it is what
/// makes "the path validated is the path written" true rather than aspirational.
/// What: an owned directory file descriptor plus the logical path it was opened
/// as (for diagnostics only; nothing resolves through it again). Constructing
/// one creates every missing component with `mkdirat`, so callers no longer need
/// `create_dir_all` — the call this race was won against.
/// Test: `paths::write_dir_tests::pinned_write_lands_at_the_validated_path`,
/// `paths::write_dir_tests::component_swapped_after_open_never_reaches_victim`.
#[derive(Debug)]
pub struct NativeWriteDir {
    fd: OwnedFd,
    path: PathBuf,
}

impl NativeWriteDir {
    /// Pin `<project>/.trusty-code/<relative>`, creating it if absent.
    ///
    /// Why: every trusty-code write target is reached through this one
    /// constructor so the boundary check and the descent cannot drift apart.
    /// What, in order:
    /// 1. [`anchor_native_root`] — the unchanged ADR-0044 membership rule (a
    ///    cross-product or escaping target keeps its existing error), which
    ///    hands back the ONE canonical `.trusty-code` it approved.
    /// 2. That exact path — not a fresh resolution of it — is pinned by
    ///    descending from `/` with `O_NOFOLLOW` at every component. Resolving
    ///    once is what keeps a `.trusty-code` symlink that stays inside the
    ///    project working while leaving nothing for a swap to redirect: a
    ///    `.trusty-code` replaced after step 1 cannot move the descent, because
    ///    the descent never reads that name again (#7779 round 2).
    /// 3. Each component of `relative` is created with `mkdirat` and opened with
    ///    `O_NOFOLLOW`, so a symlink at any of them fails CLOSED with
    ///    [`WriteTargetError::Unpinned`] rather than being followed.
    ///
    /// An absent `.trusty-code` is created under the canonical project root; a
    /// project root that cannot be canonicalised is refused, never assumed safe.
    /// Test: `paths::write_dir_tests::pinned_write_lands_at_the_validated_path`,
    /// `paths::write_dir_tests::symlinked_component_is_refused_at_open`,
    /// `paths::write_dir_tests::symlinked_native_root_escape_is_refused_at_open`,
    /// `paths::write_dir_tests::native_root_swapped_between_check_and_pin_never_reaches_victim`.
    pub fn open(project_root: &Path, relative: &Path) -> Result<Self, WriteTargetError> {
        let path = native_config_dir(project_root).join(relative);
        let mut fd = pin_native_root(project_root, &path)?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(WriteTargetError::Unpinnable {
                    path: path.clone(),
                    detail: format!(
                        "`{}` is not a plain relative path under the config root",
                        relative.display()
                    ),
                });
            };
            fd = create_dir_at(fd.as_fd(), name, &path)?;
        }
        Ok(Self { fd, path })
    }

    /// The path this handle was validated as, for diagnostics and logging.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pin a subdirectory, creating it if absent.
    ///
    /// Why: the skill-refs tree is two levels deep (`<skill>/SKILL.md`), and
    /// re-deriving the whole descent per file would reopen the window this type
    /// closes.
    /// What: [`NativeWriteDir::open`]'s component loop, rooted at this handle
    /// rather than at the project. A parent detached by a swap reports
    /// [`WriteTargetError::Unpinned`], not the bare `ENOENT` `mkdirat` raises for
    /// it.
    /// Test: `paths::write_dir_tests::child_dir_is_pinned_like_its_parent`,
    /// `agents::deploy::deploy_tests::swapped_skill_refs_dir_never_reaches_the_victim`.
    pub fn child(&self, relative: &Path) -> Result<Self, WriteTargetError> {
        let path = self.path.join(relative);
        let mut fd = self.fd.try_clone().map_err(|e| unpinnable(&path, &e))?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(WriteTargetError::Unpinnable {
                    path,
                    detail: format!("`{}` is not a plain relative path", relative.display()),
                });
            };
            fd = match create_dir_at(fd.as_fd(), name, &path) {
                Ok(fd) => fd,
                Err(e) => {
                    // #7779: say "swapped", not "missing", when this handle is
                    // the thing that went away.
                    self.verify_pinned()?;
                    return Err(e);
                }
            };
        }
        Ok(Self { fd, path })
    }

    /// Read one file in this directory; `Ok(None)` when it does not exist.
    ///
    /// Why: the deploy decides "already current" and "hand-edited" from these
    /// bytes, so reading them through the pinned handle is what keeps the
    /// decision and the write talking about the same directory.
    /// What: `openat` with `O_NOFOLLOW`. A SYMLINKED entry is
    /// [`WriteTargetError::Unpinned`], never silently read through — #7727's
    /// refusal of a committed symlink in the skill-refs tree depends on it.
    /// Test: `paths::write_dir_tests::pinned_write_lands_at_the_validated_path`,
    /// `agents::deploy::deploy_tests::symlinked_skill_ref_file_is_refused_before_any_write`.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, WriteTargetError> {
        let c = cstring(name, &self.path)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `self.fd` is a live directory descriptor for the whole call
        // and `c` is a NUL-terminated string that outlives the `openat`.
        let raw = unsafe { libc::openat(self.fd.as_raw_fd(), c.as_ptr(), flags) };
        if raw < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(open_error(&self.path, name, &err));
        }
        // SAFETY: `raw` is a fresh descriptor owned by this call alone.
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf).map_err(|e| unpinnable(&self.path, &e))?;
        Ok(Some(buf))
    }

    /// Atomically replace one file in this directory.
    ///
    /// Why: the write half of the boundary. Temp-then-rename keeps a concurrent
    /// reader from seeing a partial file, exactly as the shared deployer's
    /// `atomic_write` does; doing both `openat`-relative to the pinned handle is
    /// what keeps them inside the validated directory.
    /// What: `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` on a per-process, per-attempt
    /// scratch name, then `renameat` onto `name`. A failed write or rename
    /// removes the scratch file. [`NativeWriteDir::verify_pinned`] runs on both
    /// exits: the bytes can only ever have landed in the validated directory,
    /// but a swap means nothing will read them there, so it is reported rather
    /// than swallowed — and a handle detached by `rmdir` reports the swap rather
    /// than the bare `ENOENT` the kernel raises for it.
    /// Test: `paths::write_dir_tests::pinned_write_lands_at_the_validated_path`,
    /// `paths::write_dir_tests::component_swapped_after_open_never_reaches_victim`.
    pub fn atomic_write(&self, name: &str, content: &[u8]) -> Result<(), WriteTargetError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let scratch = format!("{name}.{}.{nanos}.tmp", std::process::id());

        let written = self
            .create_new_file(&scratch)
            .and_then(|mut f| f.write_all(content).map_err(|e| unpinnable(&self.path, &e)))
            .and_then(|()| self.rename(&scratch, name));
        if let Err(e) = written {
            self.unlink(&scratch);
            // #7779: a directory removed or renamed out from under the handle
            // surfaces as ENOENT; name the real failure instead.
            self.verify_pinned()?;
            return Err(e);
        }
        self.verify_pinned()
    }

    /// Create one file in this directory, refusing to replace an existing one.
    ///
    /// Why: the legacy import's "never overwrite a user-authored file" rule was
    /// an `exists()` test taken while planning, which the apply step could be
    /// arbitrarily long behind. `O_EXCL` makes the kernel decide it at the
    /// moment of the write.
    /// What: `openat(O_CREAT|O_EXCL|O_NOFOLLOW)`; an existing target yields
    /// [`WriteTargetError::Unpinnable`] carrying the `EEXIST` text, which the
    /// import reports as a refusal.
    /// Test: `paths::write_dir_tests::create_new_refuses_an_existing_file`.
    pub fn create_new(&self, name: &str, content: &[u8]) -> Result<(), WriteTargetError> {
        let written = self
            .create_new_file(name)
            .and_then(|mut f| f.write_all(content).map_err(|e| unpinnable(&self.path, &e)));
        if let Err(e) = written {
            // #7779: `EEXIST` is a legitimate refusal, not a swap — only a
            // detached handle escalates to `Unpinned`.
            self.verify_pinned()?;
            return Err(e);
        }
        self.verify_pinned()
    }

    /// Take the exclusive `flock(2)` on a sidecar in this directory.
    ///
    /// Why: the shared deployer serialises concurrent writers on
    /// `<dir>/.trusty-mpm-manifest.json.lock`. Deploying through a scratch
    /// directory would otherwise move that lock off the project directory and
    /// silently drop the cross-process guarantee.
    /// What: `openat(O_CREAT|O_RDWR|O_NOFOLLOW)` then a blocking `LOCK_EX`. The
    /// returned handle releases the lock when dropped.
    /// Test: `paths::write_dir_tests::ledger_lock_is_exclusive_across_handles`.
    pub fn lock_exclusive(&self, name: &str) -> Result<File, WriteTargetError> {
        let c = cstring(name, &self.path)?;
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `self.fd` is live for the call; `c` is NUL-terminated and
        // outlives it. The variadic mode argument is required by `O_CREAT`.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                flags,
                0o600 as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(open_error(
                &self.path,
                name,
                &std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: `raw` is a fresh descriptor owned by this call alone.
        let file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        // SAFETY: `file` owns `raw` for the duration of the `flock` call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(open_error(
                &self.path,
                name,
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(file)
    }

    /// Confirm the pinned directory is still the one `path()` names.
    ///
    /// Why: `openat` writes cannot be redirected, so a swap makes them land in a
    /// directory that is no longer reachable by name — correct, but useless. The
    /// caller has to hear about it; a deploy that "succeeded" into a detached
    /// inode is a failed deploy.
    /// What: compares the handle's `(dev, ino)` with the path's. Any mismatch,
    /// including a path that no longer resolves, is
    /// [`WriteTargetError::Unpinned`].
    ///
    /// This is a REPORT, never the boundary — it re-resolves `path` and so can
    /// only be trusted because [`NativeWriteDir::open`] guarantees the pinned
    /// side is a directory the membership rule approved. While that held only
    /// for the first of two resolutions, a swap could make both sides name the
    /// same victim and this function called it healthy (152 times in the
    /// code-critic race model, #7779 round 2). It cannot now: the pinned inode
    /// is inside the project, so a live swap makes the two differ and a reverted
    /// one makes them agree about a write that did land correctly.
    /// Test: `paths::write_dir_tests::component_swapped_after_open_never_reaches_victim`,
    /// `paths::write_dir_tests::native_root_swapped_between_check_and_pin_never_reaches_victim`.
    pub fn verify_pinned(&self) -> Result<(), WriteTargetError> {
        let unpinned = || WriteTargetError::Unpinned {
            path: self.path.clone(),
            component: self
                .path
                .file_name()
                .unwrap_or_else(|| OsStr::new(TRUSTY_CODE_DIRNAME))
                .to_string_lossy()
                .into_owned(),
        };
        let dup = self.fd.try_clone().map_err(|_| unpinned())?;
        let pinned = File::from(dup).metadata().map_err(|_| unpinned())?;
        let named = std::fs::metadata(&self.path).map_err(|_| unpinned())?;
        if pinned.dev() == named.dev() && pinned.ino() == named.ino() {
            Ok(())
        } else {
            Err(unpinned())
        }
    }

    /// `openat(O_CREAT|O_EXCL|O_NOFOLLOW)` — shared by both write entry points.
    fn create_new_file(&self, name: &str) -> Result<File, WriteTargetError> {
        let c = cstring(name, &self.path)?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `self.fd` is live for the call; `c` is NUL-terminated and
        // outlives it. The variadic mode argument is required by `O_CREAT`.
        let raw = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                flags,
                0o644 as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(open_error(
                &self.path,
                name,
                &std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: `raw` is a fresh descriptor owned by this call alone.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    /// `renameat` within this one pinned directory.
    fn rename(&self, from: &str, to: &str) -> Result<(), WriteTargetError> {
        let old = cstring(from, &self.path)?;
        let new = cstring(to, &self.path)?;
        // SAFETY: `self.fd` is live for the call and both strings outlive it.
        let rc = unsafe {
            libc::renameat(
                self.fd.as_raw_fd(),
                old.as_ptr(),
                self.fd.as_raw_fd(),
                new.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(open_error(&self.path, to, &std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Best-effort scratch-file removal on a failed write; never masks the
    /// original error.
    fn unlink(&self, name: &str) {
        let Ok(c) = CString::new(name) else { return };
        // SAFETY: `self.fd` is live for the call and `c` outlives it.
        unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) };
    }
}

/// Pin `<project>/.trusty-code`, creating it when absent.
///
/// Why: #7779 round 2. The membership rule and the descent used to resolve
/// `.trusty-code` independently, and the descriptor came from the SECOND
/// resolution — a swap landed between them in 1419 of 20138 completed writes in
/// a code-critic race model, 152 of which `verify_pinned` then called healthy.
/// The pinned directory is now the one [`anchor_native_root`] approved, by
/// construction: this function never resolves that name itself.
/// What: [`AnchoredRoot::Existing`]'s canonical path is descended from `/` with
/// `O_NOFOLLOW`; [`AnchoredRoot::Absent`] means the root is created under the
/// canonical project root, itself descended the same way. A project root that
/// cannot be canonicalised is refused.
///
/// What remains possible, precisely: a directory swapped in AT that approved
/// canonical path — a rename, not a symlink, since every component is opened
/// `O_NOFOLLOW`. Such a directory is still at a path the membership rule
/// authorises, which is the property ADR-0044 asks for. No path outside it can
/// be pinned.
/// Test: `paths::write_dir_tests::symlinked_native_root_escape_is_refused_at_open`,
/// `paths::write_dir_tests::native_root_swapped_between_check_and_pin_never_reaches_victim`.
fn pin_native_root(project_root: &Path, target: &Path) -> Result<OwnedFd, WriteTargetError> {
    match anchor_native_root(project_root, target)? {
        AnchoredRoot::Existing(canon) => pin_absolute(&canon, target),
        AnchoredRoot::Absent => {
            let project = project_root
                .canonicalize()
                .map_err(|e| unpinnable(target, &e))?;
            let root = pin_absolute(&project, target)?;
            create_dir_at(root.as_fd(), OsStr::new(TRUSTY_CODE_DIRNAME), target)
        }
    }
}

/// Descend an already-canonical absolute path, refusing every symlink.
///
/// Why: `canonicalize` proves the path held no symlink when it ran; walking it
/// again with `O_NOFOLLOW` proves none was introduced since.
/// What: opens `/`, then `openat(O_DIRECTORY|O_NOFOLLOW)` per component.
/// Test: `paths::write_dir_tests::pinned_write_lands_at_the_validated_path`.
fn pin_absolute(canonical: &Path, target: &Path) -> Result<OwnedFd, WriteTargetError> {
    let root = File::open(Path::new("/")).map_err(|e| unpinnable(target, &e))?;
    let mut fd = OwnedFd::from(root);
    for component in canonical.components() {
        if let Component::Normal(name) = component {
            fd = open_dir_at(fd.as_fd(), name, target)?;
        }
    }
    Ok(fd)
}

/// `mkdirat` then `openat(O_DIRECTORY|O_NOFOLLOW)`; an existing directory is
/// opened, an existing symlink is refused.
fn create_dir_at(
    parent: BorrowedFd<'_>,
    name: &OsStr,
    target: &Path,
) -> Result<OwnedFd, WriteTargetError> {
    let c = cstring_os(name, target)?;
    // SAFETY: `parent` is live for the call and `c` outlives it.
    let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o755) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(open_error(target, &name.to_string_lossy(), &err));
        }
    }
    open_dir_at(parent, name, target)
}

/// `openat(O_DIRECTORY|O_NOFOLLOW)` — the single place a symlinked component is
/// turned into [`WriteTargetError::Unpinned`].
fn open_dir_at(
    parent: BorrowedFd<'_>,
    name: &OsStr,
    target: &Path,
) -> Result<OwnedFd, WriteTargetError> {
    let c = cstring_os(name, target)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `parent` is live for the call and `c` outlives it.
    let raw = unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(), flags) };
    if raw < 0 {
        return Err(open_error(
            target,
            &name.to_string_lossy(),
            &std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: `raw` is a fresh descriptor owned by this call alone.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `ELOOP`/`ENOTDIR` means a link was swapped in; anything else is plain I/O.
///
/// Why: the two have different fixes — one is an attack or a misconfigured tree,
/// the other is a full disk or a permission problem — and collapsing them would
/// hide which.
/// Test: `paths::write_dir_tests::symlinked_component_is_refused_at_open`.
fn open_error(target: &Path, component: &str, err: &std::io::Error) -> WriteTargetError {
    let raw = err.raw_os_error();
    if raw == Some(libc::ELOOP) || raw == Some(libc::ENOTDIR) {
        return WriteTargetError::Unpinned {
            path: target.to_path_buf(),
            component: component.to_string(),
        };
    }
    WriteTargetError::Unpinnable {
        path: target.to_path_buf(),
        detail: format!("{component}: {err}"),
    }
}

fn unpinnable(target: &Path, err: &std::io::Error) -> WriteTargetError {
    WriteTargetError::Unpinnable {
        path: target.to_path_buf(),
        detail: err.to_string(),
    }
}

/// A NUL-free name that is exactly ONE path component.
///
/// Why: `O_NOFOLLOW` guards only the LAST component of an `openat` name, so a
/// `name` carrying a separator would have its leading components resolved with
/// symlinks followed — the very thing this module exists to stop. No caller
/// passes one today; [`NativeWriteDir`] is `pub`, so the type has to refuse it
/// rather than rely on that (#7779 round 2).
/// What: rejects an empty name, `.`, `..`, anything absolute, anything with a
/// separator, and anything with an interior NUL.
/// Test: `paths::write_dir_tests::a_name_with_a_path_separator_is_refused`.
fn cstring(name: &str, target: &Path) -> Result<CString, WriteTargetError> {
    let unusable = || WriteTargetError::Unpinnable {
        path: target.to_path_buf(),
        detail: format!("`{name}` is not a usable file name"),
    };
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(one)), None) if one == OsStr::new(name) => {}
        _ => return Err(unusable()),
    }
    CString::new(name).map_err(|_| unusable())
}

fn cstring_os(name: &OsStr, target: &Path) -> Result<CString, WriteTargetError> {
    CString::new(name.as_bytes()).map_err(|_| WriteTargetError::Unpinnable {
        path: target.to_path_buf(),
        detail: format!("`{}` is not a usable file name", name.to_string_lossy()),
    })
}

#[cfg(test)]
#[path = "write_dir_tests.rs"]
mod write_dir_tests;
