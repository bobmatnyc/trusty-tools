//! Validator unit tests; the expected lines are the prototype's own output
//! (`dashboard_schema.check_schema` on the same schema and values).

use serde_json::json;

use super::walk::{Patterns, check, check_keywords};
use super::*;

fn schema() -> Value {
    json!({
        "$defs": {"ts": {"type": "string", "pattern": "^[0-9]{2}$"}},
        "type": "object", "required": ["a", "b"], "additionalProperties": false,
        "properties": {
            "a": {"type": "integer", "minimum": 1, "maximum": 5},
            "e": {"enum": ["x", "y", 1]}, "c": {"const": "k"},
            "s": {"type": "string", "minLength": 3}, "t": {"$ref": "#/$defs/ts"},
            "l": {"type": "array", "items": {"type": ["string", "null"]}},
            "o": {"oneOf": [{"type": "string"}, {"type": "integer"}, {"type": "number"}]},
            "f": {"type": "number", "maximum": 1.5},
            "m": {"type": "object", "additionalProperties": {"type": "boolean"}}
        }
    })
}

fn run(value: &Value) -> Result<Vec<String>, DashError> {
    let schema = schema();
    let mut patterns = Patterns::new();
    check_keywords(&schema, "t", &mut patterns)?;
    check(&schema, value, &patterns)
}

#[test]
fn walk_reports_each_keyword_like_the_prototype() -> Result<(), DashError> {
    let many = "é".repeat(70);
    let value = json!({"a": true, "e": 1.0, "c": "z", "s": "ab", "t": "123",
        "l": ["x", null, 3, true, {"q": 1}], "o": 2, "f": 2.25, "m": {"k": true, "j": "no"},
        "z": 1, "zz": many});
    assert_eq!(
        run(&value)?,
        [
            "/b: required property is missing",
            "/a: expected integer, got boolean",
            "/c: must be \"k\"",
            "/e: 1.0 is not one of [\"x\", \"y\", 1]",
            "/f: 2.25 is above 1.5",
            "/l/2: expected string|null, got int",
            "/l/3: expected string|null, got boolean",
            "/l/4: expected string|null, got dict",
            "/m/j: expected boolean, got str",
            "/o: matches 2 of the 3 allowed shapes (need exactly 1)",
            "/s: shorter than 3",
            "/t: '123' does not match ^[0-9]{2}$",
            "/z: property is not allowed",
            "/zz: property is not allowed",
        ]
    );
    let value = json!({"a": 0, "e": "é".repeat(70), "s": "abc", "t": "12"});
    let shown = format!("\"{}", "é".repeat(59));
    assert_eq!(
        run(&value)?,
        [
            "/b: required property is missing".to_owned(),
            "/a: 0 is below 1".to_owned(),
            format!("/e: {shown} is not one of [\"x\", \"y\", 1]"),
        ]
    );
    Ok(())
}

#[test]
fn walk_refuses_unknown_keywords_and_refs() {
    let mut patterns = Patterns::new();
    let bad = json!({"properties": {"x": {"type": "string", "format": "date"}}});
    let err = check_keywords(&bad, "items", &mut patterns).map_err(|e| e.to_string());
    assert_eq!(
        err,
        Err("items/properties/x: unsupported keyword 'format'".to_owned())
    );
    let bad_pattern = json!({"oneOf": [{"pattern": "\\d+"}]});
    assert!(check_keywords(&bad_pattern, "x", &mut patterns).is_err());
    let dangling = json!({"$ref": "#/$defs/nope"});
    assert!(matches!(
        check(&dangling, &json!(1), &patterns),
        Err(DashError::Schema(_))
    ));
}

#[test]
fn goal_invariants_are_checked() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    fs::write(
        tmp.path().join("goals.schema.json"),
        "{\"type\": \"object\"}",
    )?;
    let set = SchemaDir::load(tmp.path())?;
    let data = json!({"lifecycle": "parked", "kind": "factory"});
    let errs = set.validate("goals", "G01", data.as_object().ok_or("object")?)?;
    assert_eq!(
        errs,
        [
            "goals/G01: /lifecycle: 'parked' needs a parked object",
            "goals/G01: /board: required when kind is 'factory'",
        ]
    );
    let active = json!({"lifecycle": "active", "parked": {}});
    assert_eq!(
        set.validate("goals", "G02", active.as_object().ok_or("object")?)?
            .len(),
        1
    );
    let none = set.validate("meta", "other", &Map::new())?;
    assert_eq!(
        none,
        ["meta/other: no schema for this collection or meta id"]
    );
    Ok(())
}

#[test]
fn media_is_a_known_collection() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    fs::write(
        tmp.path().join("media.schema.json"),
        "{\"type\": \"object\", \"required\": [\"project\"]}",
    )?;
    let set = SchemaDir::load(tmp.path())?;
    assert_eq!(schema_name("media", "B01").as_deref(), Some("media"));
    assert_eq!(
        set.validate("media", "B01", &Map::new())?,
        ["media/B01: /project: required property is missing"]
    );
    Ok(())
}

