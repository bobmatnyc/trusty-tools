//! Chrome native-messaging registration (`install_host.py`).
//!
//! Why: Chrome finds a native host only through a manifest in its per-user
//! `NativeMessagingHosts` directory; the manifest names a launcher in the
//! state directory, and the broker's enrollment pin expects that launcher to
//! exec this binary exactly ([`crate::peer_darwin::host_argv_ok`]).
//! What: [`registration_files`] lists the two launchers (0700) and the two
//! manifests (0600) with Python's names, JSON layout and `shlex.join`
//! quoting; only the `exec` line differs (`<exe> native-host --socket <sock>`).
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
        let content = launcher(&[&exe, HOST_VERB, "--socket", &sock]);
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
