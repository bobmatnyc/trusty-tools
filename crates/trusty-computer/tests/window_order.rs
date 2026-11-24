//! The window-on-top gate of `drag_at` (issue #11756): the path mapping from
//! capture pixels to desktop points, the covering test against every window
//! in front of the granted one, and the fail-closed refusals.

use trusty_computer::pointer::Frame;
use trusty_computer::window_order::{
    DOCK_BAND, DOCK_LAYER, REFUSAL, UNAVAILABLE, WindowRecord, on_top,
};

const PID: u32 = 100;
const WID: u64 = 1;

fn win(window_id: u64, pid: u32, layer: i64, rect: (f64, f64, f64, f64)) -> WindowRecord {
    WindowRecord {
        window_id,
        pid,
        layer,
        x: rect.0,
        y: rect.1,
        width: rect.2,
        height: rect.3,
    }
}

/// The granted window: 400 x 200 points at (100, 100), captured at 2x.
fn granted() -> WindowRecord {
    win(WID, PID, 0, (100.0, 100.0, 400.0, 200.0))
}

const FRAME: Frame = Frame {
    width: 800.0,
    height: 400.0,
};

/// Pixels (10, 10) -> (100, 10): desktop points (105, 105) -> (150, 105).
const PATH: [(f64, f64); 2] = [(10.0, 10.0), (100.0, 10.0)];

fn code(windows: &[WindowRecord], frame: Frame, path: &[(f64, f64)]) -> Option<&'static str> {
    on_top(windows, PID, WID, frame, path).err().map(|(c, _)| c)
}

#[test]
fn covered_path_is_refused_and_clear_path_admitted() {
    // A window behind the granted one never counts, even over the path.
    let behind = win(9, 7, 0, (0.0, 0.0, 1000.0, 1000.0));
    let ok = on_top(&[granted(), behind], PID, WID, FRAME, &PATH).expect("on top");
    assert_eq!(ok["windows_in_front"], 0);
    // A window in front, clear of the path, is allowed: just past the end point.
    let beside = win(9, 7, 0, (151.0, 100.0, 50.0, 50.0));
    let ok = on_top(&[beside, granted()], PID, WID, FRAME, &PATH).expect("clear path");
    assert_eq!(ok["windows_in_front"], 1);
    for (label, rect) in [
        ("start", (90.0, 90.0, 20.0, 20.0)),
        ("end", (140.0, 100.0, 20.0, 10.0)),
        ("middle only", (120.0, 104.0, 5.0, 5.0)),
        ("touching the end at its edge", (150.0, 105.0, 10.0, 10.0)),
    ] {
        for (pid, layer) in [(7, 0), (PID, 0), (7, 3), (7, 25)] {
            let front = win(9, pid, layer, rect);
            assert_eq!(
                code(&[front, granted()], FRAME, &PATH),
                Some(REFUSAL),
                "{label}: pid {pid} layer {layer}"
            );
        }
    }
    // A window with no finite frame is taken to cover everything.
    let unknown = win(9, 7, 0, (f64::NAN, 0.0, 1.0, 1.0));
    assert_eq!(code(&[unknown, granted()], FRAME, &PATH), Some(REFUSAL));
}

#[test]
fn granted_window_must_be_listed_once_under_its_pid() {
    let other = win(9, 7, 0, (0.0, 0.0, 10.0, 10.0));
    let foreign = win(WID, PID + 1, 0, (100.0, 100.0, 400.0, 200.0));
    for (label, list) in [
        ("not on screen", vec![other]),
        ("nothing listed", vec![]),
        ("another pid", vec![foreign]),
        ("listed twice", vec![granted(), granted()]),
    ] {
        assert_eq!(code(&list, FRAME, &PATH), Some(REFUSAL), "{label}");
    }
}

#[test]
fn capture_and_bounds_must_share_one_scale() {
    // A capture without the title bar is shorter than the window's bounds.
    let short = Frame {
        width: 800.0,
        height: 340.0,
    };
    assert_eq!(code(&[granted()], short, &PATH), Some(UNAVAILABLE));
    // A one-pixel rounding of a 2x capture is within tolerance.
    let rounded = Frame {
        width: 801.0,
        height: 400.0,
    };
    assert!(on_top(&[granted()], PID, WID, rounded, &PATH).is_ok());
}

#[test]
fn dock_layer_window_covers_only_its_edge_bands() {
    let display = (0.0, 0.0, 3440.0, 1440.0);
    let dock = win(27, 55, DOCK_LAYER, display);
    // The granted window spans the display's lower edge band.
    let low = win(WID, PID, 0, (100.0, 1000.0, 800.0, 400.0));
    let frame = Frame {
        width: 1600.0,
        height: 800.0,
    };
    let top = 1440.0 - DOCK_BAND - 1000.0;
    // Inside the display, clear of every band: admitted.
    let clear = [(400.0, 20.0), (1200.0, 2.0 * top - 2.0)];
    assert!(on_top(&[dock, low], PID, WID, frame, &clear).is_ok());
    // Into the bottom band, or the left band.
    let bottom = [(400.0, 20.0), (400.0, 2.0 * top + 2.0)];
    assert_eq!(code(&[dock, low], frame, &bottom), Some(REFUSAL));
    let left = [(400.0, 20.0), (2.0 * (DOCK_BAND - 100.0) - 2.0, 20.0)];
    assert_eq!(code(&[dock, low], frame, &left), Some(REFUSAL));
    // Any other layer covers its whole frame.
    let overlay = win(27, 55, DOCK_LAYER + 1, display);
    assert_eq!(code(&[overlay, low], frame, &clear), Some(REFUSAL));
}
