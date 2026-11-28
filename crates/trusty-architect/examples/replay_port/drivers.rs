//! The `replay_port` example's driver mirrors kept outside the library (#12029 P2h split).
//!
//! Why: `examples/replay_port.rs` reached the line cap; these mirrors of the
//! recording drivers `tests/fixtures/replay/<script>/driver.py` move here
//! unchanged.
//! What: `technical_level`, `architect_config`, `il_timeline`, `ids36_driver`
//! and `dashboard_schema`, and the `repr` helpers they share.
//! Test: `technical_level_recordings_replay_clean_against_the_port`,
//! `architect_config_recordings_replay_clean_against_the_port`,
//! `ids36_recordings_replay_clean_against_the_port`,
//! `dashboard_schema_recordings_replay_clean_against_the_port`.

use std::path::{Path, PathBuf};

use trusty_architect::ids36;
use trusty_architect::spec_drift::Outcome;
use trusty_architect::tasks::timeline::Timeline;
use trusty_architect::{architect_config as ac, technical_level as tl};

pub(crate) fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Python's `repr()` of a `str` whose characters are printable or C0
/// controls (the recorded values).
fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let body: String = s
        .chars()
        .map(|c| match c {
            '\\' => "\\\\".to_owned(),
            // See #12029: an ids36 recording passes `0f\n`.
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            c if c == q => format!("\\{c}"),
            c if c < ' ' || c == '\u{7f}' => format!("\\x{:02x}", u32::from(c)),
            c => c.to_string(),
        })
        .collect();
    format!("{q}{body}{q}")
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// `driver.py` of `tests/fixtures/replay/technical_level/`.
pub(crate) fn technical_level(args: &[String]) -> Outcome {
    let mut o = String::new();
    match args.split_first() {
        Some((cmd, [])) if cmd == "table" => {
            o += &format!("LEVELS {}\nDEFAULT {}\n", tl::LEVELS.join(" "), tl::DEFAULT);
            for level in tl::LEVELS {
                o += &format!("{}\n", tl::line(level));
            }
        }
        Some((cmd, rest)) if cmd == "value" && !rest.is_empty() => {
            for v in rest {
                o += &format!(
                    "{} normalize={} below_engineer={}\n{}\n",
                    repr(v),
                    tl::normalize(v),
                    py_bool(tl::below_engineer(v)),
                    tl::line(v)
                );
            }
        }
        Some((cmd, [root])) if cmd == "read" => {
            let format = std::env::var("DASHBOARD_FORMAT").ok();
            match tl::store_format(format.as_deref()).and_then(|f| tl::read(Path::new(root), f)) {
                Ok(level) => o += &format!("read {level}\n{}\n", tl::line(level)),
                Err(_) => return out(o, "read raised StoreError\n".into(), 1),
            }
        }
        _ => {
            let usage = "usage: driver.py table | value V... | read ROOT\n";
            return out(o, usage.into(), 2);
        }
    }
    out(o, String::new(), 0)
}

/// `driver.py` of `tests/fixtures/replay/architect_config/`.
pub(crate) fn architect_config(args: &[String]) -> Outcome {
    let [root] = args else {
        return out(String::new(), "usage: driver.py ROOT\n".into(), 2);
    };
    let env = |k: &str| std::env::var(k).ok();
    let root = Path::new(root);
    let mut o = format!("default_path: {}\n", ac::default_path(root, &env));
    match ac::default_claude_config_dir(&env) {
        Ok(d) => o += &format!("default_claude_config_dir: {d}\n"),
        Err(e) => return out(o, format!("{e}\n"), 1),
    }
    let c = match ac::load(root, &env) {
        Ok(c) => c,
        Err(e) => return out(o, format!("ConfigError: {e}\n"), 1),
    };
    o += &format!(
        "path: {}\nowner_name: {}\nmemory_palace: {}\ndashboard_port: {}\ntailnet_url: {}\n",
        c.path, c.owner_name, c.memory_palace, c.dashboard_port, c.tailnet_url
    );
    for (name, r) in &c.repos {
        o += &format!(
            "repo {name}: slug={} gh_config_dir={} checkout={}\n",
            r.slug, r.gh_config_dir, r.checkout
        );
    }
    for (name, v) in &c.caps {
        o += &format!("cap {name}: {}\n", v.repr());
    }
    o += &format!(
        "unknown: {}\nprojects_root: {}\nclaude_config_dir: {}\npoll_interval_s: {}\n",
        c.unknown.join(" "),
        c.projects_root,
        c.claude_config_dir,
        c.poll_interval_s
    );
    out(o, String::new(), 0)
}

/// Python's `repr()` of a float (the recorded epochs and keys).
fn frepr(x: f64) -> String {
    let s = format!("{x:?}");
    if s.contains('e') || s.contains('.') || s.contains("inf") || s.contains("NaN") {
        return s;
    }
    format!("{s}.0")
}

