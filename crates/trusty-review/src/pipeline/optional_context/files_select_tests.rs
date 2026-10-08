//! The drop order, the budget arithmetic and the deny-list (#9195).
//!
//! Why: AC2 fixes the order files drop in when over budget; AC1 and ruling D
//! bound the included bytes, the `Not shown:` list included; ruling A names
//! the paths never read; ruling C ranks the fetch cap.
//! What: drives the pure functions of `files_select` with hand-built files.
//! Test: this module.

use super::*;

fn shown(path: &str, class: Class, bytes: usize) -> Shown {
    Shown {
        path: path.to_string(),
        class,
        text: "x".repeat(bytes),
    }
}

fn kept_paths(kept: &[Shown]) -> Vec<&str> {
    kept.iter().map(|s| s.path.as_str()).collect()
}

fn dropped_paths(not_shown: &[NotShown]) -> Vec<&str> {
    not_shown.iter().map(|n| n.path.as_str()).collect()
}

/// The bytes of everything shown: the list plus the kept text.
fn included(kept: &[Shown], not_shown: &[NotShown]) -> usize {
    list_bytes(not_shown) + kept.iter().map(|s| s.text.len()).sum::<usize>()
}

/// #9195 AC2: over budget, every test file drops before any generated file,
/// and every generated file before any source file.
#[test]
fn tests_drop_before_generated_before_source() {
    let files = || {
        vec![
            shown("src/a.rs", Class::Source, 100),
            shown("tests/a.rs", Class::Test, 100),
            shown("dist/a.js", Class::Generated, 100),
        ]
    };
    let mut not_shown = Vec::new();
    let kept = select(files(), &mut not_shown, 250);
    assert_eq!(kept_paths(&kept), ["src/a.rs", "dist/a.js"]);
    assert_eq!(dropped_paths(&not_shown), ["tests/a.rs"]);
    let mut not_shown = Vec::new();
    let kept = select(files(), &mut not_shown, 170);
    assert_eq!(kept_paths(&kept), ["src/a.rs"]);
    assert_eq!(dropped_paths(&not_shown), ["tests/a.rs", "dist/a.js"]);
    assert!(not_shown.iter().all(|n| n.reason == Reason::OverBudget
        && n.state == SourceState::Omitted
        && n.chars_omitted == 100));
}

/// #9195 AC2: within a class the largest file drops first.
#[test]
fn within_a_class_the_largest_drops_first() {
    let files = vec![
        shown("tests/small.rs", Class::Test, 50),
        shown("tests/big.rs", Class::Test, 150),
        shown("src/lib.rs", Class::Source, 100),
    ];
    let mut not_shown = Vec::new();
    let kept = select(files, &mut not_shown, 230);
    assert_eq!(kept_paths(&kept), ["tests/small.rs", "src/lib.rs"]);
    assert_eq!(dropped_paths(&not_shown), ["tests/big.rs"]);
}

/// #9195 amendment 8: a total equal to the budget keeps every file; one byte
/// over drops exactly one, the test file.
#[test]
fn exact_budget_keeps_all_and_one_byte_over_drops_exactly_one() {
    let files = || {
        vec![
            shown("src/lib.rs", Class::Source, 100),
            shown("tests/t.rs", Class::Test, 100),
        ]
    };
    let mut not_shown = Vec::new();
    let kept = select(files(), &mut not_shown, 200);
    assert_eq!(kept.len(), 2, "total == budget keeps all");
    assert!(not_shown.is_empty());
    let mut not_shown = Vec::new();
    let kept = select(files(), &mut not_shown, 199);
    assert_eq!(kept_paths(&kept), ["src/lib.rs"]);
    assert_eq!(dropped_paths(&not_shown), ["tests/t.rs"]);
    assert!(included(&kept, &not_shown) <= 199);
}

/// #9195 AC2: the smallest source file is the last one standing.
#[test]
fn the_smallest_source_file_survives_a_tight_budget() {
    let files = vec![
        shown("src/big.rs", Class::Source, 500),
        shown("src/small.rs", Class::Source, 10),
        shown("src/mid.rs", Class::Source, 300),
        shown("tests/t.rs", Class::Test, 50),
    ];
    let mut not_shown = Vec::new();
    let kept = select(files, &mut not_shown, 150);
    assert_eq!(kept_paths(&kept), ["src/small.rs"]);
    assert_eq!(
        dropped_paths(&not_shown),
        ["tests/t.rs", "src/big.rs", "src/mid.rs"]
    );
}

