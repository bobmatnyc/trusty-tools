//! Telling a sibling instance from this one by an argv path flag (#11085).
//!
//! Why: a daemon started as `start --foreground --data-dir X` without the
//! data-dir variable in its environment carries every liveness argument of the
//! default instance, and its environment reads "unset" exactly as the default
//! instance's does. The `/proc` scan read it as the default instance, so
//! `stop` of the default daemon refused with `RunningUnserved`, and `restart`
//! of a healthy default daemon stopped it and then could not start it.
//! What: [`path_verdict`] reads the values of each
//! [`DaemonDescriptor::instance_path_args`] flag in a process's argv and
//! answers in three states. A process is a sibling ([`PathVerdict::Other`])
//! only when it carries the flag and every value it gives names a path other
//! than the instance's. A case it cannot decide — a flag with no value, a
//! relative value, a path that cannot be resolved — is
//! [`PathVerdict::Undecided`]. The `/proc` scan counts undecided as this
//! instance, so it refuses rather than reading a live daemon as absent; a stop
//! by pid refuses too, because nothing proved the pid is this instance.
//! Test: `path_flag_marks_a_sibling_only_when_every_value_names_another_path`,
//! `path_flag_counts_every_undecidable_value_as_undecided`,
//! `stop_tells_a_sibling_from_this_instance_by_its_path_flag`,
//! `stop_pid_and_restart_pid_refuse_an_undecided_instance_before_any_signal`.
//!
//! [`DaemonDescriptor::instance_path_args`]: crate::linux_lifecycle::DaemonDescriptor::instance_path_args

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// Where a process's argv path flags place it against this instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathVerdict<'a> {
    /// No declared flag rules it out: each is absent or names the instance's path.
    Same,
    /// A flag gives only values naming another path: a sibling.
    Other,
    /// This flag has no value, or values that do not decide.
    Undecided(&'a str),
}

/// Where `argv[1..]` of a NUL-separated `/proc/<pid>/cmdline` places the
/// process against `wanted`'s flags and paths.
///
/// What: a flag is read as `<flag> <value>` or `<flag>=<value>`, up to a `--`
/// argument. Per flag: absent, or every value naming the path, is `Same`;
/// every value naming another path is `Other`; a flag with no value after it,
/// or any other mix ([`names_path`] `None` on a value, or both answers), is
/// `Undecided`. Across flags, any `Other` wins, then the first `Undecided`.
/// An empty `wanted` is `Same`, which keeps descriptors that do not opt in
/// unchanged.
/// Test: `path_flag_marks_a_sibling_only_when_every_value_names_another_path`,
/// `path_flag_counts_every_undecidable_value_as_undecided`.
pub(crate) fn path_verdict<'a>(cmdline: &[u8], wanted: &'a [(String, PathBuf)]) -> PathVerdict<'a> {
    // The body ends in NUL; without the terminator a trailing flag has no value.
    let body = cmdline.strip_suffix(b"\0").unwrap_or(cmdline);
    let args: Vec<&[u8]> = body.split(|b| *b == 0).skip(1).collect();
    let mut verdict = PathVerdict::Same;
    for (flag, path) in wanted {
        let answers: Option<Vec<Option<bool>>> = flag_values(&args, flag.as_bytes())
            .map(|values| values.iter().map(|v| names_path(v, path)).collect());
        // #11085: only a proven "same" or "other" decides; anything else is undecided.
        let this_flag = match answers.as_deref() {
            Some(a) if a.iter().all(|v| *v == Some(true)) => PathVerdict::Same,
            Some(a) if a.iter().all(|v| *v == Some(false)) => PathVerdict::Other,
            _ => PathVerdict::Undecided(flag),
        };
        match this_flag {
            PathVerdict::Other => return PathVerdict::Other,
            PathVerdict::Undecided(_) if verdict == PathVerdict::Same => verdict = this_flag,
            _ => {}
        }
    }
    verdict
}

/// Every value `flag` is given in `args`; `None` when an occurrence has none.
fn flag_values<'a>(args: &[&'a [u8]], flag: &[u8]) -> Option<Vec<&'a [u8]>> {
    let mut values = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if *arg == b"--" {
            break;
        }
        if *arg == flag {
            values.push(*rest.next()?);
        } else if let Some(value) = arg.strip_prefix(flag).and_then(|r| r.strip_prefix(b"=")) {
            values.push(value);
        }
    }
    Some(values)
}

/// Whether the argv value `found` names the directory `wanted`: `Some(true)`
/// for the same path, `Some(false)` for another, `None` when undecidable.
///
/// What: `Some(true)` when `found` is absolute and equals `wanted` by
/// components, or both resolve through `canonicalize` to one path.
/// `Some(false)` when `found` is absolute, resolves, and either resolves to a
/// path other than `wanted`'s or `wanted` does not exist. A relative `found`
/// (it resolves against the process's own cwd), an unresolvable `found`, and
/// any error on `wanted` other than "not found" are `None`.
/// Test: `path_flag_counts_every_undecidable_value_as_undecided`.
pub(crate) fn names_path(found: &[u8], wanted: &Path) -> Option<bool> {
    let found = Path::new(OsStr::from_bytes(found));
    if !found.is_absolute() {
        return None;
    }
    if found.components().eq(wanted.components()) {
        return Some(true);
    }
    // #11085: only a resolved difference rules a value out; a read error never does.
    let found = std::fs::canonicalize(found).ok()?;
    match std::fs::canonicalize(wanted) {
        Ok(wanted) => Some(found == wanted),
        // An existing directory is not one that does not exist.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}
