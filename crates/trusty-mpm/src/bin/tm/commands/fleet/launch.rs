//! Start the Architect's session detached, and read it back (#8436).
//!
//! Why: `tm connect` names its tmux session through the daemon and then takes
//! over the terminal. The Architect needs a fixed name — the P1 poller sends
//! keys to `ARCHITECT_SESSION`, default `tm-architect` — and `tm fleet init`
//! must return, also when a PM runs it.
//! What: [`start`] runs `tm connect`'s preparation seams (framework deploy,
//! config-dir relocation, prompt, scoped MCP), refuses unless the profile
//! resolves to supervisor, creates the named session (`tm-architect` unless
//! `--session` chose another, #8878 R1) detached in the project and types the
//! `claude` line on the `opus` alias. It records the launch stamp in the tmux
//! session environment, where [`launch_stamp`] reads it, and records the
//! `claude` it started, with the session name, as the Architect's process
//! ([`record_process`], #8878 ruling A). #8981: the `claude` line resumes the
//! Architect's recorded conversation when one checks out, and carries
//! `--remote-control` ([`architect_args`]). A resumed `claude` proven gone
//! within [`RESUME_SETTLE`] is a failed resume: the session is killed, the
//! conversation record cleared, and the launch fails, so the next run starts
//! fresh. A pane that cannot be read twice keeps both, with a warning
//! ([`resume_check`]). Daemon registration is not here: `fleet::run`
//! registers a bound Architect after the poller step (`register`, #8942).
//! Test: `fleet_init_launches_the_architect_and_status_is_complete`,
//! `fleet_init_resumes_the_prior_conversation_after_the_record_is_gone`,
//! `fleet_init_clears_the_conversation_record_when_the_resume_fails`,
//! `fleet_init_keeps_the_resumed_architect_when_its_pane_cannot_be_read`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use trusty_mpm::core::architect_conversation::{self, ConversationStart};
use trusty_mpm::core::architect_session;
use trusty_mpm::core::process::PaneClaude;
use trusty_mpm::core::session_profile::{self, SESSION_PROFILE_ENV};
use trusty_mpm::core::tmux::{self, TmuxCommand};
use trusty_mpm::core::twin_identity::ArmingRecord;

/// The Architect's default tmux session; the P1 poller's `ARCHITECT_SESSION`
/// default. `--session` overrides it (#8878 R1).
pub(crate) const ARCHITECT_SESSION: &str = architect_session::DEFAULT_ARCHITECT_SESSION;

/// How long a resumed `claude` must stay up before the resume counts (#8981).
///
/// A `claude --resume` that cannot load its conversation exits within a
/// second or two; a resume that dies later is caught on the next run.
pub(crate) const RESUME_SETTLE: Duration = Duration::from_secs(5);

/// The pause before [`resume_check`] asks an unreadable pane once more.
const REPROBE: Duration = Duration::from_millis(500);

/// What tmux reports about one session and its first pane (#8436 P4 fix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneState {
    /// No session of that exact name runs, or no tmux server runs.
    Absent,
    /// The session runs in this directory and its pane's process is alive.
    Live(PathBuf),
    /// The session runs in this directory but its pane's process exited; a
    /// `remain-on-exit on` pane stays after a crash.
    Dead(PathBuf),
    /// tmux could not be asked, or gave an answer that cannot be read.
    Unknown(String),
}

impl PaneState {
    /// The session's directory when the session exists, live pane or dead.
    pub(crate) fn dir(self) -> Option<PathBuf> {
        match self {
            Self::Live(dir) | Self::Dead(dir) => Some(dir),
            Self::Absent | Self::Unknown(_) => None,
        }
    }
}

