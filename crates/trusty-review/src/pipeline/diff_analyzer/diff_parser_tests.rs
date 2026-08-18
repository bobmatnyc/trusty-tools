//! Unit tests for the unified-diff parser (#4458).
//!
//! The table `FILE_CASES` holds one fragment per diff shape git can emit. Each
//! fragment is parsed alone, and then all of them are concatenated into one
//! diff — that concatenated case is the acceptance criterion: N files in, N
//! entries out, each with its own path, status, and content.

use super::*;

/// One diff shape, with the single entry it must produce.
struct FileCase {
    /// Test-failure label.
    name: &'static str,
    /// A complete per-file diff fragment, including its `diff --git` line.
    fragment: &'static str,
    /// Path the entry must carry.
    path: &'static str,
    /// Status the entry must carry.
    status: &'static str,
    /// A substring that must appear in this entry's patch, if it has content.
    content: Option<&'static str>,
}

const FILE_CASES: &[FileCase] = &[
    FileCase {
        name: "modified",
        fragment: "diff --git a/src/auth.rs b/src/auth.rs\n\
                   index 1111111..2222222 100644\n\
                   --- a/src/auth.rs\n\
                   +++ b/src/auth.rs\n\
                   @@ -1,2 +1,2 @@\n\
                   -fn authenticate() {}\n\
                   +fn authenticate(cfg: &Config) {}\n\
                    // trailing context\n",
        path: "src/auth.rs",
        status: "modified",
        content: Some("fn authenticate(cfg: &Config) {}"),
    },
    FileCase {
        name: "added",
        fragment: "diff --git a/src/new_module.rs b/src/new_module.rs\n\
                   new file mode 100644\n\
                   index 0000000..3333333\n\
                   --- /dev/null\n\
                   +++ b/src/new_module.rs\n\
                   @@ -0,0 +1,1 @@\n\
                   +pub fn fresh() {}\n",
        path: "src/new_module.rs",
        status: "added",
        content: Some("pub fn fresh() {}"),
    },
    FileCase {
        name: "deleted",
        fragment: "diff --git a/src/gone.rs b/src/gone.rs\n\
                   deleted file mode 100644\n\
                   index 4444444..0000000\n\
                   --- a/src/gone.rs\n\
                   +++ /dev/null\n\
                   @@ -1,1 +0,0 @@\n\
                   -fn gone() {}\n",
        path: "src/gone.rs",
        status: "removed",
        content: Some("fn gone() {}"),
    },
    FileCase {
        name: "renamed with content",
        fragment: "diff --git a/src/old_edit.rs b/src/new_edit.rs\n\
                   similarity index 82%\n\
                   rename from src/old_edit.rs\n\
                   rename to src/new_edit.rs\n\
                   index 5555555..6666666 100644\n\
                   --- a/src/old_edit.rs\n\
                   +++ b/src/new_edit.rs\n\
                   @@ -1,1 +1,1 @@\n\
                   -const N: u8 = 1;\n\
                   +const N: u8 = 2;\n",
        path: "src/new_edit.rs",
        status: "renamed",
        content: Some("const N: u8 = 2;"),
    },
    FileCase {
        name: "pure rename, no content change",
        fragment: "diff --git a/src/old_pure.rs b/src/new_pure.rs\n\
                   similarity index 100%\n\
                   rename from src/old_pure.rs\n\
                   rename to src/new_pure.rs\n",
        path: "src/new_pure.rs",
        status: "renamed",
        content: None,
    },
    FileCase {
        name: "mode-only change",
        fragment: "diff --git a/scripts/run.sh b/scripts/run.sh\n\
                   old mode 100644\n\
                   new mode 100755\n",
        path: "scripts/run.sh",
        status: "modified",
        content: None,
    },
    FileCase {
        name: "binary file",
        fragment: "diff --git a/assets/logo.png b/assets/logo.png\n\
                   index 7777777..8888888 100644\n\
                   Binary files a/assets/logo.png and b/assets/logo.png differ\n",
        path: "assets/logo.png",
        status: "modified",
        content: Some("Binary files"),
    },
    FileCase {
        name: "new binary file",
        fragment: "diff --git a/assets/added.png b/assets/added.png\n\
                   new file mode 100644\n\
                   index 0000000..9999999\n\
                   Binary files /dev/null and b/assets/added.png differ\n",
        path: "assets/added.png",
        status: "added",
        content: Some("Binary files"),
    },
    FileCase {
        // A patch fixture whose own hunk body contains lines beginning `--- `
        // and `+++ `: `-- a/embedded.rs` removed, `++ b/embedded.rs` added.
        name: "diff of a diff",
        fragment: "diff --git a/testdata/sample.patch b/testdata/sample.patch\n\
                   index aaaaaaa..bbbbbbb 100644\n\
                   --- a/testdata/sample.patch\n\
                   +++ b/testdata/sample.patch\n\
                   @@ -1,1 +1,1 @@\n\
                   --- a/embedded.rs\n\
                   +++ b/replacement.rs\n",
        path: "testdata/sample.patch",
        status: "modified",
        content: Some("a/embedded.rs"),
    },
];

