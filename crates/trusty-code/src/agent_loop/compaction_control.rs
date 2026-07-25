//! `AgentLoop`'s two turn-boundary compaction entry points. Split out of
//! `mod.rs` (issues #3867/#3868) purely to keep that production file under
//! the crate's 500-SLOC cap as the compression-telemetry instrumentation
//! grew it past the limit — this is a child module of `agent_loop`
//! (declared via `#[path = ...] mod compaction_control;` in `mod.rs`), so it
//! shares full access to `AgentLoop`'s private fields exactly as if these
//! methods were still defined in that file. Mirrors the same split already
//! used for `session::registry`/`registry_events.rs` and
//! `session::protocol`/`protocol_budget.rs`.
//! What: [`AgentLoop::maybe_compact_transcript`] (the #2308 threshold
//! compactor's turn-boundary call, plus its #3867/#3868 telemetry) and
//! [`AgentLoop::maybe_cadence_compress`] (the #2346 cadence compressor's
//! turn-boundary call, plus its #3867 telemetry).
//! Test: `agent_loop::tests::*` (unchanged — these are still `AgentLoop`
//! methods, just physically relocated).

use super::*;

impl AgentLoop {
    /// Resolve the write target for this loop's own compression-telemetry
    /// calls: `self.config.telemetry_data_dir` when injected (test-only DI,
    /// issue #3902), otherwise `telemetry::default_data_dir()` — identical
    /// to production behaviour before this field existed.
    ///
    /// (#3902 recurrence guard) `#[cfg(test)]`-only: panics if a test is
    /// about to write real telemetry (this method is only ever called from
    /// an ACTUAL fire — see both call sites below) with neither
    /// `telemetry_data_dir` injected nor `telemetry::DATA_DIR_ENV_VAR` set.
    /// Why: DI closed the five tests that omitted isolation THIS time, but
    /// nothing stopped a SIXTH new test from making the exact same
    /// omission and reintroducing the race at ~5% flake — silently, since
    /// an un-isolated write either races a concurrently-locked test's
    /// directory (the #3902 failure mode) or lands in the real
    /// `~/.trusty-code` with no assertion to catch it. This turns that
    /// omission into a 100%-reproducing panic on the FIRST run instead of
    /// an occasional CI flake. The env-var branch is not dead: any test
    /// still using `telemetry::with_data_dir_env_fut`/`with_data_dir_env`
    /// (e.g. `session::protocol_budget`'s tests, which call
    /// `SessionRegistry::get_context_budget` — a path with no
    /// `AgentLoopConfig` to inject through — rather than driving an
    /// `AgentLoop` directly) sets the env var for the run's duration, so
    /// this guard accepts that path too. Production builds are entirely
    /// unaffected — this whole check compiles out.
    /// Test: `agent_loop::tests::compression_telemetry_tests::*`,
    /// `agent_loop::tests::cadence::*`,
    /// `agent_loop::budget_tests::cadence_emits_context_budget_snapshot`,
    /// `agent_loop::tests::daily_driver_mode_compacts_long_running_loop` —
    /// every one of these now injects `telemetry_data_dir` and must keep
    /// passing under this guard; a run with the injection removed from any
    /// of them must panic here rather than flake.
    fn telemetry_data_dir(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if self.config.telemetry_data_dir.is_none()
            && std::env::var(telemetry::DATA_DIR_ENV_VAR).is_err()
        {
            panic!(
                "agent_loop: about to write real compression telemetry with no isolated data \
                 dir (issue #3902) — inject `AgentLoopConfig::telemetry_data_dir` (see \
                 `telemetry::test_temp_dir`) before triggering a real cadence/threshold fire in \
                 a test, or wrap the run in `telemetry::with_data_dir_env_fut`/`with_data_dir_env`. \
                 An un-isolated write here either races a concurrently-locked test's directory or \
                 lands in the real ~/.trusty-code on whatever machine runs this test."
            );
        }
        self.config
            .telemetry_data_dir
            .clone()
            .unwrap_or_else(telemetry::default_data_dir)
    }

