//! Serialized-response byte ceiling for the result-returning MCP tools (#7493).
//!
//! Why: one conceptual `search` spilled 77k characters into a session's
//! context, where a ten-hit compact call costs 2.7k. Diverting an oversized
//! result to a worker model costs about 20k prompt tokens per round trip, so
//! the owner ruling (2026-09-14) folds it here instead — server-side, the way
//! `session_context_catchup` bounds its own payload in trusty-mpm. A caller
//! cannot trim what the tool already sent it.
//!
//! What: [`apply`] measures the serialized response and, while it exceeds the
//! ceiling, drops WHOLE items from the tail of the tool's result array — never
//! a hit cut mid-object, because half a JSON object is not a hit a caller can
//! read. `meta` then carries `truncated`, `returned`, `withheld`, and a
//! one-sentence `truncation_notice` naming the knobs that fetch the rest. A
//! per-call `max_bytes` overrides [`DEFAULT_MAX_BYTES`] up to
//! [`HARD_MAX_BYTES`]; `full: true` disables the ceiling for that call.
//!
//! A single item larger than the whole ceiling is returned ALONE with
//! `truncated: true`. A non-empty result set never folds to an empty one:
//! "nothing matched" and "the match is too big to send" are different answers,
//! and a caller that cannot tell them apart stops searching.
//!
//! Test: `tests_byte_cap.rs`.

use serde::Serialize;
use serde_json::{Map, Value};

use super::types::DispatchError;

/// Default ceiling on a serialized tool response: 48 KiB.
///
/// The observed outlier was 77k characters; a ten-hit compact `search` is
/// 2.7k. 48 KiB leaves room for a genuinely wide answer while keeping one
/// tool call off the order of a whole context budget.
pub(super) const DEFAULT_MAX_BYTES: usize = 48 * 1024;

/// Hard upper bound on a caller-supplied `max_bytes`: 512 KiB.
///
/// A caller that asks for more is clamped to this and told so in `meta`,
/// rather than being handed the unbounded response the argument asked for.
pub(super) const HARD_MAX_BYTES: usize = 512 * 1024;

/// The response key `get_call_chain` returns its prose tree under.
const TEXT_KEY: &str = "text";

/// The response key `list_chunks` pages, which needs its cursor rewritten
/// when the tail is dropped.
const CHUNKS_KEY: &str = "chunks";

/// Narrowing knobs named in a search tool's truncation notice.
const SEARCH_HINT: &str =
    "`compact: true`, a smaller `top_k`, a `path_prefix`, or `full: true` retrieves the rest";

/// One capped tool: where its droppable items live, and how its notice reads.
struct CappedTool {
    /// MCP tool name.
    name: &'static str,
    /// Response key holding the droppable items, or `None` for a tool whose
    /// body is one indivisible unit (`get_call_chain` returns a prose tree).
    items_key: Option<&'static str>,
    /// Singular and plural nouns for the notice sentence.
    one: &'static str,
    many: &'static str,
    /// The knobs this tool's notice tells the caller to narrow with.
    hint: &'static str,
}

