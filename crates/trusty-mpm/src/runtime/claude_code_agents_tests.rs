//! Tests for the #6863 live-background-session probe and the `attach` relaunch
//! shape: `claude_code_agents::{attach_id_in_registry, attach_command,
//! relaunch_command}`.
//!
//! Why: the defect is a WRONG COMMAND, not a wrong exit code — `tm session
//! resume` typed `claude --resume <uuid>` at a session Claude Code was already
//! running in the background, which refuses and exits 0. So every test here
//! asserts on the built pane command, and none of them runs a real `claude`:
//! the registry read is injected as a closure returning `Result<String, String>`
//! (#4255 — a unit test must never touch the operator's real session state).
//! The one test that does spawn — `query_registry_kills_and_reaps_a_wedged_probe`
//! — runs a stand-in shell script, never the operator's `claude`.
//! What: the registry sample is a verbatim capture of `claude agents --json`
//! from Claude Code 2.1.261, trimmed to the entry shapes that differ —
//! background-live, background-finished, and interactive.
//! Test: itself.

use std::path::Path;

use super::{RelaunchInputs, attach_command, attach_id_in_registry, relaunch_command};

/// The live background session every "attaches" test targets.
const LIVE_UUID: &str = "74ede5c9-c66d-471e-8dbe-9370d29f6f2e";
/// Its short id — what `claude attach` takes.
const LIVE_SHORT: &str = "74ede5c9";
/// A session that is on disk but absent from the registry.
const STALE_UUID: &str = "11111111-2222-3333-4444-555555555555";
/// A background session the operator ended with `claude stop`.
const STOPPED_UUID: &str = "5c30bb17-9d41-4a2e-9f0c-7ab2c1d5e604";

const TEST_CWD: &str = "/tmp/ws";
const TEST_SESSION_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// Verbatim `claude agents --json` output (Claude Code 2.1.261), reduced to one
/// entry of each shape the parser must survive: an interactive entry with
/// neither `id` nor `state`, a finished background entry, and the live
/// background entry with `pid`/`status` fields the parser ignores. The
/// `stopped` entry is the one addition to the capture — the shape `claude stop
/// <short-id>` leaves behind, which the machine that produced this capture had
/// none of (#6863).
const REGISTRY_SAMPLE: &str = r#"[
  {
    "id": "8ec01a01",
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "background",
    "startedAt": 1788315744511,
    "sessionId": "8ec01a01-3a77-4d7c-b588-456fac3100dd",
    "name": "Dog",
    "state": "done"
  },
  {
    "id": "a816af2c",
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "background",
    "startedAt": 1784481833593,
    "sessionId": "a816af2c-818a-4dfa-a2c8-463bdd4eb694",
    "name": "resume paused session",
    "state": "failed"
  },
  {
    "id": "1905c26f",
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "background",
    "startedAt": 1783783191567,
    "sessionId": "1905c26f-c66e-4081-8c93-b612090ca104",
    "name": "Research web-installer implementation",
    "state": "blocked"
  },
  {
    "id": "5c30bb17",
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "background",
    "startedAt": 1788500000000,
    "sessionId": "5c30bb17-9d41-4a2e-9f0c-7ab2c1d5e604",
    "name": "stopped by the operator",
    "state": "stopped"
  },
  {
    "pid": 6930,
    "id": "74ede5c9",
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "background",
    "startedAt": 1788618816891,
    "sessionId": "74ede5c9-c66d-471e-8dbe-9370d29f6f2e",
    "name": "Mpm dashboard",
    "status": "busy",
    "state": "working"
  },
  {
    "pid": 9672,
    "cwd": "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools",
    "kind": "interactive",
    "startedAt": 1788618881733,
    "sessionId": "e5e08169-b616-4b97-870d-08535eb2f4bc",
    "name": "trusty-tools-38",
    "status": "busy"
  }
]"#;

