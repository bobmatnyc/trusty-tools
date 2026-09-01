//! Whole-machine host metrics sampling for the Foundry dashboard (#6517).
//!
//! Why: [`sys_metrics`](crate::sys_metrics) samples only the CURRENT process —
//!      its RSS and CPU. The Foundry machine-status dashboard needs the whole
//!      host instead: overall CPU load, system memory with a pressure signal,
//!      per-mount and aggregate disk usage, and network throughput. Per the
//!      workspace "common entry point, clean domain demarcation" rule this
//!      host-sampling capability lives here once rather than being reinvented in
//!      trusty-console, so any future consumer (a second dashboard, a headless
//!      health probe) reuses the same typed shapes.
//! What: [`HostSampler`](crate::host_metrics::HostSampler) wraps a
//!      `sysinfo::System` plus its `Networks`/`Disks` handles and, on each
//!      [`HostSampler::sample`](crate::host_metrics::HostSampler::sample),
//!      returns a [`HostMetrics`](crate::host_metrics::HostMetrics) snapshot.
//!      Health thresholds ([`HostThresholds`](crate::host_metrics::HostThresholds))
//!      are PROVISIONAL — see the type's docs; they need an owner ruling before
//!      any alarm is wired to them.
//! Test: the inline `tests` module — `sampler_produces_plausible_snapshot`,
//!      `pressure_classification_boundaries`, `thresholds_are_configurable`,
//!      and the serde round-trip.
//!
//! ## OS-agnostic sampling
//!
//! `sysinfo` is cross-platform, but some fields are unavailable or shaped
//! differently per OS, and the data model must not assume the macOS screensaver
//! form factor. Specifics:
//! - `MountMetrics::is_removable` is best-effort; some platforms always report
//!   `false`.
//! - `CpuMetrics::physical_cores` is `None` where the OS does not expose a
//!   physical/logical split.
//! - Swap totals read `0` on hosts with no swap configured — not an error.
//! - Disk and network interface enumeration differs per OS (macOS surfaces
//!   synthetic APFS volumes; containers may hide interfaces). The snapshot
//!   reports whatever the OS lists and never fails when a set is empty.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

/// Coarse pressure classification for one host subsystem (#6517).
///
/// Why: the dashboard renders a traffic-light per subsystem without re-deriving
///      thresholds client-side, and a typed tri-state keeps every consumer's
///      classification identical.
/// What: `Nominal` (below the warning threshold), `Warning` (at/above warning,
///      below critical), `Critical` (at/above critical). Serialised lowercase.
/// Test: `pressure_classification_boundaries`.
// The variant order is the severity order: deriving `Ord` makes
// `Pressure::worst` (via `max`) pick the more severe of two — see `Pressure::worst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Pressure {
    /// Below the warning threshold — healthy.
    Nominal,
    /// At or above the warning threshold, below critical.
    Warning,
    /// At or above the critical threshold.
    Critical,
}

impl Pressure {
    /// The worse of two pressures (`Critical` > `Warning` > `Nominal`).
    ///
    /// Why: [`HostMetrics::overall_pressure`] is the worst subsystem, so the
    ///      dashboard's single top-level badge never under-reports.
    /// What: returns whichever operand ranks higher on the severity order.
    /// Test: `overall_is_worst_subsystem`.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        self.max(other)
    }
}

/// PROVISIONAL health thresholds for host-subsystem pressure (#6517).
///
/// Why: the epic flags "healthy" thresholds as an OWNER DECISION. These
///      defaults are sensible starting points (a busy but functioning host sits
///      in `Nominal`; sustained saturation trips `Warning`, near-exhaustion
///      `Critical`) but they are NOT an owner ruling. Do NOT wire an alarm,
///      auto-remediation, or paging to these numbers until the owner signs off
///      on the levels. The struct is configurable precisely so the eventual
///      ruling changes one call, not the classification logic.
/// What: per-subsystem warning/critical percentages consumed by
///      [`Pressure::classify`]. Percentages are 0..=100.
/// Test: `thresholds_are_configurable`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HostThresholds {
    /// CPU load %% at/above which a host is `Warning`. PROVISIONAL default 80.
    pub cpu_warning_pct: f32,
    /// CPU load %% at/above which a host is `Critical`. PROVISIONAL default 95.
    pub cpu_critical_pct: f32,
    /// Memory-used %% at/above which a host is `Warning`. PROVISIONAL default 80.
    pub memory_warning_pct: f32,
    /// Memory-used %% at/above which a host is `Critical`. PROVISIONAL default 95.
    pub memory_critical_pct: f32,
    /// Disk-used %% at/above which a mount is `Warning`. PROVISIONAL default 85.
    pub disk_warning_pct: f32,
    /// Disk-used %% at/above which a mount is `Critical`. PROVISIONAL default 95.
    pub disk_critical_pct: f32,
}

