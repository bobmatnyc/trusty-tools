//! #12029: every committed replay recording replays the same on a case-sensitive
//! (Linux ext4) and a case-insensitive (macOS APFS) filesystem.
//!
//! Why: `dashboard_ids/functions.json` seeded a case list writing both
//! `odd/owner-queue/items/A01.json` and `.../a01.json`. Recorded on APFS the
//! two names were one file, so the recording held one write; on the Linux CI
//! runner the port wrote two files and the replay failed with
//! "unexpected write".
//! What: a recording is refused when two distinct paths it names are equal
//! under ASCII case folding, comparing every ancestor too (`Odd/x` and
//! `odd/y` collide in their directory). The paths are the `files`, `reads`
//! and `writes` keys, the `argv` entries and every path-shaped string in a
//! seeded JSON input. The seeded inputs matter because APFS merges the
//! colliding writes before the recorder sees them, so the recorded channels
//! alone never show both names. A path-shaped string is a relative path with
//! no whitespace whose last component has an extension, which leaves out gh
//! slugs such as `Example-Org/b`.
//! Test: `no_recording_names_two_paths_that_differ_only_by_case`,
//! `case_fold_check_flags_a_seeded_path_that_differs_only_by_case`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use trusty_architect::replay::{Recording, load};

/// A relative path with no whitespace, no empty, `.` or `..` component, and
/// an extension on its last component.
fn path_shaped(s: &str) -> bool {
    if s.is_empty() || s.starts_with('/') || s.chars().any(char::is_whitespace) {
        return false;
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.iter().any(|p| matches!(*p, "" | "." | "..")) {
        return false;
    }
    parts
        .last()
        .is_some_and(|last| last.get(1..).is_some_and(|rest| rest.contains('.')))
}

fn strings(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::String(s) if path_shaped(s) => {
            out.insert(s.clone());
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Groups of two or more distinct paths (or ancestors) the recording names
/// that are equal under ASCII case folding.
fn case_fold_collisions(rec: &Recording) -> Vec<Vec<String>> {
    let mut paths: BTreeSet<String> = rec.files.keys().cloned().collect();
    paths.extend(rec.reads.iter().flatten().cloned());
    paths.extend(rec.writes.keys().cloned());
    paths.extend(rec.argv.iter().filter(|a| path_shaped(a)).cloned());
    for body in rec.files.values() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            strings(&v, &mut paths);
        }
    }
    let mut folded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in &paths {
        let mut prefix = String::new();
        for part in p.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            folded
                .entry(prefix.to_ascii_lowercase())
                .or_default()
                .insert(prefix.clone());
        }
    }
    folded
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|g| g.into_iter().collect())
        .collect()
}

#[test]
fn no_recording_names_two_paths_that_differ_only_by_case() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay");
    let mut seen = 0;
    let mut bad = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "json") {
                let rec = load(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
                seen += 1;
                for g in case_fold_collisions(&rec) {
                    let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
                    bad.push(format!("{rel}: {g:?}"));
                }
            }
        }
    }
    assert!(seen > 0, "no replay fixtures found");
    bad.sort();
    assert!(
        bad.is_empty(),
        "paths equal under case folding replay differently on Linux and macOS:\n{}",
        bad.join("\n")
    );
}

#[test]
fn case_fold_check_flags_a_seeded_path_that_differs_only_by_case() {
    let cases = r#"[{"op": "write", "path": "q/items/A01.json"},
        {"op": "write", "path": "q/items/a02.json"},
        {"slug": "Example-Org/b"}, {"slug": "example-org/a"}]"#;
    let rec = |extra: &str| -> Recording {
        let files = BTreeMap::from([("cases.json".to_owned(), cases.to_owned())]);
        let writes = BTreeMap::from([
            ("q/items/A01.json".to_owned(), Some("{}".to_owned())),
            (extra.to_owned(), Some("{}".to_owned())),
        ]);
        serde_json::from_value(serde_json::json!({
            "version": 3, "argv": ["cases.json"], "stdin": "", "env": {}, "files": files,
            "reads": null, "stdout": "", "stderr": "", "exit": 0, "writes": writes, "clock": 0,
        }))
        .unwrap()
    };
    assert!(case_fold_collisions(&rec("q/items/a02.json")).is_empty());
    assert_eq!(
        case_fold_collisions(&rec("Q/items/b.json")),
        [
            vec!["Q".to_owned(), "q".to_owned()],
            vec!["Q/items".to_owned(), "q/items".to_owned()]
        ]
    );
    let seeded = cases.replace("a02", "a01");
    let mut r = rec("q/items/a02.json");
    r.files.insert("cases.json".to_owned(), seeded);
    assert_eq!(
        case_fold_collisions(&r),
        [vec![
            "q/items/A01.json".to_owned(),
            "q/items/a01.json".to_owned()
        ]]
    );
}
