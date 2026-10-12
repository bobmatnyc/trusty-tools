//! The builder band: whether the host can take another Rust builder.
//!
//! Why: the Architect opens or holds builder slots by host load, free disk,
//! free memory and swap (ARCHITECT.md "Builder slots follow CPU and disk"); the
//! poller reports each band change once (#9171 A3a).
//! What: [`builder_band`] is the prototype's `builder_band` with its open/hold
//! hysteresis; [`band_alerts`] raises one `band` alert when the band differs
//! from the stored one and stores the new band.
//! Test: `test_builder_band_boundaries`, `test_builder_band_hysteresis`,
//! `test_band_alert_fires_only_on_change_and_first_run`,
//! `band_matches_python_goldens`.

use crate::classify::{Alert, alert};

/// x cores: stop above this 15-minute load (30 on 16).
pub const BAND_STOP_LOAD_FACTOR: f64 = 1.875;
/// x cores: stateless hold line, used only with no stored band.
pub const BAND_HOLD_LOAD_FACTOR: f64 = 1.125;
/// x cores: from hold or stop, open only below this (16 on 16).
pub const BAND_OPEN_LOAD_FACTOR: f64 = 1.0;
/// x cores: from open, hold only above this (20 on 16).
pub const BAND_REHOLD_LOAD_FACTOR: f64 = 1.25;
/// Data volume GiB free: stop below this.
pub const BAND_STOP_FREE_GIB: f64 = 100.0;
/// Data volume GiB free: hold below this.
pub const BAND_HOLD_FREE_GIB: f64 = 200.0;
/// Memory free percent: stop below this.
pub const BAND_STOP_MEM_FREE_PCT: f64 = 20.0;

/// The band lines, each overridable by the environment variable of the same name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandConfig {
    /// `BAND_STOP_LOAD_FACTOR`.
    pub stop_load_factor: f64,
    /// `BAND_HOLD_LOAD_FACTOR`.
    pub hold_load_factor: f64,
    /// `BAND_OPEN_LOAD_FACTOR`.
    pub open_load_factor: f64,
    /// `BAND_REHOLD_LOAD_FACTOR`.
    pub rehold_load_factor: f64,
    /// `BAND_STOP_FREE_GIB`.
    pub stop_free_gib: f64,
    /// `BAND_HOLD_FREE_GIB`.
    pub hold_free_gib: f64,
    /// `BAND_STOP_MEM_FREE_PCT`.
    pub stop_mem_free_pct: f64,
}

impl Default for BandConfig {
    fn default() -> Self {
        Self {
            stop_load_factor: BAND_STOP_LOAD_FACTOR,
            hold_load_factor: BAND_HOLD_LOAD_FACTOR,
            open_load_factor: BAND_OPEN_LOAD_FACTOR,
            rehold_load_factor: BAND_REHOLD_LOAD_FACTOR,
            stop_free_gib: BAND_STOP_FREE_GIB,
            hold_free_gib: BAND_HOLD_FREE_GIB,
            stop_mem_free_pct: BAND_STOP_MEM_FREE_PCT,
        }
    }
}

/// How many builders the host can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// No new builders.
    Stop,
    /// Keep the current builders; start no more.
    Hold,
    /// Builders may start.
    Open,
}

impl Band {
    /// The name the prototype stores in `poll-state.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Hold => "hold",
            Self::Open => "open",
        }
    }
}

/// The band for the host inputs. `mem_free` `None` skips the memory test.
///
/// `prev` is the stored band: from `open`, hold only above the rehold line;
/// from anything else, open only below the open line; with no stored band the
/// stateless hold line decides.
#[allow(clippy::too_many_arguments)] // the prototype's signature, field for field
pub fn builder_band(
    cfg: &BandConfig,
    load15: f64,
    cores: usize,
    free_gib: f64,
    mem_free: Option<f64>,
    swap_mb: Option<f64>,
    prev: Option<&str>,
) -> Band {
    let cores = cores as f64;
    let low_mem = mem_free.is_some_and(|m| m < cfg.stop_mem_free_pct);
    if load15 > cfg.stop_load_factor * cores
        || free_gib < cfg.stop_free_gib
        || low_mem
        || swap_mb.unwrap_or(0.0) > 0.0
    {
        return Band::Stop;
    }
    if free_gib < cfg.hold_free_gib {
        return Band::Hold;
    }
    let hold = match prev {
        None => load15 >= cfg.hold_load_factor * cores,
        Some("open") => load15 > cfg.rehold_load_factor * cores,
        Some(_) => load15 >= cfg.open_load_factor * cores,
    };
    if hold { Band::Hold } else { Band::Open }
}

/// One `band` alert when the band differs from `stored`; stores the new band.
/// No stored band (first run) emits once.
#[allow(clippy::too_many_arguments)] // the prototype's signature, field for field
pub fn band_alerts(
    cfg: &BandConfig,
    stored: &mut Option<String>,
    now: f64,
    load15: f64,
    cores: usize,
    free_gib: f64,
    mem_free: Option<f64>,
    swap: Option<f64>,
) -> Vec<Alert> {
    let old = stored.clone();
    let band = builder_band(cfg, load15, cores, free_gib, mem_free, swap, old.as_deref());
    if old.as_deref() == Some(band.as_str()) {
        return Vec::new();
    }
    *stored = Some(band.as_str().to_owned());
    let mem = mem_free.map_or("n/a".to_owned(), |m| format!("{m:.0}%"));
    let shown_old = old.as_deref().filter(|o| !o.is_empty()).unwrap_or("none");
    let detail = format!(
        "builder band {shown_old} -> {} (load15 {load15:.1}/{cores} cores, {free_gib:.0} GiB free, \
         mem {mem} free, swap {:.0})",
        band.as_str(),
        swap.unwrap_or(0.0)
    );
    // Python's f"{old}->{band}" prints None for a missing band.
    let fingerprint = format!("{}->{}", old.as_deref().unwrap_or("None"), band.as_str());
    vec![alert("band", &fingerprint, &detail, "", "", now)]
}