/// #10697: the code points Python's `re` `\s` matches on `str`
/// (`str.isspace()`), as one class body; the schemas spell `\s` with it.
macro_rules! py_space {
    () => {
        "\t\n\u{b}\u{c}\r\u{1c}-\u{1f} \u{85}\u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}"
    };
}

/// The distinct `pattern`s of the prototype's `schemas/dashboard/*.schema.json`.
/// #10697: the first 11 match no run of `0`s; `MB`, `T` and the sha256 were unlisted.
const SHIPPED: [&str; 16] = [
    "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]{3})?Z$",
    "^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]{3})?Z)?$",
    "^[0-9]{4}-[0-9]{2}-[0-9]{2}(T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]{3})?Z)?$",
    "^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])(T([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9])?Z)?$",
    "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
    "^[0-9]{2}:[0-9]{2}$",
    "^G[0-9a-f]{2}$",
    "^S[0-9a-f]{2}$",
    "^MB[0-9a-fA-F]{4,}$",
    "^T[0-9a-f]+$",
    "^[0-9a-f]{64}$",
    "^[^/ ,]+/[^/ ,]+(, [^/ ,]+/[^/ ,]+)*$",
    "^[^/]+/docs/briefs/.+[.]md$",
    "^docs/briefs/[^/].*[.]md$",
    "^https?://",
    concat!("^[^/", py_space!(), "]+/[^", py_space!(), "]+[.]md$"),
];

/// #10697: the status-brief pattern, `^[^/\s]+/\S+\.md$` with `\s` spelled
/// out, loads and refuses exactly Python's whitespace; the old form names `\s`.
#[test]
fn status_brief_pattern_matches_python_whitespace() -> Result<(), String> {
    let brief = Pattern::compile(SHIPPED[15])?;
    assert!(brief.search("cto/docs/briefs/2026-10-30-x.md") && brief.search("p/a/b.md\n"));
    assert!(!brief.search("no-slash.txt") && !brief.search("/a.md") && !brief.search("p/.md"));
    let py_space = [
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680, 0x2000,
        0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028,
        0x2029, 0x202f, 0x205f, 0x3000,
    ];
    for cp in 0..=0x3000_u32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let want = !py_space.contains(&cp) && c != '/';
        assert_eq!(
            brief.search(&format!("{c}/a.md")),
            want,
            "U+{cp:04X} before /"
        );
        assert_eq!(
            brief.search(&format!("p/a{c}b.md")),
            !py_space.contains(&cp),
            "U+{cp:04X} after /"
        );
    }
    assert!(brief.search("p/a\u{feff}\u{200b}b.md"));
    let old = Pattern::compile("^[^/\\s]+/\\S+\\.md$")
        .err()
        .unwrap_or_default();
    assert!(old.contains("escape class \\s is not supported"), "{old}");
    Ok(())
}

/// Every `pattern` value under `v`.
fn patterns_in(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => o.iter().for_each(|(k, x)| match (k.as_str(), x) {
            ("pattern", Value::String(s)) => out.push(s.clone()),
            _ => patterns_in(x, out),
        }),
        Value::Array(a) => a.iter().for_each(|x| patterns_in(x, out)),
        _ => {}
    }
}

/// #10063 df-G01: a 1 MiB value against every shipped pattern on a 256 KiB
/// stack; the backtracking matcher took one frame per repeated character.
#[test]
fn shipped_patterns_match_a_megabyte_on_a_small_stack() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(proto) = std::env::var_os("DASHBOARD_PROTO_DIR") {
        let mut found = Vec::new();
        let dir = std::path::Path::new(&proto).join("schemas/dashboard");
        for entry in fs::read_dir(dir)? {
            let text = fs::read_to_string(entry?.path())?;
            patterns_in(&serde_json::from_str(&text)?, &mut found);
        }
        for p in found {
            assert!(SHIPPED.contains(&p.as_str()), "unlisted pattern {p:?}");
        }
    }
    let run = || -> Result<(), String> {
        let mb = 1 << 20;
        let x = "x".repeat(mb);
        let digits = "0".repeat(mb);
        let repos = format!("a/b{}", ", c/d".repeat(mb / 5));
        let brief = format!("{x}/docs/briefs/{x}.md");
        let rel = format!("docs/briefs/{x}.md");
        let url = format!("https://{x}");
        for (source, text, want) in SHIPPED[..11].iter().map(|p| (*p, &digits, false)).chain([
            (SHIPPED[11], &repos, true),
            (SHIPPED[12], &brief, true),
            (SHIPPED[13], &rel, true),
            (SHIPPED[14], &url, true),
            (SHIPPED[12], &x, false),
            (SHIPPED[15], &brief, true),
            (SHIPPED[15], &x, false),
        ]) {
            let got = Pattern::compile(source)?.search(text);
            if got != want {
                return Err(format!("{source}: got {got}, want {want}"));
            }
        }
        Ok(())
    };
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(run)?
        .join()
        .map_err(|_| "the matcher thread panicked")??;
    Ok(())
}
