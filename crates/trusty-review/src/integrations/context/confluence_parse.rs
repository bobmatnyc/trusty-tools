//! Confluence parsing + HTML-stripping helpers (Phase 6 PR-A.1, #599).
//!
//! Why: `confluence.rs` would exceed the 500-line cap once the `body.view`
//! excerpt extraction (Fix 2) landed.  This sibling module owns the pure,
//! network-free logic — the JSON response shapes, the minimal HTML-to-text
//! stripper, and the response→section mapping — so the source file keeps only
//! the transport + orchestration glue and the parsing is unit-tested in isolation.
//!
//! What: exposes `strip_html` (tag/entity-aware text extraction) and
//! `parse_section` (maps a content-search body to a `ContextSection`, embedding
//! the stripped `body.view.value` as the snippet body).
//!
//! Test: `strip_html_*`, `parse_pages_*` in this module.

use serde::Deserialize;

use super::{
    ContextSection, ContextSnippet, ContextSourceError, SNIPPET_BODY_CHARS,
    truncate_on_char_boundary,
};

/// Source identifier used in error messages produced here.
const SOURCE_NAME: &str = "confluence";

/// Strip HTML tags + decode common entities into bounded plain text.
///
/// Why: Confluence returns the page body as rendered HTML (`body.view.value`);
/// the reviewer prompt needs readable prose, not markup.  We avoid pulling a
/// full HTML parser (none is in the workspace) for what is a best-effort excerpt:
/// a small state machine drops tag spans, collapses whitespace, and decodes the
/// handful of entities that actually appear in body text.
/// What: removes everything between `<` and `>`, decodes `&amp; &lt; &gt; &quot;
/// &#39; &nbsp;`, collapses runs of whitespace to single spaces, and truncates
/// the result to `SNIPPET_BODY_CHARS` chars (char-boundary-safe).
/// Test: `strip_html_basic`, `strip_html_entities`, `strip_html_truncates`.
pub fn strip_html(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    // Decode the common entities that appear in body prose.
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // Collapse whitespace runs (HTML formatting leaves many newlines/spaces).
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_on_char_boundary(&collapsed, SNIPPET_BODY_CHARS).to_string()
}

// ─── JSON shapes ────────────────────────────────────────────────────────────

/// Top-level Confluence content-search response.
#[derive(Debug, Deserialize)]
pub struct ConfluenceSearchResponse {
    #[serde(default)]
    results: Vec<ConfluencePage>,
}

/// One Confluence page (content) result.
#[derive(Debug, Deserialize)]
struct ConfluencePage {
    #[serde(default)]
    title: String,
    #[serde(default)]
    space: Option<ConfluenceSpace>,
    #[serde(default, rename = "_links")]
    links: Option<ConfluenceLinks>,
    /// Rendered body (present when `expand=body.view` was requested).
    #[serde(default)]
    body: Option<ConfluenceBody>,
}

