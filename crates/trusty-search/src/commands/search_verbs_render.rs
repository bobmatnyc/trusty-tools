//! Text output for the `search_verbs` subcommands (#11625).
//!
//! Why: kept apart from the request builders so each file stays small.
//! What: [`render`] prints one line per hit; a body in an unexpected shape is
//! printed as pretty JSON rather than dropped.
//! Test: `render_lines_cover_each_verb_shape`.

use serde_json::Value;

use super::search_verbs::SearchVerb;

/// Print `body`, the result of `verb`, as text.
pub(crate) fn render(verb: &SearchVerb, body: &Value) {
    match lines(verb, body) {
        Some(lines) if lines.is_empty() => println!("(no matches)"),
        Some(lines) => lines.iter().for_each(|l| println!("{l}")),
        None => println!(
            "{}",
            serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string())
        ),
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn f(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The text lines for `body`, or `None` when it has no shape this knows.
pub(crate) fn lines(verb: &SearchVerb, body: &Value) -> Option<Vec<String>> {
    match verb {
        SearchVerb::CallChain { .. } => body.as_str().map(|t| vec![t.to_string()]),
        SearchVerb::Similar { .. } => {
            let rows = body.get("results")?.as_array()?;
            Some(
                rows.iter()
                    .map(|r| {
                        format!(
                            "{}:{}-{}  (score: {:.3})",
                            s(r, "file"),
                            n(r, "start_line"),
                            n(r, "end_line"),
                            f(r, "score")
                        )
                    })
                    .collect(),
            )
        }
        SearchVerb::Grep(g) => {
            let rows = body.get("matches")?.as_array()?;
            let mut out: Vec<String> = rows
                .iter()
                .map(|m| {
                    let tag = m
                        .get("index_id")
                        .and_then(Value::as_str)
                        .map(|id| format!("[{id}] "))
                        .unwrap_or_default();
                    if g.files_with_matches {
                        format!("{tag}{}", s(m, "file"))
                    } else {
                        format!("{tag}{}:{}:{}", s(m, "file"), n(m, "line"), s(m, "text"))
                    }
                })
                .collect();
            if body.get("truncated").and_then(Value::as_bool) == Some(true) {
                out.push(format!("(truncated at {} matches)", rows.len()));
            }
            Some(out)
        }
        SearchVerb::Typeahead { .. } => {
            let rows = body.get("hits")?.as_array()?;
            Some(
                rows.iter()
                    .map(|h| {
                        format!(
                            "{}  {}:{}  ({}, {:.3})",
                            s(h, "label"),
                            s(h, "path"),
                            n(h, "start_line"),
                            s(h, "source"),
                            f(h, "score")
                        )
                    })
                    .collect(),
            )
        }
    }
}
