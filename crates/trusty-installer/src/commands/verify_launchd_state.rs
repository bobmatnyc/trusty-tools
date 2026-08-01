//! Three-state launchd diagnosis for a `down` service member (#3833/#3836).
//!
//! Why: split out of `verify_tail.rs` (issue #3836 code-critic fix round —
//! `verify_tail.rs` crossed the 500-SLOC production file cap once this
//! diagnosis machinery + the #3836 defensive-fallback support landed).
//! Distinguishing WHY a launchd-managed daemon is `down` — never loaded at
//! all, loaded then crashed, or loaded and just still starting — is its own
//! cohesive concern, independent of `verify_tail`'s polling/report-building
//! logic, and is reused by BOTH `verify_tail::verify_one` (the #3833 poll
//! wait's final diagnosis) and `service_bootstrap::bootstrap_one` (the #3836
//! defensive fallback's "is it actually loaded?" check) — a natural module
//! boundary.
//!
//! What: [`DownState`] is the three-variant diagnosis; [`classify_down_state`]
//! derives it for a binary via `launchctl list <label>`; [`is_label_loaded`]
//! answers the narrower "is it loaded at all?" question the #3836 fallback
//! needs, off the SAME `launchctl list` primitive so the two call sites can
//! never disagree about what "loaded" means; [`launchd_owner`] (#4470) answers
//! "which PID, if any, is launchd running for this label?" off that same
//! primitive, for [`super::port_guard`]'s foreign-port check.
//!
//! Test: the pure decision pieces (`classify_down_state_from_entry`,
//! `parse_launchd_list_text`, `owner_from_list`, `DownState::phrase`) are
//! unit-tested; the `launchctl` subprocess call (`launchd_list_raw`) is
//! side-effecting and validated manually.

use serde::Serialize;

/// Why a LAUNCHD daemon member is still reporting `down` after the #3833 poll
/// wait — replaces a uniform, underspecified `down` with a diagnosis an
/// operator can act on directly.
///
/// Why: "down" alone doesn't tell you whether launchd never loaded the job at
/// all, loaded it and it crashed, or it is simply still coming up — three
/// situations with three different next actions (re-run the bootstrap step,
/// read the crash log, or just wait longer).
/// What: [`DownState::NotLoaded`] — `launchctl list <label>` found no such
/// service (never bootstrapped, or booted out). [`DownState::Crashed`] —
/// loaded, no PID currently running, and the last recorded exit status was
/// nonzero. [`DownState::StillStarting`] — loaded, and either a PID is
/// currently running (health just hasn't caught up yet) or the last exit
/// status was a clean `0` (about to be (re)launched, e.g. between
/// `ThrottleInterval` respawns).
/// Test: `tests::classify_down_state_from_entry_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DownState {
    /// `launchctl` has no record of this label at all.
    NotLoaded,
    /// Loaded, but not currently running, with a nonzero last exit status.
    Crashed {
        /// The `LastExitStatus` launchd recorded.
        exit_code: i32,
    },
    /// Loaded and either currently running (health hasn't caught up) or
    /// cleanly exited and awaiting its next (re)launch.
    StillStarting,
}

impl DownState {
    /// A short, human-readable phrase for `verify_tail`'s per-row annotation.
    ///
    /// Why: centralises the wording so the JSON `state` tag and the human
    /// narration can never drift.
    /// What: maps each variant to a lowercase phrase with no leading article.
    /// Test: `tests::down_state_phrase_mapping`.
    pub(super) fn phrase(&self) -> String {
        match self {
            DownState::NotLoaded => "not loaded".to_owned(),
            DownState::Crashed { exit_code } => format!("crashed, exit {exit_code}"),
            DownState::StillStarting => "still starting".to_owned(),
        }
    }
}

/// One `launchctl list <label>` observation, pre-parsed (#3833).
///
/// Why: separating the parse from the subprocess spawn lets
/// [`classify_down_state_from_entry`] be exercised with fixed sample text —
/// no live `launchctl` needed.
/// What: `pid` — the `"PID" = N;` value when a process is currently running
/// (#4470 widened this from a bare `has_pid` flag: [`launchd_owner`] needs the
/// PID ITSELF to compare against whoever holds the daemon's port, and reading
/// it here keeps ONE parser for `launchctl list` output rather than a second
/// copy that could drift); `exit_status` — the `"LastExitStatus"` launchd last
/// recorded (`0` when absent, matching launchd's own default).
/// Test: `tests::parse_launchd_list_text_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LaunchdListEntry {
    pid: Option<u32>,
    exit_status: i32,
}

