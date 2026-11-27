//! Unit tests for `pages_capture`: mapping, staleness, viewport bounds, PNG size.

use super::*;

/// A viewport capture of a `css` viewport at `dpr` (window.devicePixelRatio,
/// which already includes page zoom) and capture `scale`.
fn shot(css: [f64; 2], dpr: f64, scale: f64) -> Capture {
    let px = |v: f64| (v * dpr * scale).round() as u32;
    Capture {
        id: "cap".into(),
        lease: "lease".into(),
        tab: 7,
        document: "doc".into(),
        taken: 100.0,
        region: [0.0, 0.0, css[0], css[1]],
        image: [px(css[0]), px(css[1])],
        viewport: css,
        metrics: Json::Null,
    }
}

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
}

#[test]
fn image_pixels_map_to_css_at_1x_2x_and_zoomed() {
    // 1x: image pixel n is CSS pixel n (centre of the pixel).
    let one = shot([800.0, 600.0], 1.0, 1.0);
    assert!(close(to_css(&one, 100, 50).unwrap(), (100.5, 50.5)));
    // 2x (Retina): 1600x1200 image of an 800x600 viewport.
    let two = shot([800.0, 600.0], 2.0, 1.0);
    assert_eq!(two.image, [1600, 1200]);
    assert!(close(to_css(&two, 200, 100).unwrap(), (100.25, 50.25)));
    // Zoomed 150% on a 2x screen: the viewport shrinks to 800x600 CSS px of a
    // 1200x900-point window, devicePixelRatio reads 3.
    let zoomed = shot([800.0, 600.0], 3.0, 1.0);
    assert_eq!(zoomed.image, [2400, 1800]);
    assert!(close(
        to_css(&zoomed, 300, 150).unwrap(),
        (100.0 + 1.0 / 6.0, 50.0 + 1.0 / 6.0)
    ));
    // A downscaled 2x capture (scale 0.5) maps like 1x.
    let half = shot([800.0, 600.0], 2.0, 0.5);
    assert!(close(to_css(&half, 100, 50).unwrap(), (100.5, 50.5)));
    // An element capture is offset by its region origin.
    let mut el = shot([100.0, 40.0], 2.0, 1.0);
    el.region = [300.0, 200.0, 100.0, 40.0];
    el.viewport = [800.0, 600.0];
    assert!(close(to_css(&el, 0, 0).unwrap(), (300.25, 200.25)));
}

#[test]
fn points_outside_the_viewport_are_refused() {
    let two = shot([800.0, 600.0], 2.0, 1.0);
    for (x, y) in [(1600, 0), (0, 1200), (-1, 0), (0, -1), (5000, 5000)] {
        let err = to_css(&two, x, y).unwrap_err();
        assert_eq!(err.code, "OutOfViewport", "({x},{y})");
        assert_eq!(err.details, vec![("tab".to_owned(), Json::Int(7))]);
    }
    assert!(to_css(&two, 1599, 1199).is_ok());
    // An element region that runs past the viewport edge refuses the part outside it.
    let mut el = shot([100.0, 40.0], 1.0, 1.0);
    el.region = [750.0, 0.0, 100.0, 40.0];
    el.viewport = [800.0, 600.0];
    assert!(to_css(&el, 10, 10).is_ok());
    assert_eq!(to_css(&el, 60, 10).unwrap_err().code, "OutOfViewport");
}

#[test]
fn a_capture_goes_stale_by_age_lease_and_document() {
    let c = shot([800.0, 600.0], 1.0, 1.0);
    assert!(fresh(&c, "lease", 7, Some("doc"), 100.0).is_ok());
    assert!(fresh(&c, "lease", 7, Some("doc"), 100.0 + MAX_AGE).is_ok());
    let stale = |r: Result<(), Refusal>| r.map_err(|e| e.code);
    let want = Err("StaleCapture".to_owned());
    assert_eq!(
        stale(fresh(&c, "lease", 7, Some("doc"), 105.01)),
        want,
        "older than 5 s"
    );
    assert_eq!(
        stale(fresh(&c, "other", 7, Some("doc"), 101.0)),
        want,
        "another lease"
    );
    assert_eq!(
        stale(fresh(&c, "lease", 8, Some("doc"), 101.0)),
        want,
        "another tab"
    );
    assert_eq!(
        stale(fresh(&c, "lease", 7, Some("doc2"), 101.0)),
        want,
        "document moved"
    );
    assert_eq!(
        stale(fresh(&c, "lease", 7, None, 101.0)),
        want,
        "no document"
    );
    assert_eq!(
        stale(fresh(&c, "lease", 7, Some("doc"), 99.0)),
        want,
        "clock went back"
    );
}

#[test]
fn png_size_reads_the_ihdr() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(1600u32.to_be_bytes());
    png.extend(1200u32.to_be_bytes());
    assert_eq!(png_size(&png), Some((1600, 1200)));
    assert_eq!(png_size(&png[..20]), None);
    let mut jpeg = png.clone();
    jpeg[1] = b'J';
    assert_eq!(png_size(&jpeg), None);
}

#[test]
fn coordinate_ops_validate_capture_id_and_pixels() {
    let ok = Json::parse(
        br#"{"op":"page_click_at","lease":"l","request_id":"r","capture_id":"c","x":3,"y":4}"#,
    )
    .unwrap();
    assert_eq!(crate::pages::validate(&ok), Ok("page_click_at"));
    for bad in [
        r#"{"op":"page_click_at","lease":"l","request_id":"r","capture_id":"","x":3,"y":4}"#,
        r#"{"op":"page_click_at","lease":"l","request_id":"r","capture_id":"c","x":-1,"y":4}"#,
        r#"{"op":"page_click_at","lease":"l","request_id":"r","capture_id":"c","x":3.5,"y":4}"#,
        r#"{"op":"page_type_at","lease":"l","request_id":"r","capture_id":"c","x":3,"y":4}"#,
        r#"{"op":"page_click_at","lease":"l","request_id":"r","capture_id":"c","x":3,"y":4,"selector":"a"}"#,
        r#"{"op":"close_tab","request_id":"r","tab":-2}"#,
        r#"{"op":"open_tab","request_id":"r","url":"ftp://x"}"#,
    ] {
        let frame = Json::parse(bad.as_bytes()).unwrap();
        assert!(crate::pages::validate(&frame).is_err(), "{bad}");
    }
    let shot = Json::parse(br#"{"op":"page_screenshot","lease":"l","ref":null}"#).unwrap();
    assert_eq!(crate::pages::validate(&shot), Ok("page_screenshot"));
}

#[test]
fn aspect_allows_rounding_and_refuses_a_resize() {
    // Rounded image sizes of a 2x viewport and a tiny element pass.
    assert!(aspect_matches(&shot([800.0, 600.0], 2.0, 1.0)).is_ok());
    assert!(aspect_matches(&shot([1.4, 1.6], 2.0, 1.0)).is_ok());
    // A viewport that lost 40 CSS px of height before the shot is refused.
    let mut resized = shot([800.0, 600.0], 2.0, 1.0);
    resized.image = [1600, 1120];
    let err = aspect_matches(&resized).unwrap_err();
    assert_eq!(err.code, "StaleCapture");
    assert_eq!(err.details, vec![("tab".to_owned(), Json::Int(7))]);
}
