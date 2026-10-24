//! #8454 S6 prep: the tripwire record's append and parse.

use super::*;

fn line(tool: &str, path: Option<&str>) -> TripLine {
    TripLine {
        v: FORMAT_VERSION,
        at: Utc::now(),
        session_id: Some("s-1".into()),
        tool_name: tool.into(),
        cause: "inside".into(),
        path: path.map(str::to_owned),
    }
}

/// 🔴 #8454 S6: each append adds exactly one parseable line, creating the
/// directory and file; a stray line is named by its number.
#[test]
fn tripwire_record_append_is_one_line_per_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = record_dir(home.path());
    assert!(!dir.exists());
    let (a, b) = (line("Read", Some("/p/a")), line("Bash", None));
    append(&dir, &a).expect("first append");
    append(&dir, &b).expect("second append");
    let text = std::fs::read_to_string(record_path(home.path())).expect("read record");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert_eq!(parse(&text), Ok(vec![a, b]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&record_path(home.path())), 0o600);
    }
    assert_eq!(parse(&format!("{text}\n{{\"v\":1}}\n")), Err(4));
    assert_eq!(parse(""), Ok(Vec::new()));
}

/// #8454 S6: a long path is cut on a character boundary.
#[test]
fn clip_cuts_on_a_char_boundary() {
    let long = "é".repeat(MAX_PATH_BYTES);
    let cut = clip(&long);
    assert!(cut.len() <= MAX_PATH_BYTES && cut.chars().all(|c| c == 'é'));
    assert_eq!(clip("/short"), "/short");
}