    /// Apply non-destructive context compaction to `transcript`, gated on mode
    /// (#2070, §5.4; reconciled with §5.9's D2 the same way `tool_definitions`
    /// is).
    ///
    /// Why: The parity-spec (D2) requires `HarnessMode::Parity` runs to stay
    /// byte-identical / full-history for cross-model benchmark fairness —
    /// compacting would silently change what a Parity run sends after enough
    /// turns, breaking that guarantee. `HarnessMode::DailyDriver` is
    /// production's token-efficiency mode, so it is the only mode this method
    /// ever calls `Transcript::maybe_compact` for. (#2349, epic #2343) Under
    /// the #2346 cadence system a threshold fire is no longer expected in
    /// steady state — it means cadence sizing / daemon-availability /
    /// turn-size assumptions were violated — so this is also where that
    /// regression signal is raised. Cadence-DISABLED contexts (`run_task`
    /// one-shot, `Parity`, delegated sub-agent engineer loops — all
    /// `cadence: None`) still use threshold compaction as their PRIMARY,
    /// EXPECTED mechanism, so the error-level framing below must never apply
    /// to them.
    /// What: No-op under `HarnessMode::Parity`; under `HarnessMode::DailyDriver`,
    /// delegates to `transcript.maybe_compact(&self.config.compaction)`. When
    /// that returns `true` (a fire actually happened), unconditionally calls
    /// `transcript.record_threshold_compaction()` (#2349's fact counter,
    /// exposed via `session::TranscriptRecord.compaction_events` —
    /// increments regardless of cadence), then — ONLY when
    /// `self.config.cadence.is_some()` — emits `tracing::error!` with the
    /// current estimated token count, the configured `token_threshold`, and
    /// `cadence_enabled = true` for context. `cadence: None` keeps today's
    /// existing behaviour exactly as it was before this ticket: no log at
    /// this call site at all. (#3867/#3868) ALSO — regardless of cadence —
    /// writes a durable `telemetry::record_threshold_event` JSONL record
    /// (`compaction_event: true`), and, only when `cadence_enabled`, an
    /// alongside durable alarm line (the never-event signal
    /// `session.get_context_budget`'s `lifetime_compaction_alarm_count`
    /// derives from).
    /// Test: `agent_loop::tests::daily_driver_mode_compacts_long_running_loop`,
    /// `agent_loop::tests::parity_mode_never_compacts_even_past_threshold`,
    /// `agent_loop::tests::cadence::forced_degradation_increments_counter_and_logs_error`
    /// (asserts the ERROR log AND both durable signals from one fire),
    /// `agent_loop::tests::cadence::cadence_none_threshold_fire_does_not_log_error`,
    /// `agent_loop::tests::compression_telemetry_tests::threshold_fire_under_cadence_writes_jsonl_and_alarm`,
    /// `agent_loop::tests::compression_telemetry_tests::threshold_fire_under_no_cadence_writes_jsonl_but_no_alarm`,
    /// `agent_loop::tests::compression_telemetry_tests::unwritable_data_dir_does_not_fail_the_loop`.
    pub(super) fn maybe_compact_transcript(&self, transcript: &mut Transcript) {
        if self.config.mode != HarnessMode::DailyDriver {
            return;
        }
        let started = Instant::now();
        let tokens_before = compaction::estimate_total_tokens(&transcript.to_messages());
        if !transcript.maybe_compact(&self.config.compaction) {
            return;
        }
        transcript.record_threshold_compaction();

        let estimated_tokens = compaction::estimate_total_tokens(&transcript.to_messages());
        let cadence_enabled = self.config.cadence.is_some();
        telemetry::record_threshold_event(
            &self.telemetry_data_dir(),
            self.session_id.clone(),
            tokens_before,
            estimated_tokens,
            crate::provider::resolve_context_window(&self.config.model),
            started.elapsed().as_millis() as u64,
            cadence_enabled,
        );

        if cadence_enabled {
            tracing::error!(
                estimated_tokens,
                token_threshold = self.config.compaction.token_threshold,
                cadence_enabled = true,
                "threshold compaction fired unexpectedly under cadence — this is a regression \
                 signal: cadence sizing, daemon availability, or turn-size assumptions were \
                 likely violated (epic #2343 expects compaction_events == 0 in steady state)"
            );
        }
    }

