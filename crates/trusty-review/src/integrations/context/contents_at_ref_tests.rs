//! Tests for the head-SHA Contents API reads (#9193).

use base64::Engine as _;

use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// #9193 criterion 1 (amendment 9): every request names the head commit as
/// `ref`, never the default branch.
#[test]
fn contents_url_carries_ref_equal_to_head_sha() {
    let url = contents_url("acme", "billing", "docs/adr/0001-totals.md", SHA).expect("valid");
    assert_eq!(
        url.as_str(),
        format!(
            "https://api.github.com/repos/acme/billing/contents/docs/adr/0001-totals.md?ref={SHA}"
        )
    );
    let refs: Vec<_> = url.query_pairs().filter(|(k, _)| k == "ref").collect();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].1, SHA);
}

/// A head SHA is 40 or 64 lowercase hex characters, nothing else.
#[test]
fn head_sha_shape_is_strict() {
    assert!(is_head_sha(SHA));
    assert!(is_head_sha(&"a".repeat(64)));
    for bad in [
        "",
        "main",
        &SHA[..39],
        &SHA.to_uppercase(),
        &format!("{}g", &SHA[..39]),
    ] {
        assert!(!is_head_sha(bad), "{bad:?} must not pass");
    }
}

/// #9193 Fail-Open Check: a bad path or SHA fails before any URL is built,
/// so no request is ever made with it.
#[tokio::test]
async fn fetcher_rejects_a_bad_path_without_a_request() {
    let fetcher = GithubDocFetcher::new("acme", "billing", "token").expect("client");
    for path in [
        "../etc/passwd.md",
        "docs/../x.md",
        "/docs/x.md",
        "docs//x.md",
        "docs/x.md?ref=main",
        "docs/%2e.md",
        "docs/./x.md",
    ] {
        let got = fetcher.fetch(path, SHA).await;
        assert!(
            matches!(got, Err(DocFetchError::InvalidPath(_))),
            "{path:?}: {got:?}"
        );
    }
    for sha in ["", "main", "abc1234"] {
        let got = fetcher.fetch("docs/adr/x.md", sha).await;
        assert_eq!(got, Err(DocFetchError::InvalidSha), "{sha:?}");
    }
    assert!(matches!(
        contents_url("acme/../x", "billing", "docs/x.md", SHA),
        Err(DocFetchError::InvalidPath(_))
    ));
}

fn file_json(content: &str, encoding: &str) -> String {
    serde_json::json!({ "type": "file", "encoding": encoding, "content": content }).to_string()
}

/// #9193 Fail-Open Check: only a decoded file is text and only a plain 404 is
/// absent; a directory, a large file, a missing fork commit and an API error
/// are each an error, never text.
#[test]
fn classify_maps_each_response_shape() {
    let encoded = base64::engine::general_purpose::STANDARD.encode("ADR text\n");
    let wrapped = format!("{}\n{}", &encoded[..4], &encoded[4..]);
    assert_eq!(
        classify_response(200, &file_json(&wrapped, "base64")),
        Ok(Some("ADR text\n".to_string()))
    );
    assert_eq!(
        classify_response(404, r#"{"message":"Not Found"}"#),
        Ok(None)
    );
    assert_eq!(
        classify_response(200, r#"[{"type":"file","name":"a.md"}]"#),
        Err(DocFetchError::Directory)
    );
    assert!(matches!(
        classify_response(200, &file_json("", "none")),
        Err(DocFetchError::NoInlineContent(_))
    ));
    assert!(matches!(
        classify_response(200, r#"{"type":"symlink","target":"x"}"#),
        Err(DocFetchError::NoInlineContent(_))
    ));
    let not_utf8 = base64::engine::general_purpose::STANDARD.encode([0xff_u8, 0xfe]);
    assert!(matches!(
        classify_response(200, &file_json(&not_utf8, "base64")),
        Err(DocFetchError::Undecodable(_))
    ));
    let missing = r#"{"message":"No commit found for the ref 0123456"}"#;
    assert!(matches!(
        classify_response(404, missing),
        Err(DocFetchError::MissingCommit(_))
    ));
    assert!(matches!(
        classify_response(422, missing),
        Err(DocFetchError::MissingCommit(_))
    ));
    assert!(matches!(
        classify_response(500, "oops"),
        Err(DocFetchError::Api { status: 500, .. })
    ));
    assert!(matches!(
        classify_response(200, "not json"),
        Err(DocFetchError::Transport(_))
    ));
}
