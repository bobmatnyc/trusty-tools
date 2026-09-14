//! Serialized-response byte ceiling for the result-returning MCP tools (#7493).
//!
//! Why: an MCP result is charged to the caller's context in full, and the
//! caller cannot trim what the tool already sent it. A `memory_recall` over a
//! dense palace serializes every hit's whole drawer body, so one call can cost
//! a large fraction of a context budget; folding it server-side is cheaper than
//! any round trip that would summarize it. A recall's L0 identity and L1
//! essential drawers are the palace's baseline grounding rather than search
//! results — `apply_score_floor` already refuses to filter them — so the fold
//! drops L2-and-deeper hits first and never drops an identity or essential one.
//! The trusty-search twin (#7493, `mcp/tools/byte_cap.rs`) enforces the same
//! ceiling on that daemon's search tools; consolidating the two into
//! trusty-common is a follow-up, not this change.
//!
//! What: [`apply`] measures the serialized response and, while it exceeds the
//! ceiling, drops WHOLE entries from the tail of the tool's result array —
//! never an entry cut mid-object, because half a JSON object is not an entry a
//! caller can read. The response then carries `truncated`, `returned`,
//! `withheld`, and a one-sentence `truncation_notice` naming the knobs that
//! fetch the rest. A per-call `max_bytes` overrides [`DEFAULT_MAX_BYTES`] up to
//! [`HARD_MAX_BYTES`]; `full: true` disables the ceiling for that call.
//!
//! These are top-level response fields, not a `meta` block: every trusty-memory
//! result body is flat (`dropped_below_floor`, `kg_triple_count`,
//! `graph_state`), and the twin's `meta` block exists because trusty-search's
//! bodies already had one.
//!
//! A response whose smallest foldable form still exceeds the ceiling is
//! returned anyway, with a notice that says so rather than one claiming the
//! response fits. A non-empty result set never folds to an empty one: "nothing
//! matched" and "the match is too big to send" are different answers, and a
//! caller that cannot tell them apart stops searching.
//!
//! Test: `byte_cap_tests.rs`.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use super::recall_projection::FIRST_FILTERABLE_LAYER;

/// Default ceiling on a serialized tool response: 48 KiB.
///
/// The same number the trusty-search twin uses, for the same reason: it leaves
/// room for a genuinely wide answer while keeping one tool call off the order
/// of a whole context budget.
pub(super) const DEFAULT_MAX_BYTES: usize = 48 * 1024;

/// Hard upper bound on a caller-supplied `max_bytes`: 512 KiB.
///
/// A caller that asks for more is clamped to this and told so in the response,
/// rather than being handed the unbounded body the argument asked for.
pub(super) const HARD_MAX_BYTES: usize = 512 * 1024;

/// Knobs named in a recall tool's truncation notice.
const RECALL_HINT: &str =
    "a smaller `top_k`, a `min_score` floor, or `full: true` retrieves the rest";

/// One capped tool: where its droppable entries live, and how its notice reads.
struct CappedTool {
    /// MCP tool name, as the dispatcher and `tools/list` spell it.
    name: &'static str,
    /// Response key holding the array of droppable entries.
    items_key: &'static str,
    /// Singular and plural nouns for the notice sentence.
    one: &'static str,
    many: &'static str,
    /// `true` when an entry carrying a `layer` below
    /// [`FIRST_FILTERABLE_LAYER`] is never dropped — the L0 identity and L1
    /// essential drawers a recall always returns.
    protect_layers: bool,
    /// The knobs this tool's notice tells the caller to narrow with.
    hint: &'static str,
}

