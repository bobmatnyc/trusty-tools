//! Every frame of the leased tab: `page_snapshot` and frame-scoped refs (#11362).
//!
//! Why: a hand-written walker cannot enter a cross-origin or out-of-process
//! iframe, so an agent saw only the outer shell of a page that embeds its
//! real work (app.pacerevenue.com/insights embedding flyr.embed-omniapp.co;
//! failure modes F5, F8). The browser's own accessibility tree, read over the
//! existing `debugger` permission, reaches every frame. The agent still never
//! writes a selector: a ref is a token a snapshot handed out.
//! What: [`route`] runs `page_snapshot`; [`assemble`] orders the worker's
//! frames depth-first, numbers them (the frame ordinal), sums each frame's
//! offset into top-viewport CSS pixels ([`rects`]) and renders the text. The
//! frame ids and loaders are kept per lease in a [`FrameTable`]; [`resolve`]
//! turns a ref `f<ordinal>:<backendDOMNodeId>` back into the worker's
//! `{frame, loader, node}`, refusing `StaleElement` when no snapshot of the
//! lease's current document holds it.
//! Test: `frame_refs_parse_and_route_only_within_their_snapshot`,
//! `frames_order_depth_first_and_sum_offsets_across_processes`,
//! `the_text_marks_cross_origin_canvas_and_refused_frames`,
//! `a_malformed_worker_snapshot_is_refused` in `pages_frames_tests.rs`;
//! `a_snapshot_routes_frame_refs_and_falls_back_on_a_busy_debugger` in
//! `tests/parity_pages.rs`; the extension side in `tests/js/frames.test.mjs`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::broker::Core;
use crate::grants::Grant;
use crate::pages::Stop;
use crate::pages_ops::{Leased, Ran, Session, hosted, leased, ok_tab, query, send};
use crate::pyjson::Json;
use crate::refusal::Refusal;

/// Frames one snapshot may number; the ordinal has at most three digits.
/// At or above the worker's own cap (150, `pages_frames.js` FRAME_MAX), so a
/// capped reply is never refused.
pub const MAX_FRAMES: usize = 200;
/// Deepest node depth accepted; the worker stops at 40 (DEPTH_MAX).
pub const MAX_DEPTH: i64 = 1000;
/// Largest encoded `page_snapshot` reply body, in bytes. The broker writes
/// the reply through `frame::encode` (`ensure_ascii`), which refuses a body
/// over `MAX_FRAME` (1 MiB) and so closes the client session; the text stops
/// at this budget, counted as escaped bytes, so the whole reply stays under it.
pub const MAX_ENCODED: usize = 900 * 1000;
/// Most escaped bytes the frames' urls and origins may take, an origin counted
/// twice (list and header line); past it the worker's reply is refused.
const MAX_META: usize = 400 * 1000;
/// The closing `[text truncated]\n` line, escaped, reserved from the budget.
const CUT_LINE: usize = 20;
/// Snapshots a session keeps, one per lease; the oldest is dropped first.
pub const KEEP: usize = 16;
/// Longest frame id or loader id, as the worker checks it.
const MAX_ID: usize = 64;

/// The frames of one lease's latest snapshot, as a later ref names them.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTable {
    /// The lease it was taken under.
    pub lease: String,
    /// The top document it read.
    pub document: String,
    /// Frame id and loader id, by ordinal.
    pub frames: Vec<(String, String)>,
    /// (ordinal, backend node id) of every node the snapshot's text named.
    pub issued: HashSet<(usize, i64)>,
}

/// One accessibility node: depth, role, name, backend node id, value.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Depth below its frame's root.
    pub depth: usize,
    /// The AX role.
    pub role: String,
    /// The accessible name.
    pub name: String,
    /// The DOM backend node id the ref carries.
    pub backend: i64,
    /// A control's value, when it has one.
    pub value: Option<String>,
}