/// Every result-returning MCP tool, with the array each one folds.
///
/// The other tools in `descriptors.rs` are bounded by construction: a status,
/// health, directory, or admin body is a fixed handful of fields, and
/// `typeahead` / `search_similar` cap their own hit count. Capping them would
/// add a `meta` block to bodies that can never reach the ceiling.
const CAPPED_TOOLS: [CappedTool; 8] = [
    CappedTool {
        name: "search",
        items_key: Some("results"),
        one: "result",
        many: "results",
        hint: SEARCH_HINT,
    },
    CappedTool {
        name: "search_lexical",
        items_key: Some("results"),
        one: "result",
        many: "results",
        hint: SEARCH_HINT,
    },
    CappedTool {
        name: "search_semantic",
        items_key: Some("results"),
        one: "result",
        many: "results",
        hint: SEARCH_HINT,
    },
    CappedTool {
        name: "search_kg",
        items_key: Some("results"),
        one: "result",
        many: "results",
        hint: SEARCH_HINT,
    },
    CappedTool {
        name: "search_all",
        items_key: Some("results"),
        one: "result",
        many: "results",
        hint: SEARCH_HINT,
    },
    CappedTool {
        name: "list_chunks",
        items_key: Some(CHUNKS_KEY),
        one: "chunk",
        many: "chunks",
        hint: "a smaller `limit`, a `path_prefix`, the returned `next_cursor`, or `full: true` \
               retrieves the rest",
    },
    CappedTool {
        name: "grep",
        items_key: Some("matches"),
        one: "match",
        many: "matches",
        hint: "a smaller `max_results`, a narrower `glob`, less `context`, or `full: true` \
               retrieves the rest",
    },
    CappedTool {
        name: "get_call_chain",
        items_key: None,
        one: "call tree",
        many: "call trees",
        hint: "a smaller `max_depth`, `include_source: false`, or `full: true` retrieves the rest",
    },
];

/// The capped-tool entry for `tool`, or `None` when the tool is not capped.
fn capped_tool(tool: &str) -> Option<&'static CappedTool> {
    CAPPED_TOOLS.iter().find(|t| t.name == tool)
}

/// Measure a value's serialized size, in bytes.
///
/// Why: the ceiling is enforced against the bytes the caller actually
/// receives, so the measurement uses the same pretty-printed encoding the
/// `content[]` envelope embeds. A measurement that cannot be taken is an
/// error, never a licence to return the response unmeasured — that is the one
/// path by which an unbounded body could still reach a context window.
/// What: pretty-prints with `serde_json` and returns the byte length, mapping
/// a serialization failure onto `DispatchError::Transport` so the caller sees
/// a tool error instead of a body.
/// Test: `a_measurement_failure_returns_an_error_not_an_unmeasured_body`.
pub(super) fn measure<T: Serialize + ?Sized>(value: &T) -> Result<usize, DispatchError> {
    serde_json::to_string_pretty(value)
        .map(|s| s.len())
        .map_err(|e| {
            DispatchError::Transport(format!(
                "response size measurement failed ({e}); the response is withheld rather \
                 than returned unmeasured (#7493)"
            ))
        })
}

/// The effective ceiling for one call.
#[derive(Clone, Copy)]
struct Bounds {
    ceiling: usize,
    /// `true` when the caller's `max_bytes` was above [`HARD_MAX_BYTES`].
    clamped: bool,
    /// `true` when the caller passed `full: true`.
    full: bool,
}

impl Bounds {
    /// Read `max_bytes` and `full` off the tool arguments.
    ///
    /// A `max_bytes` that is not a `u32` is rejected rather than ignored: a
    /// silently dropped ceiling returns a different number of hits than the
    /// caller asked for, with nothing in the response to say why.
    fn parse(args: &Value) -> Result<Self, DispatchError> {
        let requested = match args.get("max_bytes") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n) if n <= u32::MAX as u64 => Some(n as usize),
                _ => {
                    return Err(DispatchError::InvalidParams(format!(
                        "max_bytes must be an integer number of bytes in 0..={}; got {v}",
                        u32::MAX
                    )))
                }
            },
        };
        let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
        Ok(Self {
            ceiling: requested.unwrap_or(DEFAULT_MAX_BYTES).min(HARD_MAX_BYTES),
            // `full` removes the ceiling, so there is no clamp to report.
            clamped: !full && requested.is_some_and(|n| n > HARD_MAX_BYTES),
            full,
        })
    }
}

/// One response mid-fold: the fields outside the items array, the items, and
/// the ceiling they have to fit under.
struct Fold<'a> {
    spec: &'a CappedTool,
    base: Map<String, Value>,
    items: Vec<Value>,
    /// Droppable-unit count: the array length, or 1 for a single-body tool.
    total: usize,
    bounds: Bounds,
    /// `true` when the caller paged `list_chunks` with an `after` cursor.
    cursor_mode: bool,
}

