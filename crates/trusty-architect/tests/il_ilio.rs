//! `ilio` beyond the golden steps: more `check` blocks, D/A items, collapse refusals.
//!
//! Why: `tests/il_golden.rs` replays one scripted session. These tests cover the
//! block grammar on cases the script does not reach (the table is the Python's own
//! output, `tests/fixtures/il/generate_checks.py`) and the arms that exist only in the
//! port: the dashboard boundary of `apply`, a missing `ILIO_PROJECT`, and the
//! collapse invariant.
//! What: table-driven `check`, `apply` with and without `--no-dashboard`, collapse
//! against a `last-apply.json` newer than the session record, and a dry run.
//! Test: `check_matches_python_on_more_blocks`, `apply_new_decision_and_action_need_the_dashboard_flag`,
//! `collapse_refuses_a_stale_session_record`, `collapse_dry_run_writes_no_epoch`,
//! `ilio_needs_a_project`.

mod il_support;

use std::fs;
use std::path::PathBuf;

use il_support::*;
use trusty_architect::il::{Env, Json};

const NOW: i64 = 1_795_900_000;

struct Rig {
    _tmp: tempfile::TempDir,
    project: PathBuf,
    ilio: PathBuf,
    env: Env,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let ilio = tmp.path().join("ilio");
    fs::create_dir_all(&ilio).unwrap();
    fs::copy(fixture("ilio/r-library.json"), ilio.join("r-library.json")).unwrap();
    let env = Env::bare(tmp.path())
        .with("ILIO_PROJECT", project.to_str().unwrap())
        .with("ILIO_DIR", ilio.to_str().unwrap())
        .with("ILIO_WHO", "tm-architect");
    Rig {
        _tmp: tmp,
        project,
        ilio,
        env,
    }
}

#[test]
fn check_matches_python_on_more_blocks() {
    let r = rig();
    let cases: serde_json::Value =
        serde_json::from_str(&read(&fixture("."), "check_cases.json")).unwrap();
    for (name, v) in cases.as_object().unwrap() {
        let (text, stdout, code) = (
            v[0].as_str().unwrap(),
            v[1].as_str().unwrap(),
            v[2].as_i64().unwrap(),
        );
        let out = run_at(&["ilio", "check"], text, &r.env, NOW);
        assert_eq!(
            (out.stdout.as_str(), i64::from(out.code)),
            (stdout, code),
            "{name}"
        );
    }
}

#[test]
fn apply_new_decision_and_action_need_the_dashboard_flag() {
    let r = rig();
    let dir = datg_dir(&r.project);
    let detail = "This is the first sentence of the plain context. Here is the second sentence with more words in it. \
                  And a third sentence closes the paragraph so the rule is met.";
    let block = format!(
        "```il\ntask: fx-T01\ndone: asked\npct: 20\nnew: D \"Which way now?\" detail=\"{detail}\" opts \"Fast :: ship it this week with the shortcut\"*|\"Slow :: do it properly next month\"\nnew: A \"Bob: approve it\" opts yes*|no\nnext: wait\n```\n"
    );
    let before = fs::read(dir.join("state.json")).unwrap();
    let refused = run_at(&["ilio", "apply"], &block, &r.env, NOW);
    assert_eq!(refused.code, 2);
    assert!(
        refused.stderr.contains("needs a dashboard item first")
            && refused.stderr.starts_with("ilio: "),
        "{}",
        refused.stderr
    );
    assert_eq!(
        fs::read(dir.join("state.json")).unwrap(),
        before,
        "refused before any write"
    );
    let ok = run_at(&["ilio", "apply", "--no-dashboard"], &block, &r.env, NOW);
    assert_eq!(
        (ok.code, ok.stdout.as_str()),
        (0, "wrote 4 op(s)\n"),
        "{}",
        ok.stderr
    );
    let st = Json::parse(&read(&dir, "state.json")).unwrap();
    let d = st
        .arr("items")
        .iter()
        .find(|i| i.s("title") == "Which way now?")
        .unwrap();
    assert_eq!(
        d.get("opts").unwrap().dump(),
        r#"[{"l":"Fast","desc":"ship it this week with the shortcut","rec":true},{"l":"Slow","desc":"do it properly next month","rec":false}]"#
    );
    assert!(
        d.get("dash").is_none(),
        "no dashboard link without the dashboard store"
    );
    let a = st
        .arr("items")
        .iter()
        .find(|i| i.s("title") == "Bob: approve it")
        .unwrap();
    assert_eq!(
        a.get("opts").unwrap().dump(),
        r#"[{"l":"yes","rec":true},{"l":"no","rec":false}]"#
    );
    let again = run_at(
        &["ilio", "apply", "--no-dashboard"],
        &block.replace("pct: 20", "pct: 25"),
        &r.env,
        NOW + 60,
    );
    assert_eq!(
        again.stdout, "wrote 2 op(s)\n",
        "the same new lines add nothing the second time: {}",
        again.stderr
    );
}

#[test]
fn collapse_refuses_a_stale_session_record() {
    let r = rig();
    fs::write(r.ilio.join("last-apply.json"), "{\"epoch\": 9999999999}").unwrap();
    let dir = datg_dir(&r.project);
    let before = fs::read(dir.join("state.json")).unwrap();
    let out = run_at(&["ilio", "collapse", "--dry-run"], "", &r.env, NOW);
    assert_eq!(out.code, 1);
    assert!(
        out.stdout
            .starts_with("INVARIANT FAILED: session record older than the last applied turn"),
        "{}",
        out.stdout
    );
    assert_eq!(fs::read(dir.join("state.json")).unwrap(), before);
    assert!(!r.ilio.join("epoch.json").exists());
}

#[test]
fn collapse_dry_run_writes_no_epoch() {
    let r = rig();
    let out = run_at(&["ilio", "collapse", "--dry-run"], "", &r.env, NOW);
    assert_eq!(
        (out.code, out.stdout.starts_with("invariant ok: ")),
        (0, true),
        "{}",
        out.stdout
    );
    assert!(!r.ilio.join("epoch.json").exists());
    let real = run_at(&["ilio", "collapse"], "", &r.env, NOW);
    assert_eq!(real.code, 0, "{}", real.stderr);
    assert!(read(&r.ilio, "epoch.json").starts_with("{\"epoch\": 1, \"at\": "));
}

#[test]
fn ilio_needs_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_at(&["ilio", "table"], "", &Env::bare(tmp.path()), NOW);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("ILIO_PROJECT is not set"),
        "{}",
        out.stderr
    );
    let missing = run_at(
        &["ilio", "table"],
        "",
        &Env::bare(tmp.path()).with("ILIO_PROJECT", tmp.path().to_str().unwrap()),
        NOW,
    );
    assert_eq!(missing.code, 2);
    assert!(
        missing.stderr.contains("state.json missing; run init"),
        "{}",
        missing.stderr
    );
}