/// Every result-returning MCP tool, with the array each one folds.
///
/// The rest of the surface is bounded by construction: a write ack, a status,
/// a palace summary, or a count is a fixed handful of fields, and the
/// enumeration tools (`kg_list_subjects`, `chat_session_list`, `room_list`,
/// `wing_list`, `task_list`) return short rows under their own `limit`.
/// Capping them would add a truncation verdict to bodies that cannot reach the
/// ceiling. The #6318 palace-index fallback is likewise untouched: it carries
/// none of these keys, so [`apply`] leaves it exactly as the handler built it.
const CAPPED_TOOLS: [CappedTool; 7] = [
    CappedTool {
        name: "memory_recall",
        items_key: "results",
        one: "hit",
        many: "hits",
        protect_layers: true,
        hint: RECALL_HINT,
    },
    CappedTool {
        name: "memory_recall_deep",
        items_key: "results",
        one: "hit",
        many: "hits",
        protect_layers: true,
        hint: RECALL_HINT,
    },
    CappedTool {
        name: "memory_recall_all",
        items_key: "results",
        one: "hit",
        many: "hits",
        protect_layers: true,
        // No `min_score` on the fan-out: one floor over heterogeneous corpora
        // would mean a different thing per palace.
        hint: "a smaller `top_k`, or `full: true` retrieves the rest",
    },
    CappedTool {
        name: "memory_list",
        items_key: "drawers",
        one: "drawer",
        many: "drawers",
        protect_layers: false,
        hint: "a smaller `limit`, a `room`/`wing`/`tag` filter, or `full: true` retrieves the rest",
    },
    CappedTool {
        name: "kg_query",
        items_key: "triples",
        one: "triple",
        many: "triples",
        protect_layers: false,
        hint: "a narrower `subject` (see `kg_list_subjects`), or `full: true` retrieves the rest",
    },
    CappedTool {
        name: "chat_session_recall",
        items_key: "history",
        one: "turn",
        many: "turns",
        protect_layers: false,
        hint: "`full: true` returns the whole history",
    },
    CappedTool {
        name: "list_prompt_facts",
        items_key: "facts",
        one: "fact",
        many: "facts",
        protect_layers: false,
        hint: "`full: true` returns every fact",
    },
];

/// The capped-tool entry for `tool`, or `None` when the tool is not capped.
fn capped_tool(tool: &str) -> Option<&'static CappedTool> {
    CAPPED_TOOLS.iter().find(|t| t.name == tool)
}

/// Whether `tool`'s response passes through the fold.
///
/// Why: the dispatcher clones the caller's arguments only for a capped tool —
/// [`apply`] needs them, and the dispatch itself consumes them.
/// What: membership in [`CAPPED_TOOLS`].
/// Test: `an_uncapped_tool_is_returned_untouched`.
pub(super) fn is_capped(tool: &str) -> bool {
    capped_tool(tool).is_some()
}

/// Every capped tool's name, so a test never restates the list (#7493).
///
/// A hardcoded copy in a test passes while the table, the descriptors and the
/// router disagree, which is the drift the test exists to catch.
#[cfg(test)]
pub(super) fn capped_tool_names() -> Vec<&'static str> {
    CAPPED_TOOLS.iter().map(|t| t.name).collect()
}

/// Measure a value's serialized size, in bytes.
///
/// Why: the ceiling is enforced against the bytes the caller actually
/// receives, so the measurement uses the same compact encoding
/// `tools/call` puts in `content[0].text`. A measurement that cannot be taken
/// is an error, never a licence to return the response unmeasured — that is the
/// one path by which an unbounded body could still reach a context window.
/// What: serializes with `serde_json` and returns the byte length, mapping a
/// serialization failure onto an error the caller sees instead of a body.
/// Test: `a_measurement_failure_returns_an_error_not_an_unmeasured_body`.
pub(super) fn measure<T: Serialize + ?Sized>(value: &T) -> Result<usize> {
    serde_json::to_string(value).map(|s| s.len()).map_err(|e| {
        anyhow!(
            "response size measurement failed ({e}); the response is withheld \
             rather than returned unmeasured (#7493)"
        )
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
    /// Why: a `max_bytes` that is not an integer, and a `full` that is not a
    /// boolean, are rejected rather than ignored — the same reasoning
    /// `min_score_arg` records. A silently dropped ceiling returns a different
    /// number of entries than the caller asked for with nothing in the
    /// response to say why, and coercing `"true"` to `false` would re-impose a
    /// ceiling the caller just disabled.
    /// What: `None`/`null` means the default. Errors name `tool`, matching this
    /// crate's argument-error convention.
    /// Test: `a_non_boolean_full_is_rejected`,
    /// `a_non_integer_or_out_of_range_max_bytes_is_rejected`,
    /// `a_max_bytes_over_the_hard_cap_is_clamped_and_reported`.
    fn parse(args: &Value, tool: &str) -> Result<Self> {
        let requested = match args.get("max_bytes") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n) if n <= u32::MAX as u64 => Some(n as usize),
                _ => {
                    return Err(anyhow!(
                        "{tool}: 'max_bytes' must be an integer number of bytes in 0..={}, \
                         got {v} — a quoted or negative value is not coerced",
                        u32::MAX
                    ))
                }
            },
        };
        let full = match args.get("full") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(anyhow!(
                    "{tool}: 'full' must be a boolean (true returns every entry with no \
                     byte ceiling), got {other} — a quoted value is not coerced"
                ))
            }
        };
        Ok(Self {
            ceiling: requested.unwrap_or(DEFAULT_MAX_BYTES).min(HARD_MAX_BYTES),
            // `full` removes the ceiling, so there is no clamp to report.
            clamped: !full && requested.is_some_and(|n| n > HARD_MAX_BYTES),
            full,
        })
    }
}

