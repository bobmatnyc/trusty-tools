//! Unit tests for `import_json` (G0b). See G0b secrets JSON import.

use std::path::Path;

use super::import_json::{detect, parse_google_oauth, parse_json};
use crate::cli::ImportFormat;

/// A value no error text may carry.
const SECRET: &str = "s3cr3t-G0B-value-x9";

fn names_and_values(text: &str) -> Vec<(String, String)> {
    parse_json(text)
        .expect("parses")
        .iter()
        .map(|e| (e.name().to_owned(), e.raw().to_owned()))
        .collect()
}

#[test]
fn json_flattens_nested_keys_to_upper_snake_names() {
    let text = r#"{"db": {"pass-word": "p", "port": 5432, "tls": {"on": true}}, "API_KEY": "k"}"#;
    let got = names_and_values(text);
    let want = [
        ("API_KEY", "k"),
        ("DB_PASS_WORD", "p"),
        ("DB_PORT", "5432"),
        ("DB_TLS_ON", "true"),
    ];
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn json_refuses_an_array_naming_the_key_only() {
    let text = format!(r#"{{"ok": "{SECRET}", "svc": {{"hosts": ["{SECRET}"]}}}}"#);
    let err = parse_json(&text)
        .expect_err("an array is refused")
        .to_string();
    assert!(err.contains("SVC_HOSTS is an array"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
    let null = parse_json(r#"{"gone": null}"#).expect_err("null is refused");
    assert!(null.to_string().contains("GONE is null"), "{null}");
}

#[test]
fn json_refuses_malformed_text_without_echoing_it() {
    for text in [
        format!(r#"{{"k": "{SECRET}""#),
        format!(r#"{{"k": {SECRET}}}"#),
        format!(r#"["{SECRET}"]"#),
    ] {
        let err = parse_json(&text).expect_err("refused");
        for shown in [err.to_string(), format!("{err:?}"), format!("{err:#}")] {
            assert!(!shown.contains(SECRET), "{shown}");
            assert!(!shown.contains("s3cr"), "{shown}");
        }
    }
    let collide = parse_json(r#"{"a_b": "1", "a": {"b": "2"}}"#).expect_err("collision");
    assert!(
        collide.to_string().contains("A_B is defined twice"),
        "{collide}"
    );
}

fn google(shape: &str, fields: &str) -> String {
    format!(r#"{{"{shape}": {{{fields}, "auth_uri": "https://x"}}}}"#)
}

const FULL: &str =
    r#""client_id": "123.apps", "client_secret": "s3cr3t-G0B-value-x9", "project_id": "my-proj-1""#;

#[test]
fn google_installed_and_web_shapes_give_three_grouped_entries() {
    for shape in ["installed", "web"] {
        let entries = parse_google_oauth(&google(shape, FULL)).expect("parses");
        let got: Vec<(&str, &str)> = entries.iter().map(|e| (e.name(), e.raw())).collect();
        assert_eq!(
            got,
            [
                ("my-proj-1.CLIENT_ID", "123.apps"),
                ("my-proj-1.CLIENT_SECRET", SECRET),
                ("my-proj-1.PROJECT_ID", "my-proj-1"),
            ],
            "{shape}"
        );
    }
}

#[test]
fn google_missing_field_is_refused_naming_the_field_only() {
    let no_secret = google(
        "web",
        r#""client_id": "123.apps", "project_id": "my-proj-1""#,
    );
    let err = parse_google_oauth(&no_secret)
        .expect_err("refused")
        .to_string();
    assert!(err.contains("lacks a string `client_secret`"), "{err}");
    let no_project = google(
        "installed",
        &format!(r#""client_id": "1", "client_secret": "{SECRET}""#),
    );
    let err = parse_google_oauth(&no_project)
        .expect_err("refused")
        .to_string();
    assert!(err.contains("`project_id`"), "{err}");
    assert!(!err.contains(SECRET), "{err}");
    let bad_project = google(
        "web",
        &format!(r#""client_id": "1", "client_secret": "x", "project_id": "{SECRET}/x""#),
    );
    let err = parse_google_oauth(&bad_project)
        .expect_err("refused")
        .to_string();
    assert!(!err.contains(SECRET), "{err}");
}

#[test]
fn detect_picks_google_for_a_client_json_and_json_by_extension() {
    let client = google("installed", FULL);
    let json = Path::new("c.json");
    assert_eq!(detect(None, json, &client), ImportFormat::GoogleOauth);
    assert_eq!(detect(None, json, r#"{"a": "b"}"#), ImportFormat::Json);
    assert_eq!(
        detect(None, Path::new(".env"), &client),
        ImportFormat::Dotenv
    );
    let explicit = Some(ImportFormat::Json);
    assert_eq!(
        detect(explicit, Path::new("x"), &client),
        ImportFormat::GoogleOauth
    );
    assert_eq!(detect(explicit, Path::new("x"), "{}"), ImportFormat::Json);
    let dotenv = Some(ImportFormat::Dotenv);
    assert_eq!(detect(dotenv, json, &client), ImportFormat::Dotenv);
}