/// `driver.py` of `tests/fixtures/replay/il_timeline/` (#12028 P1d).
pub(crate) fn il_timeline(args: &[String]) -> Outcome {
    let cut = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let pairs: Vec<Option<(&str, &str)>> = args[..cut].iter().map(|a| a.split_once('=')).collect();
    if pairs.is_empty() || pairs.iter().any(Option::is_none) {
        let usage = "usage: driver.py PROJECT=DIR... [-- EPOCH...]\n";
        return out(String::new(), usage.into(), 2);
    }
    let dirs: std::collections::BTreeMap<String, PathBuf> = pairs
        .into_iter()
        .flatten()
        .map(|(p, d)| (p.to_owned(), PathBuf::from(d)))
        .collect();
    let epochs: Vec<f64> = args
        .get(cut + 1..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    let tl = match Timeline::load(&dirs) {
        Ok(tl) => tl,
        Err(class) => return out(String::new(), format!("load raised {class}\n"), 1),
    };
    let key = |k: Option<(&str, f64)>| {
        k.map_or_else(|| "None".to_owned(), |(i, t)| format!("{i} {}", frepr(t)))
    };
    let mut o = String::new();
    for inc in tl.incarnations() {
        let close = inc.close.map_or_else(|| "None".to_owned(), frepr);
        o += &format!(
            "inc {} close={close} title={}\n",
            key(Some((&inc.id, inc.open))),
            inc.title_repr()
        );
    }
    for (id, t) in tl.newest() {
        o += &format!("newest {id} {}\n", key(Some((id, t))));
    }
    for p in dirs.keys().map(String::as_str).chain(["-"]) {
        for e in &epochs {
            o += &format!("at {p} {} {}\n", frepr(*e), key(tl.at(p, *e)));
        }
    }
    out(o, String::new(), 0)
}

/// `driver.py` of `tests/fixtures/replay/ids36/` (#12029 P2a).
pub(crate) fn ids36_driver(args: &[String]) -> Outcome {
    let usage = "usage: driver.py cli ARGS... | table | enc N... | dec S... | next S...\n";
    let Some((cmd, rest)) = args.split_first() else {
        return out(String::new(), usage.into(), 2);
    };
    let mut o = String::new();
    match cmd.as_str() {
        "cli" => return ids36::run(rest),
        "table" if rest.is_empty() => {
            o += &format!(
                "DIGITS {}\nBASE {}\nSPACE {}\nDIGIT_RE {}\n",
                ids36::DIGITS,
                ids36::BASE,
                ids36::SPACE,
                ids36::DIGIT_RE
            );
        }
        "enc" | "dec" | "next" if !rest.is_empty() => {
            for a in rest {
                let text = |e: ids36::Ids36Error| e.to_string();
                let r = match cmd.as_str() {
                    // `int(a)` of a plain decimal; the recordings pass no other.
                    "enc" => a
                        .parse::<i64>()
                        .map_err(|_| format!("invalid literal for int() with base 10: {}", repr(a)))
                        .and_then(|n| ids36::enc(n).map_err(text))
                        .map(|d| repr(&d)),
                    "dec" => ids36::dec(a).map(|n| n.to_string()).map_err(text),
                    _ => ids36::next_digits(a).map(|d| repr(&d)).map_err(text),
                };
                o += &match r {
                    Ok(v) => format!("{cmd} {} -> {v}\n", repr(a)),
                    Err(e) => format!("{cmd} {} ValueError: {e}\n", repr(a)),
                };
            }
        }
        _ => return out(String::new(), usage.into(), 2),
    }
    out(o, String::new(), 0)
}

/// `driver.py` of `tests/fixtures/replay/dashboard_schema/` (#12029 P2a).
pub(crate) fn dashboard_schema(args: &[String]) -> Outcome {
    use serde_json::Value;
    use trusty_architect::dashboard::schema_check::{SchemaMode, schema_name, script};
    use trusty_architect::dashboard::{Doc, Snapshot};
    let [cases] = args else {
        return out(String::new(), "usage: driver.py CASES.json\n".into(), 2);
    };
    let parsed = std::fs::read_to_string(cases)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok());
    let Some(cases) = parsed else {
        return out(String::new(), "driver: cases unreadable\n".into(), 1);
    };
    let ok = |errs: Vec<String>| {
        if errs.is_empty() {
            vec!["ok".into()]
        } else {
            errs
        }
    };
    let mut o = String::new();
    for (i, case) in cases.iter().enumerate() {
        let s = |k: &str| case.get(k).and_then(Value::as_str).unwrap_or_default();
        let dir = Path::new(case.get("dir").and_then(Value::as_str).unwrap_or("schemas"));
        o += &format!("case {i} {}\n", s("op"));
        let lines = match s("op") {
            "name" => Ok(vec![
                schema_name(s("coll"), s("id")).unwrap_or_else(|| "None".into()),
            ]),
            "validate" => script::validate(dir, s("coll"), s("id"), &case["data"]).map(ok),
            "store" => {
                let mut snap = Snapshot::new();
                for (coll, docs) in case["docs"].as_object().into_iter().flatten() {
                    let list = docs.as_array().into_iter().flatten().map(|d| Doc {
                        id: d["id"].as_str().unwrap_or_default().to_owned(),
                        version: 1,
                        data: d["data"].as_object().cloned().unwrap_or_default(),
                    });
                    snap.insert(coll.clone(), list.collect());
                }
                script::validate_store(dir, &snap).map(ok)
            }
            "mode" => {
                let env = |k: &str| {
                    case["env"]
                        .get(k)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
                script::mode(&env).map(|m| {
                    let m = match m {
                        None => "None",
                        Some(SchemaMode::Off) => "off",
                        Some(SchemaMode::Warn) => "warn",
                        Some(SchemaMode::Enforce) => "enforce",
                    };
                    vec![m.to_owned()]
                })
            }
            "missing" => {
                let miss: Vec<String> = script::architect_do_missing(&case["data"])
                    .iter()
                    .map(|m| repr(m))
                    .collect();
                Ok(vec![format!("[{}]", miss.join(", "))])
            }
            other => return out(o, format!("KeyError: {}\n", repr(other)), 1),
        };
        for line in lines.unwrap_or_else(|e| vec![format!("SchemaError: {e}")]) {
            o += &format!("  {line}\n");
        }
    }
    out(o, String::new(), 0)
}