/// tmux session `name`'s directory and whether its first pane is dead.
///
/// Why: status and the one-Architect check need to know whether the running
/// session is THIS project's; the poller check also needs to know that the
/// process in it still runs, because a session alone reads a crashed poller
/// as running.
/// What: one `display-message -p -t =<name>: '#{pane_dead} #{session_path}'`.
/// Empty output, or a failure because no server runs, is [`PaneState::Absent`];
/// any other failure or answer is [`PaneState::Unknown`]. The path is
/// canonicalized when it can be.
/// Test: `a_pane_answer_parses_to_its_state`,
/// `fleet_init_fails_when_the_poller_pane_is_dead`.
pub(crate) fn pane_state(name: &str) -> PaneState {
    let argv = [
        "display-message",
        "-p",
        "-t",
        &tmux::exact_window_target(name),
        "#{pane_dead} #{session_path}",
    ]
    .map(str::to_owned);
    let out = match tmux::run_tmux_argv(&argv) {
        Ok(out) => out,
        Err(err) => return PaneState::Unknown(format!("cannot run tmux: {err}")),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if ["no server running", "error connecting to"]
            .iter()
            .any(|m| err.contains(m))
        {
            return PaneState::Absent;
        }
        return PaneState::Unknown(format!("tmux display-message failed: {err}"));
    }
    parse_pane(&String::from_utf8_lossy(&out.stdout))
}

/// Parse [`pane_state`]'s `<pane_dead> <session_path>` answer.
pub(crate) fn parse_pane(answer: &str) -> PaneState {
    let answer = answer.trim_end_matches(['\n', '\r']);
    if answer.trim().is_empty() {
        return PaneState::Absent;
    }
    let canonical = |path: &str| {
        let path = PathBuf::from(path);
        std::fs::canonicalize(&path).unwrap_or(path)
    };
    match answer.split_once(' ') {
        Some(("0", path)) if !path.is_empty() => PaneState::Live(canonical(path)),
        Some(("1", path)) if !path.is_empty() => PaneState::Dead(canonical(path)),
        _ => PaneState::Unknown(format!("unreadable tmux answer {answer:?}")),
    }
}

/// The profile stamp recorded on the running session `name`, if any.
///
/// Why: the stamp itself lives in the `claude` process environment, which
/// `tm` cannot read portably; [`start`] records the same value in the tmux
/// session environment table.
/// What: `show-environment -t =<name> TRUSTY_MPM_SESSION_PROFILE`,
/// parsed from `KEY=value`; `None` when unset or no session runs.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
pub(crate) fn launch_stamp(name: &str) -> Option<String> {
    let argv = tmux::show_environment_argv(Some(name), SESSION_PROFILE_ENV);
    let out = tmux::run_tmux_argv(&argv).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .strip_prefix(SESSION_PROFILE_ENV)
        .and_then(|rest| rest.strip_prefix('='))
        .map(str::to_owned)
}

/// The `claude` PID in session `name`: one probe, no retry (#8878 R1).
///
/// Test: `fleet_init_with_a_session_override_names_every_session`.
pub(crate) fn claude_pid(name: &str) -> Option<u32> {
    trusty_mpm::core::process::find_claude_pid_in_tmux(name, 1, std::time::Duration::ZERO)
}

