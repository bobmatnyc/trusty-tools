//! #9171 A3a: every prototype test A3a owns is ported by name.
//!
//! What: reads `tests/fixtures/a3/python_tests_a3a.txt` and every `tests/*.rs`
//! file, and requires, for each listed name, exactly one Rust `fn <name>(`
//! directly preceded by a `// Python: <name>` line (an attribute line may sit
//! between them). A dropped, renamed or merged case fails, and so does a
//! `// Python:` marker naming a test that is not on the list.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

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

#[test]
fn every_a3a_python_test_is_ported_by_name() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden = fs::read_to_string(root.join("tests/fixtures/a3/python_tests_a3a.txt"))?;
    let want: Vec<&str> = golden
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
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
    for name in &want {
        let at = found.get(*name).map_or(0, Vec::len);
        assert_eq!(
            at,
            1,
            "{name}: want exactly one ported test, found {at} ({:?})",
            found.get(*name)
        );
    }
    let extra: Vec<&String> = found
        .keys()
        .filter(|k| !want.contains(&k.as_str()))
        .collect();
    assert!(
        extra.is_empty(),
        "markers for tests not on the A3a list: {extra:?}"
    );
    assert_eq!(want.len(), 28, "the A3a list itself changed size");
    Ok(())
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