/// #9195 AC2: which files survive does not depend on input order (ties break
/// on the path).
#[test]
fn order_is_independent_of_input_order() {
    let base = vec![
        shown("src/b.rs", Class::Source, 100),
        shown("src/a.rs", Class::Source, 100),
        shown("tests/x.rs", Class::Test, 100),
        shown("gen/c.rs", Class::Generated, 100),
        shown("src/c.rs", Class::Source, 100),
    ];
    let mut expected: Option<Vec<String>> = None;
    for rotation in 0..base.len() {
        for reverse in [false, true] {
            let mut files = base.clone();
            files.rotate_left(rotation);
            if reverse {
                files.reverse();
            }
            let mut not_shown = Vec::new();
            let mut kept: Vec<String> = select(files, &mut not_shown, 300)
                .into_iter()
                .map(|s| s.path)
                .collect();
            kept.sort();
            match &expected {
                None => expected = Some(kept),
                Some(first) => assert_eq!(&kept, first, "rotation {rotation}, reverse {reverse}"),
            }
        }
    }
    // Ties on size break on the path: `src/a.rs` sorts first, so it drops first.
    assert_eq!(
        expected.expect("ran"),
        ["src/b.rs", "src/c.rs"],
        "tests, generated, then the first path of equal size drop"
    );
}

/// #9195 AC2: one file larger than the budget is omitted whole, never cut.
#[test]
fn a_single_file_over_budget_is_omitted_not_truncated() {
    let mut not_shown = Vec::new();
    let kept = select(
        vec![shown("src/lib.rs", Class::Source, 1000)],
        &mut not_shown,
        999,
    );
    assert!(kept.is_empty());
    assert_eq!(not_shown.len(), 1);
    assert_eq!(not_shown[0].reason, Reason::OverBudget);
    assert_eq!(not_shown[0].chars_omitted, 1000);
}

/// #9195 ruling D: the `Not shown:` list comes off the top of the budget; a
/// list over the budget leaves no room for text, and still names every file.
#[test]
fn omitted_list_comes_off_the_top_of_the_budget() {
    let deleted = |i: usize| {
        NotShown::new(
            &format!("src/gone_{i}.rs"),
            Reason::Deleted,
            SourceState::Absent,
            "",
        )
    };
    let mut not_shown: Vec<NotShown> = (0..10).map(deleted).collect();
    let list = list_bytes(&not_shown);
    assert!(list > 100, "ten lines take more than 100 bytes: {list}");
    let kept = select(
        vec![shown("src/lib.rs", Class::Source, 10)],
        &mut not_shown,
        list,
    );
    assert!(kept.is_empty(), "no room left once the list is paid for");
    assert_eq!(not_shown.len(), 11, "every file is still named");
    let mut not_shown: Vec<NotShown> = (0..10).map(deleted).collect();
    let kept = select(
        vec![shown("src/lib.rs", Class::Source, 10)],
        &mut not_shown,
        list + 10,
    );
    assert_eq!(kept_paths(&kept), ["src/lib.rs"]);
}

/// The list's bytes are exactly the lines the prompt carries.
#[test]
fn list_bytes_count_the_heading_and_every_line() {
    assert_eq!(list_bytes(&[]), 0);
    let entry = NotShown::new("a.rs", Reason::OverBudget, SourceState::Omitted, "");
    assert_eq!(not_shown_line(&entry), "- \"a.rs\": over budget\n");
    assert_eq!(
        list_bytes(std::slice::from_ref(&entry)),
        NOT_SHOWN_HEADING.len() + not_shown_line(&entry).len()
    );
}

/// #9195 AC1: over many random sizes, classes and multibyte texts, the list
/// plus the kept text never exceeds the budget (or no text is kept when the
/// list alone does), no kept file is cut, and every file is accounted for.
#[test]
fn included_bytes_never_exceed_budget() {
    let mut seed: u64 = 0x9195;
    let mut next = |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let alphabet = ["a", "é", "漢", "🦀"];
    let classes = [Class::Test, Class::Generated, Class::Source];
    for round in 0..400 {
        let count = next(8) as usize;
        let files: Vec<Shown> = (0..count)
            .map(|i| {
                let glyph = alphabet[next(4) as usize];
                Shown {
                    path: format!("f{round}_{i}.rs"),
                    class: classes[next(3) as usize],
                    text: glyph.repeat(next(120) as usize),
                }
            })
            .collect();
        let budget = next(1500) as usize;
        let mut not_shown = Vec::new();
        let kept = select(files.clone(), &mut not_shown, budget);
        if list_bytes(&not_shown) <= budget {
            assert!(
                included(&kept, &not_shown) <= budget,
                "round {round}: {} > {budget}",
                included(&kept, &not_shown)
            );
        } else {
            assert!(
                kept.is_empty(),
                "round {round}: list over budget keeps no text"
            );
        }
        for k in &kept {
            let source = files
                .iter()
                .find(|f| f.path == k.path)
                .expect("kept a file");
            assert_eq!(k.text, source.text, "round {round}: a kept file is whole");
        }
        assert_eq!(kept.len() + not_shown.len(), files.len(), "round {round}");
        let fits_whole = files.iter().map(|f| f.text.len()).sum::<usize>() <= budget;
        assert_eq!(
            not_shown.is_empty(),
            fits_whole,
            "round {round}: drops only when over"
        );
    }
}

