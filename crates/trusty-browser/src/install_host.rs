//! Chrome native-messaging registration (`install_host.py`).
//!
//! Why: Chrome finds a native host only through a manifest in its per-user
//! `NativeMessagingHosts` directory; the manifest names a launcher in the
//! state directory, and the broker's enrollment pin expects that launcher to
//! exec this binary exactly ([`crate::peer_darwin::host_argv_ok`]).
//! What: [`registration_files`] lists the two launchers (0700) and the two
//! manifests (0600) with Python's names, JSON layout and `shlex.join`
//! quoting. The launchers differ from Python's: [`host_launcher`] adds a guard
//! line that answers a missing binary with a framed error (#11927), then
//! `exec <exe> native-host --socket <sock> "$@"`.
//! [`write_owned`] publishes a file once and refuses to change a different one.
//! Test: `registration_files_match_python_layout`, `install_host_is_idempotent_and_refuses_a_foreign_file`
//! in `tests/install.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::peer_darwin::{HOST_VERB, ORIGIN};
use crate::pyjson::Json;

/// The paired-port host name; the enrollment host adds `.enroll`.
pub const NAME: &str = "com.trusty.browser";

/// One file the installer owns: path, exact content, exact mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Absolute path.
    pub path: PathBuf,
    /// Exact content.
    pub content: String,
    /// Exact permission bits.
    pub mode: u32,
}

/// `shlex.quote`.
pub fn shquote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if s.is_empty() {
        "''".to_owned()
    } else if s.chars().all(safe) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// `shlex.join`.
pub fn shjoin(args: &[&str]) -> String {
    args.iter()
        .map(|a| shquote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A launcher script: `#!/bin/sh`, then `exec <argv> "$@"`.
pub fn launcher(argv: &[&str]) -> String {
    format!("#!/bin/sh\nexec {} \"$@\"\n", shjoin(argv))
}

/// Reason code of the frame a launcher answers when its binary is missing.
pub const MISSING_REASON: &str = "host_binary_missing";

/// The one native-messaging frame a launcher writes when `exe` is gone.
///
/// Why: Chrome reports a missing binary only as "Native host has exited",
/// and popup Retry showed that, with no path and no repair (#11927).
/// What: `{"ok": false, "error": <message>, "reason": MISSING_REASON}`, the
/// message naming `exe` and [`crate::host_exe::INSTALL_HINT`]; the worker
/// relays `error` and `reason` to the popup status line.
/// Test: `retry_against_a_missing_host_binary_names_the_path` in `tests/install.rs`.
pub fn missing_binary_reply(exe: &str) -> String {
    let message = format!(
        "Trusty Browser cannot start: its binary {exe} is missing or not executable. {}",
        crate::host_exe::INSTALL_HINT
    );
    let body = Json::object([
        ("ok", Json::from(false)),
        ("error", Json::from(message)),
        ("reason", Json::from(MISSING_REASON)),
    ]);
    // Only strings and a bool: `dumps` cannot meet a non-finite float here.
    body.dumps(true).unwrap_or_default()
}

/// The host launcher: answer a missing `exe` with a framed error, else exec it.
///
/// Why: a popup Retry against a removed binary must name it (#11927).
/// What: `[ -x <exe> ] || { printf <len><reply>; exit 1; }` before Python's
/// `exec <exe> native-host --socket <sock> "$@"`; the length is the 4-byte
/// little-endian prefix as `printf` octal escapes, the reply
/// [`missing_binary_reply`]. `[` is a shell builtin, so Chrome stays the
/// binary's parent, which the broker's host check requires.
/// Test: `retry_against_a_missing_host_binary_names_the_path`,
/// `registration_files_match_python_layout` in `tests/install.rs`.
pub fn host_launcher(exe: &str, sock: &str) -> String {
    let reply = missing_binary_reply(exe);
    let len = u32::try_from(reply.len()).unwrap_or(u32::MAX).to_le_bytes();
    let prefix: String = len.iter().map(|b| format!("\\{b:03o}")).collect();
    format!(
        "#!/bin/sh\n[ -x {q} ] || {{ printf '{prefix}%s' {r}; exit 1; }}\nexec {exec} \"$@\"\n",
        q = shquote(exe),
        r = shquote(&reply),
        exec = shjoin(&[exe, HOST_VERB, "--socket", sock]),
    )
}

/// The socket a launcher at `path` relays to, by its file name.
pub fn launcher_socket(path: &Path) -> Option<PathBuf> {
    let state = path.parent()?;
    match path.file_name()?.to_str()? {
        "chrome-native-host" => Some(state.join("service.sock")),
        "chrome-native-host.enroll" => Some(state.join("enrollment.sock")),
        _ => None,
    }
}

/// The first word of a `shquote`d command line, unquoted.
pub fn first_word(line: &str) -> Option<String> {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (None, ' ') => break,
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => out.push(c),
        }
    }
    (quote.is_none() && !out.is_empty()).then_some(out)
}

/// Why: a permission error or an unmounted volume hides a binary that may
/// still be another install's, so only a definite absence is "gone" (#11927 review).
/// What: true only when `lstat(path)` fails with `NotFound`.
/// Test: `a_launcher_whose_binary_is_unreadable_is_not_gone`,
/// `install_service_refuses_our_plist_for_another_live_binary` in `tests/install.rs`.
pub fn binary_gone(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
}

