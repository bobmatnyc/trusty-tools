//! Starting runs: spec building, the environment allowlist, one thread a run.
//!
//! Why: the engine's rules (see [`super::engine`]) end where a child starts;
//! this half builds what to run and hands it to the injected runner on its
//! own thread, so a slow job never delays another.
//! What: [`allowed_env`] passes only `PATH`, `HOME`, `TMUX_SOCKET`, names
//! starting `ARCHITECT_` and the names a job lists in `env`: the rest of the
//! scheduler's environment (bot tokens included) never reaches a child. A
//! script job gets its argv, `cwd` under the root and the day's job log for
//! stdout and stderr. The poll job runs `live_argv` after cutover, else the
//! shadow dry-run ([`super::shadow`]); a poll slot that finds the previous
//! cycle alive is an `overrun`.
//! Test: `env_allowlist_drops_unlisted_variables`, `script_job_runs_its_argv_in_the_job_cwd_with_a_job_log`,
//! `poll_job_shadows_until_cutover` (tests/sched_engine.rs, tests/sched_shadow.rs).

use super::agent::{self, AgentPlan};
use super::config::{Job, Kind};
use super::cutover::{self, Mode};
use super::engine::Engine;
use super::runlog::RunRecord;
use super::shadow::{self, ShadowCtx};
use super::slot::day_key;
use super::spawn::{RunResult, RunSpec};
use super::{SchedError, engine::TickReport};
use std::time::Duration;

/// The poll job's id in `runs.jsonl`.
pub const POLL_JOB: &str = "poll";

/// What a finished run still has to record.
pub(super) enum Origin {
    Plain,
    Agent(AgentPlan),
    Shadow(ShadowCtx),
}

/// A finished run, sent back to the engine.
pub(super) struct Done {
    pub id: String,
    pub slot: i64,
    pub start: i64,
    pub result: RunResult,
    pub origin: Origin,
}

/// The variables a child may see: the fixed allowlist plus `extra` names.
pub fn allowed_env(all: &[(String, String)], extra: &[String]) -> Vec<(String, String)> {
    all.iter()
        .filter(|(k, _)| {
            matches!(k.as_str(), "PATH" | "HOME" | "TMUX_SOCKET")
                || k.starts_with("ARCHITECT_")
                || extra.contains(k)
        })
        .cloned()
        .collect()
}

impl Engine<'_> {
    pub(super) fn launch(&mut self, job: &Job, slot: i64, now: i64) -> Result<(), SchedError> {
        let env = allowed_env(&self.env, &job.env);
        let spec_origin = match (job.run_kind(), &job.script) {
            (Kind::Script, Some(script)) => (
                RunSpec {
                    argv: script.argv.clone(),
                    cwd: self.layout.root.join(job.cwd.as_deref().unwrap_or("")),
                    env,
                    stdin: None,
                    stdout: self.layout.job_log(&job.id, &day_key(now)),
                    stderr: self.layout.job_log(&job.id, &day_key(now)),
                    timeout: Duration::from_secs(job.timeout()),
                },
                Origin::Plain,
            ),
            (Kind::Agent, _) => {
                let plan = agent::plan(job, &self.layout, &self.scratch_root, env, now)?;
                (plan.spec.clone(), Origin::Agent(plan))
            }
            _ => return Ok(()),
        };
        self.spawn(&job.id, slot, now, spec_origin.0, spec_origin.1)
    }

    pub(super) fn spawn(
        &mut self,
        id: &str,
        slot: i64,
        start: i64,
        spec: RunSpec,
        origin: Origin,
    ) -> Result<(), SchedError> {
        let runner = std::sync::Arc::clone(&self.runner);
        let tx = self.tx.clone();
        let id_owned = id.to_owned();
        self.running.insert(id.to_owned());
        std::thread::Builder::new()
            .name(format!("sched-{id}"))
            .spawn(move || {
                let result = runner.run(&spec);
                // The engine may be gone at shutdown; the run is recorded
                // in its job log either way.
                let _ = tx.send(Done {
                    id: id_owned,
                    slot,
                    start,
                    result,
                    origin,
                });
            })
            .map(drop)
            .map_err(|e| {
                self.running.remove(id);
                SchedError::io("start run thread", e)
            })
    }

    /// The poll job: shadow until [`cutover::read_mode`] says live.
    pub(super) fn poll_tick(&mut self, now: i64, rep: &mut TickReport) {
        let poll = self.reg.poll.clone();
        if !poll.enabled {
            return;
        }
        if self.state.poll_next == 0 {
            self.state.poll_next = now;
        }
        if now < self.state.poll_next {
            return;
        }
        self.state.poll_next = now + poll.every_s as i64;
        self.mark_dirty();
        if self.running.contains(POLL_JOB) {
            self.log_run(
                RunRecord::new(POLL_JOB, now, now, now, None, "overrun"),
                rep,
            );
            return;
        }
        let env = allowed_env(&self.env, &[]);
        let built = match cutover::read_mode(&self.layout) {
            Ok(Mode::Live) => {
                let log = self.layout.job_log(POLL_JOB, &day_key(now));
                let mut env = env;
                env.push((
                    "ARCHITECT_PROJECT_DIR".to_owned(),
                    self.layout.root.display().to_string(),
                ));
                Ok((
                    RunSpec {
                        argv: poll.live_argv.clone(),
                        cwd: self.layout.root.clone(),
                        env,
                        stdin: None,
                        stdout: log.clone(),
                        stderr: log,
                        timeout: Duration::from_secs(poll.timeout_s),
                    },
                    Origin::Plain,
                ))
            }
            Ok(Mode::Shadow) => shadow::plan(&self.layout, &poll, env, now)
                .map(|(spec, ctx)| (spec, Origin::Shadow(ctx))),
            Err(e) => Err(e),
        };
        let started = built.and_then(|(spec, origin)| self.spawn(POLL_JOB, now, now, spec, origin));
        match started {
            Ok(()) => rep.started.push(POLL_JOB.to_owned()),
            Err(e) => {
                tracing::error!(error = %e, "poll job could not start");
                self.log_run(RunRecord::new(POLL_JOB, now, now, now, None, "fail"), rep);
            }
        }
    }
}
