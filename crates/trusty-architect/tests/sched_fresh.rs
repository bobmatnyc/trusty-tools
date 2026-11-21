//! Artifact freshness: glob mtimes and stamp files (MB1595). The escalation
//! flow lives in tests/sched_engine.rs.

mod sched_support;

use std::time::{Duration, UNIX_EPOCH};

use sched_support::*;
use trusty_architect::sched::config::Registry;
use trusty_architect::sched::fresh::{Freshness, check};

fn stamp_file(path: &std::path::Path, mtime: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "x").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime as u64))
        .unwrap();
}

fn check_job(kind: &str, path: &str) -> Registry {
    let check =
        format!(r#""check":{{"artifact":{{"type":"{kind}","path":"{path}","max_age_h":1}}}}"#);
    reg(&script("job1", "1h", &check))
}

#[test]
fn glob_freshness_uses_the_newest_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let r = check_job("glob", "out/*.md");
    let l = layout(dir.path());
    assert_eq!(check(&r.jobs[0], &l, T0), Freshness::Missing);
    stamp_file(&dir.path().join("out/old.md"), T0 - 7200);
    stamp_file(&dir.path().join("out/other.txt"), T0 - 10);
    assert_eq!(
        check(&r.jobs[0], &l, T0),
        Freshness::Stale(7200),
        "the .txt file does not match the glob"
    );
    stamp_file(&dir.path().join("out/new.md"), T0 - 600);
    assert_eq!(check(&r.jobs[0], &l, T0), Freshness::Fresh(600));
    // `~` expands to the home directory (the layout's home is the temp dir).
    stamp_file(&dir.path().join("h/x.log"), T0 - 5);
    assert_eq!(
        check(&check_job("glob", "~/h/*.log").jobs[0], &l, T0),
        Freshness::Fresh(5)
    );
}

#[test]
fn stamp_freshness_reads_the_at_field() {
    let dir = tempfile::tempdir().unwrap();
    let r = check_job("stamp", "");
    let l = layout(dir.path());
    assert_eq!(check(&r.jobs[0], &l, T0), Freshness::Missing);
    let put = |at: &str| {
        std::fs::create_dir_all(l.stamp("job1").parent().unwrap()).unwrap();
        std::fs::write(l.stamp("job1"), format!(r#"{{"at":"{at}"}}"#)).unwrap();
    };
    put("2023-11-14T23:30:00Z");
    assert_eq!(check(&r.jobs[0], &l, T0), Freshness::Fresh(1800));
    put("2023-11-14T20:00:00Z");
    assert_eq!(check(&r.jobs[0], &l, T0), Freshness::Stale(14_400));
    // meta_due.py owns the other two artifact types.
    assert_eq!(
        check(&check_job("jsonl_event", "x").jobs[0], &l, T0),
        Freshness::Unsupported
    );
}
