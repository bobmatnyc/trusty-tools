//! MCP tool-schema contract snapshot for trusty-memory (#9276).
//!
//! Why: ADR-0066 D1.1 freezes the trusty-memory MCP surface for 1.x, and D4
//! names this snapshot as its guard. A 1.x change is additive only: removing
//! or renaming a tool or a param, a new required param, an optional param made
//! required, or a changed param type must fail here. A new tool or a new
//! optional param must not.
//! What: [`snapshot`] reduces the default `tools/list` payload
//! (`tool_definitions()`, no `--palace` default) to one JSON Lines record per
//! tool: name, required params, param types and the deprecation flag, sorted
//! by name. [`check`] compares the committed snapshot with the live payload.
//! Descriptions and tool order are out of contract (ADR-0066 D2) and are not
//! recorded. The one description rule kept is the D3 deprecation notice,
//! checked on the live payload.
//! Deprecation: "a deprecated tool must stay for one minor" is a release rule.
//! This test enforces the part one build can see. A deprecated tool carries
//! both D3 markers (the `DEPRECATED since 1.y; removed in 2.0: use
//! <replacement>.` description prefix and `_meta.deprecated: true`), and
//! removing any tool, deprecated or not, is a break, because a deprecated
//! item keeps working through the last 1.x release (D3). The test keeps no
//! version history; the release that first ships the notice is the minor.
//! Test: `mcp_schema_matches_snapshot` (the real payload) and the comparator
//! cases below it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use trusty_memory::tools::tool_definitions;

/// The committed snapshot, relative to the crate root.
const SNAPSHOT: &str = "tests/snapshots/mcp_schema_contract.jsonl";
/// Environment variable that rewrites the snapshot instead of comparing.
const REFRESH_ENV: &str = "UPDATE_MCP_SCHEMA_CONTRACT";
const REFRESH_CMD: &str =
    "UPDATE_MCP_SCHEMA_CONTRACT=1 cargo test -p trusty-memory --test mcp_schema_contract";
/// ADR-0066 D3 notice: `DEPRECATED since 1.<minor>; removed in 2.0: use <x>.`
const NOTICE_PREFIX: &str = "DEPRECATED since 1.";
const NOTICE_REMOVAL: &str = "; removed in 2.0: use ";

/// One line of the snapshot file: the contract attributes of one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolRecord {
    name: String,
    /// Sorted, deduplicated `inputSchema.required`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required: Vec<String>,
    /// Param name to its JSON Schema type (`array<string>` for typed arrays).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    deprecated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Breaking changes and additive drift found by [`check`].
#[derive(Debug, Default)]
struct Report {
    breaks: Vec<String>,
    additions: Vec<String>,
}

/// The `type` of one param schema. A type list is sorted and joined with `|`;
/// an array records its item type; a schema with no `type` is `any`.
fn param_type(schema: &Value) -> String {
    let base = match schema.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(types)) => {
            let mut v: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
            v.sort_unstable();
            v.join("|")
        }
        _ => "any".to_string(),
    };
    match schema.get("items") {
        Some(items) if base == "array" => format!("array<{}>", param_type(items)),
        _ => base,
    }
}

fn is_deprecated(tool: &Value) -> bool {
    tool["_meta"]["deprecated"] == Value::Bool(true)
}

fn record(tool: &Value) -> ToolRecord {
    let name = tool["name"].as_str().expect("every tool has a name");
    let schema = &tool["inputSchema"];
    let mut required: Vec<String> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    required.sort();
    required.dedup();
    let params = schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), param_type(v)))
        .collect();
    ToolRecord {
        name: name.to_string(),
        required,
        params,
        deprecated: is_deprecated(tool),
    }
}

/// Records every tool of a `tools/list` payload, sorted by name.
fn snapshot(defs: &Value) -> Vec<ToolRecord> {
    let tools = defs["tools"].as_array().expect("payload has a tools array");
    let mut out: Vec<ToolRecord> = tools.iter().map(record).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in out.windows(2) {
        assert_ne!(pair[0].name, pair[1].name, "duplicate tool name");
    }
    out
}

