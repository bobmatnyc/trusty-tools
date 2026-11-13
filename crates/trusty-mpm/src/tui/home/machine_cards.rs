//! MACHINE STATUS cards and per-service history built from the console feeds
//! (#10942, #10943).
//!
//! Why: drawing takes a plain view model ([`super::console::ConsoleModel`]);
//! this file is the one place that turns [`MachineFeeds`] into it, so every
//! feed state — pending, cold, ready, each fault — has exactly one look.
//! What: [`machine_model`] builds the section badge and the CPU, MEMORY, DISK
//! and NETWORK cards with their sparklines; [`service_figures`] builds one
//! service row's `%CPU`, `MEM` and two history sparklines from
//! `HistorySnapshot::service_samples` (#10943). Only a ready feed yields
//! numbers; every other state yields a label and no values.
//! Test: `machine_tests.rs`.

use trusty_common::console_rpc::ServiceStatus;
use trusty_common::console_rpc::metrics::{HistorySnapshot, ServiceSample};
use trusty_common::host_metrics::{HostMetrics, Pressure};

use super::console::{Card, Machine, NO_DATA};
use super::machine::{Feed, MachineFeeds};
use crate::tui::theme::Tone;

/// The card titles, in display order.
pub const TITLES: [&str; 4] = ["CPU", "MEMORY", "DISK", "NETWORK"];
/// Percentages are plotted in tenths, against this ceiling.
const PCT_MAX: u64 = 1000;
const GIB: f64 = (1u64 << 30) as f64;
const MIB: f64 = (1u64 << 20) as f64;

/// The badge a host pressure shows as.
///
/// Test: `a_ready_feed_fills_every_card_with_live_values`.
pub fn pressure_badge(pressure: Pressure) -> (String, Tone) {
    let (word, tone) = match pressure {
        Pressure::Nominal => ("nominal", Tone::Ok),
        Pressure::Warning => ("warning", Tone::Warn),
        Pressure::Critical => ("critical", Tone::Err),
        // #10942: `Pressure` is non-exhaustive; a new level is unknown, not ok.
        _ => ("unknown", Tone::Degraded),
    };
    (word.to_string(), tone)
}

/// Four cards that carry a label and no values.
fn labelled_cards(value: &str, meta: &str, badge: (String, Tone)) -> Vec<Card> {
    TITLES
        .into_iter()
        .map(|title| Card {
            title,
            value: value.to_string(),
            meta: meta.to_string(),
            badge: badge.clone(),
            samples: Vec::new(),
            max: None,
            graph_note: String::new(),
        })
        .collect()
}

/// MACHINE STATUS from the feeds; `None` while nothing has been asked yet.
///
/// Why: a stale or default number drawn as live is the failure this screen
/// must not have (#10942), so only [`Feed::Ready`] produces values.
/// What: `Pending` is `None` (the "no data yet" placeholder); `Cold` is four
/// "warming up" cards; a fault is four cards carrying the fault's label and
/// detail; `Ready` is the live cards, with sparklines from the history feed
/// when it is ready and a note naming the history's state when not.
/// Test: `a_ready_feed_fills_every_card_with_live_values`,
/// `every_fault_draws_its_own_label_and_no_values`,
/// `a_cold_console_reads_warming_up`.
pub fn machine_model(feeds: &MachineFeeds) -> Option<Machine> {
    let machine = match &feeds.status {
        Feed::Pending => return None,
        Feed::Cold => {
            let badge = ("warming up".to_string(), Tone::Info);
            return Some(Machine {
                cards: labelled_cards(NO_DATA, "the console has no host sample yet", badge.clone()),
                badge,
            });
        }
        Feed::Failed(fault) => {
            let (word, tone) = fault.label();
            let badge = (word.to_string(), tone);
            return Some(Machine {
                cards: labelled_cards("unknown", &fault.detail(), badge.clone()),
                badge,
            });
        }
        Feed::Ready(machine) => machine,
    };
    let host = &machine.host;
    let history = feeds.history.ready().map(|h| h.samples.as_slice());
    let note = history_note(&feeds.history);
    let series = |f: fn(&HostMetrics) -> u64| -> Vec<Option<u64>> {
        history
            .unwrap_or_default()
            .iter()
            .map(|s| Some(f(s)))
            .collect()
    };
    let card =
        |title, value: String, meta: String, pressure: Option<Pressure>, samples, max| Card {
            title,
            value,
            meta,
            badge: pressure.map_or(("live".to_string(), Tone::Info), pressure_badge),
            samples,
            max,
            graph_note: note.clone(),
        };
    let cpu = &host.cpu;
    let mem = &host.memory;
    let disk = &host.disks;
    let net = &host.network;
    let cores = match cpu.physical_cores {
        Some(physical) => format!("{} logical / {physical} physical", cpu.logical_cores),
        None => format!("{} logical", cpu.logical_cores),
    };
    let cards = vec![
        card(
            "CPU",
            format!("{:.1}%", cpu.usage_pct),
            cores,
            Some(cpu.pressure),
            series(|s| tenths(s.cpu.usage_pct)),
            Some(PCT_MAX),
        ),
        card(
            "MEMORY",
            format!(
                "{:.1} / {:.1} GiB",
                gib(mem.used_bytes),
                gib(mem.total_bytes)
            ),
            format!(
                "swap {:.1} / {:.1} GiB · {:.1}% used",
                gib(mem.swap_used_bytes),
                gib(mem.swap_total_bytes),
                mem.usage_pct
            ),
            Some(mem.pressure),
            series(|s| tenths(s.memory.usage_pct)),
            Some(PCT_MAX),
        ),
        card(
            "DISK",
            format!(
                "{:.1} / {:.1} GiB",
                gib(disk.aggregate_used_bytes),
                gib(disk.aggregate_total_bytes)
            ),
            format!("{:.1}% used", disk.aggregate_usage_pct),
            Some(disk.pressure),
            series(|s| tenths(s.disks.aggregate_usage_pct)),
            Some(PCT_MAX),
        ),
        card(
            "NETWORK",
            format!(
                "↓ {:.1}  ↑ {:.1} MB/s",
                net.rx_bytes_per_sec / 1e6,
                net.tx_bytes_per_sec / 1e6
            ),
            format!("over {:.1} s", net.window_secs),
            None,
            series(|s| (s.network.rx_bytes_per_sec + s.network.tx_bytes_per_sec).max(0.0) as u64),
            None,
        ),
    ];
    Some(Machine {
        badge: pressure_badge(host.overall_pressure),
        cards,
    })
}