/// One frame as the worker reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// CDP frame id.
    pub id: String,
    /// The parent frame's id; `None` for the top frame.
    pub parent: Option<String>,
    /// The loader of the frame's document.
    pub loader: String,
    /// The frame's URL.
    pub url: String,
    /// Its security origin.
    pub origin: String,
    /// The root of an out-of-process (auto-attached) session.
    pub root: bool,
    /// Owner content box in the parent's local-root viewport: x, y, width, height.
    pub owner: Option<[f64; 4]>,
    /// Why the frame has no nodes (`PdfViewer`, `FrameUnreachable`).
    pub refused: Option<String>,
    /// Its accessibility nodes.
    pub nodes: Vec<Node>,
}

/// `f<ordinal>:<node>` as (ordinal, backend node id); `None` for any other ref.
pub fn parse_ref(text: &str) -> Option<(usize, i64)> {
    let (ord, node) = text.strip_prefix('f')?.split_once(':')?;
    let digits = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(ord, 3) || !digits(node, 12) {
        return None;
    }
    let node: i64 = node.parse().ok()?;
    (node > 0).then_some((ord.parse().ok()?, node))
}

/// A frame ref as the worker's `{frame, loader, node}`; any other value unchanged.
///
/// Why: a ref must act only in the frame and document it was read from.
/// What: a string that parses as [`parse_ref`] needs this lease's snapshot
/// of the lease's current top document holding that ordinal, else
/// `StaleElement`, and so does a node that snapshot never named (#11362);
/// the worker then refuses a navigated frame
/// `DocumentChanged` and a detached one `StaleElement`.
/// Test: `frame_refs_parse_and_route_only_within_their_snapshot`.
pub(crate) fn resolve(session: &Session, claim: &Leased, value: &Json) -> Result<Json, Refusal> {
    let document = claim.document.as_deref();
    route_ref(&session.frames, &claim.token, claim.tab, document, value)
}

/// [`resolve`] on plain values: the session's tables, the lease, its tab and document.
pub fn route_ref(
    tables: &[FrameTable],
    lease: &str,
    tab: i64,
    document: Option<&str>,
    value: &Json,
) -> Result<Json, Refusal> {
    let Some((ord, node)) = value.as_str().and_then(parse_ref) else {
        return Ok(value.clone());
    };
    let stale = || Refusal::new("StaleElement").with("tab", tab);
    let table = tables.iter().find(|t| t.lease == lease).ok_or_else(stale)?;
    // #11362: a snapshot of an older top document names frames that are gone.
    if document != Some(table.document.as_str()) {
        return Err(stale());
    }
    let (frame, loader) = table
        .frames
        .get(ord)
        .filter(|(_, l)| !l.is_empty())
        .ok_or_else(stale)?;
    if !table.issued.contains(&(ord, node)) {
        return Err(stale());
    }
    Ok(Json::object([
        ("frame", Json::from(frame.as_str())),
        ("loader", Json::from(loader.as_str())),
        ("node", Json::Int(node)),
    ]))
}

