//! The `session` op: merge, caps, vague expectations, refs and the size limit.
//!
//! Why: the session record is what a PM reads first on resume, so its caps are
//! enforced at write time (rows, field lengths, 2048 bytes) and a refused op writes
//! nothing. The messages are those of `datg.py`, checked against it on the same inputs.
//! What: one test per rule group, through the CLI flags and through `batch`.
//! Test: `session_set_merges_and_validates`, `session_caps_and_vague_owed`,
//! `refs_merge_replace_and_remove`, `session_record_size_limit`.

mod il_support;

use il_support::*;
use trusty_architect::il::{Env, Json};

const NOW: i64 = 1_795_900_000;

struct Rig {
    _tmp: tempfile::TempDir,
    project: String,
    env: Env,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let env = Env::bare(tmp.path());
    Rig {
        project: project.to_str().unwrap().to_owned(),
        _tmp: tmp,
        env,
    }
}

impl Rig {
    fn datg(&self, args: &[&str], stdin: &str) -> trusty_architect::il::Out {
        let mut full = vec!["datg", "--project", self.project.as_str()];
        full.extend(args);
        run_at(&full, stdin, &self.env, NOW)
    }

    fn session(&self) -> Json {
        let text = read(&datg_dir(std::path::Path::new(&self.project)), "state.json");
        Json::parse(&text).unwrap().get("session").unwrap().clone()
    }
}

#[test]
fn session_set_merges_and_validates() {
    let r = rig();
    assert_eq!(r.datg(&["session", "set", "--task", "A task"], "").code, 0);
    assert_eq!(
        r.datg(&["session", "set", "--next", "next one"], "").code,
        0
    );
    let s = r.session();
    assert_eq!(
        (s.s("task"), s.s("next")),
        ("A task", "next one"),
        "omitted fields stay"
    );
    assert_eq!(r.datg(&["session", "set", "--owe", "none"], "").code, 0);
    assert_eq!(r.session().get("owed_none"), Some(&Json::Bool(true)));
    let owe = r.datg(&["session", "set", "--owe", "ci=green@run 9"], "");
    assert_eq!(owe.code, 0, "{}", owe.stderr);
    let s = r.session();
    assert_eq!(s.get("owed_none"), Some(&Json::Bool(false)));
    assert_eq!(
        s.get("owed").unwrap().dump(),
        r#"[{"check":"ci","expect":"green","ref":"run 9"}]"#
    );
    let bad = r.datg(&["session", "set", "--ctx-use", "101:0:5:k"], "");
    assert_eq!(
        (bad.code, bad.stderr.as_str()),
        (2, "datg: ctx_use.active must be an integer 0-100\n")
    );
    let words = r.datg(&["session", "set", "--ctx-use", "a:b:c:d"], "");
    assert_eq!(
        words.stderr,
        "datg: ctx-use: ACTIVE, STALE, CTX must be integers\n"
    );
    let short = r.datg(&["session", "set", "--wait", "nocolon"], "");
    assert_eq!(short.stderr, "datg: expected WHO:WHAT, got 'nocolon'\n");
    let empty = r.datg(&["session", "set", "--task", "  "], "");
    assert_eq!(
        empty.stderr,
        "datg: task must be one line, <= 160 chars, non-empty\n"
    );
}

