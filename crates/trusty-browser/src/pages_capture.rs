//! Tabs, screenshots and coordinate actions (#11208, #11211).
//!
//! Why: an agent that can only act on element refs cannot reach canvas,
//! map or custom-drawn controls, and cannot open or close the tabs a task
//! needs. It still never writes a selector: a coordinate comes from a
//! screenshot this broker took, and a tab it closes is one it leased.
//! What: [`route`] runs `open_tab`, `close_tab`, `page_screenshot`,
//! `page_click_at` and `page_type_at`. A screenshot is stored as a
//! [`Capture`] on the session; a coordinate action names its `capture_id`,
//! is refused `StaleCapture` once the capture is older than [`MAX_AGE`], its
//! lease or document moved, or (checked by the worker) the viewport metrics
//! changed, and [`to_css`] maps its image pixels to viewport CSS pixels.
//! Test: `image_pixels_map_to_css_at_1x_2x_and_zoomed`,
//! `a_capture_goes_stale_by_age_lease_and_document`,
//! `points_outside_the_viewport_are_refused`, `png_size_reads_the_ihdr`,
//! `aspect_allows_rounding_and_refuses_a_resize` below;
//! `close_tab_refuses_a_tab_this_session_does_not_lease`,
//! `a_capture_whose_aspect_disagrees_with_its_region_is_stale`,
//! `an_unknown_open_tab_is_journaled_and_never_opens_a_second_tab` in
//! `tests/parity_pages.rs`; the extension side in `tests/js/pages.test.mjs`.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::broker::Core;
use crate::entropy::token_urlsafe;
use crate::grants::Grant;
use crate::leases::JOURNAL_LIMIT;
use crate::pages::{self, Stop};
use crate::pages_ops::{Ran, Session, claim_for, hosted, leased, ok_tab, peer_of, renew, send};
use crate::pyjson::Json;
use crate::refusal::{Refusal, request_string};

/// Seconds a capture stays usable for a coordinate action.
pub const MAX_AGE: f64 = 5.0;
/// Captures a session keeps; the oldest is dropped first.
pub const KEEP: usize = 16;
/// Largest image coordinate accepted, in pixels.
const MAX_COORD: i64 = 100_000;
/// Largest relative gap between a capture's image and region aspect ratios.
pub const ASPECT_TOLERANCE: f64 = 0.02;

/// One screenshot, as a later coordinate action checks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    /// The id handed to the agent.
    pub id: String,
    /// The lease it was taken under.
    pub lease: String,
    /// The tab.
    pub tab: i64,
    /// The document it shows.
    pub document: String,
    /// Lease-clock time it was taken.
    pub taken: f64,
    /// The captured region in viewport CSS pixels: x, y, width, height.
    pub region: [f64; 4],
    /// Image size in pixels: width, height.
    pub image: [u32; 2],
    /// Viewport size in CSS pixels: width, height.
    pub viewport: [f64; 2],
    /// The worker's metrics object, echoed back for its staleness check.
    pub metrics: Json,
}