/// The D3 notice problem of one live tool, if any. Both markers or neither.
fn notice_problem(tool: &Value) -> Option<String> {
    let name = tool["name"].as_str().unwrap_or("?");
    let desc = tool["description"].as_str().unwrap_or("");
    let well_formed = desc.strip_prefix(NOTICE_PREFIX).is_some_and(|rest| {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let tail = &rest[digits..];
        digits > 0 && tail.len() > NOTICE_REMOVAL.len() && tail.starts_with(NOTICE_REMOVAL)
    });
    match (
        is_deprecated(tool),
        desc.starts_with("DEPRECATED"),
        well_formed,
    ) {
        (true, _, true) | (false, false, _) => None,
        (true, _, false) => Some(format!(
            "tool `{name}` sets `_meta.deprecated` but its description does not open \
             with `{NOTICE_PREFIX}y{NOTICE_REMOVAL}<replacement>.` (ADR-0066 D3)"
        )),
        (false, true, _) => Some(format!(
            "tool `{name}` has a DEPRECATED description but no `_meta.deprecated: true` \
             (ADR-0066 D3)"
        )),
    }
}

/// Compares the committed `golden` records with the live payload under
/// ADR-0066's additive-only rules.
fn check(golden: &[ToolRecord], live: &Value) -> Report {
    let current = snapshot(live);
    let by_name: BTreeMap<&str, &ToolRecord> =
        current.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut report = Report::default();
    for tool in live["tools"].as_array().into_iter().flatten() {
        report.breaks.extend(notice_problem(tool));
    }
    for g in golden {
        match by_name.get(g.name.as_str()) {
            None if g.deprecated => report.breaks.push(format!(
                "deprecated tool `{}` was removed; a deprecated tool stays through the \
                 last 1.x release (ADR-0066 D3)",
                g.name
            )),
            None => report
                .breaks
                .push(format!("tool `{}` was removed or renamed", g.name)),
            Some(c) => compare_tool(g, c, &mut report),
        }
    }
    let golden_names: BTreeSet<&str> = golden.iter().map(|g| g.name.as_str()).collect();
    for c in current
        .iter()
        .filter(|c| !golden_names.contains(c.name.as_str()))
    {
        let line = serde_json::to_string(c).expect("record serializes");
        report.additions.push(format!("new tool: {line}"));
    }
    report
}

fn compare_tool(g: &ToolRecord, c: &ToolRecord, report: &mut Report) {
    let t = &g.name;
    for (p, ty) in &g.params {
        match c.params.get(p) {
            None => report
                .breaks
                .push(format!("tool `{t}`: param `{p}` was removed or renamed")),
            Some(live) if live != ty => report.breaks.push(format!(
                "tool `{t}`: param `{p}` changed type from {ty} to {live}"
            )),
            Some(_) => {}
        }
    }
    for p in c.required.iter().filter(|p| !g.required.contains(p)) {
        report.breaks.push(if g.params.contains_key(p) {
            format!("tool `{t}`: param `{p}` changed from optional to required")
        } else {
            format!("tool `{t}`: new param `{p}` is required")
        });
    }
    for p in g
        .required
        .iter()
        .filter(|p| !c.required.contains(p) && c.params.contains_key(*p))
    {
        report
            .additions
            .push(format!("tool `{t}`: param `{p}` is no longer required"));
    }
    for p in c
        .params
        .keys()
        .filter(|p| !g.params.contains_key(*p) && !c.required.contains(p))
    {
        report
            .additions
            .push(format!("tool `{t}`: new optional param `{p}`"));
    }
    if g.deprecated != c.deprecated {
        report.additions.push(format!(
            "tool `{t}`: deprecated changed to {}",
            c.deprecated
        ));
    }
}

fn render(records: &[ToolRecord]) -> String {
    let mut text: String = records
        .iter()
        .map(|r| serde_json::to_string(r).expect("record serializes"))
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

fn parse(text: &str) -> Vec<ToolRecord> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad snapshot line {l}: {e}")))
        .collect()
}

