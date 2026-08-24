//! `tm wait` — the sanctioned in-turn wait primitive (#5843).
//!
//! Why: an agent has no guard-compliant way to wait for a long-running
//! background operation. Foreground `sleep` is rejected by the guards, and the
//! harness auto-backgrounds a foreground call near ~120s — so an agent facing a
//! measured 1h04m cold build ends its turn and loses the work. The fix is not a
//! longer sleep, which no ceiling can accommodate; it is polling a CONDITION in
//! bounded slices, so a wait of any length is spread over as many invocations as
//! it needs while every single invocation returns inside the ceiling.
//!
//! What — THE VERB SURFACE. One invocation polls the condition every
//! `--interval` seconds until one of three things happens, and reports which on
//! stdout as a single `tm-wait …` line:
//!
//! | exit | `status=` | meaning |
//! |------|-----------|---------|
//! | 0    | `met`     | terminal. The condition holds. |
//! | 75   | `pending` | NOT terminal. Re-run the IDENTICAL command. |
//! | 1    | `timeout` | terminal. The hard `--timeout` budget is exhausted. |
//! | 2    | `error`   | usage error, or a probe that failed repeatedly. |
//!
//! 75 is `EX_TEMPFAIL` from `sysexits`, chosen so "not done yet" can never be
//! confused with either success or the hard timeout. The `--timeout` budget
//! SPANS re-runs — it is recorded in a state file keyed by the condition — so
//! re-issuing the command does not reset the agent's own deadline. The status
//! line carries `remaining=<secs>` and, when pending, the exact `rerun=` command.
//!
//! Test: the sibling `tests.rs`; `cli_parses_wait_*` in `tests_behavior_a.rs`.

pub(crate) mod budget;
pub(crate) mod condition;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use anyhow::Context as _;

use crate::cli::{WaitArgs, WaitFor};
use budget::Budget;
use condition::{Condition, Poll};

/// Exit code: the condition holds (terminal).
pub(crate) const EXIT_MET: i32 = 0;
/// Exit code: the hard timeout was exhausted (terminal failure).
pub(crate) const EXIT_TIMEOUT: i32 = 1;
/// Exit code: usage error, or a probe that failed past its retry budget.
pub(crate) const EXIT_ERROR: i32 = 2;
/// Exit code: still pending — re-run the identical command (`EX_TEMPFAIL`).
pub(crate) const EXIT_PENDING: i32 = 75;

/// How many CONSECUTIVE probe errors are tolerated before giving up.
///
/// Why: a `gh` call can fail transiently (rate limit, a dropped connection) and
/// killing an hour-long wait over one blip would be worse than the blip. But an
/// error that repeats is real — a missing binary, a deleted PR — and retrying it
/// forever would hide it.
/// What: the fourth consecutive failure ends the wait with `status=error`.
/// Test: `loop_gives_up_after_repeated_probe_errors`.
const MAX_CONSECUTIVE_PROBE_ERRORS: u32 = 3;

/// Seconds without a poll after which a recorded budget is abandoned.
///
/// Why: an agent that walks away leaves a budget file behind. The next
/// identical wait must not inherit its exhausted deadline.
/// What: 10 minutes since the last poll, or ten intervals, whichever is longer.
/// Test: `budget_abandons_a_stale_record`.
const ABANDON_AFTER_SECS: u64 = 600;

/// A source of time, so the loop can be tested without sleeping.
///
/// Why: the still-pending and hard-timeout paths are the two outcomes that
/// matter most, and both are defined by elapsed time. A real clock would make
/// those tests either slow or flaky.
/// What: current unix seconds, and a blocking sleep.
/// Test: `FakeClock` in `tests.rs` drives every loop test.
pub(crate) trait Clock {
    /// Current unix time in seconds.
    fn now_unix(&self) -> u64;
    /// Block for `secs` seconds.
    fn sleep(&self, secs: u64);
}

/// The production [`Clock`].
///
/// Why: `tm wait` is a one-shot CLI that has nothing else to do while waiting,
/// so a plain blocking sleep is correct here even though `main` is async —
/// there is no other task on the runtime to starve.
/// What: `SystemTime` for the clock, `std::thread::sleep` for the nap.
/// Test: exercised by the live invocation, not by unit tests.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        budget::now_unix()
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