/// The binary a launcher's `exec` line runs.
fn launcher_exe(text: &str) -> Option<String> {
    first_word(text.lines().last()?.strip_prefix("exec ")?)
}

/// Why: a re-install must replace an earlier launcher of ours, not refuse it
/// as foreign: the pre-#11927 form for this binary, and any form whose
/// binary is gone, which is the broken registration Retry reports (#11927).
/// What: true when `text` is exactly [`launcher`] or [`host_launcher`] for
/// the binary it names and `sock`, and that binary is `exe` (pre-#11927
/// form only) or is [`binary_gone`]. A launcher naming another binary that
/// exists, or cannot be checked, stays foreign.
/// Test: `install_replaces_a_launcher_whose_binary_is_gone` in `tests/install.rs`.
pub fn previous_launcher(text: &str, sock: &Path, exe: &Path) -> bool {
    let Some(old) = launcher_exe(text) else {
        return false;
    };
    let sock = sock.to_string_lossy();
    let gone = binary_gone(Path::new(&old));
    let v1 = text == launcher(&[&old, HOST_VERB, "--socket", &sock]);
    let v2 = text == host_launcher(&old, &sock);
    (v1 && (gone || Path::new(&old) == exe)) || (v2 && gone && Path::new(&old) != exe)
}

/// The manifest JSON for host `name` whose launcher is `launcher`.
pub fn manifest(name: &str, launcher: &Path) -> String {
    let body = Json::object([
        ("name", Json::from(name)),
        ("description", Json::from("Trusty Browser native host")),
        ("type", Json::from("stdio")),
        ("path", Json::from(launcher.to_string_lossy().as_ref())),
        ("allowed_origins", Json::Array(vec![Json::from(ORIGIN)])),
    ]);
    // Only strings: `dumps` cannot meet a non-finite float here.
    let text = body.dumps_indent2_ascii().unwrap_or_default();
    format!("{text}\n")
}

/// The four registration files, in Python's order (launcher, manifest; then `.enroll`).
///
/// `state` and `exe` must be absolute; `exe` is the binary Chrome will run.
pub fn registration_files(state: &Path, manifest_dir: &Path, exe: &Path) -> Vec<Registration> {
    let exe = exe.to_string_lossy();
    let mut out = Vec::new();
    for (suffix, socket) in [("", "service.sock"), (".enroll", "enrollment.sock")] {
        let path = state.join(format!("chrome-native-host{suffix}"));
        let sock = state.join(socket);
        let sock = sock.to_string_lossy();
        // #11927: the launcher names a missing binary instead of exiting silently.
        let content = host_launcher(&exe, &sock);
        out.push(Registration {
            path: path.clone(),
            content,
            mode: 0o700,
        });
        let name = format!("{NAME}{suffix}");
        out.push(Registration {
            path: manifest_dir.join(format!("{name}.json")),
            content: manifest(&name, &path),
            mode: 0o600,
        });
    }
    out
}

// #11692: the owned-file helpers are shared with trusty-computer.
pub(crate) use trusty_daemon_common::launchd::mkdir_0700;
pub use trusty_daemon_common::launchd::{replace_owned, same_file, write_owned};

/// `install_host.install`: write every registration file; return the paired manifest path.
pub fn install(state: &Path, manifest_dir: &Path, exe: &Path) -> io::Result<PathBuf> {
    mkdir_0700(state)?;
    if !crate::private::private_dir(&fs::symlink_metadata(state)?) {
        return Err(io::Error::other(
            "state must be a private directory owned by the current user",
        ));
    }
    mkdir_0700(manifest_dir)?;
    for r in registration_files(state, manifest_dir, exe) {
        // #11927: an earlier launcher of ours (old form, or a gone binary) is replaced.
        let previous = launcher_socket(&r.path).is_some_and(|sock| {
            let text = fs::read_to_string(&r.path).unwrap_or_default();
            same_file(&r.path, &text, r.mode) && previous_launcher(&text, &sock, exe)
        });
        if previous {
            replace_owned(&r.path, &r.content, r.mode)?;
        }
        write_owned(&r.path, &r.content, r.mode)?;
    }
    Ok(manifest_dir.join(format!("{NAME}.json")))
}

/// `install-host`: [`crate::codesign::ensure_install_signed`] on `exe` and its MCP sibling, then [`install`].
///
/// Test: `install_host_writes_nothing_when_signing_fails`,
/// `install_host_signs_the_mcp_sibling_and_refuses_a_missing_one` in `tests/install.rs`.
pub fn install_signed(
    state: &Path,
    manifest_dir: &Path,
    exe: &Path,
    signer: &crate::codesign::Signer<'_>,
) -> io::Result<PathBuf> {
    // #11440: a manifest must never name a binary whose signing failed.
    crate::codesign::ensure_install_signed(exe, signer)?;
    install(state, manifest_dir, exe)
}

/// `install_host.default_directory` under `home`.
pub fn default_directory(home: &Path) -> io::Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts"))
    } else if cfg!(target_os = "linux") {
        Ok(home.join(".config/google-chrome/NativeMessagingHosts"))
    } else {
        Err(io::Error::other("a Unix operating system is required"))
    }
}