/// Reads the committed snapshot. A missing file is an error, never created.
fn read_golden(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| {
        format!(
            "cannot read {}: {e}\ncreate it with: {REFRESH_CMD}",
            path.display()
        )
    })
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT)
}

/// Why: the guard ADR-0066 D4 assigns to the MCP surface.
/// What: compares `tests/snapshots/mcp_schema_contract.jsonl` with the live
/// default `tool_definitions()`; with `UPDATE_MCP_SCHEMA_CONTRACT=1` it
/// rewrites the snapshot instead. Additions pass with a note.
/// Test: this function.
#[test]
fn mcp_schema_matches_snapshot() {
    let path = snapshot_path();
    let live = tool_definitions();
    if std::env::var(REFRESH_ENV).is_ok_and(|v| !v.is_empty() && v != "0") {
        std::fs::create_dir_all(path.parent().expect("snapshot has a parent")).expect("mkdir");
        std::fs::write(&path, render(&snapshot(&live))).expect("write snapshot");
        eprintln!("rewrote {}", path.display());
        return;
    }
    let golden = read_golden(&path).unwrap_or_else(|e| panic!("{e}"));
    let report = check(&parse(&golden), &live);
    if !report.additions.is_empty() {
        eprintln!(
            "note: the MCP schema gained {} item(s) the snapshot does not pin yet. \
             Additive changes are allowed in 1.x (ADR-0066 D3); pin them with \
             `{REFRESH_CMD}` together with a changelog fragment that names the \
             addition (ADR-0066 D4):\n  {}",
            report.additions.len(),
            report.additions.join("\n  ")
        );
    }
    assert!(
        report.breaks.is_empty(),
        "trusty-memory MCP contract broken (ADR-0066 D1.1, guard #9276):\n  - {}\n\n\
         The 1.x MCP schema changes additively only. To retire a tool, keep it and \
         mark it with the ADR-0066 D3 deprecation notice instead.\n\
         Refreshing the snapshot over a break IS a contract break: it needs a major \
         version and Bob's override (ADR-0066 D6), plus a `Breaking` changelog \
         fragment. Only then refresh with:\n    {REFRESH_CMD}\nand commit {SNAPSHOT}.",
        report.breaks.join("\n  - "),
    );
}

/// Why: a snapshot that reorders between runs would fail on no change.
/// What: two renders are byte-equal, tool order in the payload does not move
/// the render, and render/parse round-trips. Test: this function.
#[test]
fn mcp_schema_snapshot_is_deterministic() {
    let live = tool_definitions();
    let first = render(&snapshot(&live));
    assert_eq!(first, render(&snapshot(&tool_definitions())));
    let mut reversed = live.clone();
    reversed["tools"]
        .as_array_mut()
        .expect("tools array")
        .reverse();
    assert_eq!(first, render(&snapshot(&reversed)));
    assert_eq!(parse(&first), snapshot(&live));
}

/// Why: "a plain run never writes"; a missing snapshot must fail, not appear.
/// What: reading an absent path is an error naming the refresh command, and
/// the file is not created. Test: this function.
#[test]
fn mcp_schema_missing_snapshot_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent.jsonl");
    let err = read_golden(&path).expect_err("a missing snapshot is an error");
    assert!(err.contains(REFRESH_CMD), "{err}");
    assert!(!path.exists());
}

/// A real tool with at least one required and one optional param:
/// `(tool, required param, optional param)`, first by name.
fn target(live: &Value) -> (String, String, String) {
    snapshot(live)
        .into_iter()
        .find_map(|r| {
            let req = r.required.iter().find(|p| r.params.contains_key(*p))?;
            let opt = r.params.keys().find(|p| !r.required.contains(p))?;
            Some((r.name.clone(), req.clone(), opt.clone()))
        })
        .expect("some tool has a required and an optional param")
}

/// Applies `edit` to the tool named `name` in a copy of `live`.
fn edit_tool(live: &Value, name: &str, edit: impl FnOnce(&mut Value)) -> Value {
    let mut out = live.clone();
    let tool = out["tools"]
        .as_array_mut()
        .expect("tools array")
        .iter_mut()
        .find(|t| t["name"] == name)
        .expect("tool exists");
    edit(tool);
    out
}