impl LaunchdListEntry {
    /// Whether launchd currently has a process running for this job.
    ///
    /// Why: the #3833 down-state classification only ever asked the yes/no
    /// question; keeping it as an accessor over the richer `pid` field means
    /// widening the parse for #4470 did not change that logic at all.
    /// What: `true` iff `pid` is `Some`.
    /// Test: covered via `tests::parse_launchd_list_text_*`.
    fn has_pid(&self) -> bool {
        self.pid.is_some()
    }
}

/// The three distinguishable answers `launchctl list <label>` can give (#4470).
///
/// Why: the pre-#4470 [`launchd_list_raw`] returned `Option<String>`, folding
/// "launchd positively reports no such label" together with "launchd could not
/// be asked at all" (binary missing from `PATH`, sandboxed, spawn failure).
/// PR #4466 established that exact conflation as a HIGH defect on the
/// trusty-mpm side — it is what made `tm doctor` prescribe a `kill` against a
/// correctly supervised daemon. [`super::port_guard`] needs the same
/// distinction to phrase an honest refusal, so the primitive keeps all three
/// states and the existing `Option`-shaped callers collapse them themselves.
/// What: `Found` carries the raw stdout dump; `NotFound` is launchd's
/// non-zero-exit "no such service" signature; `Unavailable` means the query
/// could not be performed.
/// Test: `tests::owner_from_list_*` covers how each state is interpreted.
///
/// `cfg_attr(not(macos), allow(dead_code))`: only the macOS
/// [`launchd_list_raw`] can construct `Found`/`NotFound` — off macOS there is
/// no `launchctl` to answer, so that arm returns `Unavailable` unconditionally
/// and the other two variants are genuinely never built in a non-test Linux
/// build. `dead_code` then fires under Linux CI's `-D warnings` (verified by
/// compiling this module with the non-macOS arm forced active). Narrowly
/// allowing it here is correct: the variants are not dead, they are
/// platform-conditional, and `#[cfg]`-ing them away instead would fork the
/// enum and force every `match` to grow a platform split.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchdList {
    /// launchd knows the label; the payload is `launchctl list`'s stdout.
    Found(String),
    /// launchd positively answered that it has no such label.
    NotFound,
    /// launchd could not be asked (no `launchctl`, spawn failure, non-macOS).
    Unavailable,
}

/// Who launchd is running for a given label, as a three-state answer (#4470).
///
/// Why: [`super::port_guard`] must decide whether the process holding a
/// daemon's port IS the launchd-supervised daemon. That needs launchd's own
/// PID, and needs "launchd says nothing is running" kept distinct from
/// "launchd could not be asked" — the second is an unverifiable state that
/// must fail CLOSED rather than be read as "nothing is running, all clear".
/// What: `Running` carries the supervised PID; `NotRunning` means launchd
/// positively has no process for the label (unloaded, or loaded but not
/// currently running); `Unavailable` means the question could not be answered.
/// Test: `tests::owner_from_list_running`, `tests::owner_from_list_not_running`,
/// `tests::owner_from_list_missing_label_is_not_running`,
/// `tests::owner_from_list_unavailable_is_not_collapsed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchdOwner {
    /// launchd supervises this label and is running it as this PID.
    Running(u32),
    /// launchd has no process running for this label right now.
    NotRunning,
    /// launchd could not be queried; ownership is UNKNOWN, not "none".
    Unavailable,
}

/// Parse `launchctl list <label>`'s property-list-style stdout dump.
///
/// Why: isolates the (pure) text parsing from the (side-effecting) subprocess
/// spawn so the format assumptions are unit-testable.
/// What: scans for a `"PID" = N;` line (sets `pid` — #4470 keeps the VALUE,
/// not just its presence) and a `"LastExitStatus" = N;` line (sets
/// `exit_status`, defaulting to `0` when absent — launchd omits the key before
/// the job has ever exited). Never panics on malformed input; a `"PID"` line
/// whose value will not parse yields `pid: None`, which every caller treats as
/// "launchd is not running it", the conservative reading.
/// Test: `tests::parse_launchd_list_text_running`,
/// `tests::parse_launchd_list_text_crashed`,
/// `tests::parse_launchd_list_text_clean_exit`,
/// `tests::parse_launchd_list_text_keeps_pid_value`.
fn parse_launchd_list_text(text: &str) -> LaunchdListEntry {
    let pid = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"PID\" ="))
        .and_then(|rest| rest.trim().trim_end_matches(';').trim().parse::<u32>().ok());
    let exit_status = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"LastExitStatus\" ="))
        .and_then(|rest| rest.trim().trim_end_matches(';').trim().parse::<i32>().ok())
        .unwrap_or(0);
    LaunchdListEntry { pid, exit_status }
}

