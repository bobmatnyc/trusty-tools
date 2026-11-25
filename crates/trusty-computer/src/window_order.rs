//! The window-on-top gate of `drag_at` (#11756).
//!
//! Why: cua-driver 0.23.2 drags only in the foreground, posted as global
//! screen input, so the drag goes to whatever window is on top under the
//! pointer. If another window covers the granted one on the drag path, the
//! drag reaches that window's app (delegation-authority refusal; architect
//! decision fc7ec9af2b6d). The bounded daemon cannot answer this: it refuses
//! `list_windows` for any pid but the granted one
//! (`bounded_resource_outside_manifest`), so the broker reads the window
//! order itself.
//! What: a [`WindowLister`] lists the on-screen windows front to back.
//! [`SystemWindowLister`] reads CoreGraphics `CGWindowListCopyWindowInfo`
//! (number, owner pid, layer, bounds; never the title, so no Screen
//! Recording is needed). A transparent window still counts: whether it takes
//! mouse events is not in the list. [`on_top`] maps the drag path from window pixels to
//! desktop points and refuses [`REFUSAL`] unless the granted window is listed
//! once, under the granted pid, and no window in front of it, of any app or
//! layer, touches the path. The Dock's display-sized window counts only along
//! the display edges where the Dock can be ([`DOCK_BAND`]).
//! Test: `tests/window_order.rs`; live: `tests/live_pointer.rs`.

use serde_json::{Value, json};

use crate::pointer::{Frame, Refusal};

/// The refusal code of a drag whose path the granted window does not own.
pub const REFUSAL: &str = "drag_blocked_window_not_on_top";
/// The refusal code when the window order cannot be read or mapped.
pub const UNAVAILABLE: &str = "window_order_unavailable";
/// The largest relative difference of the x and y pixel-to-point scales.
///
/// A capture is `scale` times the window's bounds on both axes; a capture
/// without the title bar or with a shadow breaks the match.
pub const SCALE_TOLERANCE: f64 = 0.02;
/// The window layer of the Dock (`kCGDockWindowLevel`).
pub const DOCK_LAYER: i64 = 20;
/// The width, in points, of the display-edge bands a Dock-layer window covers.
///
/// The largest Dock tile is 128 points; the band adds room for its margin.
pub const DOCK_BAND: f64 = 160.0;

/// One on-screen window as the window server reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowRecord {
    /// The window number (`window_id` of the driver).
    pub window_id: u64,
    /// The owning process.
    pub pid: u32,
    /// The window layer; 0 is a normal app window.
    pub layer: i64,
    /// The frame in desktop points, top-left origin.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Lists the on-screen windows, front to back.
pub trait WindowLister: Send + Sync {
    /// Every on-screen window of every app and layer, frontmost first.
    fn on_screen(&self) -> Result<Vec<WindowRecord>, String>;
}

/// The window server's own list, through CoreGraphics.
#[derive(Debug, Default)]
pub struct SystemWindowLister;

impl WindowLister for SystemWindowLister {
    fn on_screen(&self) -> Result<Vec<WindowRecord>, String> {
        cg::on_screen()
    }
}

/// The rectangles of `w` that take mouse events, as far as the list shows.
///
/// Why: the Dock owns one display-sized window at [`DOCK_LAYER`] that is
/// transparent except for the Dock itself, so taking it whole would refuse
/// every drag; its position and size are not in the list.
/// What: a window at [`DOCK_LAYER`] covers the [`DOCK_BAND`]-wide bands
/// along its left, right and bottom edges, where the Dock can sit or pop up;
/// any other window covers its whole frame.
/// Test: `dock_layer_window_covers_only_its_edge_bands`.
fn cover_rects(w: &WindowRecord) -> Vec<WindowRecord> {
    if w.layer != DOCK_LAYER {
        return vec![*w];
    }
    let band = |x: f64, y: f64, width: f64, height: f64| WindowRecord {
        x,
        y,
        width,
        height,
        ..*w
    };
    vec![
        band(w.x, w.y, DOCK_BAND, w.height),
        band(w.x + w.width - DOCK_BAND, w.y, DOCK_BAND, w.height),
        band(w.x, w.y + w.height - DOCK_BAND, w.width, DOCK_BAND),
    ]
}

/// True when the segment `a`-`b` meets the closed rectangle of `w`.
///
/// What: Liang-Barsky clipping; a non-finite bound counts as touching (fail
/// closed).
fn touches(w: &WindowRecord, a: (f64, f64), b: (f64, f64)) -> bool {
    if ![w.x, w.y, w.width, w.height].iter().all(|v| v.is_finite()) {
        return true;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-dx, a.0 - w.x),
        (dx, w.x + w.width - a.0),
        (-dy, a.1 - w.y),
        (dy, w.y + w.height - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else if p < 0.0 {
            t0 = t0.max(q / p);
        } else {
            t1 = t1.min(q / p);
        }
    }
    t0 <= t1
}

