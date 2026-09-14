//! Call-site coverage for #7762: concurrent writers of one project
//! `.claude/settings.json` never lose an update.
//!
//! Why a separate file rather than asserts inside `settings_lock_tests.rs`: the
//! unit tests prove the primitive; these prove it is WIRED — that the launch
//! path's `merge_settings` and the `tm doctor --fix` repair that shares the file
//! hold the SAME lock across their whole read-modify-write cycle. The reported
//! failure mode is silent (both writers report success and one writer's keys are
//! simply gone), so only a racing test can catch a regression here.
//! What:
//! - `concurrent_merges_of_distinct_keys_never_lose_an_update` — two threads,
//!   one writer, barrier-aligned rounds.
//! - `a_concurrent_launch_merge_and_doctor_exclude_lose_neither_write` — two
//!   DIFFERENT writers of the same file, which is the shape #7762 reported.
//! - `merge_settings_refuses_when_the_lock_cannot_be_acquired` — the Fail-Open
//!   Check: no lock, no write.
//! - `merge_settings_leaves_no_bak_sibling` — the litter half of the issue.
//!
//! Test: this is the test module.

use std::sync::{Arc, Barrier};

use tempfile::TempDir;

use super::*;

/// Rounds each racing thread runs.
///
/// Why: one unsynchronised round loses an update only if the two cycles happen
/// to interleave. Against the pre-#7762 writer 64 barrier-aligned rounds lost at
/// least one key on every run; a smaller count made the red run flaky.
const ROUNDS: usize = 64;

/// Read `<project>/.claude/settings.json` as a JSON object.
fn read_settings(project: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(project.join(".claude").join("settings.json"))
        .expect("settings.json must exist");
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("settings.json is not JSON: {e}: {raw}"))
}

/// Two threads adding DISTINCT keys keep every key both of them wrote.
///
/// Why this shape: each round's writer preserves whatever it read, so a lost
/// update leaves a permanent hole — checking every key at the end catches a loss
/// in any round, not only the last. The barrier is what makes the two cycles
/// overlap; without it the threads drift apart and the race stops reproducing.
/// Two SEQUENTIAL calls cannot pass this test, because sequential calls never
/// produce the interleaving it measures.
#[test]
fn concurrent_merges_of_distinct_keys_never_lose_an_update() {
    let tmp = TempDir::new().unwrap();
    let project: Arc<std::path::PathBuf> = Arc::new(tmp.path().to_path_buf());
    let barrier = Arc::new(Barrier::new(2));

    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|prefix| {
            let project = Arc::clone(&project);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    barrier.wait();
                    settings::merge_settings_key(
                        &project,
                        &format!("{prefix}{round}"),
                        serde_json::Value::Bool(true),
                    )
                    .expect("merge");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread");
    }

    let settings = read_settings(&project);
    let missing: Vec<String> = (0..ROUNDS)
        .flat_map(|round| [format!("a{round}"), format!("b{round}")])
        .filter(|key| settings.get(key).is_none())
        .collect();
    assert!(missing.is_empty(), "lost updates: {missing:?}");
}

/// The launch writer and the `claudeMdExcludes` repair race one file and both
/// survive.
///
/// Why: #7762 is not about one function racing itself. `merge_settings` runs on
/// every launch and `claude_md_excludes::add_exclude` is a `tm doctor --fix`
/// repair over the project tier — the same file, from two entry points that
/// shared nothing before this fix.
#[test]
fn a_concurrent_launch_merge_and_doctor_exclude_lose_neither_write() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().to_path_buf();
    let settings_path = project.join(".claude").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, b"{}").unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let launch = {
        let project = project.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            for round in 0..ROUNDS {
                barrier.wait();
                settings::merge_settings_key(
                    &project,
                    &format!("launch{round}"),
                    serde_json::Value::Bool(true),
                )
                .expect("merge");
            }
        })
    };
    let doctor = {
        let settings_path = settings_path.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            for round in 0..ROUNDS {
                barrier.wait();
                let excluded = std::path::PathBuf::from(format!("/tmp/excluded-{round}.md"));
                let outcome =
                    crate::core::claude_md_excludes::add_exclude(&settings_path, &excluded);
                assert!(
                    matches!(
                        outcome,
                        crate::core::claude_md_excludes::ExcludeWrite::Added
                    ),
                    "unexpected outcome: {outcome:?}"
                );
            }
        })
    };
    launch.join().expect("launch thread");
    doctor.join().expect("doctor thread");

    let settings = read_settings(&project);
    let missing: Vec<String> = (0..ROUNDS)
        .map(|round| format!("launch{round}"))
        .filter(|key| settings.get(key).is_none())
        .collect();
    assert!(missing.is_empty(), "lost launch keys: {missing:?}");
    assert_eq!(
        settings["claudeMdExcludes"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        ROUNDS,
        "lost an exclude: {settings}"
    );
}

/// The Fail-Open Check: a lock that cannot be taken writes nothing.
///
/// Why: a writer that proceeds unlocked IS the lost update, reported as success.
/// A directory occupying the sidecar's name is the portable way to make the
/// acquisition fail without also making the settings file unwritable — the
/// pre-#7762 writer would happily have rewritten it.
#[test]
fn merge_settings_refuses_when_the_lock_cannot_be_acquired() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude = project.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings_path = claude.join("settings.json");
    std::fs::write(&settings_path, b"{\"kept\":true}").unwrap();
    std::fs::create_dir(claude.join("settings.json.lock")).unwrap();

    let err = settings::merge_settings_key(project, "added", serde_json::Value::Bool(true))
        .expect_err("an unacquirable lock must refuse the write");

    assert!(
        matches!(err, PrepError::SettingsLock { .. }),
        "unexpected error: {err}"
    );
    assert!(!err.is_fatal(), "the session must still launch");
    assert_eq!(
        std::fs::read(&settings_path).unwrap(),
        b"{\"kept\":true}",
        "the settings file must be byte-for-byte what it was"
    );
}

/// The launch write leaves no `<path>.bak` in the operator's project (#7762).
#[test]
fn merge_settings_leaves_no_bak_sibling() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude = project.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("settings.json"), b"{\"outputStyle\":\"old\"}").unwrap();

    settings::merge_settings_key(project, "outputStyle", serde_json::json!("new")).expect("merge");

    assert!(
        !claude.join("settings.json.bak").exists(),
        "a launch must not drop a .bak into the project"
    );
    let strays: Vec<String> = std::fs::read_dir(&claude)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "left staging files: {strays:?}");
}
