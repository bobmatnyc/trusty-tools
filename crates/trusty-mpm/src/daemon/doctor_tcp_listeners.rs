//! `tm doctor` row `tcp_listeners`: which trusty-* processes listen on TCP
//! (#8926, ADR-0032).
//!
//! Why: ADR-0032 makes trusty-console the only TCP surface. The CI lint
//! `scripts/check_no_tcp_listeners.sh` stops a new bind in source; this row
//! reports what actually runs on the host — an older installed binary, or an
//! opt-in `--http` flag someone passed.
//!
//! What: [`check_tcp_listeners`] lists the host's LISTEN sockets through
//! [`probe::listening_sockets`] (libproc on macOS, `/proc` on Linux; no `lsof`
//! or `netstat`), keeps those owned by a trusty-* executable, and grades each
//! against the embedded `tcp_listener_allowlist.tsv`, the same file the lint
//! reads. A listener no `permanent`/`temporary` row names is FAIL; a
//! `temporary` row's listener is WARN naming its issue; the console is OK. A
//! probe that fails, or a trusty-* process whose sockets could not be read, is
//! UNKNOWN — never OK. Read-only.
//!
//! Test: `doctor_tcp_listeners_tests.rs`.

use crate::core::doctor::{CheckStatus, DoctorCheck};

#[path = "doctor_tcp_listeners_probe.rs"]
pub(crate) mod probe;

/// The `tm doctor` row name.
pub(crate) const CHECK_NAME: &str = "tcp_listeners";

/// The allowlist the CI lint reads too; its header documents the columns.
pub(crate) const ALLOWLIST_TSV: &str = include_str!("tcp_listener_allowlist.tsv");

/// Executable basenames that are trusty-* without the `trusty-` prefix.
const TRUSTY_SHORT_NAMES: &[&str] = &[
    "tm",
    "tagent",
    "tcode",
    "tctl",
    "tga",
    "taudit",
    "slack-mcp",
    "telegram-mcp",
    "tickets-mcp",
];

/// One TCP socket in LISTEN state and the process holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListenSocket {
    /// Owning process id.
    pub pid: u32,
    /// Executable basename.
    pub process: String,
    /// Local address, e.g. `127.0.0.1`.
    pub addr: String,
    /// Local port.
    pub port: u16,
}

/// A trusty-* process whose sockets the probe could not read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uninspected {
    /// Process id.
    pub pid: u32,
    /// Executable basename.
    pub process: String,
    /// The OS error.
    pub error: String,
}

/// What one probe pass saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProbeReport {
    /// LISTEN sockets of the processes the probe was asked about.
    pub listeners: Vec<ListenSocket>,
    /// Processes the probe was asked about but could not read.
    pub uninspected: Vec<Uninspected>,
}

/// An allowlist row's `kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllowKind {
    /// The console (ADR-0032): its live listener is OK.
    Permanent,
    /// A daemon still being migrated: its live listener WARNs.
    Temporary,
    /// A bind in source only; a live listener from it FAILs.
    SourceOnly,
}

/// One allowlist row, as far as the doctor reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AllowEntry {
    /// Row kind.
    pub kind: AllowKind,
    /// The crate owning the listener.
    pub crate_name: String,
    /// Executable basenames the row covers; empty for `source-only`.
    pub processes: Vec<String>,
    /// `ADR-NNNN` or `#N`.
    pub issue: String,
}

/// Parse the allowlist TSV.
///
/// Why: the embedded file is shared with the lint, which validates the same
/// shape; a row the doctor cannot read must fail the row, not be skipped.
/// What: skips `#` comments and blank lines; each other line needs six
/// tab-separated fields (`kind crate processes paths issue reason`), a known
/// kind, and a process list unless the kind is `source-only`.
/// Test: `malformed_allowlist_rows_are_refused`,
/// `embedded_allowlist_keeps_the_console_as_the_only_permanent_row`.
pub(crate) fn parse_allowlist(text: &str) -> Result<Vec<AllowEntry>, String> {
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [kind, crate_name, processes, _paths, issue, _reason] = fields[..] else {
            return Err(format!("line {}: want 6 tab-separated fields", idx + 1));
        };
        let kind = match kind {
            "permanent" => AllowKind::Permanent,
            "temporary" => AllowKind::Temporary,
            "source-only" => AllowKind::SourceOnly,
            other => return Err(format!("line {}: unknown kind `{other}`", idx + 1)),
        };
        let processes: Vec<String> = match processes {
            "-" | "" => Vec::new(),
            list => list.split(',').map(str::to_string).collect(),
        };
        if kind != AllowKind::SourceOnly && processes.is_empty() {
            return Err(format!("line {}: a {kind:?} row names no process", idx + 1));
        }
        rows.push(AllowEntry {
            kind,
            crate_name: crate_name.to_string(),
            processes,
            issue: issue.to_string(),
        });
    }
    Ok(rows)
}