fn num(v: &Json) -> Option<f64> {
    match v {
        Json::Int(i) => Some(*i as f64),
        Json::UInt(u) => Some(*u as f64),
        Json::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn id_of(v: Option<&Json>) -> Option<String> {
    v.and_then(Json::as_str)
        .filter(|s| !s.is_empty() && s.chars().count() <= MAX_ID)
        .map(str::to_owned)
}

fn short(v: Option<&Json>, max: usize) -> Option<String> {
    v.and_then(Json::as_str)
        .filter(|s| s.chars().count() <= max)
        .map(str::to_owned)
}

fn node_of(v: &Json) -> Option<Node> {
    let Json::Array(items) = v else { return None };
    let [depth, role, name, backend, value] = items.as_slice() else {
        return None;
    };
    let depth = match depth {
        Json::Int(d) if (0..=MAX_DEPTH).contains(d) => usize::try_from(*d).ok()?,
        _ => return None,
    };
    let backend = match backend {
        Json::Int(b) if *b > 0 => *b,
        _ => return None,
    };
    let value = match value {
        Json::Null => None,
        v => Some(short(Some(v), 160)?),
    };
    Some(Node {
        depth,
        role: short(Some(role), 64)?,
        name: short(Some(name), 160)?,
        backend,
        value,
    })
}

fn frame_of(v: &Json) -> Option<Frame> {
    let keys = [
        "frame", "parent", "loader", "url", "origin", "root", "owner", "refused", "nodes",
    ];
    if !crate::pairing::key_set_is(v, &keys) {
        return None;
    }
    let parent = match v.get("parent")? {
        Json::Null => None,
        p => Some(id_of(Some(p))?),
    };
    let owner = match v.get("owner")? {
        Json::Null => None,
        Json::Array(q) if q.len() == 4 => {
            let mut out = [0.0; 4];
            for (slot, item) in out.iter_mut().zip(q) {
                *slot = num(item)?;
            }
            Some(out)
        }
        _ => return None,
    };
    let refused = match v.get("refused")? {
        Json::Null => None,
        Json::Str(r) if matches!(r.as_str(), "PdfViewer" | "FrameUnreachable") => Some(r.clone()),
        _ => return None,
    };
    let Json::Array(nodes) = v.get("nodes")? else {
        return None;
    };
    Some(Frame {
        id: id_of(v.get("frame"))?,
        parent,
        // The top document's placeholder loader may be empty; only a real one routes refs.
        loader: short(v.get("loader"), MAX_ID)?,
        url: short(v.get("url"), 2048)?,
        origin: short(v.get("origin"), 2048)?,
        root: matches!(v.get("root")?, Json::Bool(true)),
        owner,
        refused,
        nodes: nodes.iter().map(node_of).collect::<Option<_>>()?,
    })
}

/// The worker's frames, each checked, else `BrowserRefused`.
pub fn frames_of(raw: &Json) -> Result<Vec<Frame>, Refusal> {
    let refused = || Refusal::new("BrowserRefused");
    let Some(Json::Array(items)) = raw.get("frames") else {
        return Err(refused());
    };
    let frames: Vec<Frame> = items
        .iter()
        .map(frame_of)
        .collect::<Option<_>>()
        .ok_or_else(refused)?;
    let tops = frames.iter().filter(|f| f.parent.is_none()).count();
    if tops != 1 || frames.len() > MAX_FRAMES {
        return Err(refused());
    }
    // #11362: urls and origins are page-chosen and cannot be cut from the list.
    let meta: usize = frames
        .iter()
        .map(|f| esc_len(&f.url) + 2 * esc_len(&f.origin))
        .sum();
    if meta > MAX_META {
        return Err(refused());
    }
    Ok(frames)
}

/// Depth-first order from the top frame: (index into `frames`, parent ordinal).
///
/// Why: an ordinal must name the same frame for every reader of one snapshot.
/// What: the top frame is ordinal 0; children follow their parent in the
/// worker's order. A frame whose parent is not listed hangs under the top
/// frame; a repeated id is listed once.
/// Test: `frames_order_depth_first_and_sum_offsets_across_processes`.
pub fn order(frames: &[Frame]) -> Vec<(usize, Option<usize>)> {
    let known = |id: &str| frames.iter().any(|f| f.id == id);
    let parent_of = |f: &Frame| match &f.parent {
        Some(p) if known(p) => Some(p.clone()),
        Some(_) => frames
            .iter()
            .find(|t| t.parent.is_none())
            .map(|t| t.id.clone()),
        None => None,
    };
    let mut out: Vec<(usize, Option<usize>)> = Vec::new();
    let mut stack: Vec<(usize, Option<usize>)> = frames
        .iter()
        .position(|f| f.parent.is_none())
        .map(|i| vec![(i, None)])
        .unwrap_or_default();
    while let Some((i, parent)) = stack.pop() {
        if out.iter().any(|(j, _)| frames[*j].id == frames[i].id) {
            continue;
        }
        let ord = out.len();
        out.push((i, parent));
        let children: Vec<usize> = (0..frames.len())
            .filter(|&c| parent_of(&frames[c]).as_deref() == Some(frames[i].id.as_str()))
            .collect();
        stack.extend(children.into_iter().rev().map(|c| (c, Some(ord))));
    }
    out
}

/// Each ordered frame's box in top-viewport CSS pixels; `None` when unknown.
///
/// Why: CDP reports an owner box in its own process's local root, and the
/// coordinate tools take top-viewport pixels (#11211).
/// What: a frame's box is its owner box plus the origin of its parent's local
/// root. That origin is zero for the top frame's process; an out-of-process
/// frame's own root sits at its owner box's corner plus its parent's origin;
/// an in-process frame shares its parent's.
/// Test: `frames_order_depth_first_and_sum_offsets_across_processes`.
pub fn rects(frames: &[Frame], ordered: &[(usize, Option<usize>)]) -> Vec<Option<[f64; 4]>> {
    let mut base: Vec<Option<(f64, f64)>> = Vec::with_capacity(ordered.len());
    let mut out = Vec::with_capacity(ordered.len());
    for &(i, parent) in ordered {
        let f = &frames[i];
        let Some(p) = parent else {
            base.push(Some((0.0, 0.0)));
            out.push(None);
            continue;
        };
        let at = base[p];
        let rect = at
            .zip(f.owner)
            .map(|((x, y), [ox, oy, w, h])| [ox + x, oy + y, w, h]);
        base.push(if f.root {
            rect.map(|r| (r[0], r[1]))
        } else {
            at
        });
        out.push(rect);
    }
    out
}

/// Bytes `s` takes inside a JSON string under `frame::encode` (`ensure_ascii`).
///
/// Why: the budget counts what the wire carries, not raw UTF-8. What: asks
/// [`Json::dumps`] for the escaped string, the same code `frame::encode` runs,
/// and drops its two quotes. Escaping is per character, so the lengths of
/// consecutive pieces add up to the length of their concatenation.
/// Test: `an_encoded_reply_stays_within_one_frame`.
fn esc_len(s: &str) -> usize {
    Json::from(s).dumps(true).map_or(0, |d| d.len() - 2)
}

fn line(text: &mut String, ord: usize, n: &Node) {
    let indent = "  ".repeat(n.depth + 1);
    let label = if n.role == "StaticText" {
        "text"
    } else {
        &n.role
    };
    let _ = write!(text, "{indent}{label} {:?}", n.name);
    if let Some(v) = &n.value {
        let _ = write!(text, " = {v:?}");
    }
    let _ = write!(text, " [f{ord}:{}]", n.backend);
    // #11362: a canvas draws pixels the tree cannot name.
    if n.role.eq_ignore_ascii_case("canvas") {
        text.push_str(
            " (canvas: pixels only; read it with page_screenshot, act with page_click_at)",
        );
    }
    text.push('\n');
}

/// The snapshot's text and frame list, and the table its refs route through.
///
/// Why: one compact text an agent reads top to bottom, with every frame's
/// origin and every node's ref. What: frames in [`order`], each a header
/// (ordinal, origin, cross-origin and out-of-process marks, parent and box)
/// and its nodes indented by depth; a refused frame names its reason and the
/// tools that still reach it. `reserved` is the encoded size of the rest of
/// the reply; the frames list, the header lines and the node lines together
/// stay within [`MAX_ENCODED`] less that, each counted as its escaped bytes.
/// Past it no more node lines are written; the text ends with a
/// `[text truncated]` line and the table holds only the nodes it named. The
/// bool says whether the text was cut.
/// Test: `the_text_marks_cross_origin_canvas_and_refused_frames`,
/// `the_text_stops_at_its_cap_and_issues_only_the_nodes_it_named`,
/// `an_encoded_reply_stays_within_one_frame`.
pub fn assemble(
    frames: &[Frame],
    lease: &str,
    document: &str,
    reserved: usize,
) -> (String, Json, FrameTable, bool) {
    assemble_capped(
        frames,
        lease,
        document,
        MAX_ENCODED.saturating_sub(reserved),
    )
}

/// [`assemble`] with the encoded budget of the list and the text as a parameter.
pub fn assemble_capped(
    frames: &[Frame],
    lease: &str,
    document: &str,
    budget: usize,
) -> (String, Json, FrameTable, bool) {
    let ordered = order(frames);
    let boxes = rects(frames, &ordered);
    let top = &frames[ordered[0].0].origin;
    let (mut text, mut list, mut table) = (String::new(), Vec::new(), Vec::new());
    let (mut issued, mut cut) = (HashSet::new(), false);
    // Escaped bytes of everything but the node lines written so far.
    let mut used = 0usize;
    for (ord, (&(i, parent), rect)) in ordered.iter().zip(&boxes).enumerate() {
        let f = &frames[i];
        let rect = rect.map_or(Json::Null, |r| {
            Json::object(
                ["x", "y", "width", "height"]
                    .into_iter()
                    .zip(r)
                    .map(|(k, v)| (k, Json::Float(v))),
            )
        });
        let entry = Json::object([
            ("frame", Json::Int(ord as i64)),
            ("parent", parent.map_or(Json::Null, |p| Json::Int(p as i64))),
            ("origin", Json::from(f.origin.as_str())),
            ("url", Json::from(f.url.as_str())),
            ("cross_origin", Json::Bool(f.origin != *top)),
            ("out_of_process", Json::Bool(f.root)),
            ("rect", rect),
            ("refused", f.refused.clone().map_or(Json::Null, Json::from)),
        ]);
        used += entry.dumps(true).map_or(0, |d| d.len()) + 1;
        list.push(entry);
        table.push((f.id.clone(), f.loader.clone()));
    }
    let budget = budget.saturating_sub(CUT_LINE);
    for (ord, (&(i, parent), rect)) in ordered.iter().zip(&boxes).enumerate() {
        let f = &frames[i];
        let cross = f.origin != *top;
        let before = text.len();
        let _ = write!(text, "frame f{ord} {}", f.origin);
        let marks: Vec<&str> = [(cross, "cross-origin"), (f.root, "out-of-process")]
            .into_iter()
            .filter_map(|(on, m)| on.then_some(m))
            .collect();
        if !marks.is_empty() {
            let _ = write!(text, " ({})", marks.join(", "));
        }
        if let Some(p) = parent {
            let _ = write!(text, " in f{p}");
        }
        if let Some([x, y, w, h]) = rect {
            let _ = write!(text, " at {x:.0},{y:.0} {w:.0}x{h:.0}");
        }
        match &f.refused {
            Some(r) => {
                let _ = writeln!(
                    text,
                    " refused {r}: read it with page_screenshot, act with page_click_at and page_type_at"
                );
            }
            None => text.push('\n'),
        }
        used += esc_len(&text[before..]);
        for n in &f.nodes {
            let mut row = String::new();
            line(&mut row, ord, n);
            let size = esc_len(&row);
            if cut || used + size > budget {
                cut = true;
                break;
            }
            text.push_str(&row);
            used += size;
            issued.insert((ord, n.backend));
        }
    }
    let table = FrameTable {
        lease: lease.to_owned(),
        document: document.to_owned(),
        frames: table,
        issued,
    };
    if cut {
        text.push_str("[text truncated]\n");
    }
    (text, Json::Array(list), table, cut)
}

/// The tabs the leased tab opened, each `{tab, window}`, else `BrowserRefused`.
fn popups_of(raw: &Json, grant: &Grant) -> Result<Json, Refusal> {
    let Some(Json::Array(items)) = raw.get("popups") else {
        return Err(Refusal::new("BrowserRefused"));
    };
    let shape: crate::pages::Shape = &[
        ("tab", &[crate::pages::Ty::Int]),
        ("window", &[crate::pages::Ty::Int]),
    ];
    let seen = items
        .iter()
        .take(50)
        .map(|p| crate::pages::shaped(p, shape))
        .collect::<Result<Vec<_>, _>>()?;
    // #11362: a tab grant cannot select another tab, so it is told of none.
    Ok(Json::Array(if grant.scope == "tab" {
        Vec::new()
    } else {
        seen
    }))
}

/// The `note` of a `DebuggerBusy` fallback reply.
pub const PARTIAL_NOTE: &str =
    "top document and same-origin frames only; cross-origin frames not read";

/// Run `page_snapshot`; `None` for any other op.
pub async fn route(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    op: &str,
    request: &Json,
) -> Option<Result<Ran, Stop>> {
    (op == "page_snapshot").then_some(())?;
    Some(snapshot(core, session, grant, request).await)
}

/// Every frame's accessibility tree as one text with frame-scoped refs (#11362).
///
/// Why/What: see the module doc. On `DebuggerBusy` (DevTools or another
/// debugger holds the tab) it answers `page_read` of the top document with
/// `fallback`, `reason`, `partial: true` and a `note` that cross-origin
/// frames were not read, as #11362 asks.
/// Test: `a_snapshot_routes_frame_refs_and_falls_back_on_a_busy_debugger`.
async fn snapshot(
    core: &Arc<Core>,
    session: &mut Session,
    grant: &Grant,
    request: &Json,
) -> Result<Ran, Stop> {
    let claim = leased(core, session, grant, request)?;
    let document = claim.document.clone().map_or(Json::Null, Json::from);
    let args = vec![("tab", Json::Int(claim.tab)), ("document", document)];
    let raw = match send(core, grant, claim.key(), &claim.peer, "page_snapshot", args).await {
        Ok(raw) => raw,
        Err(exc) if exc.code == "DebuggerBusy" => {
            let read = Json::object([
                ("op", Json::from("page_read")),
                ("lease", Json::from(claim.token.as_str())),
                ("include_text", Json::Bool(true)),
            ]);
            let (mut reply, tab, host) = query(core, grant, &claim, "page_read", &read).await?;
            reply.insert("fallback", Json::from("page_read"));
            reply.insert("reason", Json::from("DebuggerBusy"));
            reply.insert("partial", Json::Bool(true));
            reply.insert("note", Json::from(PARTIAL_NOTE));
            return Ok((reply, tab, host));
        }
        Err(exc) => return Err(exc.into()),
    };
    let refused = || Refusal::new("BrowserRefused");
    let keys = ["document", "frames", "popups", "truncated"];
    let doc = raw
        .get("document")
        .and_then(Json::as_str)
        .ok_or_else(refused)?;
    if !crate::pairing::key_set_is(&raw, &keys) || claim.document.as_deref() != Some(doc) {
        return Err(refused().into());
    }
    let frames = frames_of(&raw)?;
    let popups = popups_of(&raw, grant)?;
    let truncated = matches!(raw.get("truncated"), Some(Json::Bool(true)));
    // The reply without its list and text: what the budget must leave room for.
    let shell = reply_of(claim.tab, doc, Json::Null, "", popups.clone(), true);
    let reserved = shell.dumps(true).map_or(0, |d| d.len());
    let (text, list, table, cut) = assemble(&frames, &claim.token, doc, reserved);
    let truncated = truncated || cut;
    session.frames.retain(|t| t.lease != claim.token);
    if session.frames.len() >= KEEP {
        session.frames.remove(0);
    }
    session.frames.push(table);
    crate::pages_ops::renew(core, &claim.token);
    let reply = reply_of(claim.tab, doc, list, &text, popups, truncated);
    Ok((reply, Some(claim.tab), hosted(claim.origin.as_deref())?))
}

/// The `page_snapshot` reply the broker writes to the client.
pub fn reply_of(tab: i64, doc: &str, list: Json, text: &str, popups: Json, cut: bool) -> Json {
    let mut reply = Json::object(ok_tab(tab));
    for (k, v) in [
        ("document", Json::from(doc)),
        ("frames", list),
        ("text", Json::from(text)),
        ("popups", popups),
        ("truncated", Json::Bool(cut)),
    ] {
        reply.insert(k, v);
    }
    reply
}

#[cfg(test)]
#[path = "pages_frames_tests.rs"]
mod tests;