/// What a sparkline slot says when it has no samples to draw.
///
/// Test: `every_fault_draws_its_own_label_and_no_values`.
pub fn history_note(history: &Feed<HistorySnapshot>) -> String {
    match history {
        Feed::Pending => "waiting for console.history".to_string(),
        Feed::Cold | Feed::Ready(_) => "no samples yet".to_string(),
        Feed::Failed(fault) => format!("history {}", fault.label().0),
    }
}

/// One service row's live figures; every field is `None` unless the history
/// feed is ready.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceFigures {
    /// `%CPU` of the newest sample.
    pub cpu: Option<String>,
    /// `MEM` of the newest sample.
    pub mem: Option<String>,
    /// CPU over the window; an unmeasured or unknown-status sample is a gap.
    pub cpu_history: Option<Vec<Option<u64>>>,
    /// Resident memory over the window, same gap rule.
    pub mem_history: Option<Vec<Option<u64>>>,
}

/// The figures of the service whose wire id is `id` (#10943).
///
/// What: a sample whose status this build does not know is a gap, never a
/// value (#10996 `ServiceStatus::Unknown`); when the newest one is unknown,
/// `%CPU` and `MEM` read `unknown`. A service the history has no samples for
/// gets empty series, which draw as "no samples yet".
/// Test: `service_rows_take_cpu_and_memory_from_service_samples`,
/// `an_unknown_service_status_is_a_gap_not_a_value`.
pub fn service_figures(history: &Feed<HistorySnapshot>, id: &str) -> ServiceFigures {
    let Some(snapshot) = history.ready() else {
        return ServiceFigures::default();
    };
    let samples: &[ServiceSample] = snapshot.service_samples.get(id).map_or(&[], Vec::as_slice);
    let known = |s: &ServiceSample| s.status != ServiceStatus::Unknown;
    let (cpu, mem) = match samples.last() {
        None => (None, None),
        Some(s) if !known(s) => (Some("unknown".to_string()), Some("unknown".to_string())),
        Some(s) => (
            s.cpu_pct.map(|c| format!("{c:.1}")),
            s.rss_bytes.map(format_rss),
        ),
    };
    ServiceFigures {
        cpu,
        mem,
        cpu_history: Some(
            samples
                .iter()
                .map(|s| s.cpu_pct.filter(|_| known(s)).map(tenths))
                .collect(),
        ),
        mem_history: Some(
            samples
                .iter()
                .map(|s| s.rss_bytes.filter(|_| known(s)))
                .collect(),
        ),
    }
}

/// A percentage in tenths, clamped at zero.
fn tenths(pct: f32) -> u64 {
    (f64::from(pct) * 10.0).max(0.0).round() as u64
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / GIB
}

/// Resident memory as `212 MiB` or `1.2 GiB`.
fn format_rss(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else {
        format!("{:.0} MiB", b / MIB)
    }
}
