//! Telling a sibling instance from this one by an argv path flag (#11085).
//!
//! Why: a daemon started as `start --foreground --data-dir X` without the
//! data-dir variable in its environment carries every liveness argument of the
//! default instance, and its environment reads "unset" exactly as the default
//! instance's does. The `/proc` scan read it as the default instance, so
//! `stop` of the default daemon refused with `RunningUnserved`, and `restart`
//! of a healthy default daemon stopped it and then could not start it.
//! What: [`names_other_instance`] reads the values of each
//! [`DaemonDescriptor::instance_path_args`] flag in a process's argv. A process
//! is a sibling only when it carries the flag and every value it gives names a
//! path other than the instance's. Every case it cannot decide — a flag with
//! no value, a relative value, a path that cannot be resolved — counts as this
//! instance, so the scan refuses rather than reading a live daemon as absent.
//! Test: `path_flag_marks_a_sibling_only_when_every_value_names_another_path`,
//! `path_flag_counts_every_undecidable_value_as_this_instance`,
//! `stop_tells_a_sibling_from_this_instance_by_its_path_flag`.
//!
//! [`DaemonDescriptor::instance_path_args`]: crate::linux_lifecycle::DaemonDescriptor::instance_path_args

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

/// Whether `argv[1..]` of a NUL-separated `/proc/<pid>/cmdline` gives one of
/// `wanted`'s flags only values that name another path than the instance's.
///
/// What: a flag is read as `<flag> <value>` or `<flag>=<value>`, up to a `--`
/// argument. A flag with no value after it ends the decision as "not a
/// sibling"; so does any value [`may_name`] cannot rule out. An empty `wanted`
/// is never a sibling, which keeps descriptors that do not opt in unchanged.
/// Test: `path_flag_marks_a_sibling_only_when_every_value_names_another_path`.
pub(crate) fn names_other_instance(cmdline: &[u8], wanted: &[(String, PathBuf)]) -> bool {
    // The body ends in NUL; without the terminator a trailing flag has no value.
    let body = cmdline.strip_suffix(b"\0").unwrap_or(cmdline);
    let args: Vec<&[u8]> = body.split(|b| *b == 0).skip(1).collect();
    wanted.iter().any(|(flag, path)| {
        flag_values(&args, flag.as_bytes())
            .is_some_and(|values| !values.is_empty() && values.iter().all(|v| !may_name(v, path)))
    })
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

/// Whether the argv value `found` may name the directory `wanted`.
///
/// What: `false` only when `found` is absolute, differs from `wanted` by
/// components, resolves through `canonicalize`, and either resolves to a path
/// other than `wanted`'s or `wanted` does not exist. A relative `found` (it
/// resolves against the process's own cwd), an unresolvable `found`, and any
/// error on `wanted` other than "not found" are `true`.
/// Test: `path_flag_counts_every_undecidable_value_as_this_instance`.
pub(crate) fn may_name(found: &[u8], wanted: &Path) -> bool {
    let found = Path::new(OsStr::from_bytes(found));
    if !found.is_absolute() || found.components().eq(wanted.components()) {
        return true;
    }
    // #11085: only a resolved difference rules a value out; a read error never does.
    let Ok(found) = std::fs::canonicalize(found) else {
        return true;
    };
    match std::fs::canonicalize(wanted) {
        Ok(wanted) => found == wanted,
        // An existing directory is not one that does not exist.
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}