fn num(v: Option<&Json>) -> Option<f64> {
    match v? {
        Json::Int(i) => Some(*i as f64),
        Json::UInt(u) => Some(*u as f64),
        Json::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

/// Width and height from a PNG's IHDR chunk; `None` for anything else.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != SIG || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Refuse a capture that no longer describes the leased tab (`StaleCapture`).
///
/// Why: a point read from an old picture can land on whatever moved there.
/// What: refuses when the capture was taken under another lease or tab, the
/// lease now holds another document, or it is older than [`MAX_AGE`].
/// Test: `a_capture_goes_stale_by_age_lease_and_document`.
pub fn fresh(
    c: &Capture,
    lease: &str,
    tab: i64,
    document: Option<&str>,
    now: f64,
) -> Result<(), Refusal> {
    let same = c.lease == lease && c.tab == tab && document == Some(c.document.as_str());
    if !same || now - c.taken > MAX_AGE || now < c.taken {
        return Err(Refusal::new("StaleCapture").with("tab", tab));
    }
    Ok(())
}

/// Refuse an image whose aspect ratio is not its region's (`StaleCapture`).
///
/// Why: the worker reads the viewport metrics, then captures; a resize in
/// between yields a picture of another viewport, and [`to_css`] would map
/// its pixels to offset points. What: the image height expected from its
/// width and the region's aspect may differ from the real height by at most
/// [`ASPECT_TOLERANCE`] of it, plus one pixel of rounding.
/// Test: `a_capture_whose_aspect_disagrees_with_its_region_is_stale` in
/// `tests/parity_pages.rs`, `aspect_allows_rounding_and_refuses_a_resize`.
pub fn aspect_matches(c: &Capture) -> Result<(), Refusal> {
    let [_, _, rw, rh] = c.region;
    let [iw, ih] = c.image.map(f64::from);
    let expected = iw * rh / rw;
    if !expected.is_finite() || (ih - expected).abs() > expected * ASPECT_TOLERANCE + 1.0 {
        return Err(Refusal::new("StaleCapture").with("tab", c.tab));
    }
    Ok(())
}

/// Image pixels to viewport CSS pixels, else `OutOfViewport`.
///
/// Why: CDP input takes viewport CSS pixels, while the agent reads image
/// pixels; device pixel ratio, page zoom and any capture downscale all sit
/// between them. What: the scale is the measured image-to-region ratio per
/// axis, which equals `devicePixelRatio` (Chrome folds page zoom into it)
/// times the capture scale; the point must lie inside the image and the
/// mapped point inside the viewport.
/// Test: `image_pixels_map_to_css_at_1x_2x_and_zoomed`,
/// `points_outside_the_viewport_are_refused`.
pub fn to_css(c: &Capture, x: i64, y: i64) -> Result<(f64, f64), Refusal> {
    let out = || Refusal::new("OutOfViewport").with("tab", c.tab);
    let [iw, ih] = c.image.map(i64::from);
    if !(0..iw).contains(&x) || !(0..ih).contains(&y) {
        return Err(out());
    }
    let [rx, ry, rw, rh] = c.region;
    let cx = rx + (x as f64 + 0.5) * rw / iw as f64;
    let cy = ry + (y as f64 + 0.5) * rh / ih as f64;
    if !(0.0..c.viewport[0]).contains(&cx) || !(0.0..c.viewport[1]).contains(&cy) {
        return Err(out());
    }
    Ok((cx, cy))
}

/// Field checks of the five ops beyond [`pages::validate`]'s shared ones.
pub fn validate(op: &str, request: &Json) -> Result<(), Refusal> {
    if matches!(op, "page_click_at" | "page_type_at") {
        request_string(request, "capture_id")?;
        for k in ["x", "y"] {
            if !matches!(request.get(k), Some(Json::Int(v)) if (0..=MAX_COORD).contains(v)) {
                return Err(Refusal::new("InvalidRequest"));
            }
        }
    }
    Ok(())
}

/// Run one of this module's ops; `None` for any other op.
pub async fn route(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    op: &str,
    request: &Json,
) -> Option<Result<Ran, Stop>> {
    Some(match op {
        "open_tab" => open_tab(core, session, grant, request).await,
        "close_tab" => close_tab(core, session, grant, request).await,
        "page_screenshot" => screenshot(core, session, grant, request).await,
        "page_click_at" | "page_type_at" => act_at(core, session, grant, op, request).await,
        _ => return None,
    })
}

fn journal(session: &mut Session, request: &Json, reply: &Json) -> Result<String, Stop> {
    if session.journal.len() >= JOURNAL_LIMIT {
        return Err(Refusal::new("SessionFull").into());
    }
    let rid = request_string(request, "request_id")?.to_owned();
    session
        .journal
        .insert(rid.clone(), (pages::remembered(request), reply.clone()));
    Ok(rid)
}

fn tab_scope(grant: &Grant, tab: Option<i64>) -> Result<(), Refusal> {
    if grant.scope == "tab" {
        let r = Refusal::new("OutOfScope");
        return Err(match tab.or(grant.tab) {
            Some(t) => r.with("tab", t),
            None => r,
        });
    }
    Ok(())
}

/// Open a tab and lease it to this session (#11208).
async fn open_tab(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    request: &Json,
) -> Result<Ran, Stop> {
    tab_scope(grant, None)?;
    if session.journal.len() >= JOURNAL_LIMIT {
        return Err(Refusal::new("SessionFull").into());
    }
    let peer = peer_of(core, grant)?;
    let url = request.get("url").cloned().unwrap_or(Json::Null);
    let args = vec![("url", url)];
    // #11211 review: once the worker may have created a tab, every answer is
    // journaled, so a retry with this request_id never opens a second one.
    let leased = match send(core, grant, None, &peer, "page_open_tab", args).await {
        Ok(raw) => lease_opened(core, session, &peer, &raw),
        Err(exc) if exc.code == "OutcomeUnknown" => Err(exc),
        Err(exc) => return Err(exc.into()),
    };
    let (reply, tab, origin) = match leased {
        Ok(done) => done,
        Err(exc) => {
            journal(session, request, &exc.response())?;
            return Err(exc.into());
        }
    };
    journal(session, request, &reply)?;
    Ok((reply, Some(tab), hosted(origin.as_deref())?))
}

/// Lease the tab a worker opened: the reply, the tab and its origin when readable.
fn lease_opened(
    core: &Arc<Core>,
    session: &Session,
    peer: &Arc<crate::peer_conn::Peer>,
    raw: &Json,
) -> Result<(Json, i64, Option<String>), Refusal> {
    let seen = pages::shaped(raw, pages::OPENED)?;
    let Some(Json::Int(tab)) = seen.get("tab").cloned() else {
        return Err(Refusal::new("BrowserRefused"));
    };
    let access = seen.get("access") == Some(&Json::Bool(true));
    let origin = seen
        .get("origin")
        .and_then(Json::as_str)
        .map(str::to_owned)
        .filter(|_| access);
    let token = token_urlsafe(&*core.entropy, 24);
    {
        let mut inner = core.lock();
        let now = core.lease_clock.now();
        if let Some(c) = inner.active(&peer.generation, tab, now) {
            // A tab id Chrome just minted cannot be held; refuse rather than share it.
            return Err(Refusal::new("TabClaimed").with("holder", c.owner.clone()));
        }
        let mut claim = claim_for(session, &peer.generation, tab, token.clone(), now);
        claim.document = seen
            .get("document")
            .and_then(Json::as_str)
            .map(str::to_owned);
        claim.origin = origin.clone();
        inner.claims.push(claim);
    }
    let mut reply = Json::object([
        ("ok", Json::Bool(true)),
        ("lease", Json::from(token)),
        ("tab", Json::Int(tab)),
    ]);
    if let Json::Object(pairs) = seen {
        for (k, v) in pairs.into_iter().filter(|(k, _)| k != "tab") {
            reply.insert(k, v);
        }
    }
    Ok((reply, tab, origin))
}

/// Close a tab only when this session holds its lease (#11208).
async fn close_tab(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    request: &Json,
) -> Result<Ran, Stop> {
    let Some(Json::Int(tab)) = request.get("tab").cloned() else {
        return Err(Refusal::new("InvalidRequest").into());
    };
    tab_scope(grant, Some(tab))?;
    let peer = peer_of(core, grant)?;
    let token = {
        let now = core.lease_clock.now();
        let mut inner = core.lock();
        match inner.active(&peer.generation, tab, now) {
            Some(c) if c.owner == session.owner => c.token.clone(),
            // #11208: another session's tab, or no lease at all, is never closed.
            _ => return Err(Refusal::new("TabNotLeased").with("tab", tab).into()),
        }
    };
    if session.journal.len() >= JOURNAL_LIMIT {
        return Err(Refusal::new("SessionFull").into());
    }
    let key = Some((peer.generation.clone(), tab));
    let args = vec![("tab", Json::Int(tab))];
    let raw = match send(core, grant, key, &peer, "page_close_tab", args).await {
        Ok(raw) => raw,
        Err(exc) => {
            // #11211 review: a close that may have run is not retried.
            if exc.code == "OutcomeUnknown" {
                journal(session, request, &exc.response())?;
            }
            return Err(exc.into());
        }
    };
    pages::shaped(&raw, pages::CLOSED)?;
    core.lock().drop_claim(&token);
    session.captures.retain(|c| c.tab != tab);
    let mut reply = Json::object(ok_tab(tab));
    reply.insert("closed", Json::Bool(true));
    journal(session, request, &reply)?;
    Ok((reply, Some(tab), None))
}

/// A worker capture: PNG bytes, region, metrics object and viewport size.
type Shot = (Vec<u8>, [f64; 4], Json, [f64; 2]);

/// Check a worker's `page_screenshot` result, else `BrowserRefused`.
fn capture_of(raw: &Json, claim_tab: i64) -> Result<Shot, Refusal> {
    let refused = || Refusal::new("BrowserRefused");
    let keys = ["document", "data", "region", "metrics"];
    if !crate::pairing::key_set_is(raw, &keys)
        || raw.get("document").and_then(Json::as_str).is_none()
    {
        return Err(refused());
    }
    let data = raw.get("data").and_then(Json::as_str).ok_or_else(refused)?;
    let bytes = STANDARD.decode(data).map_err(|_| refused())?;
    let region = raw.get("region").ok_or_else(refused)?;
    let mut r = [0.0; 4];
    for (slot, k) in r.iter_mut().zip(["x", "y", "width", "height"]) {
        *slot = num(region.get(k)).ok_or_else(refused)?;
    }
    let metrics = raw.get("metrics").cloned().ok_or_else(refused)?;
    let keys = [
        "css_width",
        "css_height",
        "device_pixel_ratio",
        "zoom",
        "scroll_x",
        "scroll_y",
    ];
    if !crate::pairing::key_set_is(&metrics, &keys)
        || keys.iter().any(|k| num(metrics.get(k)).is_none())
    {
        return Err(refused());
    }
    let vw = num(metrics.get("css_width")).unwrap_or(0.0);
    let vh = num(metrics.get("css_height")).unwrap_or(0.0);
    let inside = r[0] >= 0.0 && r[1] >= 0.0 && r[2] > 0.0 && r[3] > 0.0;
    if !inside || r[0] + r[2] > vw + 1.0 || r[1] + r[3] > vh + 1.0 {
        return Err(refused().with("tab", claim_tab));
    }
    Ok((bytes, r, metrics, [vw, vh]))
}

/// A PNG of the viewport or one element, with a `capture_id` (#11211).
async fn screenshot(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    request: &Json,
) -> Result<Ran, Stop> {
    let claim = leased(core, session, grant, request)?;
    let document = claim.document.clone().map_or(Json::Null, Json::from);
    // #11362: an element screenshot of a frame ref.
    let field =
        crate::pages_frames::resolve(session, &claim, request.get("ref").unwrap_or(&Json::Null))?;
    let args = vec![
        ("tab", Json::Int(claim.tab)),
        ("document", document),
        ("ref", field),
    ];
    let raw = send(
        core,
        grant,
        claim.key(),
        &claim.peer,
        "page_screenshot",
        args,
    )
    .await?;
    let (bytes, region, metrics, viewport) = capture_of(&raw, claim.tab)?;
    let (w, h) = png_size(&bytes).ok_or_else(|| Refusal::new("BrowserRefused"))?;
    let doc = raw
        .get("document")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let id = token_urlsafe(&*core.entropy, 12);
    let capture = Capture {
        id: id.clone(),
        lease: claim.token.clone(),
        tab: claim.tab,
        document: doc.clone(),
        taken: core.lease_clock.now(),
        region,
        image: [w, h],
        viewport,
        metrics: metrics.clone(),
    };
    // #11211 review: a viewport resized between the metrics read and the shot.
    aspect_matches(&capture)?;
    if session.captures.len() >= KEEP {
        session.captures.remove(0);
    }
    session.captures.push(capture);
    renew(core, &claim.token);
    let region = Json::object(
        ["x", "y", "width", "height"]
            .into_iter()
            .zip(region)
            .map(|(k, v)| (k, Json::Float(v))),
    );
    let mut reply = Json::object(ok_tab(claim.tab));
    for (k, v) in [
        ("capture_id", Json::from(id)),
        ("document", Json::from(doc)),
        ("mime", Json::from("image/png")),
        ("width", Json::Int(i64::from(w))),
        ("height", Json::Int(i64::from(h))),
        ("region", region),
        ("viewport", metrics),
        ("data", raw.get("data").cloned().unwrap_or(Json::Null)),
    ] {
        reply.insert(k, v);
    }
    Ok((reply, Some(claim.tab), hosted(claim.origin.as_deref())?))
}

/// A trusted click, or click and insert text, at a captured point (#11211).
async fn act_at(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    op: &str,
    request: &Json,
) -> Result<Ran, Stop> {
    let claim = leased(core, session, grant, request)?;
    let id = request_string(request, "capture_id")?;
    let stale = || Refusal::new("StaleCapture").with("tab", claim.tab);
    let capture = session
        .captures
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or_else(stale)?;
    let now = core.lease_clock.now();
    fresh(
        &capture,
        &claim.token,
        claim.tab,
        claim.document.as_deref(),
        now,
    )?;
    let px = |k: &str| match request.get(k) {
        Some(Json::Int(v)) => *v,
        _ => -1,
    };
    let (x, y) = to_css(&capture, px("x"), px("y"))?;
    if session.journal.len() >= JOURNAL_LIMIT {
        return Err(Refusal::new("SessionFull").into());
    }
    let mut args = vec![
        ("tab", Json::Int(claim.tab)),
        ("document", Json::from(capture.document.as_str())),
        ("x", Json::Float(x)),
        ("y", Json::Float(y)),
        ("metrics", capture.metrics.clone()),
        ("dialog", Json::from("dismiss")),
    ];
    if op == "page_type_at" {
        args.push(("text", request.get("text").cloned().unwrap_or(Json::Null)));
    }
    let raw = match send(core, grant, claim.key(), &claim.peer, op, args).await {
        Ok(raw) => raw,
        Err(exc) => {
            if exc.code == "OutcomeUnknown" {
                journal(session, request, &exc.response())?;
            }
            return Err(exc.into());
        }
    };
    let result = pages::clicked(&raw)?;
    renew(core, &claim.token);
    let mut reply = Json::object(ok_tab(claim.tab));
    if let Json::Object(pairs) = result {
        for (k, v) in pairs {
            reply.insert(k, v);
        }
    }
    journal(session, request, &reply)?;
    Ok((reply, Some(claim.tab), hosted(claim.origin.as_deref())?))
}

#[cfg(test)]
#[path = "pages_capture_tests.rs"]
mod tests;
