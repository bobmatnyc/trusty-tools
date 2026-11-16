//! #11366: the Rust fresh/ready verdict equals `datg.py fresh` / `ready`.
//!
//! Each case is a store directory: the four scrubbed fixture stores, plus
//! variants of `pm-small` that are stale, lack a SESSION block, or cannot be
//! read. Every case asserts its expected code; when `python3` is on PATH the
//! same directory is also run through the bundled `datg.py` and the codes
//! (and `fresh`'s one line) must match.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use trusty_architect::ledger::fresh::{fresh, ready, state_dir};

type R = Result<(), Box<dyn std::error::Error>>;
/// (name, store dir, expected fresh code, expected ready code).
type Case = (String, PathBuf, i32, i32);

/// pm-small's `written_epoch`.
const WRITTEN: i64 = 1_792_573_814;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ledger")
}

fn datg_py() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/trusty-architect/scripts/datg.py")
}

/// `datg.py --state-dir <dir> <verb>`: (exit code, stdout), or `None` without python3.
fn python(dir: &Path, verb: &str) -> Option<(i32, String)> {
    let out = Command::new("python3")
        .arg(datg_py())
        .args(["--project"])
        .arg(dir)
        .arg("--state-dir")
        .arg(dir)
        .arg(verb)
        .output()
        .ok()?;
    Some((
        out.status.code()?,
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    ))
}

/// Copies fixture `store` into `root/<name>` and returns it.
fn copy(root: &Path, store: &str, name: &str) -> std::io::Result<PathBuf> {
    let dir = root.join(name);
    fs::create_dir_all(&dir)?;
    for e in fs::read_dir(fixtures().join(store))? {
        let e = e?;
        fs::copy(e.path(), dir.join(e.file_name()))?;
    }
    Ok(dir)
}

fn edit_state(dir: &Path, f: impl FnOnce(&mut Value)) -> R {
    let p = dir.join("state.json");
    let mut st: Value = serde_json::from_str(&fs::read_to_string(&p)?)?;
    f(&mut st);
    fs::write(p, serde_json::to_string(&st)?)?;
    Ok(())
}

/// Builds every case: (name, dir, expected fresh code, expected ready code).
fn cases(root: &Path) -> Result<Vec<Case>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for (store, f, r) in [
        ("pm-medium", 0, 0),
        ("pm-over-refuse", 1, 1),
        ("pm-over-warn", 0, 0),
        ("pm-small", 0, 0),
    ] {
        out.push((store.to_owned(), fixtures().join(store), f, r));
    }
    let stale = copy(root, "pm-small", "stale")?;
    fs::write(
        stale.join("stamp.json"),
        json!({"changed_epoch": WRITTEN + 30}).to_string(),
    )?;
    out.push(("stale: changed after the write".into(), stale, 1, 1));
    let rewritten = copy(root, "pm-small", "rewritten")?;
    fs::write(
        rewritten.join("stamp.json"),
        json!({"changed_epoch": WRITTEN + 30, "v": 1}).to_string(),
    )?;
    out.push(("a write after the stamp's v".into(), rewritten, 0, 0));
    let no_session = copy(root, "pm-small", "no-session")?;
    edit_state(&no_session, |st| {
        st.as_object_mut().map(|o| o.remove("session"));
    })?;
    out.push(("missing SESSION block".into(), no_session, 1, 1));
    let minute = copy(root, "pm-small", "minute")?;
    edit_state(&minute, |st| {
        st["session"]
            .as_object_mut()
            .map(|o| o.remove("written_epoch"));
    })?;
    // 09:10:40Z floors to 09:10, the write's own minute: still fresh.
    fs::write(
        minute.join("stamp.json"),
        json!({"changed_epoch": 1_792_573_840}).to_string(),
    )?;
    out.push(("minute precision, same minute".into(), minute, 0, 0));
    let garbage = copy(root, "pm-small", "garbage")?;
    fs::write(garbage.join("state.json"), "{not json")?;
    out.push(("unreadable state.json".into(), garbage, 2, 2));
    let shape = copy(root, "pm-small", "shape")?;
    edit_state(&shape, |st| st["v"] = json!("7"))?;
    out.push(("bad shape".into(), shape, 2, 2));
    let missing = root.join("missing");
    fs::create_dir_all(&missing)?;
    out.push(("missing state.json".into(), missing, 2, 2));
    Ok(out)
}

#[test]
fn rust_verdict_equals_datg_py_on_every_case() -> R {
    let tmp = tempfile::tempdir()?;
    let mut ran_python = false;
    for (name, dir, want_fresh, want_ready) in cases(tmp.path())? {
        let f = fresh(&dir, None, None, 0.0);
        let r = ready(&dir, None, None, 0.0);
        assert_eq!(f.code, want_fresh, "{name}: fresh {f:?}");
        assert_eq!(r.code, want_ready, "{name}: ready {r:?}");
        if let Some((code, line)) = python(&dir, "fresh") {
            ran_python = true;
            assert_eq!(code, f.code, "{name}: fresh vs datg.py");
            // A code-2 line carries the JSON parser's own error text.
            if code != 2 {
                assert_eq!(line, f.reasons[0], "{name}: fresh line vs datg.py");
            }
            let (pcode, _) = python(&dir, "ready").ok_or("python ready")?;
            assert_eq!(pcode, r.code, "{name}: ready vs datg.py");
        }
    }
    if !ran_python {
        eprintln!("python3 not found: expected codes checked, datg.py parity not run");
    }
    Ok(())
}

#[test]
fn a_stale_write_and_a_read_error_are_never_ok() -> R {
    let tmp = tempfile::tempdir()?;
    for (name, dir, want, _) in cases(tmp.path())? {
        assert_eq!(fresh(&dir, None, None, 0.0).ok(), want == 0, "{name}");
    }
    let old = fresh(
        &fixtures().join("pm-small"),
        None,
        Some(10.0),
        (WRITTEN + 601) as f64,
    );
    assert_eq!(old.code, 1, "{old:?}");
    assert!(
        old.reasons[0].starts_with("stale: write older than 10 min"),
        "{old:?}"
    );
    Ok(())
}

#[test]
fn state_dir_picks_the_named_session_only_when_it_holds_a_state() -> R {
    let tmp = tempfile::tempdir()?;
    let base = tmp.path().join(".trusty-mpm/datg");
    assert_eq!(state_dir(tmp.path(), Some("tm-architect")), base);
    fs::create_dir_all(base.join("tm-architect"))?;
    fs::write(base.join("tm-architect/state.json"), "{}")?;
    assert_eq!(
        state_dir(tmp.path(), Some("tm-architect")),
        base.join("tm-architect")
    );
    assert_eq!(state_dir(tmp.path(), None), base);
    Ok(())
}