fn push_required(tool: &mut Value, param: &str) {
    let schema = &mut tool["inputSchema"];
    if !schema["required"].is_array() {
        schema["required"] = json!([]);
    }
    schema["required"]
        .as_array_mut()
        .expect("required array")
        .push(json!(param));
}

/// One mutation of a copy of the real payload.
type Change = Box<dyn Fn(&Value, &str, &str, &str) -> Value>;

/// Why: criterion "a rename, removal or new required param fails" (#9276),
/// proved on a copy of the real payload. What: each forbidden change class
/// yields a break naming the tool and the change. Test: this function.
#[test]
fn mcp_schema_flags_breaking_changes() {
    let live = tool_definitions();
    let golden = snapshot(&live);
    let (tool, req, opt) = target(&live);
    let cases: Vec<(&str, String, Change)> = vec![
        (
            "removed tool",
            format!("tool `{tool}` was removed or renamed"),
            Box::new(|l: &Value, t: &str, _: &str, _: &str| {
                let mut out = l.clone();
                out["tools"]
                    .as_array_mut()
                    .expect("tools array")
                    .retain(|x| x["name"] != t);
                out
            }),
        ),
        (
            "renamed tool",
            format!("tool `{tool}` was removed or renamed"),
            Box::new(|l: &Value, t: &str, _: &str, _: &str| {
                edit_tool(l, t, |x| x["name"] = json!(format!("{t}_v2")))
            }),
        ),
        (
            "removed param",
            format!("tool `{tool}`: param `{opt}` was removed or renamed"),
            Box::new(|l: &Value, t: &str, _: &str, o: &str| {
                edit_tool(l, t, |x| {
                    x["inputSchema"]["properties"]
                        .as_object_mut()
                        .expect("properties")
                        .remove(o);
                })
            }),
        ),
        (
            "renamed required param",
            format!("tool `{tool}`: param `{req}` was removed or renamed"),
            Box::new(|l: &Value, t: &str, r: &str, _: &str| {
                edit_tool(l, t, |x| {
                    let props = x["inputSchema"]["properties"]
                        .as_object_mut()
                        .expect("properties");
                    let schema = props.remove(r).expect("param exists");
                    props.insert(format!("{r}_v2"), schema);
                    for name in x["inputSchema"]["required"]
                        .as_array_mut()
                        .expect("required array")
                    {
                        if name.as_str() == Some(r) {
                            *name = json!(format!("{r}_v2"));
                        }
                    }
                })
            }),
        ),
        (
            "optional param made required",
            format!("tool `{tool}`: param `{opt}` changed from optional to required"),
            Box::new(|l: &Value, t: &str, _: &str, o: &str| {
                edit_tool(l, t, |x| push_required(x, o))
            }),
        ),
        (
            "new required param",
            format!("tool `{tool}`: new param `x_9276` is required"),
            Box::new(|l: &Value, t: &str, _: &str, _: &str| {
                edit_tool(l, t, |x| {
                    x["inputSchema"]["properties"]["x_9276"] = json!({"type": "string"});
                    push_required(x, "x_9276");
                })
            }),
        ),
        (
            "param type changed",
            format!("tool `{tool}`: param `{opt}` changed type"),
            Box::new(|l: &Value, t: &str, _: &str, o: &str| {
                edit_tool(l, t, |x| {
                    let ty = &mut x["inputSchema"]["properties"][o]["type"];
                    *ty = json!(if *ty == "integer" {
                        "string"
                    } else {
                        "integer"
                    });
                })
            }),
        ),
    ];
    for (name, expected, change) in cases {
        let breaks = check(&golden, &change(&live, &tool, &req, &opt)).breaks;
        assert!(
            breaks.iter().any(|b| b.starts_with(&expected)),
            "{name} must be reported as `{expected}…`, got {breaks:?}"
        );
    }
}