/// The resolved, clamped budget knobs for one wait.
///
/// Why: `--interval` and `--slice` are the two flags an agent can get wrong in
/// a way that hurts someone else — a 0-second interval is a blind-poll storm
/// against GitHub, and a 600-second slice is exactly the ceiling this command
/// exists to stay under. Clamping them at resolve time means the loop never has
/// to defend itself.
/// What: the hard timeout, the poll spacing, and the single-invocation ceiling,
/// all already clamped.
/// Test: `plan_clamps_interval_to_the_verb_floor`, `plan_clamps_slice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Hard timeout in seconds, spanning every re-run.
    pub(crate) timeout_s: u64,
    /// Seconds between polls.
    pub(crate) interval_s: u64,
    /// Seconds ONE invocation may block before reporting `pending`.
    pub(crate) slice_s: u64,
}

/// Smallest permitted single-invocation slice, in seconds.
const SLICE_MIN: u64 = 5;
/// Largest permitted single-invocation slice, in seconds.
///
/// Why: the harness allows a 600s foreground call at most, and a slice that
/// reaches for the whole of it leaves no room for the poll itself.
const SLICE_MAX: u64 = 500;
/// Largest permitted hard timeout, in seconds (24h).
const TIMEOUT_MAX: u64 = 86_400;

impl WaitFor {
    /// This verb's default poll spacing, in seconds.
    ///
    /// Why: a local file or PID can be checked cheaply and often; a GitHub read
    /// costs an API call and settles on the order of minutes, so polling it
    /// every 5 seconds is pure waste.
    /// What: 5 for `run`/`file`, 20 for `check`.
    /// Test: `plan_uses_the_verb_default_interval`.
    fn default_interval(self) -> u64 {
        match self {
            WaitFor::Run | WaitFor::File => 5,
            WaitFor::Check => 20,
        }
    }

    /// The floor this verb's poll spacing is clamped to.
    ///
    /// Why: prevents a blind-poll storm. The GitHub floor is higher because the
    /// cost lands on someone else's rate limit.
    /// What: 1 for `run`/`file`, 10 for `check`.
    /// Test: `plan_clamps_interval_to_the_verb_floor`.
    fn interval_floor(self) -> u64 {
        match self {
            WaitFor::Run | WaitFor::File => 1,
            WaitFor::Check => 10,
        }
    }

    /// The `--for` value, for the status line.
    fn as_str(self) -> &'static str {
        match self {
            WaitFor::Run => "run",
            WaitFor::File => "file",
            WaitFor::Check => "check",
        }
    }
}

/// Resolve and clamp the budget knobs.
///
/// Why: see [`Plan`].
/// What: applies the verb's default interval when none was given, clamps the
/// interval to its floor, clamps the slice to `SLICE_MIN..=SLICE_MAX`, caps the
/// timeout at 24h, and never lets a slice exceed the whole timeout.
/// Test: `plan_uses_the_verb_default_interval`,
/// `plan_clamps_interval_to_the_verb_floor`, `plan_clamps_slice`.
pub(crate) fn resolve_plan(args: &WaitArgs) -> anyhow::Result<Plan> {
    anyhow::ensure!(args.timeout > 0, "--timeout must be at least 1 second");
    let timeout_s = args.timeout.min(TIMEOUT_MAX);
    let interval_s = args
        .interval
        .unwrap_or_else(|| args.condition.default_interval())
        .max(args.condition.interval_floor());
    let slice_s = args.slice.clamp(SLICE_MIN, SLICE_MAX).min(timeout_s);
    Ok(Plan {
        timeout_s,
        interval_s,
        slice_s,
    })
}

/// Which of the four terminal-or-not states a wait ended in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// The condition holds.
    Met,
    /// Not yet — re-run the identical command.
    Pending,
    /// The hard timeout is exhausted.
    Timeout,
    /// The probe failed past its retry budget.
    Error,
}

