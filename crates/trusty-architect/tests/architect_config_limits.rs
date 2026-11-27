//! #12028 P1c: `architect_config::load` survives hostile JSON.
//!
//! Deep nesting is a `ConfigError`, never a stack overflow, and an object
//! with many keys loads in linear time with Python's last-wins rule. These
//! live in their own test binary: before the fix the nesting case aborted
//! the process.

use std::time::{Duration, Instant};

use trusty_architect::architect_config::{Cap, load};

fn root_with(config: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("local")).unwrap();
    std::fs::write(root.path().join("local/architect.json"), config).unwrap();
    root
}

fn env(_: &str) -> Option<String> {
    None
}

fn nested(levels: usize) -> String {
    // `levels` arrays inside the top-level object, under an unknown key.
    format!("{{\"deep\": {}{}}}", "[".repeat(levels), "]".repeat(levels))
}

#[test]
fn deep_nesting_is_a_config_error_not_a_crash() {
    // The object is level 1: 999 arrays inside it is the deepest accepted.
    let ok = root_with(&nested(999));
    let c = load(ok.path(), &env).unwrap();
    assert_eq!(c.unknown, ["deep"]);

    let over = root_with(&nested(1000));
    let e = load(over.path(), &env).unwrap_err();
    assert!(
        e.0.contains(
            "not valid JSON (Nesting deeper than 1000 arrays and objects at line 1 column"
        ),
        "{e}"
    );

    let hostile = root_with(&"[".repeat(200_000));
    let e = load(hostile.path(), &env).unwrap_err();
    assert!(e.0.contains("Nesting deeper than 1000"), "{e}");
}

#[test]
fn many_keys_load_in_linear_time_last_wins() {
    // 200,000 distinct keys, then the first one again. A quadratic scan
    // needs about 2e10 key comparisons here (minutes); the bound is ~20x the
    // linear run on a loaded debug build.
    let mut text = String::from("{\"caps\": {");
    for i in 0..200_000 {
        text.push_str(&format!("\"k{i}\": {i}, "));
    }
    text.push_str("\"k0\": 7}}");
    let root = root_with(&text);
    let start = Instant::now();
    let c = load(root.path(), &env).unwrap();
    let took = start.elapsed();
    assert_eq!(c.caps.len(), 200_000);
    assert_eq!(c.caps[0], ("k0".to_owned(), Cap::Int(7)));
    assert_eq!(c.caps[199_999], ("k199999".to_owned(), Cap::Int(199_999)));
    assert!(took < Duration::from_secs(20), "took {took:?}");
}
