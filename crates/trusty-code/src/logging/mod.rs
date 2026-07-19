//! Tracing and logging initialisation for tcode.
//!
//! Why: All tcode binaries and the library's test harness need consistent
//! tracing initialisation that writes to stderr (never stdout — stdout is the
//! API transport channel). Centralising it here prevents duplicated setup and
//! ensures the `try_init` pattern is used everywhere so test binaries remain
//! idempotent.
//! What: `init_tracing` initialises the global tracing subscriber from the
//! `RUST_LOG` env var. `init_tracing_for_test` is a lightweight variant for
//! test binaries that silently drops duplicate init errors.
//! Test: `init_tracing_for_test_is_idempotent` calls the function twice and
//! asserts no panic.
//!
//! # The log-level convention (#2857) — READ THIS BEFORE ADDING A DECISION POINT
//!
//! **A decision the harness makes about a run must never be invisible to the
//! operator running it.**
//!
//! This crate is an agent harness: it continuously decides things *on behalf
//! of* the model and the user — refusing a delegation, capping a retry loop,
//! suppressing a test re-run, truncating context, falling back to a degraded
//! search lane. When such a decision fires silently, the run's outcome changes
//! and the only remaining evidence is the outcome itself. Diagnosing it then
//! requires forensics (cross-run comparison, mtime archaeology) rather than
//! reading one run's stderr.
//!
//! This is not a hypothesis. It is a measured, repeated cost:
//!
//! - **#2852** — the re-delegation cap fired with *no log line anywhere*. A
//!   total-loss run (0/9 tests, 0/5 deliverables) looked random; the only
//!   trace was a string buried in the report's `task` field, and diagnosis
//!   required comparing delegation indices across runs.
//! - **#2823** — no build SHA was recorded anywhere, so a wrong conclusion was
//!   reported and retracted only after mtime forensics.
//! - **`CadenceOutcome`** — computed `{fired, rounds, within_budget}` "for
//!   observability", then discarded by its only call site, leaving the epic
//!   #2343 ≥60%-working-context guarantee unobservable.
//!
//! The **positive control** proves the rule: incremental index-file transient
//! failures *were* logged at `warn`, which is exactly why "27 failures across
//! 23 files in one run" was discoverable at all and became #2785. Logging
//! worked precisely where it existed.
//!
//! ## Choosing a level
//!
//! | Level | When | Examples |
//! |---|---|---|
//! | [`error`](tracing::error) | A genuine fault — an invariant broke or a never-event happened | threshold compaction firing under cadence (#2349) |
//! | [`warn`](tracing::warn) | A **harness policy decision silently changes the run's outcome**: a refusal, cap, gate, abort, or a config value resolved to something other than what was asked for | re-delegation cap exhausted; turn cap hit; deadline exceeded; verify gate intercepting `finish_task`; RBAC/allowlist denial; path-traversal rejection; redundant-run suppression |
//! | [`info`](tracing::info) | A **degradation the model or user then acts on**: a fallback, truncation, suppression, or context held back — the run continues, but on different material than requested | `STAGE_NOT_READY` → lexical fallback; `recall_session` dropping lowest-scored memories to fit the token budget; user-requested cancellation |
//! | [`debug`](tracing::debug)/[`trace`](tracing::trace) | Routine, high-volume, or per-turn detail with no outcome change | cadence firing on schedule; model supplying bad tool args (it self-corrects next turn) |
//!
//! ## The load-bearing distinction
//!
//! **Harness policy decision → `warn`. Model input error → `debug`.**
//!
//! Both surface to the model as a `ToolResult::err`, which is why they are
//! easy to conflate. They are not the same event. When the *model* sends bad
//! arguments it sees the error and self-corrects on the next turn — routine,
//! self-healing, high-volume, `debug`. When the *harness* overrides what the
//! model asked for, the model cannot fix it by trying harder: the run's
//! trajectory has been changed by us, and only the operator's stderr can ever
//! record that. That is the `warn`.
//!
//! ## Rules
//!
//! 1. **stderr only, never stdout.** `init_tracing` binds the fmt subscriber
//!    to stderr because stdout carries MCP JSON-RPC framing — a single stray
//!    stdout write corrupts the protocol.
//! 2. **Name the numbers and the consequence.** "cap reached" is not
//!    actionable. "re-delegation cap reached after 3 attempts; stopping the PM
//!    loop" is. State WHAT fired, the relevant counts/budgets/scores, and what
//!    it DID to the run.
//! 3. **Respect the level discipline.** Everything at `warn` is the same as
//!    nothing at `warn`. A default `RUST_LOG=info` run must stay quiet unless
//!    something genuinely worth an operator's attention happened.
//! 4. **Logs are not events.** [`crate::events`] is structured telemetry for
//!    the UI; these logs are for the operator reading stderr. A decision may
//!    warrant both. Adding one is never a reason to skip the other.