/// Start the Architect's session in `dir`, detached.
///
/// Why: acceptance 1 of #8436 — a running supervisor session on the Opus
/// tier alias, with no manual follow-up.
/// What: see the module doc. Fails before creating the session when the
/// profile would resolve to PM; kills the session when the `claude` line
/// cannot be sent. `home` is the user home every write goes under; `session`
/// is the validated session name. Returns the [`record_process`] outcome and
/// how the conversation started (#8981). A failed resume ([`resume_check`])
/// kills the session, clears the conversation record and is `Err`.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`,
/// `fleet_init_with_a_session_override_names_every_session`,
/// `fleet_init_resumes_the_prior_conversation_after_the_record_is_gone`,
/// `fleet_init_clears_the_conversation_record_when_the_resume_fails`.
pub(crate) fn start(
    dir: &Path,
    home: &Path,
    session: &str,
) -> anyhow::Result<(Result<ArmingRecord, String>, ConversationStart)> {
    require_supervisor(dir, session)?;
    match trusty_mpm::core::session_launch::prepare_isolated_session_under(dir, None, Some(home)) {
        Ok(report) => {
            for err in &report.roster_errors {
                eprintln!(
                    "warning: roster provisioning gap for {}: {err}",
                    dir.display()
                );
            }
        }
        Err(err) if err.is_fatal() => bail!("{err}"),
        Err(err) => eprintln!(
            "warning: session prep failed for {} (non-fatal): {err}",
            dir.display()
        ),
    }
    let config_dir = crate::commands::launch_home::relocate_config_dir(dir, Some(home));
    let cli = trusty_mpm::core::session_launch::cli_launch(dir, None);
    if !cli.profile.is_supervisor() {
        bail!("{}", pm_refusal(dir, session));
    }
    let config = trusty_mpm::core::config::MpmConfig::load_effective_default(Some(dir));
    let model = session_profile::launch_model(cli.profile, &config);
    let prompt = trusty_mpm::core::model_inject::write_prompt_file(&cli.prompt)
        .context("failed to write the Architect's system prompt file")?;
    let scoped_mcp = crate::commands::launch_home::provision_session_mcp(
        dir,
        config_dir.as_deref(),
        Some(home),
    )?;
    // #8308: the launch travels in a spec under `home`; the pane types a short line.
    let mut claude_spec = crate::commands::launch::launch_claude_spec(
        dir,
        trusty_mpm::core::alt_screen::operator_config_root().as_deref(),
        &model,
        Some(prompt.as_path()),
        config_dir.as_deref(),
        &cli.env,
        scoped_mcp.as_deref(),
    );
    // #8981: the conversation id comes from tm's own record, which outlives
    // the daemon's session record.
    let root = home.join(".trusty-mpm");
    let conversation =
        architect_conversation::resolve_conversation(&root, dir, config_dir.as_deref());
    claude_spec.args.extend(architect_args(&conversation));
    let spec_dir = crate::commands::launch::launch_spec_dir(Some(home))?;
    let workdir = dir
        .to_str()
        .context("the Architect directory is not valid UTF-8")?;
    let created = tmux::create_managed_session_exclusive(None, session, Some(workdir))
        .with_context(|| format!("failed to run tmux to create {session}"))?;
    if !created.output.status.success() {
        bail!(
            "tmux refused to create {session}: {}",
            String::from_utf8_lossy(&created.output.stderr).trim()
        );
    }
    if let Err(err) = stamp_and_send(&cli.profile, &claude_spec, &spec_dir, session) {
        let _ = tmux::run_tmux(&TmuxCommand::KillSession {
            name: session.to_owned(),
        });
        return Err(err);
    }
    // #8981: a fresh conversation's id is recorded only once `claude` was
    // started under it; a failed write means the next launch starts fresh.
    if let ConversationStart::Fresh { id, .. } = &conversation
        && let Err(err) = architect_conversation::record_conversation(&root, dir, id)
    {
        eprintln!(
            "warning: conversation {id} was NOT recorded, so the next `tm fleet init` \
             starts fresh: {err}"
        );
    }
    let binding = record_process(dir, home, session);
    // #8981 critic HIGH: a dead resume must not be retried by every later run.
    if let ConversationStart::Resume(id) = &conversation {
        let check = resume_check(
            &binding,
            || trusty_mpm::core::process::pane_claude(session),
            std::thread::sleep,
        );
        settle_resume(check, &root, id, session)?;
    }
    Ok((binding, conversation))
}

/// What [`resume_check`] found in the resumed Architect's pane (#8981).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeCheck {
    /// A `claude` runs in the pane.
    Running,
    /// Proven: no `claude` runs in the pane, for the reason given.
    Failed(String),
    /// tmux or the process table could not be read, twice.
    Unknown,
}