/// The page's space (we render its name/key).
#[derive(Debug, Deserialize)]
struct ConfluenceSpace {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

/// The page's `_links` block (we use `webui` to build the canonical URL).
#[derive(Debug, Deserialize)]
struct ConfluenceLinks {
    #[serde(default)]
    webui: Option<String>,
}

/// The page's expanded `body` (we read the `view` rendition's HTML `value`).
#[derive(Debug, Deserialize)]
struct ConfluenceBody {
    #[serde(default)]
    view: Option<ConfluenceBodyView>,
}

/// The `view` rendition carrying rendered HTML in `value`.
#[derive(Debug, Deserialize)]
struct ConfluenceBodyView {
    #[serde(default)]
    value: Option<String>,
}

/// Parse a Confluence content-search body into a `ContextSection`.
///
/// Why: separate parsing from the network call for unit-testability, and embed a
/// stripped excerpt of each page's body (Fix 2) so the model sees the documented
/// design intent — not just the page title (incumbent `pr_review_service.py:4848`).
/// What: maps each page to a `ContextSnippet` (title, subtitle = space name or
/// key, body = stripped `body.view.value` excerpt, link = `{base}/wiki{webui}`),
/// wrapped in a `Related Confluence docs` section.
/// Test: `parse_pages_to_section`, `parse_embeds_body_excerpt`,
/// `parse_error_on_garbage`.
pub fn parse_section(body: &str, base_url: &str) -> Result<ContextSection, ContextSourceError> {
    let resp: ConfluenceSearchResponse =
        serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
            src: SOURCE_NAME,
            detail: e.to_string(),
        })?;
    let snippets = resp
        .results
        .into_iter()
        .map(|page| {
            let subtitle = page
                .space
                .and_then(|s| s.name.or(s.key))
                .map(|s| format!("space: {s}"));
            let link = page
                .links
                .and_then(|l| l.webui)
                .map(|webui| format!("{base_url}/wiki{webui}"));
            let body_excerpt = page
                .body
                .and_then(|b| b.view)
                .and_then(|v| v.value)
                .map(|html| strip_html(&html))
                .filter(|s| !s.is_empty());
            ContextSnippet {
                title: page.title,
                subtitle,
                body: body_excerpt,
                link,
            }
        })
        .collect();
    Ok(ContextSection {
        heading: "Related Confluence docs".to_string(),
        snippets,
    })
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_basic() {
        let html = "<p>Hello <b>world</b></p>";
        assert_eq!(strip_html(html), "Hello world");
    }

    #[test]
    fn strip_html_collapses_whitespace() {
        let html = "<div>\n  line one\n\n  line two  </div>";
        assert_eq!(strip_html(html), "line one line two");
    }

    #[test]
    fn strip_html_entities() {
        let html = "<p>a &amp; b &lt;tag&gt; &quot;q&quot; &#39;x&#39;&nbsp;end</p>";
        assert_eq!(strip_html(html), "a & b <tag> \"q\" 'x' end");
    }

    #[test]
    fn strip_html_truncates() {
        let html = format!("<p>{}</p>", "x".repeat(SNIPPET_BODY_CHARS + 100));
        assert_eq!(strip_html(&html).chars().count(), SNIPPET_BODY_CHARS);
    }

    #[test]
    fn parse_pages_to_section() {
        let body = r#"{
            "results": [
                {"title": "Auth Architecture", "space": {"name": "Engineering"},
                 "_links": {"webui": "/spaces/ENG/pages/123/Auth"}},
                {"title": "Sessions", "space": {"key": "ENG"}}
            ]
        }"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(section.heading, "Related Confluence docs");
        assert_eq!(section.snippets.len(), 2);
        assert_eq!(section.snippets[0].title, "Auth Architecture");
        assert_eq!(
            section.snippets[0].subtitle.as_deref(),
            Some("space: Engineering")
        );
        assert_eq!(
            section.snippets[0].link.as_deref(),
            Some("https://acme.atlassian.net/wiki/spaces/ENG/pages/123/Auth")
        );
        // No body expanded → no body.
        assert!(section.snippets[0].body.is_none());
        // Second page has only a space key and no link.
        assert_eq!(section.snippets[1].subtitle.as_deref(), Some("space: ENG"));
        assert!(section.snippets[1].link.is_none());
    }

    #[test]
    fn parse_embeds_body_excerpt() {
        // Fix 2: `body.view.value` HTML is stripped and embedded as the body.
        let body = r#"{
            "results": [
                {"title": "Design", "space": {"name": "Eng"},
                 "body": {"view": {"value": "<p>The <b>session</b> token expires.</p>"}}}
            ]
        }"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(
            section.snippets[0].body.as_deref(),
            Some("The session token expires.")
        );
    }

    #[test]
    fn parse_error_on_garbage() {
        let r = parse_section("xx", "https://acme.atlassian.net");
        assert!(matches!(r, Err(ContextSourceError::Parse { .. })));
    }
}
