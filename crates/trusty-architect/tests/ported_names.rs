//! #9171 A3: every prototype test A3a and A3b own is ported by name; #9801
//! slice 1 adds `test_usage.py`'s.
//!
//! What: reads each golden list (`tests/fixtures/a3/python_tests_a3a.txt`,
//! `python_tests_a3b.txt`, `tests/fixtures/9801/python_tests_9801.txt`) and
//! every `tests/*.rs` file, and requires, for each listed name, exactly one
//! Rust `fn <name>(` directly preceded by a `// Python: <name>` line (an
//! attribute line may sit between them). A dropped, renamed or merged case
//! fails, and so does a `// Python:` marker naming a test on no list.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `// Python: <name>` markers in `src`, each mapped to the `fn` it precedes.
fn markers(src: &str) -> Vec<(String, Option<String>)> {
    let lines: Vec<&str> = src.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(name) = l.strip_prefix("// Python: ") else {
            continue;
        };
        let f = lines[i + 1..]
            .iter()
            .find(|l| !l.starts_with("#["))
            .and_then(|l| l.strip_prefix("fn "))
            .and_then(|l| l.split('(').next())
            .map(str::to_owned);
        out.push((name.trim().to_owned(), f));
    }
    out
}

/// The names on one golden list, comments and blank lines dropped.
fn golden(list: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(root.join("tests/fixtures").join(list))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// Every `// Python:` marker under `tests/`, by name, with the files it sits in.
fn found() -> Result<BTreeMap<String, Vec<String>>, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in fs::read_dir(root.join("tests"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "rs") {
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for (name, f) in markers(&fs::read_to_string(&path)?) {
                assert_eq!(
                    f.as_deref(),
                    Some(name.as_str()),
                    "{file}: `// Python: {name}` must sit on `fn {name}`"
                );
                found.entry(name).or_default().push(file.clone());
            }
        }
    }
    Ok(found)
}

/// Each name on `list` is ported exactly once, the list holds `size` names,
/// and no marker names a test outside the A3a and A3b lists.
fn check(list: &str, size: usize) -> TestResult {
    let want = golden(list)?;
    let known: Vec<String> = [
        golden("a3/python_tests_a3a.txt")?,
        golden("a3/python_tests_a3b.txt")?,
        // #9801 slice 1: test_usage.py.
        golden("9801/python_tests_9801.txt")?,
    ]
    .concat();
    let found = found()?;
    for name in &want {
        let at = found.get(name).map_or(0, Vec::len);
        assert_eq!(
            at,
            1,
            "{name}: want exactly one ported test, found {at} ({:?})",
            found.get(name)
        );
    }
    let extra: Vec<&String> = found.keys().filter(|k| !known.contains(k)).collect();
    assert!(
        extra.is_empty(),
        "markers for tests on no golden list: {extra:?}"
    );
    assert_eq!(want.len(), size, "the {list} list itself changed size");
    Ok(())
}

#[test]
fn every_a3a_python_test_is_ported_by_name() -> TestResult {
    check("a3/python_tests_a3a.txt", 28)
}

/// #9171 A3b: plan cases 1-10 (A3b-1) and 11-19 (A3b-2).
#[test]
fn every_a3b_python_test_is_ported_by_name() -> TestResult {
    check("a3/python_tests_a3b.txt", 19)
}

/// #9801 slice 1: test_usage.py's poller tests.
#[test]
fn every_9801_slice1_python_test_is_ported_by_name() -> TestResult {
    check("9801/python_tests_9801.txt", 12)
}

#[test]
fn marker_reader_sees_attribute_lines_and_mismatches() {
    let src = "// Python: test_a\n#[test]\nfn test_a() {}\n// Python: test_b\nfn other() {}\n";
    assert_eq!(
        markers(src),
        [
            ("test_a".to_owned(), Some("test_a".to_owned())),
            ("test_b".to_owned(), Some("other".to_owned()))
        ]
    );
}