/// Whether the resumed `claude` in the session is running (#8981 critic MEDIUM).
///
/// Why: `find_claude_pid_in_tmux` answers `None` both for "no `claude`" and
/// for "tmux or ps could not be read", and the old check killed a live
/// Architect and dropped its record on the second.
/// What: `probe` is a three-state [`PaneClaude`] read; an `Unknown` answer is
/// asked once more after [`REPROBE`]. When [`record_process`] found no
/// `claude` (`binding` is `Err`), an `Absent` pane fails with that reason.
/// Otherwise the pane is read again after [`RESUME_SETTLE`]: `Absent` fails,
/// `Present` runs. Only a proven `Absent` is [`ResumeCheck::Failed`]; two
/// `Unknown` answers are [`ResumeCheck::Unknown`].
/// Test: `a_resume_fails_only_on_a_proven_absent_claude`.
pub(crate) fn resume_check(
    binding: &Result<ArmingRecord, String>,
    mut probe: impl FnMut() -> PaneClaude,
    mut sleep: impl FnMut(Duration),
) -> ResumeCheck {
    let mut read = |sleep: &mut dyn FnMut(Duration)| match probe() {
        PaneClaude::Unknown => {
            sleep(REPROBE);
            probe()
        }
        seen => seen,
    };
    if let Err(why) = binding {
        match read(&mut sleep) {
            PaneClaude::Absent => return ResumeCheck::Failed(why.clone()),
            PaneClaude::Unknown => return ResumeCheck::Unknown,
            PaneClaude::Present => {}
        }
    }
    sleep(RESUME_SETTLE);
    match read(&mut sleep) {
        PaneClaude::Present => ResumeCheck::Running,
        PaneClaude::Unknown => ResumeCheck::Unknown,
        PaneClaude::Absent => ResumeCheck::Failed(format!(
            "`claude` exited within {} s of starting",
            RESUME_SETTLE.as_secs()
        )),
    }
}

/// Act on [`resume_check`]'s answer for conversation `id` in `session`.
///
/// What: `Running` is `Ok`. `Unknown` keeps the session and the record and
/// warns on stderr, naming both. `Failed` kills the session, clears the
/// record and is `Err`, saying whether each step worked.
/// Test: `fleet_init_clears_the_conversation_record_when_the_resume_fails`,
/// `fleet_init_keeps_the_resumed_architect_when_its_pane_cannot_be_read`,
/// `fleet_init_says_so_when_the_failed_resume_session_cannot_be_killed`.
fn settle_resume(check: ResumeCheck, root: &Path, id: &str, session: &str) -> anyhow::Result<()> {
    let path = architect_conversation::conversation_path(root);
    let why = match check {
        ResumeCheck::Running => return Ok(()),
        ResumeCheck::Unknown => {
            eprintln!(
                "warning: could not tell whether the resumed `claude` (conversation {id}) runs \
                 in tmux session {session}: tmux or the process table could not be read; kept \
                 the session and the conversation record {}",
                path.display()
            );
            return Ok(());
        }
        ResumeCheck::Failed(why) => why,
    };
    // #8981 critic LOW: a kill that failed must not be reported as done.
    let killed = match kill_session(session) {
        Ok(()) => format!("killed tmux session {session}"),
        Err(err) => format!(
            "could NOT kill tmux session {session}; run `tmux kill-session -t ={session}` \
             ({err})"
        ),
    };
    let record = match architect_conversation::clear_conversation(root) {
        Ok(()) => format!(
            "cleared {}, so the next `tm fleet init` starts a new conversation",
            path.display()
        ),
        Err(err) => format!(
            "could NOT clear {} ({err}); remove it, or the next `tm fleet init` resumes {id} \
             again",
            path.display()
        ),
    };
    bail!("resuming the Architect's conversation {id} failed ({why}); {killed}, and {record}")
}