use tracing_subscriber::EnvFilter;

/// Build the filter both init paths use: `RUST_LOG` when set and valid,
/// otherwise [`DEFAULT_LOG_LEVEL`].
///
/// Why (#2857): `EnvFilter::from_default_env()` enables **nothing** when
/// `RUST_LOG` is unset — it builds an empty directive set, not a default
/// level. [`DEFAULT_LOG_LEVEL`] has always documented itself as "the log level
/// used when no `RUST_LOG` env var is set", but nothing ever read it, so the
/// documented default was fiction: a default `tcode` run emitted **zero** log
/// lines at any level.
///
/// That is the root of the invisibility this issue exists to fix, and it sits
/// underneath every individual missing log line: instrumenting a decision site
/// accomplishes nothing if the subscriber discards the event. #2852's cap was
/// doubly invisible — it had no `warn!` to emit, and had it emitted one, the
/// default filter would have dropped it anyway.
///
/// An invalid `RUST_LOG` also falls back here (rather than silently disabling
/// logging), and says so — per this module's own convention, a config value
/// resolved to something other than what was asked for is a `warn`.
/// What: `EnvFilter::try_from_default_env()`, falling back to
/// `EnvFilter::new(DEFAULT_LOG_LEVEL)`.
/// Test: `logging::tests::env_filter_defaults_to_info_without_rust_log`,
/// `logging::tests::env_filter_honours_rust_log`.
fn env_filter() -> EnvFilter {
    match std::env::var("RUST_LOG") {
        // Set and non-empty: honour it, but do not let a typo mean silence.
        Ok(raw) if !raw.trim().is_empty() => EnvFilter::try_new(&raw).unwrap_or_else(|e| {
            eprintln!(
                "warning: RUST_LOG={raw:?} is not a valid filter ({e}); \
                 falling back to {DEFAULT_LOG_LEVEL}"
            );
            EnvFilter::new(DEFAULT_LOG_LEVEL)
        }),
        // Unset or empty: the documented default, not silence.
        _ => EnvFilter::new(DEFAULT_LOG_LEVEL),
    }
}

/// Initialise the global tracing subscriber.
///
/// Why: All daemons and CLI entry points call this once at startup to ensure
/// log output is consistently routed to stderr with the `RUST_LOG` filter.
/// What: Installs a stderr-bound fmt subscriber filtered by [`env_filter`] —
/// `RUST_LOG` when set, else [`DEFAULT_LOG_LEVEL`]. Panics if called twice
/// (use `init_tracing_for_test` in test binaries).
/// Test: Called in `main.rs`; the filter itself is covered by
/// `logging::tests::env_filter_*`.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .init();
}

/// Initialise the global tracing subscriber, silently ignoring duplicate inits.
///
/// Why: Test binaries may link multiple test modules that all call setup code.
/// `try_init` returns an error on the second call instead of panicking, so
/// test runs remain stable regardless of execution order.
/// What: Calls `try_init()`; swallows the error if already initialised.
/// Test: `init_tracing_for_test_is_idempotent`.
pub fn init_tracing_for_test() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .try_init();
}