#[test]
fn session_caps_and_vague_owed() {
    let r = rig();
    let rows = |n: usize, key: &str, row: &str| {
        let list: Vec<String> = (0..n).map(|_| row.to_owned()).collect();
        format!("[{{\"op\":\"session\",\"{key}\":[{}]}}]", list.join(","))
    };
    let six = r.datg(
        &["batch"],
        &rows(6, "waiting_on", r#"{"who":"owner","what":"x"}"#),
    );
    assert_eq!(
        six.stderr,
        "datg: waiting_on: 6 rows is over the cap of 5 (max 5); drop or merge rows\n"
    );
    let nine = r.datg(
        &["batch"],
        &rows(9, "owed", r#"{"check":"c","expect":"1","ref":"r"}"#),
    );
    assert!(
        nine.stderr.contains("owed: 9 rows is over the cap of 8"),
        "{}",
        nine.stderr
    );
    let vague = r.datg(
        &["batch"],
        &rows(1, "owed", r#"{"check":"c","expect":"TBD","ref":"r"}"#),
    );
    assert_eq!(
        vague.stderr,
        "datg: owed.expect must be an exact value (SHA, run id, count), not 'TBD'\n"
    );
    let not_list = r.datg(&["batch"], r#"[{"op":"session","agents":"x"}]"#);
    assert_eq!(
        not_list.stderr,
        "datg: agents: a list of at most 6 objects\n"
    );
    let long = r.datg(
        &[
            "session",
            "set",
            "--agent",
            &format!("{}:g:w", "i".repeat(41)),
        ],
        "",
    );
    assert_eq!(
        long.stderr,
        "datg: agents.id must be one line, <= 40 chars, non-empty\n"
    );
}

#[test]
fn refs_merge_replace_and_remove() {
    let r = rig();
    let refs = || r.session().get("refs").map(Json::dump).unwrap_or_default();
    // The fixture already carries a `spec` ref; `none` clears it.
    assert_eq!(r.datg(&["session", "set", "--ref", "none"], "").code, 0);
    assert_eq!(
        r.datg(&["session", "set", "--ref", "a=one", "--ref", "b=two"], "")
            .code,
        0
    );
    assert_eq!(
        refs(),
        r#"[{"key":"a","value":"one"},{"key":"b","value":"two"}]"#
    );
    assert_eq!(r.datg(&["session", "set", "--ref", "a=uno"], "").code, 0);
    assert_eq!(
        refs(),
        r#"[{"key":"a","value":"uno"},{"key":"b","value":"two"}]"#
    );
    assert_eq!(r.datg(&["session", "set", "--ref", "b=none"], "").code, 0);
    assert_eq!(refs(), r#"[{"key":"a","value":"uno"}]"#);
    let key = r.datg(&["session", "set", "--ref", "Bad Key=x"], "");
    assert_eq!(
        key.stderr,
        "datg: refs.key must match ^[a-z0-9][a-z0-9_.-]{0,31}$\n"
    );
    assert_eq!(r.datg(&["session", "set", "--ref", "none"], "").code, 0);
    assert_eq!(refs(), "[]", "`none` clears every ref");
    let many: Vec<String> = (0..13).map(|n| format!("--ref=k{n}=v")).collect();
    let mut args = vec!["session", "set"];
    args.extend(many.iter().map(String::as_str));
    let over = r.datg(&args, "");
    assert!(
        over.stderr.contains("refs: 13 rows is over the cap of 12"),
        "{}",
        over.stderr
    );
}

#[test]
fn session_record_size_limit() {
    let r = rig();
    let batch = format!(
        "[{{\"op\":\"session\",\"task\":\"{}\",\"next\":\"{}\",\"agents\":[{}],\"crons\":[{}]}}]",
        "t".repeat(160),
        "n".repeat(200),
        (0..6)
            .map(|k| format!(
                "{{\"id\":\"a{k}{}\",\"goal\":\"{}\",\"what\":\"{}\"}}",
                "i".repeat(30),
                "g".repeat(40),
                "w".repeat(80)
            ))
            .collect::<Vec<_>>()
            .join(","),
        (0..4)
            .map(|k| format!(
                "{{\"id\":\"c{k}\",\"schedule\":\"{}\",\"prompt\":\"{}\"}}",
                "s".repeat(40),
                "p".repeat(200)
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let before = read(&datg_dir(std::path::Path::new(&r.project)), "state.json");
    let out = r.datg(&["batch"], &batch);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.starts_with("datg: session record would be ")
            && out.stderr.ends_with(" B (> 2048 B limit)\n"),
        "{}",
        out.stderr
    );
    assert_eq!(
        read(&datg_dir(std::path::Path::new(&r.project)), "state.json"),
        before
    );
}
