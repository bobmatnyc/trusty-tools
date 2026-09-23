//! `tm doctor` row for the launchd scheduling class of tm's tmux hosts (#8415).
//!
//! Why: a process launchd starts passes its `ProcessType` to every child. The
//! supervisor plist shipped `Background`, so each tmux server it auto-resumed,
//! and every PM session and `cargo` gate inside it, ran at Darwin priority 4 on
//! efficiency cores with throttled I/O. `taskpolicy -B` cannot lift that clamp
//! from user space. The installer template now says `Interactive`, but a plist
//! already on disk keeps its old value until something rewrites it: `tctl
//! install` does when it next replaces the supervisor, but `cargo install`
//! never touches a plist, and the `com.trusty.mpm` daemon plist has no
//! generator at all. This row is how such an install learns its plist is stale.
//!
//! What: [`check_launchd_process_type`] reads the `com.trusty.mpm` daemon and
//! `com.trusty.mpm.supervisor` plists — the two tm jobs that start tmux
//! servers — and [`build_process_type_check`] folds the readings into one row.
//! `Background` fails; any other value short of `Interactive`, including an
//! absent key (launchd's throttled `Standard` default), warns. Read-only.
//!
//! Test: `doctor_launchd_process_type_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_common::launchd_labels::{MPM, MPM_SUPERVISOR};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "launchd_process_type";

/// The `ProcessType` a tmux-hosting tm job must declare.
///
/// Why: Apple's `launchd.plist(5)` documents four values. `Background` clamps
/// the job's whole process tree; `Standard` (also the value of an absent key)
/// still applies light CPU and I/O throttling; `Adaptive` needs XPC traffic
/// these jobs never have. `Interactive` is the only class with no limits.
pub(crate) const EXPECTED_PROCESS_TYPE: &str = "Interactive";

/// The value that pins the tree to the background band.
const BACKGROUND: &str = "Background";

/// What one tm LaunchAgent plist declares.
///
/// What: `NotInstalled` when the file is absent; `Declared(None)` when the
/// plist has no `ProcessType` key; `Unjudged` when it exists but could not be
/// read as XML text.
/// Test: `reading_classifies_absent_keyless_and_binary_plists`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessTypeReading {
    /// No plist for this label.
    NotInstalled,
    /// The plist's `ProcessType`, or `None` when the key is absent.
    Declared(Option<String>),
    /// The plist exists but its value could not be read, and why.
    Unjudged(String),
}

/// One label's plist and what it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlistReading {
    /// The launchd label.
    pub label: &'static str,
    /// The plist path.
    pub path: PathBuf,
    /// What the plist declares.
    pub reading: ProcessTypeReading,
}

/// Extract `ProcessType`'s string value from XML plist text.
///
/// Why: XML comments are stripped first, so a comment that quotes the key
/// (the deploy template carries one) is never mistaken for the key itself.
/// A duplicated key resolves to the LAST occurrence, as CoreFoundation's
/// parser does, so the row judges the value launchd actually loads.
/// What: `Ok(None)` when the key is absent; `Ok(Some(v))` when the last
/// `<key>ProcessType</key>` is followed — after whitespace only — by
/// `<string>v</string>`; `Err` for `<string/>`, a non-string value, or any
/// other token between key and value, which the row reports as Unknown.
/// Test: `process_type_of_ignores_commented_keys`,
/// `process_type_of_rejects_malformed_values`,
/// `process_type_of_takes_the_last_duplicate_key`.
pub(crate) fn process_type_of(xml: &str) -> Result<Option<String>, String> {
    const KEY: &str = "<key>ProcessType</key>";
    let mut text = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(open) = rest.find("<!--") {
        text.push_str(&rest[..open]);
        rest = match rest[open..].find("-->") {
            Some(close) => &rest[open + close + "-->".len()..],
            None => "",
        };
    }
    text.push_str(rest);
    let Some(at) = text.rfind(KEY) else {
        return Ok(None);
    };
    let after = text[at + KEY.len()..].trim_start();
    if after.starts_with("<string/>") {
        return Err("ProcessType is an empty <string/>".to_owned());
    }
    let Some(body) = after.strip_prefix("<string>") else {
        let token: String = after.chars().take(24).collect();
        return Err(format!(
            "ProcessType is not followed by a <string>: {token:?}"
        ));
    };
    match body.find("</string>") {
        Some(end) => Ok(Some(body[..end].trim().to_owned())),
        None => Err("ProcessType <string> is not closed".to_owned()),
    }
}

/// Read one plist file into a [`ProcessTypeReading`].
///
/// What: a missing file is `NotInstalled`; a binary (`bplist00`) plist or an
/// unreadable file is `Unjudged`; otherwise `Declared` with the parsed value.
/// Test: `reading_classifies_absent_keyless_and_binary_plists`.
pub(crate) fn read_plist(path: &Path) -> ProcessTypeReading {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProcessTypeReading::NotInstalled,
        Err(e) => ProcessTypeReading::Unjudged(e.to_string()),
        Ok(bytes) if bytes.starts_with(b"bplist00") => ProcessTypeReading::Unjudged(
            "binary plist; convert with `plutil -convert xml1 <plist>`".to_owned(),
        ),
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(xml) => match process_type_of(&xml) {
                Ok(value) => ProcessTypeReading::Declared(value),
                Err(why) => ProcessTypeReading::Unjudged(why),
            },
            Err(_) => ProcessTypeReading::Unjudged("not UTF-8 text".to_owned()),
        },
    }
}