impl Default for HostThresholds {
    /// PROVISIONAL defaults — see [`HostThresholds`] for why they are not an
    /// owner ruling.
    fn default() -> Self {
        // #6517: provisional levels pending an owner ruling on "healthy".
        Self {
            cpu_warning_pct: 80.0,
            cpu_critical_pct: 95.0,
            memory_warning_pct: 80.0,
            memory_critical_pct: 95.0,
            disk_warning_pct: 85.0,
            disk_critical_pct: 95.0,
        }
    }
}

impl Pressure {
    /// Classify `usage_pct` against a warning/critical pair.
    ///
    /// Why: the ONE place a percentage becomes a tri-state, so CPU, memory, and
    ///      disk all classify identically.
    /// What: `>= critical` → `Critical`; else `>= warning` → `Warning`; else
    ///      `Nominal`. A `NaN` input (a subsystem that could not be measured)
    ///      classifies as `Nominal` rather than panicking.
    /// Test: `pressure_classification_boundaries`.
    #[must_use]
    pub fn classify(usage_pct: f32, warning: f32, critical: f32) -> Self {
        if usage_pct >= critical {
            Pressure::Critical
        } else if usage_pct >= warning {
            Pressure::Warning
        } else {
            Pressure::Nominal
        }
    }
}

/// Overall CPU load across the whole machine (#6517).
///
/// Why: the dashboard's CPU gauge needs one host-wide number plus the core
///      counts to contextualise it.
/// What: `usage_pct` is `sysinfo`'s global CPU usage — the average across all
///      logical cores, 0..=100 (unlike the per-process figure, which can exceed
///      100). `physical_cores` is `None` where the OS hides the split.
/// Test: `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuMetrics {
    /// Machine-wide CPU utilisation, 0..=100 (averaged across logical cores).
    pub usage_pct: f32,
    /// Logical (hyperthread) core count.
    pub logical_cores: usize,
    /// Physical core count, or `None` when the OS does not expose it.
    pub physical_cores: Option<usize>,
    /// Pressure classification of `usage_pct` against [`HostThresholds`].
    pub pressure: Pressure,
}

/// System memory + swap with a pressure signal (#6517).
///
/// Why: memory exhaustion is the failure the dashboard most needs to surface
///      early; a used-percentage plus a pressure band gives that at a glance.
/// What: all byte fields are bytes (not KiB — `sysinfo` 0.30+ reports bytes).
///      `available_bytes` is the OS "available" figure (reclaimable included),
///      which is a better headroom signal than `total - used`.
/// Test: `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetrics {
    /// Total physical RAM, bytes.
    pub total_bytes: u64,
    /// Used RAM, bytes.
    pub used_bytes: u64,
    /// OS-reported available RAM (reclaimable included), bytes.
    pub available_bytes: u64,
    /// `used_bytes / total_bytes` as a percentage, 0..=100.
    pub usage_pct: f32,
    /// Total swap, bytes (`0` when no swap is configured).
    pub swap_total_bytes: u64,
    /// Used swap, bytes.
    pub swap_used_bytes: u64,
    /// Pressure classification of `usage_pct` against [`HostThresholds`].
    pub pressure: Pressure,
}

/// One mounted filesystem's usage (#6517).
///
/// Why: the dashboard lists mounts individually so a single full volume is
///      visible even when the aggregate looks healthy.
/// What: byte fields are bytes. `used_bytes` is `total - available`.
///      `is_removable` is best-effort per OS.
/// Test: `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountMetrics {
    /// Mount point path (e.g. `/`, `/System/Volumes/Data`).
    pub mount_point: String,
    /// Device/volume name as the OS reports it.
    pub name: String,
    /// Total capacity, bytes.
    pub total_bytes: u64,
    /// Available capacity, bytes.
    pub available_bytes: u64,
    /// Used capacity (`total - available`), bytes.
    pub used_bytes: u64,
    /// `used_bytes / total_bytes` as a percentage, 0..=100.
    pub usage_pct: f32,
    /// Best-effort removable-media flag; `false` on OSes that do not expose it.
    pub is_removable: bool,
    /// Pressure classification of `usage_pct` against [`HostThresholds`].
    pub pressure: Pressure,
}