/// The command inputs every builder test shares.
fn inputs(cwd: &Path) -> RelaunchInputs<'_> {
    RelaunchInputs {
        cwd,
        claude_bin: "/usr/local/bin/claude",
        config_dir: None,
        session_id: TEST_SESSION_ID,
        prompt_file: None,
        oauth_token: None,
        gh_env_file: None,
        mcp_env: &[],
    }
}

/// Why (#6863): this is the whole defect. A `claude_session_id` Claude Code is
/// still running as a background job must produce `attach <short-id>` — the
/// only re-entry it offers — and must NOT produce `--resume`, which it refuses
/// with a status-0 exit that leaves the pane a bare shell.
#[test]
fn relaunch_attaches_to_a_live_background_session() {
    let cwd = Path::new(TEST_CWD);
    let cmd = relaunch_command(&inputs(cwd), Some(LIVE_UUID), Some(LIVE_UUID), || {
        Ok(REGISTRY_SAMPLE.to_owned())
    });
    assert!(
        cmd.contains(&format!("attach {LIVE_SHORT}")),
        "a live background session must be attached: {cmd}"
    );
    assert!(
        !cmd.contains("--resume"),
        "a live background session must never be sent --resume: {cmd}"
    );
}

/// Why (#6863): the probe must not change the answer for the ordinary case. An
/// id that is on disk but absent from the live registry is a FINISHED
/// conversation, and `--resume <uuid>` is exactly right for it.
#[test]
fn relaunch_resumes_a_session_absent_from_the_registry() {
    let cwd = Path::new(TEST_CWD);
    let cmd = relaunch_command(&inputs(cwd), Some(STALE_UUID), Some(STALE_UUID), || {
        Ok(REGISTRY_SAMPLE.to_owned())
    });
    assert!(
        cmd.contains(&format!("--resume {STALE_UUID}")),
        "an unlisted id must resume by id: {cmd}"
    );
    assert!(
        !cmd.contains("attach"),
        "an unlisted id must not be attached: {cmd}"
    );
}

/// Why (#6863): the probe fails OPEN. A missing `claude`, a non-zero exit, or a
/// timeout must leave the pre-#6863 behavior exactly as it was — the probe can
/// only ever fix a refused relaunch, never break a working one.
#[test]
fn relaunch_resumes_when_the_registry_call_fails() {
    let cwd = Path::new(TEST_CWD);
    let cmd = relaunch_command(&inputs(cwd), Some(LIVE_UUID), Some(LIVE_UUID), || {
        Err("`claude agents --json` did not answer within 3s".to_owned())
    });
    assert!(
        cmd.contains(&format!("--resume {LIVE_UUID}")),
        "an unreadable registry must fall back to --resume: {cmd}"
    );
    assert!(
        !cmd.contains("attach"),
        "an unreadable registry must not attach: {cmd}"
    );
}

/// Why (#6765, #6863): no stored id still means a FRESH launch — the probe adds
/// no third way to guess at a conversation.
#[test]
fn relaunch_starts_fresh_without_a_usable_id() {
    let cwd = Path::new(TEST_CWD);
    let cmd = relaunch_command(&inputs(cwd), None, None, || Ok(REGISTRY_SAMPLE.to_owned()));
    assert!(
        !cmd.contains("--resume"),
        "fresh launch, no --resume: {cmd}"
    );
    assert!(!cmd.contains("attach"), "fresh launch, no attach: {cmd}");
    assert!(
        !cmd.contains("--continue"),
        "never a bare --continue: {cmd}"
    );
}

/// Why (#6863): with no stored id there is nothing to look up, and the probe
/// spawns the operator's real `claude` — which this module's docs forbid a test
/// to do (#4255) and which cost ~20 s on every fresh-launch test. The closure
/// must stay uncalled, not merely ignored.
#[test]
fn relaunch_never_probes_the_registry_without_a_session_id() {
    let cwd = Path::new(TEST_CWD);
    let probed = std::cell::Cell::new(false);
    let cmd = relaunch_command(&inputs(cwd), None, None, || {
        probed.set(true);
        Ok(REGISTRY_SAMPLE.to_owned())
    });
    assert!(
        !probed.get(),
        "the registry must not be read without a claude_session_id: {cmd}"
    );
}