impl Fold<'_> {
    /// Build the response that keeps the first `keep` items.
    fn candidate(&self, keep: usize, truncated: bool) -> Value {
        let mut map = self.base.clone();
        let withheld = self.total - keep;
        if let Some(key) = self.spec.items_key {
            map.insert(key.to_string(), Value::Array(self.items[..keep].to_vec()));
            if withheld > 0 && key == CHUNKS_KEY {
                self.repage(&mut map, keep);
            }
        }
        let notice = truncated.then(|| self.notice(keep));
        let meta = meta_entry(&mut map);
        meta.insert("truncated".into(), Value::Bool(truncated));
        if let Some(notice) = notice {
            meta.insert("returned".into(), Value::from(keep));
            meta.insert("withheld".into(), Value::from(withheld));
            meta.insert("truncation_notice".into(), Value::String(notice));
        }
        if self.bounds.clamped {
            meta.insert("max_bytes_clamped".into(), Value::Bool(true));
            meta.insert("max_bytes".into(), Value::from(self.bounds.ceiling));
        }
        Value::Object(map)
    }

    /// Keep `list_chunks` paging honest after the tail is dropped.
    ///
    /// Why: the daemon's `next_cursor` names the last chunk it FETCHED. Left
    /// alone, the next page would start after chunks this response never
    /// returned, so a capped walk would silently skip them.
    /// What: in cursor mode the cursor becomes the last RETURNED chunk's id;
    /// in offset mode — where the cursor is always null and ordering differs,
    /// so an id cursor must not be invented — `limit` becomes the returned
    /// count, which is what an offset walk adds to `offset` for its next page.
    /// Test: `list_chunks_cursor_points_at_the_last_returned_chunk`,
    /// `list_chunks_offset_paging_resumes_at_the_first_withheld_chunk`.
    fn repage(&self, map: &mut Map<String, Value>, keep: usize) {
        if self.cursor_mode {
            let last_id = self
                .items
                .get(keep.saturating_sub(1))
                .and_then(|c| c.get("id"))
                .cloned()
                .unwrap_or(Value::Null);
            map.insert("next_cursor".into(), last_id);
        } else {
            map.insert("limit".into(), Value::from(keep));
        }
    }

    /// The one-sentence notice `meta.truncation_notice` carries.
    fn notice(&self, keep: usize) -> String {
        let (ceiling, hint) = (self.bounds.ceiling, self.spec.hint);
        match self.total - keep {
            0 => format!(
                "The single {one} exceeds the {ceiling}-byte ceiling and is returned whole \
                 rather than as an empty result; {hint}.",
                one = self.spec.one,
            ),
            withheld => format!(
                "{withheld} of {total} {many} were withheld to keep this response under \
                 {ceiling} bytes; {hint}.",
                total = self.total,
                many = self.spec.many,
            ),
        }
    }
}

/// Borrow the response's `meta` object, creating it when absent.
///
/// A non-object `meta` is replaced: the truncation verdict has to reach the
/// caller, and there is nothing to merge into a scalar.
fn meta_entry(map: &mut Map<String, Value>) -> &mut Map<String, Value> {
    if !map.get("meta").is_some_and(Value::is_object) {
        map.insert("meta".into(), Value::Object(Map::new()));
    }
    map.get_mut("meta")
        .and_then(Value::as_object_mut)
        .expect("meta was just ensured to be an object")
}