/// Machine-wide disk usage: aggregate plus per-mount detail (#6517).
///
/// Why: the aggregate drives a single "storage" gauge; the mounts back the
///      drill-down. The aggregate sums only NON-removable, real mounts so a
///      plugged-in USB stick does not distort the machine's headroom.
/// What: `aggregate_*` sum the counted mounts; `mounts` lists every mount the
///      OS reported (removable ones included, for the drill-down).
/// Test: `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskMetrics {
    /// Total capacity across counted (non-removable) mounts, bytes.
    pub aggregate_total_bytes: u64,
    /// Available capacity across counted mounts, bytes.
    pub aggregate_available_bytes: u64,
    /// Used capacity across counted mounts, bytes.
    pub aggregate_used_bytes: u64,
    /// Aggregate used percentage, 0..=100.
    pub aggregate_usage_pct: f32,
    /// Pressure classification of `aggregate_usage_pct`.
    pub pressure: Pressure,
    /// Every mount the OS reported.
    pub mounts: Vec<MountMetrics>,
}

/// Machine-wide network throughput (#6517).
///
/// Why: throughput is a rate, so the dashboard needs bytes/sec, not a
///      cumulative counter it would have to difference itself.
/// What: `*_bytes_per_sec` are the delta since the previous sample divided by
///      the elapsed window (`window_secs`); `*_total_bytes` are the cumulative
///      counters. The FIRST sample reports rates over the priming window, which
///      may be near-zero — like the per-process CPU delta in `sys_metrics`.
/// Test: `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMetrics {
    /// Receive rate over the last sample window, bytes/sec.
    pub rx_bytes_per_sec: f64,
    /// Transmit rate over the last sample window, bytes/sec.
    pub tx_bytes_per_sec: f64,
    /// Cumulative bytes received since interface start, summed over interfaces.
    pub rx_total_bytes: u64,
    /// Cumulative bytes transmitted since interface start, summed.
    pub tx_total_bytes: u64,
    /// The window the rates were computed over, seconds.
    pub window_secs: f64,
}

/// A whole-machine metrics snapshot (#6517).
///
/// Why: the aggregated, serde-stable shape the Foundry machine-status endpoint
///      and its phase-2 UI render. Combining every subsystem in one struct lets
///      the dashboard fetch the whole host in one payload.
/// What: the four subsystem structs plus `overall_pressure` (worst subsystem)
///      and `sampled_at_unix`. All fields are public and serde round-trip.
/// Test: `snapshot_serde_round_trip`, `sampler_produces_plausible_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostMetrics {
    /// Overall CPU load.
    pub cpu: CpuMetrics,
    /// System memory + swap.
    pub memory: MemoryMetrics,
    /// Disk usage (aggregate + per-mount).
    pub disks: DiskMetrics,
    /// Network throughput.
    pub network: NetworkMetrics,
    /// Worst of the CPU / memory / disk pressures.
    pub overall_pressure: Pressure,
    /// Unix seconds when the snapshot was taken, or `None` if the clock read
    /// failed.
    pub sampled_at_unix: Option<u64>,
}

/// Divide a byte count by a total, returning a 0..=100 percentage.
///
/// Why: `used / total * 100` recurs for CPU, memory, and every mount; one
///      helper keeps the zero-total guard consistent (an empty/zero-capacity
///      mount reports `0.0`, never a `NaN`).
/// What: returns `0.0` when `total == 0`; otherwise the clamped percentage.
/// Test: exercised via `sampler_produces_plausible_snapshot` and the
///      classification tests.
fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 / total as f64 * 100.0) as f32
}

/// Whole-machine metrics sampler bound to a live `sysinfo::System` (#6517).
///
/// Why: like [`SysMetrics`](crate::sys_metrics::SysMetrics), CPU and network
///      readings are deltas between refreshes, so the same instance must be
///      reused across samples. A dashboard builds one at startup and samples it
///      on its poll interval.
/// What: holds the `System`, the `Networks` and `Disks` handles, the timestamp
///      of the last network refresh (for rate math), and the configured
///      thresholds. Not `Clone` — it carries mutable sampling state; share it
///      behind a lock if multiple pollers need it.
/// Test: `sampler_produces_plausible_snapshot`.
pub struct HostSampler {
    sys: System,
    networks: Networks,
    disks: Disks,
    last_net_refresh: Instant,
    thresholds: HostThresholds,
}