impl Status {
    /// The `status=` token on the stdout line.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Met => "met",
            Status::Pending => "pending",
            Status::Timeout => "timeout",
            Status::Error => "error",
        }
    }

    /// The process exit code carrying this status.
    pub(crate) fn exit_code(self) -> i32 {
        match self {
            Status::Met => EXIT_MET,
            Status::Pending => EXIT_PENDING,
            Status::Timeout => EXIT_TIMEOUT,
            Status::Error => EXIT_ERROR,
        }
    }

    /// Whether this status ends the wait for good.
    pub(crate) fn is_terminal(self) -> bool {
        !matches!(self, Status::Pending)
    }
}

/// How one invocation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// The status token.
    pub(crate) status: Status,
    /// Human-readable reason, quoted onto the status line.
    pub(crate) detail: String,
}

/// Poll `cond` until it is met, the slice ends, or the budget is exhausted.
///
/// Why: this is the whole primitive. Everything else is argument handling.
/// What: polls, then decides in a fixed order — met wins over expired, expired
/// wins over slice-exhausted — sleeping `min(interval, remaining budget,
/// remaining slice)` between polls so a nap can never overshoot either
/// boundary. `budget.polls` and `budget.updated_at` are advanced in place so
/// the caller can persist them whatever the outcome.
/// Test: `loop_returns_met_on_first_poll`, `loop_returns_pending_at_slice_end`,
/// `loop_returns_timeout_when_budget_exhausted`,
/// `loop_gives_up_after_repeated_probe_errors`,
/// `loop_recovers_from_a_transient_probe_error`.
pub(crate) fn poll_until<C: Condition + ?Sized, K: Clock>(
    cond: &C,
    plan: &Plan,
    budget: &mut Budget,
    clock: &K,
) -> anyhow::Result<Outcome> {
    let slice_end = clock.now_unix().saturating_add(plan.slice_s);
    let mut consecutive_errors = 0u32;
    // Deliberately uninitialised: every path that reaches a read assigns it
    // first, so the compiler — not a placeholder string — proves it is set.
    let mut detail: String;

    loop {
        budget.polls += 1;
        budget.updated_at = clock.now_unix();

        match cond.poll() {
            Ok(Poll::Met(d)) => {
                return Ok(Outcome {
                    status: Status::Met,
                    detail: d,
                });
            }
            Ok(Poll::Pending(d)) => {
                consecutive_errors = 0;
                detail = d;
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors > MAX_CONSECUTIVE_PROBE_ERRORS {
                    return Err(e.context(format!(
                        "condition probe failed {consecutive_errors} times in a row"
                    )));
                }
                detail = format!("probe error {consecutive_errors}, retrying: {e}");
            }
        }

        let now = clock.now_unix();
        if budget.expired(now) {
            return Ok(Outcome {
                status: Status::Timeout,
                detail,
            });
        }
        if now >= slice_end {
            return Ok(Outcome {
                status: Status::Pending,
                detail,
            });
        }
        let nap = plan
            .interval_s
            .min(budget.remaining(now))
            .min(slice_end.saturating_sub(now))
            .max(1);
        clock.sleep(nap);
    }
}

/// Render the single machine-readable stdout line.
///
/// Why: the agent re-running this command reads ONE line. It has to carry
/// everything a decision needs — whether to re-run, how much budget is left,
/// and what to re-run — without the agent parsing prose.
/// What: `tm-wait status=… for=… elapsed=… remaining=… polls=… detail="…"`,
/// plus `rerun="…"` when the status is `pending`. Inner double quotes in the
/// detail become single quotes so the line always parses as five bare
/// `key=value` pairs followed by quoted fields.
/// Test: `status_line_carries_remaining_budget`, `status_line_quotes_detail`.
pub(crate) fn status_line(
    outcome: &Outcome,
    verb: WaitFor,
    budget: &Budget,
    now: u64,
    rerun: &str,
) -> String {
    let detail = outcome.detail.replace('"', "'").replace('\n', " ");
    let mut line = format!(
        "tm-wait status={} for={} elapsed={} remaining={} polls={} detail=\"{}\"",
        outcome.status.as_str(),
        verb.as_str(),
        budget.elapsed(now),
        budget.remaining(now),
        budget.polls,
        detail
    );
    if outcome.status == Status::Pending {
        line.push_str(&format!(" rerun=\"{rerun}\""));
    }
    line
}