/// Log level used when no `RUST_LOG` env var is set.
///
/// Why: Documents and centralises the default so operators know what to expect
/// without reading source.
/// What: A static string literal; the default filter used by `EnvFilter::from_default_env()`.
/// Test: Not directly tested — the value is advisory documentation.
pub const DEFAULT_LOG_LEVEL: &str = "info";

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `init_tracing_for_test` must be idempotent across multiple calls.
    ///
    /// Why: Test binaries are often multi-crate and may call init from several
    /// setup functions; a panic on re-init would break the test suite.
    /// What: Calls `init_tracing_for_test` twice; asserts no panic.
    /// Test: This test.
    #[test]
    fn init_tracing_for_test_is_idempotent() {
        init_tracing_for_test();
        init_tracing_for_test(); // second call must not panic
    }

    /// The default log level constant is non-empty.
    ///
    /// Why: Guard against accidental empty string.
    /// What: Asserts `DEFAULT_LOG_LEVEL` is non-empty.
    /// Test: This test.
    #[test]
    fn default_log_level_is_non_empty() {
        assert!(!DEFAULT_LOG_LEVEL.is_empty());
    }

    /// Serializes the `RUST_LOG`-mutating tests below, mirroring
    /// `crate::mode::MODE_ENV_LOCK`'s identical rationale — cargo runs tests
    /// in parallel and the environment is process-global.
    static RUST_LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `RUST_LOG` set to `value` (or removed when `None`),
    /// restoring the prior value afterwards.
    fn with_rust_log<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = RUST_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("RUST_LOG").ok();
        // SAFETY: test-only env mutation, serialized by `RUST_LOG_LOCK`.
        unsafe {
            match value {
                Some(v) => std::env::set_var("RUST_LOG", v),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
        let out = f();
        unsafe {
            match &prior {
                Some(v) => std::env::set_var("RUST_LOG", v),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
        out
    }

    /// **The #2857 root-cause regression guard.** With `RUST_LOG` unset the
    /// filter must enable [`DEFAULT_LOG_LEVEL`], not nothing.
    ///
    /// Why: `EnvFilter::from_default_env()` builds an EMPTY directive set when
    /// `RUST_LOG` is unset, so every log line this crate emits was discarded
    /// on a default run — `DEFAULT_LOG_LEVEL` documented an "info default"
    /// that no code path ever applied. That made instrumenting decision sites
    /// pointless: #2852's cap would have stayed invisible even with a `warn!`
    /// at the cap. This test is what stops that regressing back to silence.
    /// What: Asserts `env_filter()`'s directives render as `DEFAULT_LOG_LEVEL`
    /// when `RUST_LOG` is absent.
    /// Test: This test.
    #[test]
    fn env_filter_defaults_to_info_without_rust_log() {
        let rendered = with_rust_log(None, || env_filter().to_string());
        assert_eq!(
            rendered, DEFAULT_LOG_LEVEL,
            "an unset RUST_LOG must mean the documented default level, never silence"
        );
    }

    /// An explicit `RUST_LOG` still wins over the default.
    ///
    /// Why: The default must not become a ceiling — operators debugging a run
    /// need `RUST_LOG=debug` to work exactly as before.
    /// What: Asserts `env_filter()` honours an explicit valid directive.
    /// Test: This test.
    #[test]
    fn env_filter_honours_rust_log() {
        let rendered = with_rust_log(Some("debug"), || env_filter().to_string());
        assert_eq!(rendered, "debug");
    }

    /// An INVALID `RUST_LOG` falls back to the default rather than to silence.
    ///
    /// Why: A typo'd filter silently disabling all logging is the same
    /// invisibility failure #2857 exists to fix, just triggered by the
    /// operator instead of by the default.
    /// What: Asserts a malformed directive yields `DEFAULT_LOG_LEVEL`.
    /// Test: This test.
    #[test]
    fn env_filter_falls_back_to_default_on_invalid_rust_log() {
        let rendered = with_rust_log(Some("not=a=valid=level"), || env_filter().to_string());
        assert_eq!(rendered, DEFAULT_LOG_LEVEL);
    }
}