/// How a candidate response relates to the ceiling.
///
/// #7493: `Oversized` is not "the tail was dropped" — it is "the smallest form
/// this response folds to is still over the ceiling, and it is returned
/// anyway". The two need different notices, because saying entries "were
/// withheld to keep this response under N bytes" about a response that is OVER
/// N bytes is false.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    /// Everything fits.
    Whole,
    /// A prefix of whole entries fits; the tail was dropped.
    Folded,
    /// Even the smallest foldable form exceeds the ceiling; it is returned
    /// regardless.
    Oversized,
}

/// One response mid-fold: the fields outside the entries array, the entries,
/// and the ceiling they have to fit under.
struct Fold<'a> {
    spec: &'a CappedTool,
    base: Map<String, Value>,
    items: Vec<Value>,
    /// How many entries the fold may never drop (L0/L1 on a recall).
    protected: usize,
    bounds: Bounds,
}

/// Whether this entry is exempt from dropping.
///
/// L0 identity and L1 essential drawers are the palace's baseline grounding,
/// not query results — the same distinction
/// [`super::recall_projection::apply_score_floor`] draws when it refuses to
/// filter them.
fn is_protected(spec: &CappedTool, item: &Value) -> bool {
    spec.protect_layers
        && item
            .get("layer")
            .and_then(Value::as_u64)
            .is_some_and(|layer| layer < u64::from(FIRST_FILTERABLE_LAYER))
}

impl Fold<'_> {
    /// How many entries may be dropped at all.
    fn droppable(&self) -> usize {
        self.items.len() - self.protected
    }

    /// Build the response that keeps every protected entry plus the first
    /// `keep` droppable ones, in their original order.
    fn candidate(&self, keep: usize, fit: Fit) -> Value {
        let mut map = self.base.clone();
        let mut rank = 0usize;
        let mut kept: Vec<Value> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            if is_protected(self.spec, item) {
                kept.push(item.clone());
                continue;
            }
            if rank < keep {
                kept.push(item.clone());
            }
            rank += 1;
        }
        let returned = kept.len();
        let withheld = self.items.len() - returned;
        map.insert(self.spec.items_key.to_string(), Value::Array(kept));
        let truncated = fit != Fit::Whole;
        map.insert("truncated".into(), Value::Bool(truncated));
        if truncated {
            map.insert("returned".into(), Value::from(returned));
            map.insert("withheld".into(), Value::from(withheld));
            map.insert(
                "truncation_notice".into(),
                Value::String(self.notice(returned, withheld, fit)),
            );
        }
        if self.bounds.clamped {
            map.insert("max_bytes_clamped".into(), Value::Bool(true));
            map.insert("max_bytes".into(), Value::from(self.bounds.ceiling));
        }
        Value::Object(map)
    }

    /// The noun for a count, so the notice never reads "1 hits".
    fn noun(&self, n: usize) -> &'static str {
        if n == 1 {
            self.spec.one
        } else {
            self.spec.many
        }
    }

    /// The one-sentence notice `truncation_notice` carries.
    ///
    /// The `Oversized` wording never claims the response fits, whatever the
    /// counts are: what was returned is itself over the ceiling.
    fn notice(&self, returned: usize, withheld: usize, fit: Fit) -> String {
        let (ceiling, hint, total) = (self.bounds.ceiling, self.spec.hint, self.items.len());
        let many = self.spec.many;
        match (fit, withheld) {
            (Fit::Oversized, 0) => format!(
                "This response exceeds the {ceiling}-byte ceiling on its own: its {returned} \
                 {returned_noun} cannot be folded smaller without answering an empty result, \
                 so they are returned whole; {hint}.",
                returned_noun = self.noun(returned),
            ),
            (Fit::Oversized, _) => format!(
                "This response exceeds the {ceiling}-byte ceiling on its own: the smallest it \
                 folds to is {returned} of {total} {many}, and the other {withheld} were \
                 dropped; {hint}."
            ),
            _ => format!(
                "{withheld} of {total} {many} were withheld to keep this response under \
                 {ceiling} bytes; {hint}."
            ),
        }
    }
}

