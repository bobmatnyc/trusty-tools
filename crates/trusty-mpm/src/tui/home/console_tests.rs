//! Tests for the console look of the home screen (#10966).
//!
//! The filled model below exists only under `cfg(test)`: it proves the machine
//! cards and per-service history draw once #10942 and #10943 give them a live
//! source. Production code never builds it; the live screen says "no data yet".

use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};

use super::console::{
    Card, ConsoleModel, Machine, NO_DATA, ServiceRow, model_from_state, render_body, status_of,
};
use super::probe::no_uds_endpoint;
use super::services::ServiceId;
use super::state::{HealthUpdate, HomeState, TileHealth};
use crate::tui::theme::Tone;

fn text(buffer: &Buffer) -> String {
    let w = buffer.area.width as usize;
    buffer
        .content()
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
        .collect()
}

/// A wave of `n` samples between `lo` and `hi`, with gaps where `gap` says.
fn wave(n: usize, lo: u64, hi: u64, seed: u64, gap: impl Fn(usize) -> bool) -> Vec<Option<u64>> {
    (0..n)
        .map(|i| {
            let x = (i as u64 * 7 + seed * 13) % 17;
            (!gap(i)).then_some(lo + (hi - lo) * x / 16)
        })
        .collect()
}

/// A filled model: plausible numbers, made-up history.
fn filled() -> ConsoleModel {
    let none = |_| false;
    let card = |title, value: &str, meta: &str, samples| Card {
        title,
        value: value.into(),
        meta: meta.into(),
        badge: ("ok".into(), Tone::Ok),
        samples,
    };
    let machine = Machine {
        badge: ("nominal".into(), Tone::Ok),
        cards: vec![
            card(
                "CPU",
                "18.6%",
                "16 logical / 16 physical",
                wave(60, 10, 40, 1, none),
            ),
            card(
                "MEMORY",
                "57.9 / 128.0 GiB",
                "swap 2.6 / 4.0 GiB · 45.2% used",
                wave(60, 50, 60, 2, none),
            ),
            card(
                "DISK",
                "5815.3 / 7443.7 GiB",
                "78.1% used",
                wave(60, 77, 79, 3, none),
            ),
            card(
                "NETWORK",
                "↓ 1.2  ↑ 0.3 MB/s",
                "over 1.0 s",
                wave(60, 0, 30, 4, |i| i % 23 == 5),
            ),
        ],
    };
    let svc =
        |name: &str, version: &str, word: &str, tone, cpu: &str, mem: &str, seed| ServiceRow {
            name: name.into(),
            version: version.into(),
            status: (word.into(), tone),
            cpu: Some(cpu.into()),
            mem: Some(mem.into()),
            cpu_history: Some(wave(60, 0, 20, seed, |i| i < 3)),
            mem_history: Some(wave(60, 30, 40, seed + 1, none)),
        };
    ConsoleModel {
        machine: Some(machine),
        rows: vec![
            svc(
                "Agents",
                "0.31.0",
                "available",
                Tone::Warn,
                "0.4",
                "212 MiB",
                1,
            ),
            svc("Analyze", "0.8.4", "ready", Tone::Ok, "0.0", "48 MiB", 2),
            svc("Console", "0.12.6", "running", Tone::Ok, "1.1", "96 MiB", 3),
            svc("Memory", "0.9.2", "ready", Tone::Ok, "0.2", "310 MiB", 4),
            svc("MPM", "1.12.1", "running", Tone::Ok, "2.3", "140 MiB", 5),
            svc("Review", "0.6.0", "ready", Tone::Ok, "0.0", "31 MiB", 6),
            svc("Search", "0.57.1", "running", Tone::Ok, "3.8", "1.2 GiB", 7),
        ],
        selected: 4,
    }
}

#[test]
fn console_home_shows_no_data_yet_for_machine_and_history() {
    let mut state = HomeState::default();
    state.apply_health(HealthUpdate {
        id: ServiceId::Search,
        health: TileHealth::Up {
            version: Some("0.57.1".into()),
        },
    });
    let model = model_from_state(&state);
    assert_eq!(model.machine, None, "no host-metrics source yet (#10942)");
    assert!(
        model
            .rows
            .iter()
            .all(|r| r.cpu_history.is_none() && r.cpu.is_none())
    );
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).expect("test backend");
    terminal
        .draw(|f| render_body(f, f.area(), &model))
        .expect("draw");
    let screen = text(terminal.backend().buffer());
    assert!(screen.contains("MACHINE STATUS"));
    assert!(screen.contains(" NO DATA "), "section badge: {screen}");
    for title in ["CPU", "MEMORY", "DISK", "NETWORK"] {
        assert!(screen.contains(title), "card {title}");
    }
    assert!(screen.matches(NO_DATA).count() >= 4 + 2 * ServiceId::ALL.len());
    assert!(screen.contains("CPU · last 10 min"));
    assert!(screen.contains("0.57.1"));
    // Under 100 columns the history columns drop.
    let mut narrow = Terminal::new(TestBackend::new(90, 30)).expect("test backend");
    narrow
        .draw(|f| render_body(f, f.area(), &model))
        .expect("draw");
    assert!(!text(narrow.backend().buffer()).contains("last 10 min"));
}

#[test]
fn a_filled_model_draws_cards_and_sparklines() {
    let mut terminal = Terminal::new(TestBackend::new(160, 26)).expect("test backend");
    let model = filled();
    terminal
        .draw(|f| render_body(f, f.area(), &model))
        .expect("draw the filled model");
    let buffer = terminal.backend().buffer();
    let screen = text(buffer);
    assert!(screen.contains(" NOMINAL "), "{screen}");
    assert!(screen.contains("18.6%"));
    assert!(screen.contains("212 MiB"));
    assert!(
        !screen.contains(NO_DATA),
        "a filled model fills every panel"
    );
    // Sparkline bars are drawn in the accent.
    let bar = buffer
        .content()
        .iter()
        .find(|c| !c.symbol().trim().is_empty() && "▁▂▃▄▅▆▇█".contains(c.symbol()))
        .expect("a sparkline bar");
    assert_eq!(bar.fg, Color::Rgb(0xd9, 0x77, 0x42));
}

// See #10899: Agents and Console have no UDS endpoint; their rows read so in
// a neutral tone, never the red of Down.
#[test]
fn no_uds_endpoint_rows_are_muted_never_down() {
    let mut state = HomeState::default();
    for id in [ServiceId::Agents, ServiceId::Console] {
        let health = no_uds_endpoint(id).expect("no UDS endpoint");
        assert_eq!(
            status_of(&health),
            ("no UDS endpoint".to_string(), Tone::Muted)
        );
        state.apply_health(HealthUpdate { id, health });
    }
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).expect("test backend");
    terminal
        .draw(|f| render_body(f, f.area(), &model_from_state(&state)))
        .expect("draw");
    let screen = text(terminal.backend().buffer());
    assert_eq!(screen.matches(" NO UDS ENDPOINT ").count(), 2, "{screen}");
    assert!(!screen.contains(" DOWN "), "{screen}");
}
