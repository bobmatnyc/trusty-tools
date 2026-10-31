//! The bounded, fail-closed read of a script body for
//! `pm_guard_secret_script` (#8879), split out for the 500-SLOC cap.
//!
//! Why: a body the guard cannot read in full is a body it cannot clear, so
//! every class of read it cannot finish is an [`Unread`] refusal.
//! What: [`read_script`] and the types it answers with, plus the rule class
//! that refused a body ([`Rule`]) and the refusal text ([`deny_reason`]),
//! moved here for the same cap (#10548).
//! Test: `pm_guard_secret_script::tests::read_script_reports_each_unread_class`.

use std::path::Path;

/// Largest script body the guard reads; a larger one is not inspected.
pub(crate) const MAX_SCRIPT_BYTES: u64 = 256 * 1024;

/// Why a script body was not judged. Every class refuses: the guard fails
/// closed on a body it cannot read in full (#8879).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread {
    /// An open, stat or read error: permission denied, gone mid-check.
    Unreadable,
    /// Not a regular file: a directory, a FIFO, a device.
    NotRegular,
    /// A script run past the `MAX_SCRIPT_DEPTH` nesting bound: its body is
    /// not followed (#9037).
    TooDeep,
    /// Command substitutions nested past `MAX_SUBSTITUTION_DEPTH`: the
    /// innermost bodies are not read (#10566).
    SubstitutionTooDeep,
    /// Larger than [`MAX_SCRIPT_BYTES`].
    TooLarge,
    /// Not UTF-8 text, or carrying a NUL byte.
    NotText,
    /// A literal script that does not exist and that nothing else in the
    /// command names, so no earlier stage can have written it.
    Missing,
    /// An interpreter's script named through a variable, a substitution or a
    /// glob: no path to read.
    Unresolvable,
}

/// What [`read_script`] found at a path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Body {
    /// A script body, judged as text.
    Script(String),
    /// A compiled executable (ELF or Mach-O magic): not a script.
    Executable,
}

/// The first bytes of a compiled executable: ELF, then the Mach-O thin
/// (32/64-bit, both byte orders) and fat magics.
const EXECUTABLE_MAGIC: &[[u8; 4]] = &[
    [0x7f, b'E', b'L', b'F'],
    [0xfe, 0xed, 0xfa, 0xce],
    [0xfe, 0xed, 0xfa, 0xcf],
    [0xce, 0xfa, 0xed, 0xfe],
    [0xcf, 0xfa, 0xed, 0xfe],
    [0xca, 0xfe, 0xba, 0xbe],
    [0xbe, 0xba, 0xfe, 0xca],
];

/// Read a bounded prefix of a script body, failing closed.
///
/// Why: #8879 — a body the guard cannot read in full is a body it cannot
/// clear, so every unread class refuses instead of allowing.
/// What: opens the path `O_NONBLOCK` (a FIFO cannot block the hook) and
/// requires the OPENED handle to be a regular file; reads through
/// `take(MAX_SCRIPT_BYTES + 1)` so no size report is trusted. A compiled
/// executable is [`Body::Executable`]. A symlink is judged by the target the
/// open reached (#9037: `node_modules/.bin` and Homebrew shims); a symlink
/// loop fails the open and is [`Unread::Unreadable`]. An over-bound body and
/// non-UTF-8 or NUL-bearing bytes are each an [`Unread`] error. Nothing the
/// body names is opened here.
/// Test: `read_script_reports_each_unread_class`,
/// `every_unread_class_refuses_8879`, `a_symlinked_script_is_judged_by_its_target_9037`.
pub(crate) fn read_script(path: &Path) -> Result<Body, Unread> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Unread::Unreadable)?;
    let opened = file.metadata().map_err(|_| Unread::Unreadable)?;
    if !opened.file_type().is_file() {
        return Err(Unread::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unread::Unreadable)?;
    // #8879: a compiled program run by path is not a script; the kernel runs it.
    if EXECUTABLE_MAGIC.iter().any(|m| bytes.starts_with(m)) {
        return Ok(Body::Executable);
    }
    if bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return Err(Unread::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(Unread::NotText);
    }
    String::from_utf8(bytes)
        .map(Body::Script)
        .map_err(|_| Unread::NotText)
}

/// The rule class that refused a script body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    /// The credential-print rule (#8596, #8248).
    CredentialCommand,
    /// The secret-bearing file rule (#7266).
    SecretFile,
    /// A nested command or a pod/process environment dump (#7648, #8756).
    NestedOrDump,
    /// A credential-bearing launchd plist (#8523).
    LaunchdPlist,
    /// A body the guard could not read in full; it fails closed (#8879).
    Unread(Unread),
}

/// The refusal: the script word from the command, the rule class, and the
/// ruling-263 remedy. No byte of the body is quoted.
pub(crate) fn deny_reason(word: &str, inner: Option<&str>, rule: Rule) -> String {
    let what = match rule {
        Rule::CredentialCommand => {
            "its body, or a script it runs, runs a credential-reading command (a Keychain, \
             token or secret CLI; #8596, #8248)"
        }
        Rule::SecretFile => "its body, or a script it runs, names a secret-bearing file (#7266)",
        Rule::NestedOrDump => {
            "its body, or a script it runs, reads a credential through a nested command or an \
             environment dump (#8756)"
        }
        Rule::LaunchdPlist => {
            "its body, or a script it runs, reads a credential-bearing launchd plist (#8523)"
        }
        // #8879: every unread class fails closed and says which it was.
        Rule::Unread(why) => match why {
            Unread::Unreadable => "the guard could not read its body, so it fails closed",
            Unread::NotRegular => "it is not a regular file, so the guard fails closed",
            Unread::TooDeep => {
                "it runs scripts nested past the guard's depth bound, so the guard fails closed"
            }
            Unread::SubstitutionTooDeep => {
                "it nests command substitutions past the guard's depth bound, so the guard \
                 fails closed"
            }
            Unread::TooLarge => {
                "its body is over the guard's 256 KiB read bound, so the guard fails closed"
            }
            Unread::NotText => "its body is not UTF-8 text, so the guard fails closed",
            Unread::Missing => {
                "it does not exist and nothing else in the command names it, so the guard \
                 cannot judge its body and fails closed"
            }
            Unread::Unresolvable => {
                "its path is computed at run time (a variable, a substitution or a glob), so \
                 the guard cannot judge its body and fails closed"
            }
        },
    };
    // #10548: a nested refusal says which script it was about, not the outer one.
    let what = match (inner, what.strip_prefix("it ")) {
        (Some(inner), Some(rest)) => format!("it runs the script `{inner}`, which {rest}"),
        (Some(inner), None) => format!("it runs the script `{inner}`: {what}"),
        (None, _) => what.to_string(),
    };
    format!(
        "`tm hook --pm-guard` refused running the script `{word}` (#8879): {what}. A script \
         gets the same credential-read refusal as the command written inline (owner rulings \
         263 and 268), so moving a refused read into a script does not change the answer. \
         Stop and report this refusal to the Architect; do not retry the read in another form."
    )
}