/// Why: ADR-0066 D3 allows additive change in 1.x. What: a new tool (even
/// with a required param), a new optional param, a required param made
/// optional, a description edit and reordered tools are not breaks.
/// Test: this function.
#[test]
fn mcp_schema_allows_additive_changes() {
    let live = tool_definitions();
    let golden = snapshot(&live);
    let (tool, req, _) = target(&live);
    let new_tool = json!({
        "name": "zz_new_tool_9276",
        "description": "New.",
        "inputSchema": {
            "type": "object",
            "properties": {"id": {"type": "string"}},
            "required": ["id"]
        }
    });
    let mut with_new_tool = live.clone();
    with_new_tool["tools"]
        .as_array_mut()
        .expect("tools array")
        .push(new_tool);
    let mut reordered = live.clone();
    reordered["tools"]
        .as_array_mut()
        .expect("tools array")
        .reverse();
    let cases: Vec<(&str, Value, bool)> = vec![
        ("unchanged", live.clone(), false),
        ("new tool with a required param", with_new_tool, true),
        (
            "new optional param",
            edit_tool(&live, &tool, |x| {
                x["inputSchema"]["properties"]["y_9276"] = json!({"type": "boolean"});
            }),
            true,
        ),
        (
            "required param made optional",
            edit_tool(&live, &tool, |x| {
                x["inputSchema"]["required"]
                    .as_array_mut()
                    .expect("required array")
                    .retain(|p| p.as_str() != Some(req.as_str()));
            }),
            true,
        ),
        (
            "description edited",
            edit_tool(&live, &tool, |x| x["description"] = json!("Reworded.")),
            false,
        ),
        ("tools reordered", reordered, false),
    ];
    for (name, changed, expect_addition) in cases {
        let report = check(&golden, &changed);
        assert!(
            report.breaks.is_empty(),
            "{name} must not break the contract: {:?}",
            report.breaks
        );
        assert_eq!(
            !report.additions.is_empty(),
            expect_addition,
            "{name}: additions {:?}",
            report.additions
        );
    }
}

/// Why: ADR-0066 D3's MCP deprecation notice has two markers, and a
/// deprecated tool is never removed in 1.x. No live tool is deprecated yet,
/// so a synthetic tool proves the rule. What: a well-formed notice passes and
/// is recorded; a flag without the prefix, a prefix without the flag, a
/// malformed prefix, and removing the deprecated tool each break.
/// Test: this function.
#[test]
fn mcp_schema_deprecation_notice_shape() {
    const OLD: &str = "old_tool_9276";
    let mut base = tool_definitions();
    base["tools"]
        .as_array_mut()
        .expect("tools array")
        .push(json!({
            "name": OLD,
            "description": "DEPRECATED since 1.2; removed in 2.0: use memory_recall. Old.",
            "_meta": {"deprecated": true},
            "inputSchema": {"type": "object", "properties": {}}
        }));
    let golden = snapshot(&base);
    assert!(golden.iter().any(|r| r.name == OLD && r.deprecated));
    let clean = check(&golden, &base);
    assert!(clean.breaks.is_empty(), "{:?}", clean.breaks);

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "flag without the description prefix",
            edit_tool(&base, OLD, |x| x["description"] = json!("Old tool.")),
            "sets `_meta.deprecated` but its description does not open",
        ),
        (
            "prefix without the flag",
            edit_tool(&base, OLD, |x| {
                x.as_object_mut().expect("tool object").remove("_meta");
            }),
            "has a DEPRECATED description but no `_meta.deprecated: true`",
        ),
        (
            "malformed prefix",
            edit_tool(&base, OLD, |x| {
                x["description"] = json!("DEPRECATED: use memory_recall.");
            }),
            "sets `_meta.deprecated` but its description does not open",
        ),
        (
            "deprecated tool removed",
            {
                let mut out = base.clone();
                out["tools"]
                    .as_array_mut()
                    .expect("tools array")
                    .retain(|x| x["name"] != OLD);
                out
            },
            "deprecated tool `old_tool_9276` was removed",
        ),
    ];
    for (name, changed, expected) in cases {
        let breaks = check(&golden, &changed).breaks;
        assert!(
            breaks
                .iter()
                .any(|b| b.contains(OLD) && b.contains(expected)),
            "{name} must be reported as `{expected}`, got {breaks:?}"
        );
    }
}
