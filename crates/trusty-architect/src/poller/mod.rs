//! The poller's sensors and state: the A3a half of the `fleet-poll.py` port.
//!
//! Why: the Architect's deterministic poller is a 1,636-line Python prototype
//! (`fleet-poll.py` at 988152c). #9171 A3a ports the parts that read the world
//! and persist state, held to the prototype's bytes; A3b adds the wake and
//! the cycle loop on top. Nothing here sends keys.
//! What: [`Config`] (environment read once), [`PollState`] (`poll-state.json`,
//! atomic save, unknown keys kept), [`list_panes`] and [`capture`],
//! [`read_events`] (`events.jsonl`), [`host_alerts`] and [`band_alerts`]
//! behind [`HostProbe`], [`TranscriptCtx`] (a pane's context from its own
//! transcript), [`self_ctx_alerts`], and the `alerts.jsonl` / `poll.log`
//! appends. Every function takes `now` and paths from its caller. A3b-1 adds
//! the send path: [`Waker::wake`] types the wake pointer into the Architect's
//! box and [`dispatch_wake`] carries the pending count (#9171). A3b-2 adds
//! the cycle and the loop: [`collect`] (one cycle's sensing and dedup),
//! [`Runner`] (`run_once`, `run_loop`), [`PollSession`], [`PollLock`],
//! [`rename_windows`], [`architect_busy`] and the [`Triage`] seam. #9801
//! slice 1 adds [`steps`]: the usage step and `notify_macos`. #9167 adds
//! [`MessageSink`], through which a submitted wake is recorded.
//! #9779 adds [`watchdog`] (dead/frozen/silent/ok and the relaunch),
//! [`routines`] (D5r: routines ran, push due; nothing sends) and the shadow
//! run ([`prepare_shadow`], `Config::shadow`).
//! df-T7i P1b ports the live script's drift since 988152c: [`ctx_bar`],
//! [`digest`] (the wake digest), [`zone`], [`self_refresh`], the inbox and
//! completion alerts, and [`steps::scripts`] (the `run_*` Python steps).
//! Test: `tests/poller_ported.rs`, `tests/wake_ported.rs`,
//! `tests/cycle_ported.rs` and `tests/architect_ported.rs` (the prototype's
//! tests, by name), `tests/parity_a3.rs` and `tests/parity_wake.rs` (goldens
//! from `regen_a3.py`), `tests/state_atomic.rs`, `tests/wake_send_path.rs`,
//! `tests/cycle_loop.rs`, `tests/state_session.rs`, `tests/watchdog_ported.rs`,
//! `tests/d5r_shadow.rs`, `tests/p1b_ported.rs`, `tests/parity_p1b.rs`.

mod architect;
mod band;
pub(crate) mod box_text;
mod completion;
mod config;
mod ctx;
pub mod ctx_bar;
mod cycle;
pub mod digest;
mod events;
mod host;
mod inbox_alerts;
mod listing;
mod lock;
// #9167: a delivered wake is reported through the caller's sink.
mod messages;
pub mod push;
mod pyfmt;
pub mod routines;
mod run;
mod self_ctx_alerts;
pub mod self_refresh;
mod session;
mod shadow;
mod sink;
pub mod slack;
mod state;
pub mod steps;
mod triage;
mod wake;
pub mod watchdog;
mod windows;
pub mod zone;

pub use architect::{DONE_SUMMARY, SHELLS, architect_busy, architect_busy_in, is_shell};

pub use crate::error::{CycleError, RunError};
pub use band::{
    BAND_HOLD_FREE_GIB, BAND_HOLD_LOAD_FACTOR, BAND_OPEN_LOAD_FACTOR, BAND_REHOLD_LOAD_FACTOR,
    BAND_STOP_FREE_GIB, BAND_STOP_LOAD_FACTOR, BAND_STOP_MEM_FREE_PCT, BAND_SWAP_ACTIVE_PAGES,
    Band, BandConfig, band_alerts, builder_band,
};
pub use completion::{COMPLETE_MIN_CTX_PCT, completion_alerts, il_task_complete_for};
pub use config::{
    Config, LOAD_FACTOR_DEFAULT, SELF_CTX_THRESHOLD_DEFAULT, SELF_CTX_WINDOW_DEFAULT,
    SelfCtxThreshold,
};
pub use ctx::{TranscriptCtx, pane_session_id};
pub use cycle::{Collected, Sensors, Stopped, collect};
pub use events::{EVENT_TYPES, read_events};
pub use host::{
    DISK_HIGH, DiskUsage, HostProbe, SystemHostProbe, host_alerts, parse_vm_stat_swaps,
};
pub use inbox_alerts::{ctx_cycle_alerts, selftest_alerts};
pub use listing::{LIST_FORMAT, Listing, Pane, capture, list_panes, parse_list_panes};
pub use lock::PollLock;
pub use messages::{MessageSink, NoMessages, PaneMessage, PaneMessageKind};
pub use run::{CycleSummary, Phase, Runner, parse_interval};
pub use self_ctx_alerts::self_ctx_alerts;
pub use session::PollSession;
pub use shadow::prepare_shadow;
pub use sink::{append_alerts, append_log};
pub use state::{PollState, SaveStep, StoredCount, load_state, save_state, save_state_with};
pub use triage::{Triage, WakeAll};
pub use wake::{
    BOX_CHANGED, WAKE_SUPPRESS_MAX_S, WakeContext, Waker, dispatch_wake, dispatch_wake_with,
    record_wake_result,
};
pub use windows::{plan_window_renames, rename_windows};

/// The Architect's own tmux session: the only pane the poller may wake.
pub const SUPERVISOR: &str = "tm-architect";

/// Sessions the poller never watches, and whose events it drops.
/// df-T7i P1b adds the ILIO trial Architect's two sessions (ar-T2f).
pub const SKIP_SESSIONS: [&str; 5] = [
    "tm-architect",
    "tm-supervisor-collector",
    "tm-architect-poll",
    "tm-architect-ilio",
    "tm-architect-ilio-poll",
];