/// Why (#6863): `claude attach` takes the SHORT id, not the UUID a tm record
/// stores, so the registry is the translation as well as the liveness answer.
#[test]
fn attach_id_found_for_a_live_background_entry() {
    assert_eq!(
        attach_id_in_registry(REGISTRY_SAMPLE, LIVE_UUID).as_deref(),
        Some(LIVE_SHORT)
    );
}

/// Why: an id nobody is running is a resume, not an attach.
#[test]
fn attach_id_absent_for_an_unlisted_session() {
    assert_eq!(attach_id_in_registry(REGISTRY_SAMPLE, STALE_UUID), None);
}

/// Why: `--all` is never passed, but a terminal entry can still appear in the
/// active listing, and attaching to a finished session is not what the operator
/// asked for.
#[test]
fn attach_id_absent_for_a_finished_background_entry() {
    for finished in [
        "8ec01a01-3a77-4d7c-b588-456fac3100dd",
        "a816af2c-818a-4dfa-a2c8-463bdd4eb694",
    ] {
        assert_eq!(
            attach_id_in_registry(REGISTRY_SAMPLE, finished),
            None,
            "a done/failed entry must not be attached: {finished}"
        );
    }
}

/// Why (#6863): `claude stop <short-id>` ends the background job, so there is no
/// process left for `attach` to enter — but the transcript is still on disk, so
/// `--resume <uuid>` works. Before this the predicate treated every state but
/// `done`/`failed` as live and would have attached to it.
#[test]
fn attach_id_absent_for_a_stopped_background_entry() {
    assert_eq!(
        attach_id_in_registry(REGISTRY_SAMPLE, STOPPED_UUID),
        None,
        "a stopped entry must resume, never attach"
    );
}

/// Why (#6863): the same rule read from the seam the resume path actually
/// calls — a stopped id must reach the pane as `--resume <uuid>`.
#[test]
fn relaunch_resumes_a_stopped_background_session() {
    let cwd = Path::new(TEST_CWD);
    let cmd = relaunch_command(&inputs(cwd), Some(STOPPED_UUID), Some(STOPPED_UUID), || {
        Ok(REGISTRY_SAMPLE.to_owned())
    });
    assert!(
        cmd.contains(&format!("--resume {STOPPED_UUID}")),
        "a stopped session must resume by id: {cmd}"
    );
    assert!(
        !cmd.contains("attach"),
        "a stopped session must not be attached: {cmd}"
    );
}

/// Why: an `interactive` entry is a live TTY Claude Code. `attach` cannot take
/// one over, and the entry carries no short id to attach to in any case.
#[test]
fn attach_id_absent_for_an_interactive_entry() {
    assert_eq!(
        attach_id_in_registry(REGISTRY_SAMPLE, "e5e08169-b616-4b97-870d-08535eb2f4bc"),
        None
    );
}

/// Why: a `claude` release that changes the `--json` shape, or an empty read,
/// must degrade to `--resume` rather than panic on a hot path.
#[test]
fn attach_id_absent_for_unparsable_json() {
    for junk in ["", "not json", "{}", "[{\"sessionId\": 7}]"] {
        assert_eq!(
            attach_id_in_registry(junk, LIVE_UUID),
            None,
            "unparsable registry must yield no attach id: {junk:?}"
        );
    }
}