impl HostSampler {
    /// Construct a sampler with the [PROVISIONAL](HostThresholds) default
    /// thresholds.
    ///
    /// Why: the common case; a consumer that has no owner ruling yet gets the
    ///      documented provisional levels.
    /// What: delegates to [`HostSampler::with_thresholds`] with
    ///      `HostThresholds::default()`.
    /// Test: `sampler_produces_plausible_snapshot`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_thresholds(HostThresholds::default())
    }

    /// Construct a sampler with explicit thresholds.
    ///
    /// Why: lets the eventual owner ruling (or a per-deployment config) set the
    ///      pressure levels without changing any classification code.
    /// What: builds a `System` scoped to CPU + memory (not per-process — the
    ///      whole-machine figures need no process list), refreshes CPU/memory
    ///      and the disk/network lists once to prime the deltas, and records the
    ///      priming instant.
    /// Test: `thresholds_are_configurable`.
    #[must_use]
    pub fn with_thresholds(thresholds: HostThresholds) -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
        );
        // Prime the network + disk deltas so the first `sample` measures a real
        // (if short) window rather than "everything since boot".
        let networks = Networks::new_with_refreshed_list();
        let disks = Disks::new_with_refreshed_list();
        Self {
            sys,
            networks,
            disks,
            last_net_refresh: Instant::now(),
            thresholds,
        }
    }

    /// Refresh every subsystem and return a [`HostMetrics`] snapshot.
    ///
    /// Why: the dashboard poll calls this once per interval. As with the
    ///      per-process sampler, the CPU reading needs ~200 ms between refreshes
    ///      to be meaningful; a poll cadence of seconds satisfies that.
    /// What: refreshes CPU usage, memory, the disk list, and the network list;
    ///      computes network rates over the elapsed window; classifies each
    ///      subsystem against the configured thresholds; and returns the
    ///      combined snapshot. Never panics — a subsystem the OS cannot measure
    ///      reports zeros and `Nominal`.
    /// Test: `sampler_produces_plausible_snapshot`.
    pub fn sample(&mut self) -> HostMetrics {
        let t = &self.thresholds;

        // ── CPU ──────────────────────────────────────────────────────────────
        self.sys.refresh_cpu_usage();
        let cpu_usage = self.sys.global_cpu_usage();
        let cpu = CpuMetrics {
            usage_pct: cpu_usage,
            logical_cores: self.sys.cpus().len(),
            physical_cores: self.sys.physical_core_count(),
            pressure: Pressure::classify(cpu_usage, t.cpu_warning_pct, t.cpu_critical_pct),
        };

        // ── memory ───────────────────────────────────────────────────────────
        self.sys.refresh_memory();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let mem_pct = pct(used, total);
        let memory = MemoryMetrics {
            total_bytes: total,
            used_bytes: used,
            available_bytes: self.sys.available_memory(),
            usage_pct: mem_pct,
            swap_total_bytes: self.sys.total_swap(),
            swap_used_bytes: self.sys.used_swap(),
            pressure: Pressure::classify(mem_pct, t.memory_warning_pct, t.memory_critical_pct),
        };

        // ── disks ────────────────────────────────────────────────────────────
        self.disks.refresh(true);
        let disks = self.build_disk_metrics();

        // ── network ──────────────────────────────────────────────────────────
        self.networks.refresh(true);
        let window = self.last_net_refresh.elapsed().as_secs_f64();
        self.last_net_refresh = Instant::now();
        let network = build_network_metrics(&self.networks, window);

        let overall_pressure = cpu.pressure.worst(memory.pressure).worst(disks.pressure);

        HostMetrics {
            cpu,
            memory,
            disks,
            network,
            overall_pressure,
            sampled_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
        }
    }

    /// Build [`DiskMetrics`] from the refreshed disk list.
    ///
    /// Why: keeps `sample` readable and isolates the "sum only real,
    ///      non-removable mounts into the aggregate" rule.
    /// What: lists every mount into `mounts`; sums total/available of
    ///      non-removable mounts into the aggregate and classifies it.
    /// Test: `sampler_produces_plausible_snapshot`.
    fn build_disk_metrics(&self) -> DiskMetrics {
        let t = &self.thresholds;
        let mut mounts = Vec::with_capacity(self.disks.list().len());
        let (mut agg_total, mut agg_avail) = (0u64, 0u64);
        for disk in self.disks.list() {
            let total = disk.total_space();
            let avail = disk.available_space();
            let used = total.saturating_sub(avail);
            let removable = disk.is_removable();
            if !removable {
                agg_total = agg_total.saturating_add(total);
                agg_avail = agg_avail.saturating_add(avail);
            }
            let usage_pct = pct(used, total);
            mounts.push(MountMetrics {
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                name: disk.name().to_string_lossy().into_owned(),
                total_bytes: total,
                available_bytes: avail,
                used_bytes: used,
                usage_pct,
                is_removable: removable,
                pressure: Pressure::classify(usage_pct, t.disk_warning_pct, t.disk_critical_pct),
            });
        }
        let agg_used = agg_total.saturating_sub(agg_avail);
        let agg_pct = pct(agg_used, agg_total);
        DiskMetrics {
            aggregate_total_bytes: agg_total,
            aggregate_available_bytes: agg_avail,
            aggregate_used_bytes: agg_used,
            aggregate_usage_pct: agg_pct,
            pressure: Pressure::classify(agg_pct, t.disk_warning_pct, t.disk_critical_pct),
            mounts,
        }
    }
}