/// Single-quote `path` for a POSIX shell, escaping any embedded `'`.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The remedy for one stale plist, with its real path quoted for the shell.
///
/// Test: `remedy_quotes_a_path_with_a_space`.
fn remedy(path: &Path) -> String {
    let p = shell_quote(path);
    format!(
        "`plutil -replace ProcessType -string {EXPECTED_PROCESS_TYPE} {p}`, then \
         `launchctl bootout gui/$(id -u) {p}` and `launchctl bootstrap gui/$(id -u) {p}`"
    )
}

/// Fold the readings into one row.
///
/// What: `Fail` when any plist declares `Background`; else `Warn` when any
/// declares another value or none (launchd's `Standard` default); else
/// `Unknown` when any could not be read; else `Ok`. A tmux server already
/// running keeps the class it started with, so every non-Ok message says so.
/// Test: `background_supervisor_plist_fails`,
/// `keyless_daemon_plist_warns_standard_default`,
/// `interactive_plists_pass`, `no_plists_pass`, `binary_plist_is_unknown`.
pub(crate) fn build_process_type_check(readings: &[PlistReading]) -> DoctorCheck {
    let mut fails = Vec::new();
    let mut warns = Vec::new();
    let mut unknown = Vec::new();
    for r in readings {
        match &r.reading {
            ProcessTypeReading::NotInstalled => {}
            ProcessTypeReading::Declared(Some(v)) if v == EXPECTED_PROCESS_TYPE => {}
            ProcessTypeReading::Declared(Some(v)) if v == BACKGROUND => fails.push(format!(
                "`{}` declares ProcessType={BACKGROUND}, which clamps tmux and every \
                 session it hosts to background QoS (#8415); fix: {}",
                r.label,
                remedy(&r.path)
            )),
            ProcessTypeReading::Declared(v) => warns.push(format!(
                "`{}` declares ProcessType={}, which launchd throttles; fix: {}",
                r.label,
                v.as_deref().unwrap_or("<absent, Standard>"),
                remedy(&r.path)
            )),
            ProcessTypeReading::Unjudged(why) => {
                unknown.push(format!("`{}` ({}): {why}", r.label, r.path.display()))
            }
        }
    }
    const RESTART: &str = "A tmux server already running keeps its class until it exits.";
    if !fails.is_empty() {
        fails.extend(warns);
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!("{} {RESTART}", fails.join("; ")),
        );
    }
    if !warns.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!("{} {RESTART}", warns.join("; ")),
        );
    }
    if !unknown.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!("could not read ProcessType: {}", unknown.join("; ")),
        );
    }
    let installed: Vec<_> = readings
        .iter()
        .filter(|r| r.reading != ProcessTypeReading::NotInstalled)
        .map(|r| r.label)
        .collect();
    let message = if installed.is_empty() {
        "no tm LaunchAgent that starts tmux is installed".to_owned()
    } else {
        format!(
            "{} declare ProcessType={EXPECTED_PROCESS_TYPE}",
            installed.join(", ")
        )
    };
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, message)
}

/// Environment variable naming the LaunchAgents directory the row reads.
///
/// Why (#8415, owner rule 2026-09-23): no test may read the operator's real
/// `~/Library/LaunchAgents`, even read-only. The `tm` bin's tests build this
/// library without `cfg(test)`, so they need an override they can set.
pub const LAUNCH_AGENTS_DIR_ENV: &str = "TRUSTY_MPM_LAUNCH_AGENTS_DIR";

/// The LaunchAgents directory `run_doctor` reads for this row.
///
/// What: [`LAUNCH_AGENTS_DIR_ENV`] when set and non-empty; otherwise an empty
/// temp path under `cfg(test)`; otherwise `<home>/Library/LaunchAgents`, the
/// production default.
/// Test: `launch_agents_dir_honours_the_env_override`,
/// `test_builds_never_read_the_real_launch_agents`.
pub(crate) fn launch_agents_dir(home: &Path) -> PathBuf {
    launch_agents_dir_from(home, std::env::var_os(LAUNCH_AGENTS_DIR_ENV))
}

/// [`launch_agents_dir`] with the override passed in, so tests need not
/// mutate the process environment.
pub(crate) fn launch_agents_dir_from(home: &Path, env: Option<std::ffi::OsString>) -> PathBuf {
    match env {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ if cfg!(test) => std::env::temp_dir().join("tm-doctor-test-no-launch-agents"),
        _ => home.join("Library").join("LaunchAgents"),
    }
}

/// Read the tmux-hosting tm plists under `home` and build the row.
///
/// Test: `check_reads_plists_under_the_given_home`.
#[cfg(test)]
pub(crate) fn check_launchd_process_type(home: &Path) -> DoctorCheck {
    check_launchd_process_type_in(&home.join("Library").join("LaunchAgents"))
}

/// Read `<agents>/<label>.plist` for the daemon and supervisor labels and
/// build the row with [`build_process_type_check`].
///
/// Test: `check_reads_plists_under_the_given_home`,
/// `launch_agents_dir_honours_the_env_override`.
pub(crate) fn check_launchd_process_type_in(agents: &Path) -> DoctorCheck {
    let readings: Vec<PlistReading> = [MPM, MPM_SUPERVISOR]
        .into_iter()
        .map(|label| {
            let path = agents.join(format!("{label}.plist"));
            PlistReading {
                label,
                reading: read_plist(&path),
                path,
            }
        })
        .collect();
    build_process_type_check(&readings)
}

#[cfg(test)]
#[path = "doctor_launchd_process_type_tests.rs"]
mod tests;