/// Each shape on its own must produce exactly one entry with the right path,
/// status, and content.
#[test]
fn each_file_shape_yields_one_entry() {
    for case in FILE_CASES {
        let parsed = parse_diff_files_detailed(case.fragment);
        assert_eq!(
            parsed.files.len(),
            1,
            "{}: expected 1 file, got {:?}",
            case.name,
            parsed.files.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
        assert_eq!(parsed.files[0].0, case.path, "{}: path", case.name);
        assert_eq!(parsed.files[0].1, case.status, "{}: status", case.name);
        assert!(
            parsed.unparsed.is_empty(),
            "{}: nothing should be unparsed, got {:?}",
            case.name,
            parsed.unparsed
        );
        if let Some(needle) = case.content {
            assert!(
                parsed.files[0].2.contains(needle),
                "{}: patch missing {needle:?}, got {:?}",
                case.name,
                parsed.files[0].2
            );
        }
    }
}

/// #4458 acceptance criterion: a diff of N files yields N entries, in order,
/// with each file's content confined to its own entry.
#[test]
fn concatenated_diff_yields_one_entry_per_file() {
    let diff: String = FILE_CASES.iter().map(|c| c.fragment).collect();
    let parsed = parse_diff_files_detailed(&diff);

    assert_eq!(
        parsed.files.len(),
        FILE_CASES.len(),
        "expected {} files, got {:?}",
        FILE_CASES.len(),
        parsed.files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert!(parsed.unparsed.is_empty(), "{:?}", parsed.unparsed);

    for (case, file) in FILE_CASES.iter().zip(parsed.files.iter()) {
        assert_eq!(file.0, case.path, "{}: path", case.name);
        assert_eq!(file.1, case.status, "{}: status", case.name);
    }

    // Content isolation: the modified file's patch must not have absorbed the
    // added file's line, which is what the pre-fix parser did on a diff whose
    // per-file markers it failed to honour.
    assert!(
        parsed.files[0]
            .2
            .contains("fn authenticate(cfg: &Config) {}")
    );
    assert!(!parsed.files[0].2.contains("pub fn fresh() {}"));
}

/// The #4458 collapse itself: a diff carrying `---`/`+++` pairs but no
/// `diff --git ` markers.
///
/// Pre-fix this returned ONE entry named `src/delta.rs` holding all four
/// files' hunks, because `+++ b/` rebound the open record's path without ever
/// flushing it.
#[test]
fn multi_file_diff_without_git_markers() {
    const PATHS: &[&str] = &[
        "src/alpha.rs",
        "src/beta.rs",
        "src/gamma.rs",
        "src/delta.rs",
    ];

    let mut diff = String::new();
    for path in PATHS {
        let stem = path.trim_start_matches("src/").trim_end_matches(".rs");
        diff.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
        diff.push_str("@@ -1,1 +1,1 @@\n");
        diff.push_str(&format!("-{stem}_old\n+{stem}_new\n"));
    }

    let parsed = parse_diff_files_detailed(&diff);

    assert_eq!(
        parsed.files.len(),
        PATHS.len(),
        "multi-file diff collapsed: {:?}",
        parsed.files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert!(parsed.unparsed.is_empty(), "{:?}", parsed.unparsed);

    for (path, file) in PATHS.iter().zip(parsed.files.iter()) {
        assert_eq!(&file.0, path);
        assert_eq!(file.1, "modified");
    }
    assert!(parsed.files[0].2.contains("alpha_new"));
    assert!(
        !parsed.files[0].2.contains("beta_new"),
        "alpha's patch absorbed beta's content: {:?}",
        parsed.files[0].2
    );
    assert!(parsed.files[3].2.contains("delta_new"));
    assert!(!parsed.files[3].2.contains("gamma_new"));
}

/// A hunk body whose lines begin `--- `/`+++ ` must not open a new file. The
/// `@@` header's line budget is what tells body from header.
#[test]
fn hunk_body_lines_do_not_open_a_new_file() {
    let diff = "--- a/testdata/one.patch\n\
                +++ b/testdata/one.patch\n\
                @@ -1,2 +1,2 @@\n\
                --- a/inner.rs\n\
                +++ b/inner.rs\n\
                --- a/other.rs\n\
                +++ b/other.rs\n";
    let parsed = parse_diff_files_detailed(diff);
    assert_eq!(
        parsed.files.len(),
        1,
        "body lines were read as file headers: {:?}",
        parsed.files.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert_eq!(parsed.files[0].0, "testdata/one.patch");
}

/// Content the parser cannot attribute is reported, never dropped in silence.
#[test]
fn headerless_hunks_are_reported_unparsed() {
    let diff = "@@ -1,1 +1,1 @@\n-old line\n+new line\n";
    let parsed = parse_diff_files_detailed(diff);
    assert!(parsed.files.is_empty());
    assert_eq!(parsed.unparsed.len(), 1);
    assert_eq!(parsed.unparsed[0].line_count, 3);
    assert_eq!(parsed.unparsed[0].header, "@@ -1,1 +1,1 @@");
}

/// A commit-message preamble ahead of the first file is reported too.
#[test]
fn preamble_before_the_first_file_is_reported_unparsed() {
    let diff = "commit deadbeef\nAuthor: Someone\n\n    subject line\n\n\
                diff --git a/src/a.rs b/src/a.rs\n\
                --- a/src/a.rs\n\
                +++ b/src/a.rs\n\
                @@ -1,1 +1,1 @@\n\
                -a\n\
                +b\n";
    let parsed = parse_diff_files_detailed(diff);
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].0, "src/a.rs");
    assert_eq!(parsed.unparsed.len(), 1);
    assert_eq!(parsed.unparsed[0].header, "commit deadbeef");
}

/// Blank input produces neither files nor a spurious unparsed section.
#[test]
fn empty_diff_reports_nothing() {
    for diff in ["", "\n", "   \n\n"] {
        let parsed = parse_diff_files_detailed(diff);
        assert!(parsed.files.is_empty(), "{diff:?}");
        assert!(parsed.unparsed.is_empty(), "{diff:?}");
    }
}

/// The compatibility wrapper returns exactly the detailed form's file list.
#[test]
fn wrapper_matches_detailed_files() {
    let diff: String = FILE_CASES.iter().map(|c| c.fragment).collect();
    assert_eq!(
        parse_diff_files(&diff),
        parse_diff_files_detailed(&diff).files
    );
}

/// `diff --git` header splitting, including a path that itself contains ` b/`.
#[test]
fn git_header_paths_split_on_the_matching_boundary() {
    let cases: &[(&str, Option<&str>, Option<&str>)] = &[
        ("a/src/x.rs b/src/x.rs", Some("src/x.rs"), Some("src/x.rs")),
        ("a/old.rs b/new.rs", Some("old.rs"), Some("new.rs")),
        (
            "a/pkg b/mod.rs b/pkg b/mod.rs",
            Some("pkg b/mod.rs"),
            Some("pkg b/mod.rs"),
        ),
        ("\"a/odd path.rs\" \"b/odd path.rs\"", None, None),
    ];
    for (input, old, new) in cases {
        let got = split_git_header_paths(input);
        assert_eq!(
            (got.0.as_deref(), got.1.as_deref()),
            (*old, *new),
            "input {input:?}"
        );
    }
}

/// Hunk-header line budgets, including the bare `@@ -1 +1 @@` form.
#[test]
fn hunk_counts_parse() {
    let cases: &[(&str, Option<(usize, usize)>)] = &[
        ("@@ -1,3 +1,5 @@", Some((3, 5))),
        ("@@ -0,0 +1,2 @@ fn ctx()", Some((0, 2))),
        ("@@ -1 +1 @@", Some((1, 1))),
        ("@@@ -1,1 -1,1 +1,1 @@@", None),
        ("not a hunk header", None),
    ];
    for (input, want) in cases {
        assert_eq!(parse_hunk_counts(input), *want, "input {input:?}");
    }
}

// ─── Tests carried over from the pre-#4458 parser ────────────────────────────

const SAMPLE_DIFF: &str = r#"diff --git a/Cargo.lock b/Cargo.lock
index abc..def 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1,3 +1,3 @@
-serde = "1.0.100"
+serde = "1.0.200"
diff --git a/src/auth.rs b/src/auth.rs
index abc..def 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -1,3 +1,5 @@
-pub fn authenticate(user: &str) -> Result<Token, Error> {
+pub fn authenticate(user: &str, config: &Config) -> Result<Token, Error> {
+    validate(user)?;
     Ok(Token::new(user))
 }
"#;

#[test]
fn parse_diff_files_basic() {
    let files = parse_diff_files(SAMPLE_DIFF);
    assert_eq!(files.len(), 2);
    let paths: Vec<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
    assert!(paths.contains(&"Cargo.lock"));
    assert!(paths.contains(&"src/auth.rs"));
}

#[test]
fn parse_diff_files_new_file() {
    let diff = "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fn new() {}\n";
    let files = parse_diff_files(diff);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "new.rs");
    assert_eq!(files[0].1, "added");
}
