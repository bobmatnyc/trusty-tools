//! The Prometheus recorder and the index-count gauge (issue #41 Phase 1).
//!
//! Why: emit sites across the crate use the `metrics` facade. #9535 deleted
//! `GET /metrics` and the request middleware (nothing served them after #9214
//! removed the TCP listener), but `start` still installs the recorder, so the
//! recorder stays.
//!
//! What:
//!   - [`install_recorder`] — install the global Prometheus recorder once at
//!     daemon startup.
//!   - [`set_index_count`], called after each registry mutation.
//!
//! Test: `install_recorder_renders_a_recorded_counter`.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Shared handle to the Prometheus recorder. Cloneable.
#[derive(Clone)]
pub struct MetricsState {
    handle: PrometheusHandle,
}

impl MetricsState {
    /// Render the current Prometheus text snapshot.
    pub fn render(&self) -> String {
        self.handle.render()
    }
}

/// Install the global Prometheus recorder and return a state handle.
///
/// Why: must be called exactly once at daemon startup, before any
/// `metrics::counter!`/etc. macros fire — the facade's "no recorder
/// installed" branch is a silent no-op.
/// What: builds the recorder, installs it as the global, and returns the
/// render handle wrapped in `MetricsState`. Errors propagate as `anyhow`.
/// Test: `install_recorder_renders_a_recorded_counter`.
pub fn install_recorder() -> anyhow::Result<MetricsState> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("failed to install prometheus recorder: {e}"))?;
    Ok(MetricsState { handle })
}

/// Set the static `trusty_index_count` gauge. Call after each index registry
/// mutation (register / unregister) so the gauge reflects the current value.
///
/// Why: a periodic ticker would be acceptable, but having handlers call this
/// directly means dashboards stay in sync within microseconds of a mutation
/// rather than seconds.
/// What: writes the count to the named gauge.
/// Test: indirect — exercised by `create_index` / `delete_index` flows.
pub fn set_index_count(count: usize) {
    metrics::gauge!("trusty_index_count").set(count as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    // `install_recorder` installs a *global* recorder, so a second call in the
    // same test binary errors. Only this test calls it.
    #[test]
    #[serial_test::parallel]
    fn install_recorder_renders_a_recorded_counter() {
        let state = install_recorder().expect("recorder installs");
        metrics::counter!("trusty_test_counter").increment(1);
        let text = state.render();
        assert!(
            text.contains("trusty_test_counter"),
            "rendered metrics missing counter: {text}"
        );
    }
}