/// Interpret a [`LaunchdList`] observation as a [`LaunchdOwner`] (#4470).
///
/// Why: the pure decision half of [`launchd_owner`], so every state — including
/// the `Unavailable` one that must NOT be mistaken for "nothing is running" —
/// is unit-tested without a live `launchctl`.
/// What: `Found` with a running PID → [`LaunchdOwner::Running`]; `Found`
/// without one, and `NotFound` (launchd's positive "no such service" answer),
/// both → [`LaunchdOwner::NotRunning`]; `Unavailable` stays
/// [`LaunchdOwner::Unavailable`].
/// Test: `tests::owner_from_list_running`, `tests::owner_from_list_not_running`,
/// `tests::owner_from_list_missing_label_is_not_running`,
/// `tests::owner_from_list_unavailable_is_not_collapsed`.
fn owner_from_list(list: &LaunchdList) -> LaunchdOwner {
    match list {
        LaunchdList::Found(text) => match parse_launchd_list_text(text).pid {
            Some(pid) => LaunchdOwner::Running(pid),
            None => LaunchdOwner::NotRunning,
        },
        LaunchdList::NotFound => LaunchdOwner::NotRunning,
        LaunchdList::Unavailable => LaunchdOwner::Unavailable,
    }
}

/// Which PID launchd is running for `label`, if any (#4470).
///
/// Why: [`super::port_guard`] compares this against whoever actually holds the
/// daemon's TCP port; a mismatch is the #4230 orphan signature that makes a
/// `launchctl bootstrap` report success while a foreign process keeps serving
/// the port. Built on the SAME [`launchd_list_raw`] primitive as
/// [`is_label_loaded`] and [`classify_down_state`], so no third notion of "what
/// launchd thinks" can drift into the crate.
/// What: composes the side-effecting [`launchd_list_raw`] with the pure
/// [`owner_from_list`].
/// Test: side-effecting; the decision half is `tests::owner_from_list_*`.
pub fn launchd_owner(label: &str) -> LaunchdOwner {
    owner_from_list(&launchd_list_raw(label))
}

/// Classify a parsed `launchctl list` observation into a [`DownState`].
///
/// Why: the pure decision half of [`classify_down_state`] — kept separate so
/// every branch is unit-tested without a live `launchctl`.
/// What: `None` (label not found) → [`DownState::NotLoaded`]; a running PID →
/// [`DownState::StillStarting`] (loaded, health just hasn't caught up); a
/// nonzero last exit status with no running PID → [`DownState::Crashed`];
/// anything else (loaded, no PID, clean last exit) → [`DownState::StillStarting`]
/// (about to be (re)launched).
/// Test: `tests::classify_down_state_from_entry_not_loaded`,
/// `tests::classify_down_state_from_entry_running`,
/// `tests::classify_down_state_from_entry_crashed`,
/// `tests::classify_down_state_from_entry_clean_exit_no_pid`.
fn classify_down_state_from_entry(entry: Option<LaunchdListEntry>) -> DownState {
    match entry {
        None => DownState::NotLoaded,
        Some(e) if e.has_pid() => DownState::StillStarting,
        Some(e) if e.exit_status != 0 => DownState::Crashed {
            exit_code: e.exit_status,
        },
        Some(_) => DownState::StillStarting,
    }
}