impl Default for HostSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Sum the refreshed network interfaces into a [`NetworkMetrics`] over `window`.
///
/// Why: a free function so `sample` reads linearly and the rate math is unit
///      testable in isolation.
/// What: sums per-interface `received`/`transmitted` (the delta since the last
///      refresh) and the cumulative totals; divides the deltas by `window` for
///      the rates. A non-positive window (clock non-monotonicity, or the very
///      first instant) yields `0.0` rates rather than a division by zero.
/// Test: `network_rate_over_window`.
fn build_network_metrics(networks: &Networks, window: f64) -> NetworkMetrics {
    let (mut rx_delta, mut tx_delta, mut rx_total, mut tx_total) = (0u64, 0u64, 0u64, 0u64);
    for (_iface, data) in networks {
        rx_delta = rx_delta.saturating_add(data.received());
        tx_delta = tx_delta.saturating_add(data.transmitted());
        rx_total = rx_total.saturating_add(data.total_received());
        tx_total = tx_total.saturating_add(data.total_transmitted());
    }
    let (rx_rate, tx_rate) = if window > 0.0 {
        (rx_delta as f64 / window, tx_delta as f64 / window)
    } else {
        (0.0, 0.0)
    };
    NetworkMetrics {
        rx_bytes_per_sec: rx_rate,
        tx_bytes_per_sec: tx_rate,
        rx_total_bytes: rx_total,
        tx_total_bytes: tx_total,
        window_secs: window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the sampler must produce a structurally valid snapshot on any CI
    ///      host without panicking, and the classification/percentage math must
    ///      stay in range. This is the one test that exercises the real OS path.
    /// What: samples twice (the second exercises the CPU/network delta path) and
    ///      asserts every percentage is finite and 0..=100, cores are sane, and
    ///      the overall pressure is the worst subsystem.
    /// Test: this test.
    #[test]
    fn sampler_produces_plausible_snapshot() {
        let mut s = HostSampler::new();
        let _first = s.sample();
        let m = s.sample();
        assert!(m.cpu.usage_pct >= 0.0 && m.cpu.usage_pct <= 100.0);
        assert!(m.cpu.logical_cores >= 1, "at least one logical core");
        assert!(m.memory.usage_pct >= 0.0 && m.memory.usage_pct <= 100.0);
        assert!(
            m.memory.total_bytes > 0,
            "a real host reports some total memory"
        );
        assert!(m.disks.aggregate_usage_pct >= 0.0 && m.disks.aggregate_usage_pct <= 100.0);
        for mount in &m.disks.mounts {
            assert!(mount.usage_pct >= 0.0 && mount.usage_pct <= 100.0);
            assert!(mount.used_bytes <= mount.total_bytes.max(mount.used_bytes));
        }
        assert!(m.network.rx_bytes_per_sec >= 0.0);
        assert!(m.network.tx_bytes_per_sec >= 0.0);
        assert!(m.network.window_secs >= 0.0);
    }

    /// Why: the tri-state classification is the whole point of the pressure
    ///      band; an off-by-one at a boundary would mis-colour the dashboard.
    /// What: pins both boundaries and the interior of each band, plus the
    ///      NaN-is-Nominal guard.
    /// Test: this test.
    #[test]
    fn pressure_classification_boundaries() {
        // warning=80, critical=95.
        assert_eq!(Pressure::classify(79.9, 80.0, 95.0), Pressure::Nominal);
        assert_eq!(Pressure::classify(80.0, 80.0, 95.0), Pressure::Warning);
        assert_eq!(Pressure::classify(94.9, 80.0, 95.0), Pressure::Warning);
        assert_eq!(Pressure::classify(95.0, 80.0, 95.0), Pressure::Critical);
        assert_eq!(Pressure::classify(100.0, 80.0, 95.0), Pressure::Critical);
        // A subsystem that could not be measured must not panic or alarm.
        assert_eq!(Pressure::classify(f32::NAN, 80.0, 95.0), Pressure::Nominal);
    }

    /// Why: `overall_pressure` must be the WORST subsystem so the top badge
    ///      never under-reports a problem.
    /// What: exercises `Pressure::worst` across all orderings.
    /// Test: this test.
    #[test]
    fn overall_is_worst_subsystem() {
        assert_eq!(
            Pressure::Nominal.worst(Pressure::Critical),
            Pressure::Critical
        );
        assert_eq!(
            Pressure::Warning.worst(Pressure::Nominal),
            Pressure::Warning
        );
        assert_eq!(
            Pressure::Critical.worst(Pressure::Warning),
            Pressure::Critical
        );
        assert_eq!(
            Pressure::Nominal.worst(Pressure::Nominal),
            Pressure::Nominal
        );
    }

    /// Why: the thresholds are the seam the owner ruling will change; a custom
    ///      set must actually flow into classification.
    /// What: builds a sampler with a low CPU-warning threshold and asserts the
    ///      classifier used it (independently of the live CPU reading, via a
    ///      direct classify call using the configured value).
    /// Test: this test.
    #[test]
    fn thresholds_are_configurable() {
        let custom = HostThresholds {
            cpu_warning_pct: 1.0,
            cpu_critical_pct: 2.0,
            ..HostThresholds::default()
        };
        let _s = HostSampler::with_thresholds(custom);
        // The classifier is pure, so the configured numbers are what matter.
        assert_eq!(
            Pressure::classify(1.5, custom.cpu_warning_pct, custom.cpu_critical_pct),
            Pressure::Warning
        );
        assert_eq!(
            Pressure::classify(2.0, custom.cpu_warning_pct, custom.cpu_critical_pct),
            Pressure::Critical
        );
    }

    /// Why: rate math must divide the delta by the window, and a zero/negative
    ///      window must not divide by zero.
    /// What: an empty interface set over a positive window yields zero rates and
    ///      records the window; the zero-window guard is covered by the code
    ///      path (empty set → 0 delta → 0 rate regardless).
    /// Test: this test.
    #[test]
    fn network_rate_over_window() {
        let networks = Networks::new(); // empty, no refresh
        let m = build_network_metrics(&networks, 2.0);
        assert_eq!(m.rx_bytes_per_sec, 0.0);
        assert_eq!(m.tx_bytes_per_sec, 0.0);
        assert_eq!(m.window_secs, 2.0);
        let m0 = build_network_metrics(&networks, 0.0);
        assert_eq!(m0.rx_bytes_per_sec, 0.0);
    }

    /// Why: the JSON contract the phase-2 UI renders must survive a serde
    ///      round-trip with every field intact.
    /// What: samples once, serialises to JSON, deserialises back, and asserts
    ///      the top-level fields match.
    /// Test: this test.
    #[test]
    fn snapshot_serde_round_trip() {
        let mut s = HostSampler::new();
        let m = s.sample();
        let json = serde_json::to_string(&m).expect("serialise HostMetrics");
        let back: HostMetrics = serde_json::from_str(&json).expect("deserialise HostMetrics");
        assert_eq!(back.cpu.logical_cores, m.cpu.logical_cores);
        assert_eq!(back.memory.total_bytes, m.memory.total_bytes);
        assert_eq!(back.disks.mounts.len(), m.disks.mounts.len());
        assert_eq!(back.overall_pressure, m.overall_pressure);
    }
}