/// Fold an oversized tool response down to the ceiling (#7493).
///
/// Why: see the module header — an unbounded MCP result is charged to the
/// caller's context in full. Enforcing it here, at the one place every tool
/// result passes through, is what keeps a new tool arm from shipping
/// unbounded.
/// What: a no-op for an uncapped tool, and for a capped tool whose body is not
/// a result body (the #6318 palace index, which carries no entries array).
/// Otherwise it measures the whole response, returns it unchanged when it fits
/// (adding only `truncated: false`), and otherwise binary-searches the longest
/// prefix of whole droppable entries that fits, keeping every protected entry
/// and never folding a non-empty result set to an empty one. Propagates a
/// measurement failure instead of returning an unmeasured body.
/// Test: `byte_cap_tests.rs`.
pub(super) fn apply(tool: &str, args: &Value, resp: &mut Value) -> Result<()> {
    let Some(spec) = capped_tool(tool) else {
        return Ok(());
    };
    // Before the body is inspected: an unusable argument is an error whatever
    // the handler happened to answer with.
    let bounds = Bounds::parse(args, tool)?;
    let Some(base) = resp.as_object() else {
        return Ok(());
    };
    let Some(items) = base.get(spec.items_key).and_then(Value::as_array) else {
        return Ok(());
    };
    let items = items.clone();
    let protected = items.iter().filter(|i| is_protected(spec, i)).count();

    let fold = Fold {
        spec,
        base: base.clone(),
        items,
        protected,
        bounds,
    };
    let droppable = fold.droppable();

    // `full: true` skips the measurement entirely — the point of the escape
    // hatch is to pay for the whole body on purpose.
    if bounds.full {
        *resp = fold.candidate(droppable, Fit::Whole);
        return Ok(());
    }

    let whole = fold.candidate(droppable, Fit::Whole);
    // An empty body is already as small as it gets; there is nothing to drop.
    if measure(&whole)? <= bounds.ceiling || fold.items.is_empty() {
        *resp = whole;
        return Ok(());
    }

    // Never fold a non-empty result set to an empty one. With protected
    // entries present they are the floor; without them, one entry is.
    let min_keep = usize::from(fold.protected == 0);
    // Longest prefix that fits. Monotone in `keep`, so a binary search costs
    // log2(n) measurements instead of one per dropped entry. `best` stays
    // `None` until a probe actually fits — seeding it would ship an unverified
    // response under the "withheld to keep this response under N bytes"
    // notice, which is false when that response is itself over N.
    let (mut lo, mut hi, mut best) = (min_keep, droppable.saturating_sub(1), None::<usize>);
    while lo <= hi && droppable > 0 {
        let mid = lo + (hi - lo) / 2;
        if measure(&fold.candidate(mid, Fit::Folded))? <= bounds.ceiling {
            best = Some(mid);
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    *resp = match best {
        Some(keep) => fold.candidate(keep, Fit::Folded),
        None => fold.candidate(min_keep, Fit::Oversized),
    };
    Ok(())
}

/// Add `max_bytes` and `full` to every capped tool's input schema (#7493).
///
/// Why: the descriptors and the dispatcher read the same [`CAPPED_TOOLS`]
/// list, so a tool cannot advertise a knob it does not honour, or honour one it
/// never advertised.
/// What: for each tool named in [`CAPPED_TOOLS`], inserts the two optional
/// properties into `inputSchema.properties`. Called from
/// [`super::definitions::tool_definitions_with`] on the assembled tool array.
/// Test: `every_capped_tool_advertises_max_bytes_and_full`.
pub(super) fn annotate_capped_tools(tools: &mut Value) {
    let Some(tools) = tools.as_array_mut() else {
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
                     the clamp is reported in the response. Over the ceiling, whole {many} are \
                     dropped from the tail — never a partial one — and `truncated`, \
                     `withheld`, and `truncation_notice` say what is missing (#7493)."
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