/// Why (#6863): the sample is a real capture, so parsing it end to end is what
/// proves the `Option` fields match the live shape — an entry missing `id` and
/// `state` (interactive) must not poison the whole array, and the `pid` /
/// `status` / `cwd` / `name` / `startedAt` fields the decision ignores must not
/// be required either.
#[test]
fn registry_sample_parses_every_entry_shape() {
    // Every UUID in the sample resolves to a decision without a parse failure;
    // exactly one of them is the live background entry.
    let uuids = [
        "8ec01a01-3a77-4d7c-b588-456fac3100dd",
        "a816af2c-818a-4dfa-a2c8-463bdd4eb694",
        "1905c26f-c66e-4081-8c93-b612090ca104",
        STOPPED_UUID,
        LIVE_UUID,
        "e5e08169-b616-4b97-870d-08535eb2f4bc",
    ];
    let attachable: Vec<String> = uuids
        .iter()
        .filter_map(|u| attach_id_in_registry(REGISTRY_SAMPLE, u))
        .collect();
    assert_eq!(
        attachable,
        vec![
            "1905c26f".to_owned(),
            LIVE_SHORT.to_owned(),
            // `blocked` is a live state — a background session waiting on input
            // still refuses `--resume`.
        ],
        "only the non-terminal background entries are attachable"
    );
}

/// Why (#6863): the probe is the module's only real spawn, and it runs on the
/// interactive resume path. A `claude` that never answers must cost the deadline
/// and nothing more — before this fix the reader thread owned the child and the
/// deadline merely abandoned it, leaving a probe process alive indefinitely.
/// A stand-in script wedges the way a hung `claude` would; the fake binary path
/// keeps the test off the operator's real session state (#4255).
#[test]
#[cfg(unix)]
fn query_registry_kills_and_reaps_a_wedged_probe() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("pid");
    let script = dir.path().join("wedged-claude");
    // `exec` so the recorded pid IS the long sleep, not a shell wrapper.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 300\n",
            pidfile.display()
        ),
    )
    .expect("write stand-in claude");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let started = std::time::Instant::now();
    let err = super::query_registry(script.to_str().expect("utf-8 tempdir path"), None)
        .expect_err("a wedged probe must report a timeout, not hang");
    assert!(
        err.contains("did not answer within"),
        "the timeout must be the reported reason: {err}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the probe must return on its own deadline, took {:?}",
        started.elapsed()
    );

    let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
        .expect("the stand-in must have recorded its pid before wedging")
        .trim()
        .parse()
        .expect("pid file must hold a pid");
    // SAFETY: signal 0 performs the existence/permission check only.
    let alive = unsafe { libc::kill(pid, 0) };
    assert_eq!(
        alive, -1,
        "no probe child may outlive the deadline (pid {pid} still exists)"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH),
        "the probe child must be gone, not merely unsignalable"
    );
}

/// Why (#6766, #3025, #2023, #2250): attaching must keep every wrapper the
/// resume path carries, or the attached pane loses the `cd`, the managed
/// session id, the launch clock, or the on-exit report.
#[test]
fn attach_command_carries_the_resume_prefixes() {
    let cwd = Path::new(TEST_CWD);
    let cmd = attach_command(&inputs(cwd), LIVE_SHORT);
    assert!(cmd.starts_with("cd '/tmp/ws' && {"), "rooted at cwd: {cmd}");
    assert!(
        cmd.contains(&format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'")),
        "exports the managed session id: {cmd}"
    );
    assert!(
        cmd.contains("env -u ANTHROPIC_API_KEY"),
        "keeps the env scrub: {cmd}"
    );
    assert!(
        cmd.contains("__tm_t0"),
        "keeps the #6766 launch clock: {cmd}"
    );
    assert!(
        cmd.contains(&format!("/usr/local/bin/claude attach {LIVE_SHORT}")),
        "attaches by short id: {cmd}"
    );
}

/// Why (#6863): `claude attach <id>` accepts no options at all — passing the
/// resume path's flags would make it reject the invocation outright.
#[test]
fn attach_command_omits_flags_attach_cannot_take() {
    let cwd = Path::new(TEST_CWD);
    let prompt = Path::new("/tmp/prompt.md");
    let mut with_prompt = inputs(cwd);
    with_prompt.prompt_file = Some(prompt);
    let cmd = attach_command(&with_prompt, LIVE_SHORT);
    for flag in [
        "--append-system-prompt-file",
        "--setting-sources",
        "--permission-mode",
        "--resume",
    ] {
        assert!(!cmd.contains(flag), "attach must not carry {flag}: {cmd}");
    }
}