/// Fold an oversized tool response down to the ceiling (#7493).
///
/// Why: see the module header — an unbounded MCP result is charged to the
/// caller's context in full, and folding it server-side is cheaper than any
/// round trip that would summarize it.
/// What: a no-op for an uncapped tool, and for a capped tool whose body is not
/// a result body (an index directory, a not-ready payload — neither carries
/// the items array). Otherwise it measures the whole response, returns it
/// unchanged when it fits (adding only `meta.truncated: false`), and
/// otherwise binary-searches the longest prefix of whole items that fits,
/// keeping at least one. Propagates a measurement failure instead of
/// returning an unmeasured body.
/// Test: `tests_byte_cap.rs`.
pub(super) fn apply(tool: &str, args: &Value, resp: &mut Value) -> Result<(), DispatchError> {
    let Some(spec) = capped_tool(tool) else {
        return Ok(());
    };
    let bounds = Bounds::parse(args)?;
    let Some(base) = resp.as_object() else {
        return Ok(());
    };
    let (items, total) = match spec.items_key {
        Some(key) => match base.get(key).and_then(Value::as_array) {
            Some(a) => (a.clone(), a.len()),
            None => return Ok(()),
        },
        // A single indivisible body: present or the tool answered something
        // else (an error, a directory) that must not gain a `meta` block.
        None if base.contains_key(TEXT_KEY) => (Vec::new(), 1),
        None => return Ok(()),
    };

    let fold = Fold {
        spec,
        base: base.clone(),
        items,
        total,
        bounds,
        cursor_mode: args.get("after").and_then(Value::as_str).is_some(),
    };

    // `full: true` skips the measurement entirely — the point of the escape
    // hatch is to pay for the whole body on purpose.
    if bounds.full {
        *resp = fold.candidate(total, false);
        return Ok(());
    }

    let whole = fold.candidate(total, false);
    if measure(&whole)? <= bounds.ceiling {
        *resp = whole;
        return Ok(());
    }
    if total <= 1 {
        *resp = fold.candidate(total, true);
        return Ok(());
    }

    // Longest prefix that fits. Monotone in `keep`, so a binary search costs
    // log2(n) measurements instead of one per dropped item.
    let (mut lo, mut hi, mut best) = (1usize, total - 1, 1usize);
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        if measure(&fold.candidate(mid, true))? <= bounds.ceiling {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    *resp = fold.candidate(best, true);
    Ok(())
}

/// Add `max_bytes` and `full` to every capped tool's input schema (#7493).
///
/// Why: the descriptors and the dispatcher read the same [`CAPPED_TOOLS`]
/// list, so a tool cannot advertise a knob it does not honour, or honour one
/// it never advertised.
/// What: for each tool named in [`CAPPED_TOOLS`], inserts the two optional
/// properties into `inputSchema.properties`. Called from `tool_descriptors`.
/// Test: `every_capped_tool_advertises_max_bytes_and_full`.
pub(super) fn annotate_capped_tools(defs: &mut Value) {
    let Some(tools) = defs.as_array_mut() else {
        return;
    };
    for tool in tools.iter_mut() {
        let Some(spec) = tool
            .get("name")
            .and_then(Value::as_str)
            .and_then(capped_tool)
        else {
            continue;
        };
        let Some(props) = tool
            .get_mut("inputSchema")
            .and_then(Value::as_object_mut)
            .and_then(|schema| schema.get_mut("properties"))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let many = spec.many;
        props.insert(
            "max_bytes".into(),
            serde_json::json!({
                "type": "integer",
                "default": DEFAULT_MAX_BYTES,
                "description": format!(
                    "Ceiling on this response's serialized size, in bytes (default \
                     {DEFAULT_MAX_BYTES}). A larger value is clamped to {HARD_MAX_BYTES} and \
                     the clamp is reported in `meta`. Over the ceiling, whole {many} are \
                     dropped from the tail — never a partial one — and `meta.truncated`, \
                     `meta.withheld`, and `meta.truncation_notice` say what is missing (#7493)."
                ),
            }),
        );
        props.insert(
            "full".into(),
            serde_json::json!({
                "type": "boolean",
                "default": false,
                "description": format!(
                    "Return every {many} with no byte ceiling. Reach for it after a capped \
                     call has told you what it withheld, not before (#7493)."
                ),
            }),
        );
    }
}
