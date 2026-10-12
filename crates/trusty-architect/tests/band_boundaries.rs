//! #9171 A3b-2 (A3a critic carry-over): every `BAND_*` line is pinned from
//! both sides. Each row sits on the line or a hair past it on 16 cores, so a
//! constant moved either way (REHOLD 1.25 -> 1.27, or -> 1.24) turns a row
//! red. Every row is checked before the assertion.

use trusty_architect::poller::{BandConfig, builder_band};

/// `(line, load15, free GiB, mem free %, stored band, want)`.
type Row = (
    &'static str,
    f64,
    f64,
    f64,
    Option<&'static str>,
    &'static str,
);

const ROWS: [Row; 14] = [
    (
        "STOP_LOAD 1.875 x 16 = 30: on the line",
        30.0,
        300.0,
        70.0,
        None,
        "hold",
    ),
    ("STOP_LOAD: past it", 30.01, 300.0, 70.0, None, "stop"),
    (
        "HOLD_LOAD 1.125 x 16 = 18: under",
        17.99,
        300.0,
        70.0,
        None,
        "open",
    ),
    ("HOLD_LOAD: on the line", 18.0, 300.0, 70.0, None, "hold"),
    (
        "OPEN_LOAD 1.0 x 16 = 16: under",
        15.99,
        300.0,
        70.0,
        Some("hold"),
        "open",
    ),
    (
        "OPEN_LOAD: on the line",
        16.0,
        300.0,
        70.0,
        Some("hold"),
        "hold",
    ),
    (
        "REHOLD_LOAD 1.25 x 16 = 20: on the line",
        20.0,
        300.0,
        70.0,
        Some("open"),
        "open",
    ),
    (
        "REHOLD_LOAD: past it",
        20.01,
        300.0,
        70.0,
        Some("open"),
        "hold",
    ),
    ("STOP_FREE_GIB 100: under", 0.0, 99.99, 70.0, None, "stop"),
    ("STOP_FREE_GIB: on the line", 0.0, 100.0, 70.0, None, "hold"),
    ("HOLD_FREE_GIB 200: under", 0.0, 199.99, 70.0, None, "hold"),
    ("HOLD_FREE_GIB: on the line", 0.0, 200.0, 70.0, None, "open"),
    (
        "STOP_MEM_FREE_PCT 20: under",
        0.0,
        300.0,
        19.99,
        None,
        "stop",
    ),
    (
        "STOP_MEM_FREE_PCT: on the line",
        0.0,
        300.0,
        20.0,
        None,
        "open",
    ),
];

#[test]
fn every_band_line_is_pinned_from_both_sides() {
    let cfg = BandConfig::default();
    let failures: Vec<String> = ROWS
        .iter()
        .filter_map(|(name, load, free, mem, prev, want)| {
            let got = builder_band(&cfg, *load, 16, *free, Some(*mem), Some(0.0), *prev);
            (got.as_str() != *want).then(|| format!("{name}: got {}, want {want}", got.as_str()))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}
