//! Unit tests of the `ruling_pack.py` port (#12028 P1i). Expected values
//! are the frozen script's, run under Python 3.14 on the same inputs.

use super::pack::{suggest_checks, write_pack};
use super::pyerr::{Key, subscript};
use super::scan::{identifiers, label, resolve, trim};
use crate::clock::FakeClock;
use crate::pyjson_load::{PyValue, loads};

fn projects() -> Vec<(String, PyValue)> {
    let doc = loads(
        r#"{"sample-app": {"aliases": ["sapp", "Sample"], "palace": "sample-palace",
            "index": "sample-idx", "repo": "example-org/sample-app"},
            "second": {"repo": "example-two/second-repo", "palace": "sapp"}}"#,
    )
    .expect("sample key map");
    match doc {
        PyValue::Dict(p) => p,
        _ => unreachable!("a dict"),
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn resolve_matches_key_alias_palace_index_and_repo() {
    let p = projects();
    let key = |n: &str| resolve(n, &p).map(|(k, _)| k.to_owned());
    for (name, want) in [
        ("sample-app", "sample-app"),
        (" SAMPLE ", "sample-app"),
        // An alias beats the other entry's palace of the same name.
        ("sapp", "sample-app"),
        ("sample-palace", "sample-app"),
        ("SAMPLE-IDX", "sample-app"),
        ("example-two/second-repo", "second"),
        ("second-repo", "second"),
    ] {
        assert_eq!(key(name).as_deref(), Some(want), "{name}");
    }
    assert_eq!(key("nope"), None);
}

#[test]
fn identifiers_follow_the_script_patterns() {
    let q = "crm #880 bookedTotal in booking.ts and provider_usage_events #1234567 a.tsx x.json5 foo.py.bak";
    let ids = identifiers(
        q,
        &strs(&["5"]),
        &strs(&["880"]),
        &[],
        &strs(&["bookedTotal"]),
    );
    // `#880` repeats a flag; `#1234567` gives its first six digits.
    assert_eq!(ids.nums, strs(&["5", "880", "123456"]));
    assert_eq!(ids.files, strs(&["booking.ts", "a.tsx", "foo.py"]));
    assert_eq!(ids.symbols, strs(&["bookedTotal", "provider_usage_events"]));
    let generic = identifiers("What_ does with_ this That", &[], &[], &[], &[]);
    assert_eq!(generic.symbols, strs(&["What_", "with_"]));
}

#[test]
fn trim_and_label_follow_the_script() {
    assert_eq!(trim("  a \n b\t c ", 10), "a b c");
    assert_eq!(trim("abcdef", 4), "abc…");
    assert_eq!(trim("abcd", 4), "abcd");
    let cases = [
        ((true, true, true), "HIGH"),
        ((true, true, false), "MEDIUM"),
        ((false, true, true), "MEDIUM"),
        ((true, false, false), "LOW"),
        ((false, false, false), "LOW"),
    ];
    for ((p, f, c), want) in cases {
        assert_eq!(label(p, f, c), want);
    }
}

#[test]
fn subscript_errors_match_python() {
    let v = |t: &str| loads(t).expect("json");
    let err = |t: &str, k: Key<'_>| subscript(&v(t), k).map_err(|e| e.to_string());
    assert_eq!(
        err("{}", Key::Str("result")),
        Err("KeyError: 'result'".into())
    );
    assert_eq!(
        err("[]", Key::Idx(0)),
        Err("IndexError: list index out of range".into())
    );
    assert_eq!(
        err("[]", Key::Str("text")),
        Err("TypeError: list indices must be integers or slices, not str".into())
    );
    assert_eq!(
        err("null", Key::Str("projects")),
        Err("TypeError: 'NoneType' object is not subscriptable".into())
    );
    assert_eq!(err("{\"a\": 1}", Key::Str("a")), Ok(v("1")));
}

#[test]
fn suggest_checks_follow_the_script() {
    let e = loads(r#"{"repo": "example-org/r", "gh_config": "gh-x", "root": "/w"}"#).expect("json");
    let merge = suggest_checks(&e, &strs(&["1", "2", "3", "4"]), "merge");
    assert_eq!(merge.len(), 4, "three PR views and the git fetch line");
    assert!(merge[0].starts_with("env -u GH_TOKEN -u GITHUB_TOKEN GH_CONFIG_DIR=$HOME/.config/gh-x gh pr view 1 -R example-org/r"));
    assert_eq!(
        merge[3],
        "git -C /w fetch origin && git -C /w log --oneline -5 origin/main"
    );
    let bare = loads("{}").expect("json");
    assert_eq!(
        suggest_checks(&bare, &[], "spec"),
        [
            "name one read-only check that could falsify the PM's claim (gh, git, curl, grep) and run it"
        ]
    );
}

#[test]
fn write_pack_lands_under_the_given_root() {
    // See #12028 P1i: the pack goes under the root it is given, never elsewhere.
    let dir = tempfile::tempdir().expect("tempdir");
    let packs = dir.path().join("local/packs");
    let clock = FakeClock::new(1_795_784_400.9);
    let path = write_pack("pack body", "sample-app", &clock, &packs).expect("written");
    let want = packs.join("20261127T130000Z-sample-app.md");
    assert_eq!(path, want.to_string_lossy());
    assert!(want.starts_with(dir.path()));
    assert_eq!(std::fs::read_to_string(&want).expect("read"), "pack body\n");
    let entries: Vec<_> = std::fs::read_dir(&packs).expect("list").collect();
    assert_eq!(entries.len(), 1, "one file, nothing beside it");
}