/// #9195: each path lands in its class; a word that only contains "test"
/// is source.
#[test]
fn classify_puts_each_path_in_its_class() {
    for (path, generated, class) in [
        ("crates/x/tests/a.rs", false, Class::Test),
        ("src/parser_tests.rs", false, Class::Test),
        ("src/parser_test.go", false, Class::Test),
        ("src/tests.rs", false, Class::Test),
        ("web/a.test.ts", false, Class::Test),
        ("web/a.spec.js", false, Class::Test),
        ("py/test_api.py", false, Class::Test),
        ("web/__tests__/a.js", false, Class::Test),
        ("crates/x/testdata/in.json", false, Class::Test),
        ("src/gen.generated.rs", false, Class::Generated),
        ("web/dist/app.js", false, Class::Generated),
        ("src/generated/x.rs", false, Class::Generated),
        ("Cargo.lock", true, Class::Generated),
        ("src/main.rs", false, Class::Source),
        ("src/contest.rs", false, Class::Source),
        ("src/attestation.rs", false, Class::Source),
    ] {
        assert_eq!(classify(path, generated), class, "{path}");
    }
}

/// #9195 fail-open arm 4: a source file in a tests-looking directory is
/// misclassified as a test. That only moves it earlier in the drop order; it
/// is still named, never lost.
#[test]
fn a_source_file_in_a_tests_looking_dir_is_dropped_early_and_named() {
    let path = "tools/tests/helper_source.rs";
    assert_eq!(classify(path, false), Class::Test);
    let mut not_shown = Vec::new();
    let kept = select(
        vec![
            shown(path, classify(path, false), 100),
            shown("src/big.rs", Class::Source, 100),
        ],
        &mut not_shown,
        160,
    );
    assert_eq!(kept_paths(&kept), ["src/big.rs"]);
    assert_eq!(dropped_paths(&not_shown), [path]);
}

/// #9195 ruling A: the deny-list matches the file name, any case.
#[test]
fn sensitive_paths_are_on_the_deny_list() {
    for path in [
        ".env",
        "config/.env.production",
        "certs/server.pem",
        "keys/app.KEY",
        "home/id_rsa",
        "home/id_rsa.pub",
        "aws/credentials",
        "src/my_credentials.rs",
        "src/secret_store.rs",
        "docs/SECRETS.md",
        // A directory segment matches too (ruling A globs have no slash).
        "k8s/secrets/db.yaml",
        "config/credentials/prod.yml",
        "deploy/Prod-Secrets/eu/app/values.yaml",
        // The substring rule already denies this unrelated name; recorded,
        // not endorsed.
        "src/secretary.rs",
    ] {
        assert!(is_sensitive(path), "{path} must be denied");
    }
    for path in [
        "src/keyboard.rs",
        "src/envelope.rs",
        "docs/pem.md",
        "src/main.rs",
    ] {
        assert!(!is_sensitive(path), "{path} must be allowed");
    }
}

/// #9195 ruling C: past the fetch cap, tests go first, then generated
/// files, then the most diff lines; the reads keep the diff order.
#[test]
fn over_the_fetch_cap_ranks_tests_then_generated_then_diff_lines() {
    let candidate = |path: &str, class: Class, diff_lines: usize| Candidate {
        path: path.to_string(),
        class,
        diff_lines,
        removed: false,
        carried: true,
    };
    let candidates = vec![
        candidate("src/five.rs", Class::Source, 5),
        candidate("src/fifty.rs", Class::Source, 50),
        candidate("Cargo.lock", Class::Generated, 1),
        candidate("src/ten.rs", Class::Source, 10),
        candidate("tests/t.rs", Class::Test, 2),
    ];
    let (fetch, over) = split_fetch_cap(candidates, 2);
    let names = |v: &[Candidate]| v.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
    assert_eq!(names(&fetch), ["src/five.rs", "src/ten.rs"]);
    assert_eq!(names(&over), ["tests/t.rs", "Cargo.lock", "src/fifty.rs"]);
}

/// #9195 amendment 4: a path is cut to 512 characters in the list.
#[test]
fn a_long_path_is_capped_in_the_list() {
    let long = "a/".repeat(2000);
    assert_eq!(cap_path(&long).chars().count(), 512);
    let line = not_shown_line(&NotShown::new(
        &long,
        Reason::ReadFailed,
        SourceState::Unavailable,
        "",
    ));
    assert!(line.len() < 540, "{}", line.len());
}