/// Refuse unless the granted window is on top along the whole drag path.
///
/// What: `points` are window-local capture pixels (already inside `frame`).
/// They map to desktop points as `bounds.origin + p * bounds.size / frame`.
/// Refuses [`REFUSAL`] when `window_id` is not listed, is listed more than
/// once or under another pid, or any window listed before it touches the
/// path between consecutive points ([`cover_rects`] of that window). Refuses [`UNAVAILABLE`] when the x and y
/// scales differ by more than [`SCALE_TOLERANCE`].
/// Postcondition: `Ok` means no window in front of the granted one meets the
/// mapped path; the value is the audit record.
/// Test: `covered_path_is_refused_and_clear_path_admitted`,
/// `granted_window_must_be_listed_once_under_its_pid`,
/// `capture_and_bounds_must_share_one_scale`.
pub fn on_top(
    windows: &[WindowRecord],
    pid: u32,
    window_id: u64,
    frame: Frame,
    points: &[(f64, f64)],
) -> Result<Value, Refusal> {
    let blocked = |detail: String| (REFUSAL, detail);
    let mut named = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.window_id == window_id);
    let Some((rank, granted)) = named.next() else {
        return Err(blocked(format!(
            "window {window_id} is not on screen; focus it and look again"
        )));
    };
    if named.next().is_some() || granted.pid != pid {
        return Err(blocked(format!(
            "window {window_id} is not one window of the granted pid {pid}"
        )));
    }
    let (sx, sy) = (granted.width / frame.width, granted.height / frame.height);
    let scaled = sx.is_finite() && sy.is_finite() && sx > 0.0 && sy > 0.0;
    if !scaled || (sx - sy).abs() > SCALE_TOLERANCE * sx.max(sy) {
        return Err((
            UNAVAILABLE,
            "the window's capture and its on-screen bounds do not match".into(),
        ));
    }
    let path: Vec<(f64, f64)> = points
        .iter()
        .map(|(x, y)| (granted.x + x * sx, granted.y + y * sy))
        .collect();
    let segments: Vec<((f64, f64), (f64, f64))> = match path.as_slice() {
        [one] => vec![(*one, *one)],
        many => many.windows(2).map(|s| (s[0], s[1])).collect(),
    };
    let cover = windows[..rank].iter().find(|w| {
        cover_rects(w)
            .iter()
            .any(|r| segments.iter().any(|(a, b)| touches(r, *a, *b)))
    });
    if let Some(w) = cover {
        return Err(blocked(format!(
            "window {} of pid {} (layer {}) is in front of the granted window on the drag path; \
             focus the granted window first",
            w.window_id, w.pid, w.layer
        )));
    }
    Ok(json!({"windows_in_front": rank, "layer": granted.layer}))
}

/// The CoreGraphics reader; off macOS every read fails (fail closed).
#[cfg(not(target_os = "macos"))]
mod cg {
    pub fn on_screen() -> Result<Vec<super::WindowRecord>, String> {
        Err("the window order is read on macOS only".into())
    }
}

#[cfg(target_os = "macos")]
mod cg {
    use std::ffi::c_void;

    use super::WindowRecord;

    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// `kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements`.
    const ON_SCREEN: u32 = 1 | 1 << 4;
    /// `kCFNumberSInt64Type`.
    const SINT64: isize = 4;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, out: *mut c_void) -> u8;
        fn CFGetTypeID(value: CFTypeRef) -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFRelease(value: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
        static kCGWindowNumber: CFTypeRef;
        static kCGWindowOwnerPID: CFTypeRef;
        static kCGWindowLayer: CFTypeRef;
        static kCGWindowBounds: CFTypeRef;
    }

    /// Releases the copied window list.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the non-null result of a Copy function, released once.
            unsafe { CFRelease(self.0) }
        }
    }

    /// The integer under `key` of `dict`, when it is a CFNumber.
    ///
    /// # Safety
    /// `dict` is a live CFDictionary and `key` a live CFString.
    unsafe fn int(dict: CFTypeRef, key: CFTypeRef) -> Option<i64> {
        // SAFETY: the caller's contract; the value is type-checked before use.
        unsafe {
            let value = CFDictionaryGetValue(dict, key);
            if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
                return None;
            }
            let mut out = 0_i64;
            (CFNumberGetValue(value, SINT64, std::ptr::addr_of_mut!(out).cast()) != 0)
                .then_some(out)
        }
    }

    /// One window record; `None` when a field is missing or malformed.
    ///
    /// # Safety
    /// `dict` is a live CFDictionary from the window list.
    unsafe fn record(dict: CFTypeRef) -> Option<WindowRecord> {
        // SAFETY: the caller's contract; the keys are CoreGraphics constants.
        unsafe {
            let window_id = u64::try_from(int(dict, kCGWindowNumber)?).ok()?;
            let pid = u32::try_from(int(dict, kCGWindowOwnerPID)?).ok()?;
            let layer = int(dict, kCGWindowLayer)?;
            let bounds = CFDictionaryGetValue(dict, kCGWindowBounds);
            if bounds.is_null() || CFGetTypeID(bounds) != CFDictionaryGetTypeID() {
                return None;
            }
            let mut r = CGRect::default();
            if !CGRectMakeWithDictionaryRepresentation(bounds, &mut r) {
                return None;
            }
            Some(WindowRecord {
                window_id,
                pid,
                layer,
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            })
        }
    }

    /// Every on-screen window, front to back; any malformed record fails the read.
    pub fn on_screen() -> Result<Vec<WindowRecord>, String> {
        // SAFETY: a Copy call; a null result is checked before use and the
        // array is released by `Owned` after every element is read.
        unsafe {
            let list = CGWindowListCopyWindowInfo(ON_SCREEN, 0);
            if list.is_null() {
                return Err("the window server returned no window list".into());
            }
            let list = Owned(list);
            if CFGetTypeID(list.0) != CFArrayGetTypeID() {
                return Err("the window list is not an array".into());
            }
            let count = CFArrayGetCount(list.0);
            let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
            for i in 0..count {
                let dict = CFArrayGetValueAtIndex(list.0, i);
                if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                    return Err(format!("window record {i} is not a dictionary"));
                }
                out.push(record(dict).ok_or_else(|| format!("window record {i} is malformed"))?);
            }
            Ok(out)
        }
    }
}