/// Kill tmux session `session`; `Err` says why it may still run.
fn kill_session(session: &str) -> Result<(), String> {
    let out = tmux::run_tmux(&TmuxCommand::KillSession {
        name: session.to_owned(),
    })
    .map_err(|e| format!("cannot run tmux: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "tmux kill-session failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The arguments the Architect's `claude` line adds for #8981.
///
/// Why: the owner (2026-10-01): "We should not require a remote session" —
/// no `tmux attach` and `/remote-control` after a relaunch.
/// What: the conversation's `--resume <id>` or `--session-id <id>`, then a
/// bare `--remote-control` last, so its optional name never takes a
/// following argument.
/// Test: `the_architect_line_resumes_and_enables_remote_control`.
pub(crate) fn architect_args(conversation: &ConversationStart) -> Vec<String> {
    let mut args = conversation.claude_args().to_vec();
    args.push("--remote-control".to_owned());
    args
}

/// Bind the Architect identity to the `claude` [`start`] just launched
/// (#8878, ruling A).
///
/// Why: the trust-anchor floor trusts a process record, not the environment,
/// and only this launch path may write one.
/// What: finds the `claude` under the pane shell of the `session`
/// [`start`] created exclusively (one hop through the #2997 disclaim
/// wrapper), so the PID is always a process tm started, never the caller.
/// Then records its PID, start time and `session` under `home`'s
/// `~/.trusty-mpm` ([`architect_session::record_launch`]) and reads the name
/// back. A failure, or a name that reads back as another session (#8878 R1),
/// is reported as unbound, never fatal.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`,
/// `fleet_init_with_a_session_override_names_every_session`.
fn record_process(dir: &Path, home: &Path, session: &str) -> Result<ArmingRecord, String> {
    let pid = trusty_mpm::core::process::find_claude_pid_in_tmux(
        session,
        20,
        std::time::Duration::from_millis(500),
    )
    .ok_or_else(|| format!("no `claude` process appeared in {session} within 10 s"))?;
    let root = home.join(".trusty-mpm");
    let record = architect_session::record_launch(&root, pid, dir, session)?;
    match architect_session::architect_session_name(&root, pid) {
        Ok(name) if name == session => Ok(record),
        Ok(name) => Err(format!(
            "the launch record reads back session {name}, not {session}"
        )),
        Err(why) => Err(why.to_string()),
    }
}

/// Record the stamp on the session, then start `claude` in it from its spec.
fn stamp_and_send(
    profile: &session_profile::SessionProfile,
    claude_spec: &trusty_mpm::runtime::launch_spec::LaunchSpec,
    spec_dir: &Path,
    session: &str,
) -> anyhow::Result<()> {
    let (key, value) = session_profile::launch_env(*profile);
    let set = tmux::run_tmux(&TmuxCommand::SetEnvironment {
        session: session.to_owned(),
        key,
        value,
    })?;
    if !set.status.success() {
        bail!("failed to record the launch stamp on {session}");
    }
    // #8308: one call types the short line naming the spec.
    trusty_mpm::runtime::cli_launch::send_spec_launch(session, claude_spec, spec_dir).map_err(|e| {
        anyhow::anyhow!("{session} was created but `claude` could not be started in it: {e}")
    })
}

/// Fail unless a launch in `dir` resolves to the supervisor profile.
fn require_supervisor(dir: &Path, session: &str) -> anyhow::Result<()> {
    if session_profile::resolve_ambient(dir).is_supervisor() {
        return Ok(());
    }
    bail!("{}", pm_refusal(dir, session))
}

/// Why a launch in `dir` would run the PM profile.
fn pm_refusal(dir: &Path, session: &str) -> String {
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    let reason = session_profile::refusal(dir, &config)
        .unwrap_or("the project does not request `profile = \"supervisor\"`");
    format!(
        "not starting {session}: a launch in {} would run the PM profile ({reason})",
        dir.display()
    )
}
