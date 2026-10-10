//! The `channels_*` rows of `tm doctor` (#8454 S2c).
//!
//! Why: stub for the red-first commit; the rows are not wired yet.
//! What: the final signatures; every row reads Ok.
//! Test: `doctor_channels_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use trusty_channels::policy::{HostCeiling, LoadReport, LoadRequest};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// The budget for each channel load.
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A route-policy load, injectable so tests can stall or record it.
pub(crate) type Loader = Arc<dyn Fn(&LoadRequest) -> LoadReport + Send + Sync>;

/// Why a load gave no report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unfinished {
    /// The load ran past its budget.
    TimedOut(Duration),
    /// The load stopped without a result.
    Stopped,
}

/// A load's report, or why there is none.
pub(crate) type Outcome<T> = Result<T, Unfinished>;

/// The host ceiling load.
pub(crate) struct HostProbe {
    /// The host file.
    pub(crate) path: PathBuf,
    /// The load with no channel.
    pub(crate) report: LoadReport,
    /// The parsed ceiling when the load accepted it.
    pub(crate) ceiling: Option<HostCeiling>,
}

/// The four rows for this host.
pub(crate) async fn channel_rows() -> Vec<DoctorCheck> {
    rows_with(
        LoadRequest::for_host(None, &[]),
        Arc::new(trusty_channels::policy::load_effective),
        LOAD_TIMEOUT,
    )
    .await
}

/// The four rows for `base`, loading through `loader`.
pub(crate) async fn rows_with(
    _base: LoadRequest,
    _loader: Loader,
    _limit: Duration,
) -> Vec<DoctorCheck> {
    [
        "channels_host",
        "channels_routes",
        "channels_gchat",
        "channels_gate",
    ]
    .into_iter()
    .map(|n| DoctorCheck::new(n, CheckStatus::Ok, "stub"))
    .collect()
}

/// The `channels_host` row.
pub(crate) fn host_row(_probe: &Outcome<HostProbe>) -> DoctorCheck {
    DoctorCheck::new("channels_host", CheckStatus::Ok, "stub")
}

/// A `channels_routes` or `channels_gchat` row.
pub(crate) fn view_row(
    name: &'static str,
    _view: &str,
    _reports: &Outcome<Vec<LoadReport>>,
) -> DoctorCheck {
    DoctorCheck::new(name, CheckStatus::Ok, "stub")
}

/// The `channels_gate` row.
pub(crate) fn gate_row(
    _daemon: &Outcome<Vec<LoadReport>>,
    _gchat: &Outcome<Vec<LoadReport>>,
) -> DoctorCheck {
    DoctorCheck::new("channels_gate", CheckStatus::Ok, "stub")
}

#[cfg(test)]
#[path = "doctor_channels_tests.rs"]
mod tests;