/// Rebuild the exact command an agent should re-issue.
///
/// Why: "re-run the identical command" is the contract, and an agent that
/// retypes it from memory will drift — dropping `--timeout` silently restarts
/// the deadline. Printing the canonical form removes the guesswork.
/// What: the flags this invocation actually used, in a fixed order. `--reset`
/// is deliberately NOT reproduced: re-running with it would wipe the budget the
/// pending line just reported.
/// Test: `rerun_command_reproduces_the_selector`,
/// `rerun_command_never_repeats_reset`.
pub(crate) fn rerun_command(args: &WaitArgs) -> String {
    let mut parts = vec![
        "tm wait".to_string(),
        format!("--for {}", args.condition.as_str()),
    ];
    if let Some(pid) = args.pid {
        parts.push(format!("--pid {pid}"));
    }
    if let Some(h) = &args.handle {
        parts.push(format!("--handle {}", h.display()));
    }
    if let Some(p) = &args.path {
        parts.push(format!("--path {}", p.display()));
    }
    if let Some(c) = &args.contains {
        parts.push(format!("--contains {c:?}"));
    }
    if let Some(pr) = args.pr {
        parts.push(format!("--pr {pr}"));
    }
    if let Some(r) = &args.repo {
        parts.push(format!("--repo {r}"));
    }
    if args.allow_empty_checks {
        parts.push("--allow-empty-checks".to_string());
    }
    parts.push(format!("--timeout {}", args.timeout));
    if let Some(i) = args.interval {
        parts.push(format!("--interval {i}"));
    }
    parts.push(format!("--slice {}", args.slice));
    if let Some(d) = &args.state_dir {
        parts.push(format!("--state-dir {}", d.display()));
    }
    parts.join(" ")
}

/// Drive one `tm wait` invocation to its exit code.
///
/// Why: every outcome — including a usage error — has to leave through a
/// documented exit code, so the caller never has to distinguish "the wait
/// failed" from "the command was wrong" by reading prose. Exiting here rather
/// than returning a `Result` to `main` is what makes that guarantee hold.
/// What: resolves the plan and condition (usage errors exit 2), loads or starts
/// the cross-invocation budget, runs [`poll_until`], persists the budget on
/// `pending` and clears it on a terminal outcome, prints the status line, and
/// exits.
/// Test: the pieces are unit-tested individually; the whole path is exercised
/// live (see the `tm wait --for file` evidence on PR for #5843).
pub(crate) fn run(args: WaitArgs) -> ! {
    let code = match run_inner(&args) {
        Ok(code) => code,
        Err(e) => {
            let outcome = Outcome {
                status: Status::Error,
                detail: format!("{e:#}"),
            };
            let spent = Budget {
                spec: condition::spec(&args),
                started_at: 0,
                updated_at: 0,
                polls: 0,
                timeout_s: 0,
            };
            println!("{}", status_line(&outcome, args.condition, &spent, 0, ""));
            eprintln!("tm wait: {e:#}");
            EXIT_ERROR
        }
    };
    std::process::exit(code)
}

/// The fallible body of [`run`], split out so every error leaves one way.
fn run_inner(args: &WaitArgs) -> anyhow::Result<i32> {
    let plan = resolve_plan(args)?;
    let cond = condition::build(args)?;

    let spec = condition::spec(args);
    let dir = args
        .state_dir
        .clone()
        .unwrap_or_else(budget::default_state_dir);
    let state_path = budget::path_for(&dir, &spec);
    if args.reset {
        budget::clear(&state_path);
    }

    let clock = SystemClock;
    let now = clock.now_unix();
    let abandon_after = ABANDON_AFTER_SECS.max(plan.interval_s.saturating_mul(10));
    let mut budget = budget::load_or_start(&state_path, &spec, plan.timeout_s, now, abandon_after);

    let outcome = poll_until(cond.as_ref(), &plan, &mut budget, &clock)?;
    let end = clock.now_unix();

    if outcome.status.is_terminal() {
        budget::clear(&state_path);
    } else {
        budget::save(&state_path, &budget).context("cannot persist the wait budget")?;
    }

    println!(
        "{}",
        status_line(&outcome, args.condition, &budget, end, &rerun_command(args))
    );
    Ok(outcome.status.exit_code())
}