/// Whether an executable basename belongs to the trusty-* family.
///
/// What: a `trusty-` prefix, or one of the short binary names.
/// Test: `trusty_process_names_match_prefix_and_short_names`.
pub(crate) fn is_trusty_process(name: &str) -> bool {
    name.starts_with("trusty-") || TRUSTY_SHORT_NAMES.contains(&name)
}

/// Grade one probe pass against the allowlist.
///
/// Why: the probe is the one part that touches the host, so this pure fold is
/// where every verdict is decided and tested.
/// What: ignores non-trusty processes. A listener whose process a
/// `permanent` row names is OK, a `temporary` row WARN (naming the issue), no
/// such row FAIL — a `source-only` row never excuses a live listener. A probe
/// error or an uninspected trusty-* process is UNKNOWN. The status is the worst
/// finding; the message lists every finding.
/// Test: `only_the_console_listening_is_ok`,
/// `a_temporary_listener_warns_naming_its_issue`,
/// `a_listener_no_live_row_names_fails`, `a_failed_probe_is_unknown_never_ok`,
/// `an_uninspected_trusty_process_is_unknown_never_ok`.
pub(crate) fn classify(probe: Result<ProbeReport, String>, allow: &[AllowEntry]) -> DoctorCheck {
    let report = match probe {
        Ok(report) => report,
        Err(e) => {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!("could not list TCP listeners: {e}; not judged (ADR-0032)"),
            );
        }
    };
    let mut status = CheckStatus::Ok;
    let mut findings = Vec::new();
    for sock in report
        .listeners
        .iter()
        .filter(|s| is_trusty_process(&s.process))
    {
        let who = format!(
            "{} (pid {}) {}:{}",
            sock.process, sock.pid, sock.addr, sock.port
        );
        let row = allow
            .iter()
            .find(|r| r.kind != AllowKind::SourceOnly && r.processes.contains(&sock.process));
        let (verdict, text) = match row {
            Some(r) if r.kind == AllowKind::Permanent => {
                (CheckStatus::Ok, format!("{who} allowed ({})", r.issue))
            }
            Some(r) => (
                CheckStatus::Warn,
                format!("{who} TEMPORARY until {} ({})", r.issue, r.crate_name),
            ),
            None => (
                CheckStatus::Fail,
                format!(
                    "{who} is not allowlisted: only trusty-console may listen on TCP (ADR-0032)"
                ),
            ),
        };
        status = status.worst(verdict);
        findings.push(text);
    }
    for gap in report
        .uninspected
        .iter()
        .filter(|u| is_trusty_process(&u.process))
    {
        status = status.worst(CheckStatus::Unknown);
        findings.push(format!(
            "could not read the sockets of {} (pid {}): {}",
            gap.process, gap.pid, gap.error
        ));
    }
    let message = if findings.is_empty() {
        "no trusty-* process listens on TCP (ADR-0032)".to_string()
    } else {
        findings.join("; ")
    };
    DoctorCheck::new(CHECK_NAME, status, message)
}

/// The `tcp_listeners` row for this host.
///
/// What: parses the embedded allowlist (a malformed one FAILs the row) and
/// grades [`probe::listening_sockets`] with [`classify`]. Read-only.
/// Test: `the_host_probe_finds_a_listener_this_test_holds`.
pub(crate) fn check_tcp_listeners() -> DoctorCheck {
    match parse_allowlist(ALLOWLIST_TSV) {
        Ok(allow) => classify(probe::listening_sockets(is_trusty_process), &allow),
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!("the embedded tcp_listener_allowlist.tsv is malformed: {e}"),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_tcp_listeners_tests.rs"]
mod tests;