/// Run `launchctl list <label>` and classify the outcome (macOS only; #3833,
/// widened to three states by #4470).
///
/// Why: isolated as its own thin side-effecting function so
/// [`classify_down_state`] and [`launchd_owner`] compose it with the pure
/// parse/decide helpers. #4470 stopped it collapsing a failed SPAWN into the
/// same answer as launchd's "no such service" — see [`LaunchdList`].
/// What: [`LaunchdList::Found`] with stdout on a successful invocation;
/// [`LaunchdList::NotFound`] on a non-zero exit (launchd's "no such service"
/// signature); [`LaunchdList::Unavailable`] when the command could not be run
/// at all — including on non-macOS, where there is no `launchctl` to ask.
/// Test: side-effecting; not invoked in the test suite.
#[cfg(target_os = "macos")]
fn launchd_list_raw(label: &str) -> LaunchdList {
    let Ok(out) = std::process::Command::new("launchctl")
        .args(["list", label])
        .output()
    else {
        return LaunchdList::Unavailable;
    };
    if !out.status.success() {
        return LaunchdList::NotFound;
    }
    LaunchdList::Found(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn launchd_list_raw(_label: &str) -> LaunchdList {
    LaunchdList::Unavailable
}

/// Whether launchd currently has `label` loaded at all (#3836).
///
/// Why: `service_bootstrap::bootstrap_one`'s #3836 defensive fallback needs
/// to know whether a component binary's own `service install` actually
/// loaded the agent, not just that the subprocess exited 0 (#3832's root
/// cause — trusty-memory's `service install` used to write the plist without
/// loading it). Reuses the exact same `launchctl list <label>` primitive
/// [`classify_down_state`] is built on ([`launchd_list_raw`]), so the two
/// call sites can never disagree about what "loaded" means.
/// What: `true` iff `launchctl list <label>` finds the label at all —
/// regardless of whether a PID is currently running; a loaded-but-not-yet-
/// running job is still "loaded" (launchd owns it and will start it).
/// Test: side-effecting; not invoked in the test suite (mirrors
/// `classify_down_state`) — the underlying `Option`-based decision is the
/// SAME one `classify_down_state_from_entry`'s `None` branch already covers.
pub(super) fn is_label_loaded(label: &str) -> bool {
    matches!(launchd_list_raw(label), LaunchdList::Found(_))
}

/// Classify why a LAUNCHD member is still `down` after the poll wait (#3833).
///
/// Why: `verify_tail::verify_one` calls this once it has a final `down`
/// verdict for a LAUNCHD member — composes the side-effecting `launchctl
/// list` call with the pure parse + classify pair above.
/// What: resolves the member's launchd label, runs `launchctl list <label>`,
/// and classifies the result via [`classify_down_state_from_entry`].
/// Test: side-effecting (subprocess); the decision half is
/// `classify_down_state_from_entry`.
pub(super) fn classify_down_state(binary: &str) -> DownState {
    let label = super::plist_label::plist_label_for(binary);
    // Unchanged behaviour: both non-`Found` states have always classified as
    // `NotLoaded` here; #4470 only stopped the PRIMITIVE from conflating them,
    // leaving each caller to collapse as it sees fit.
    let entry = match launchd_list_raw(&label) {
        LaunchdList::Found(text) => Some(parse_launchd_list_text(&text)),
        LaunchdList::NotFound | LaunchdList::Unavailable => None,
    };
    classify_down_state_from_entry(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pins the exact wording each `DownState` variant renders, since
    /// both the human summary and (transitively, via `serde`) the `--json`
    /// `state` tag depend on it staying stable.
    /// What: asserts the phrase for each variant.
    /// Test: This is the test.
    #[test]
    fn down_state_phrase_mapping() {
        assert_eq!(DownState::NotLoaded.phrase(), "not loaded");
        assert_eq!(
            DownState::Crashed { exit_code: 78 }.phrase(),
            "crashed, exit 78"
        );
        assert_eq!(DownState::StillStarting.phrase(), "still starting");
    }

    /// Why: the running-PID branch must win regardless of last exit status —
    /// a currently-running process means the daemon IS up; `down` health
    /// just hasn't caught up yet (e.g. still loading models).
    /// What: an entry with `has_pid: true` classifies as `StillStarting`
    /// even when `exit_status` is nonzero (stale from a PRIOR crash).
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_running() {
        let entry = LaunchdListEntry {
            pid: Some(4242),
            exit_status: 1,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::StillStarting
        );
    }

    /// Why: THE #3833 core diagnosis — no running PID plus a nonzero last
    /// exit status is a genuine crash, not a startup race.
    /// What: asserts `Crashed` carries the exact exit code.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_crashed() {
        let entry = LaunchdListEntry {
            pid: None,
            exit_status: 2,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::Crashed { exit_code: 2 }
        );
    }

    /// Why: no PID + a clean (`0`) last exit is ambiguous between "never
    /// started yet" and "cleanly stopped, about to relaunch" — both read as
    /// "still starting" rather than a false `Crashed`.
    /// What: asserts `StillStarting`.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_clean_exit_no_pid() {
        let entry = LaunchdListEntry {
            pid: None,
            exit_status: 0,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::StillStarting
        );
    }

    /// Why: THE #3832/#3833 root-cause signature — a label `launchctl list`
    /// cannot find at all (never bootstrapped, e.g. the #3832 trusty-memory
    /// bug) must be reported as `NotLoaded`, not lumped in with a crash.
    /// What: asserts `None` (label not found) classifies as `NotLoaded`.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_not_loaded() {
        assert_eq!(classify_down_state_from_entry(None), DownState::NotLoaded);
    }

    /// Why: `launchctl list <label>`'s dump format is `"Key" = value;` lines
    /// inside a `{ ... }` block; the parser must find `PID`/`LastExitStatus`
    /// regardless of surrounding whitespace/indentation and must not mistake
    /// unrelated keys (e.g. `"PerJobMachServices"`) for them.
    /// What: parses a realistic "running" dump; asserts `has_pid` and the
    /// default `exit_status` (absent key → `0`).
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_running() {
        let text = r#"{
	"LimitLoadToSessionType" = "Aqua";
	"Label" = "com.trusty.trusty-search";
	"OnDemand" = false;
	"LastExitStatus" = 0;
	"PID" = 4242;
	"Program" = "/usr/local/bin/trusty-search";
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(entry.has_pid());
        assert_eq!(entry.exit_status, 0);
    }

    /// Why: the crash signature — no `PID` line, a nonzero `LastExitStatus`.
    /// What: parses a realistic "crashed" dump; asserts `has_pid` is false
    /// and `exit_status` matches.
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_crashed() {
        let text = r#"{
	"LimitLoadToSessionType" = "Aqua";
	"Label" = "com.trusty.memory";
	"LastExitStatus" = 78;
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(!entry.has_pid());
        assert_eq!(entry.exit_status, 78);
    }

    /// Why: a loaded-but-never-yet-run (or cleanly stopped) job omits `PID`
    /// and reports `LastExitStatus = 0` (or omits it entirely) — must not be
    /// misparsed as a crash.
    /// What: parses a dump with neither key; asserts `has_pid: false`,
    /// `exit_status: 0` (the documented default).
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_clean_exit() {
        let text = r#"{
	"Label" = "com.trusty.trusty-review";
	"OnDemand" = false;
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(!entry.has_pid());
        assert_eq!(entry.exit_status, 0);
    }

    /// Why (#4470): the port guard compares launchd's PID against the PID
    /// holding the daemon's port, so the parser must keep the VALUE, not just
    /// its presence. A parser that only answered `has_pid` would make every
    /// comparison impossible and force a second, drift-prone parser.
    /// What: asserts the exact PID is recovered from a realistic dump.
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_keeps_pid_value() {
        let text = "{\n\t\"PID\" = 4242;\n\t\"LastExitStatus\" = 0;\n};\n";
        assert_eq!(parse_launchd_list_text(text).pid, Some(4242));
    }

    /// Why (#4470): a running supervised job is the ONLY state in which the
    /// port guard may conclude the port holder is legitimate, so the PID must
    /// travel out of the parse intact.
    /// What: `Found` with a PID maps to `Running(pid)`.
    /// Test: This is the test.
    #[test]
    fn owner_from_list_running() {
        let list = LaunchdList::Found("{\n\t\"PID\" = 777;\n};\n".to_owned());
        assert_eq!(owner_from_list(&list), LaunchdOwner::Running(777));
    }

    /// Why (#4470): launchd knowing the label but running no process means any
    /// process holding that port is NOT the supervised daemon — the #4230
    /// orphan signature.
    /// What: `Found` without a PID maps to `NotRunning`.
    /// Test: This is the test.
    #[test]
    fn owner_from_list_not_running() {
        let list = LaunchdList::Found("{\n\t\"LastExitStatus\" = 0;\n};\n".to_owned());
        assert_eq!(owner_from_list(&list), LaunchdOwner::NotRunning);
    }

    /// Why (#4470): launchd's non-zero-exit "no such service" is a POSITIVE
    /// answer — it genuinely runs nothing for this label — and must read the
    /// same as a loaded-but-idle job, not as an unanswerable query.
    /// What: `NotFound` maps to `NotRunning`.
    /// Test: This is the test.
    #[test]
    fn owner_from_list_missing_label_is_not_running() {
        assert_eq!(
            owner_from_list(&LaunchdList::NotFound),
            LaunchdOwner::NotRunning
        );
    }

    /// Why (#4470, mirroring the PR #4466 HIGH): "launchd could not be asked"
    /// must NEVER collapse into "launchd runs nothing". The port guard fails
    /// CLOSED on `Unavailable`; folding it into `NotRunning` would instead make
    /// it emit a confident, possibly wrong accusation about a supervised
    /// daemon. This test fails the moment the two are merged.
    /// What: `Unavailable` stays `Unavailable` and is not `NotRunning`.
    /// Test: This is the test.
    #[test]
    fn owner_from_list_unavailable_is_not_collapsed() {
        let owner = owner_from_list(&LaunchdList::Unavailable);
        assert_eq!(owner, LaunchdOwner::Unavailable);
        assert_ne!(owner, LaunchdOwner::NotRunning);
    }
}