    /// Apply the #2346 cadence compressor to `transcript`, gated on mode the
    /// SAME way [`Self::maybe_compact_transcript`] is, plus an explicit
    /// per-run opt-in.
    ///
    /// Why: Cadence must respect the identical `HarnessMode::Parity` carve-out
    /// as threshold compaction (byte-identical benchmark runs), AND must stay
    /// off by default (`self.config.cadence == None`) so every call site that
    /// doesn't explicitly opt in — the delegated engineer's own loop,
    /// `run_task`'s legacy one-shot/bake-off path — sees zero behavioural
    /// change from before this ticket. Only
    /// `task::executor::run_and_record`'s persistent-session PM loop sets
    /// `AgentLoopConfig.cadence = Some(_)`.
    /// What: No-op unless `mode == HarnessMode::DailyDriver` AND
    /// `self.config.cadence` is `Some`; otherwise resolves the model's real
    /// context window (`crate::provider::resolve_context_window`, mirroring
    /// `Self::maybe_compact_transcript`'s own model-aware sibling) and
    /// delegates to `cadence::maybe_cadence_compress`, reusing
    /// `self.config.compaction.keep_last_messages` as the shared active-zone
    /// size rather than introducing a second, possibly-inconsistent knob.
    /// (#2857) `cadence::maybe_cadence_compress`'s returned `CadenceOutcome`
    /// was previously discarded entirely — its own doc says it exists "for
    /// observability/tests", yet nothing observed it. When `outcome.fired`,
    /// this now emits `tracing::info!` with the round count and whether the
    /// transcript ended within budget — a notable-but-normal event (routine
    /// scheduled compression), not a failure, hence `info` rather than
    /// `warn`. The budget-floor-exceeded case is already `warn!`-logged
    /// inside `cadence::enforce_budget` itself. The SAME outcome is also
    /// forwarded to `self.sink` (when one is attached) as a
    /// [`ContextBudgetSnapshot`], so a `session.attach`ed UI can render a live
    /// budget indicator. Emission sits AFTER both guards above, which is what
    /// keeps it PM-only for free: only `task::executor::run_and_record`'s
    /// persistent-session PM loop sets `cadence: Some(_)`, so a delegated
    /// engineer loop never emits. (#3867) ALSO — only when
    /// `outcome.rounds > 0` (compaction work actually happened this turn) —
    /// writes a durable `telemetry::record_cadence_event` JSONL record.
    /// Test: `agent_loop::tests::cadence_disabled_by_default`,
    /// `agent_loop::tests::cadence_never_fires_in_parity_mode`,
    /// `agent_loop::tests::cadence_fires_in_daily_driver_when_configured`,
    /// `agent_loop::tests::cadence_fire_logs_info`,
    /// `agent_loop::tests::cadence_emits_context_budget_snapshot`,
    /// `agent_loop::tests::no_context_budget_event_when_cadence_disabled`,
    /// `agent_loop::tests::compression_telemetry_tests::cadence_fire_writes_compression_telemetry`,
    /// `agent_loop::tests::compression_telemetry_tests::cadence_disabled_writes_no_cadence_telemetry`.
    pub(super) async fn maybe_cadence_compress(&self, transcript: &mut Transcript) {
        if self.config.mode != HarnessMode::DailyDriver {
            return;
        }
        let Some(cadence_cfg) = &self.config.cadence else {
            return;
        };
        let started = Instant::now();
        let tokens_before = compaction::estimate_total_tokens(&transcript.to_messages());
        let context_window = crate::provider::resolve_context_window(&self.config.model);
        let outcome = cadence::maybe_cadence_compress(
            transcript,
            cadence_cfg,
            self.config.compaction.keep_last_messages,
            context_window,
        );
        if outcome.fired {
            tracing::info!(
                rounds = outcome.rounds,
                within_budget = outcome.within_budget,
                "agent_loop: cadence compression fired (#2346)"
            );
        }

        if outcome.rounds > 0 {
            telemetry::record_cadence_event(
                &self.telemetry_data_dir(),
                self.session_id.clone(),
                tokens_before,
                outcome.overhead_tokens,
                context_window,
                started.elapsed().as_millis() as u64,
                outcome.rounds,
            );
        }

        let Some(sink) = &self.sink else {
            return;
        };
        sink.context_budget(&ContextBudgetSnapshot {
            context_window,
            overhead_tokens: outcome.overhead_tokens,
            overhead_cap_tokens: cadence_cfg.overhead_cap_tokens(context_window),
            working_context_pct: outcome.working_context_pct(context_window),
            overhead_pct: outcome.overhead_pct(context_window),
            within_budget: outcome.within_budget,
            fired: outcome.fired,
            rounds: outcome.rounds,
        })
        .await;
    }
}
