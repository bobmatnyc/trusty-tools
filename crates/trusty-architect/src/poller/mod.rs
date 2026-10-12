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
//! appends. Every function takes `now` and paths from its caller.
//! Test: `tests/poller_ported.rs` (the prototype's tests, by name),
//! `tests/parity_a3.rs` (goldens from `regen_a3.py`), `tests/state_atomic.rs`.

mod band;
mod config;
mod ctx;
mod events;
mod host;
mod listing;
mod pyfmt;
mod self_ctx_alerts;
mod sink;
mod state;

pub use band::{
    BAND_HOLD_FREE_GIB, BAND_HOLD_LOAD_FACTOR, BAND_OPEN_LOAD_FACTOR, BAND_REHOLD_LOAD_FACTOR,
    BAND_STOP_FREE_GIB, BAND_STOP_LOAD_FACTOR, BAND_STOP_MEM_FREE_PCT, Band, BandConfig,
    band_alerts, builder_band,
};
pub use config::{
    Config, LOAD_FACTOR_DEFAULT, SELF_CTX_THRESHOLD_DEFAULT, SELF_CTX_WINDOW_DEFAULT,
};
pub use ctx::{TranscriptCtx, pane_session_id};
pub use events::{EVENT_TYPES, read_events};
pub use host::{DISK_HIGH, DiskUsage, HostProbe, SystemHostProbe, host_alerts};
pub use listing::{Listing, Pane, capture, list_panes, parse_list_panes};
pub use self_ctx_alerts::self_ctx_alerts;
pub use sink::{append_alerts, append_log};
pub use state::{PollState, SaveStep, load_state, save_state, save_state_with};

/// The Architect's own tmux session: the only pane the poller may wake.
pub const SUPERVISOR: &str = "tm-architect";

/// Sessions the poller never watches, and whose events it drops.
pub const SKIP_SESSIONS: [&str; 3] = [
    "tm-architect",
    "tm-supervisor-collector",
    "tm-architect-poll",
];
