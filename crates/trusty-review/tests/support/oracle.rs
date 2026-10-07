//! The independent citation resolver shared by the hallucination corpus and
//! the model-eval harness (#9188).
//!
//! Why: a survivor is a hallucination when its citation does not resolve at
//! the head. The resolver is written apart from the citation gate, so a run
//! does not grade the gate with the gate's own code. It lives here, outside
//! `src/`, so both the corpus test (`src/pipeline/
//! runner_hallucination_corpus_tests.rs`, by `#[path]`) and
//! `tests/model_eval.rs` use one copy.
//! What: [`diff_lines`] reads a diff's new-side and removed text by line;
//! [`resolves`] decides whether one finding's `file`, `line` and quoted code
//! hold there, and (#9193) whether each `[doc: path@sha — "excerpt"]` quotes
//! the [`Docs`] text at the head. Plain `std` only, so it compiles in both
//! crates.
//! Test: `hallucination_count_is_zero`, `bad_recording_scores_hallucinations`.

use std::collections::HashMap;

/// Collapse every whitespace run to one space.
pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Doc text a `[doc:]` citation may quote: by path, at commit `head` (#9193).
#[derive(Debug, Default, Clone)]
pub struct Docs {
    /// The full head SHA the docs were read at.
    pub head: String,
    /// Doc text by repository path.
    pub text: HashMap<String, String>,
}

/// Whether every `[doc: path@sha — "excerpt"]` in `description` names a doc
/// in `docs`, a 7+ character hex prefix of `docs.head`, and excerpts that
/// occur in that doc. Written apart from the gate's parser (#9193).
fn docs_resolve(description: &str, docs: &Docs) -> bool {
    let mut rest = description;
    while let Some(at) = rest.find("[doc:") {
        let after = &rest[at + 5..];
        let Some(end) = after.find(']') else {
            return false;
        };
        let body = &after[..end];
        rest = &after[end..];
        let token = body.split('"').next().unwrap_or("");
        let token = token.trim().trim_end_matches(['—', '–', '-']).trim();
        let Some((path, sha)) = token.rsplit_once('@') else {
            return false;
        };
        let sha = sha.to_lowercase();
        let quotes: Vec<String> = body.split('"').skip(1).step_by(2).map(norm).collect();
        let Some(text) = docs.text.get(path).map(|t| norm(t)) else {
            return false;
        };
        let at_head = sha.len() >= 7
            && sha.chars().all(|c| c.is_ascii_hexdigit())
            && docs.head.starts_with(&sha);
        if !at_head
            || body.matches('"').count() % 2 != 0
            || quotes.is_empty()
            || !quotes.iter().all(|q| text.contains(q.as_str()))
        {
            return false;
        }
    }
    true
}

/// Line-numbered text of every file in a diff, by new-side line number.
pub type Lines = HashMap<String, HashMap<u32, String>>;

/// New-side text of every file in `diff`, and the removed (base) text placed
/// at the new-side position of its deletion: the next new-side line, or the
/// hunk's last one for a trailing deletion.
pub fn diff_lines(diff: &str) -> (Lines, Lines) {
    let (mut head, mut base) = (Lines::new(), Lines::new());
    let (mut file, mut next, mut pending) = (String::new(), 0u32, Vec::new());
    let mut flush = |file: &str, at: u32, pending: &mut Vec<String>| {
        if !pending.is_empty() {
            let slot: &mut String = base
                .entry(file.to_string())
                .or_default()
                .entry(at)
                .or_default();
            *slot = norm(&format!("{slot} {}", pending.join(" ")));
            pending.clear();
        }
    };
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            flush(&file, next.saturating_sub(1), &mut pending);
            let new = rest.split_whitespace().find_map(|t| t.strip_prefix('+'));
            next = new
                .and_then(|n| n.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if let Some(body) = line.strip_prefix("-").filter(|_| !line.starts_with("---")) {
            pending.push(norm(body));
        } else if let Some(body) = line.strip_prefix('+').or_else(|| line.strip_prefix(' ')) {
            flush(&file, next, &mut pending);
            head.entry(file.clone())
                .or_default()
                .insert(next, norm(body));
            next += 1;
        }
    }
    flush(&file, next.saturating_sub(1), &mut pending);
    (head, base)
}

/// The lines of `file` in `lines`, matched as the gate's caller cites it.
pub fn file_lines<'a>(lines: &'a Lines, file: &str) -> Option<&'a HashMap<u32, String>> {
    lines
        .iter()
        .find(|(k, _)| **k == file || k.ends_with(&format!("/{file}")))
        .map(|(_, v)| v)
}

/// Whether a finding resolves at the head: its `file` is there, its `line`
/// exists, every backtick code span in `description` is in that file, and one
/// of them is on the cited line. A finding that quotes no code never resolves.
/// A finding about a removal (`removal`, by ground truth) also resolves its
/// quotes against the removed lines, placed at their deletion's position.
/// #9193: every `[doc:]` citation must also resolve in `docs`.
pub fn resolves(
    file: &str,
    line: Option<u32>,
    description: &str,
    (head, base): &(Lines, Lines),
    removal: bool,
    docs: &Docs,
) -> bool {
    if !docs_resolve(description, docs) {
        return false;
    }
    let Some(lines) = file_lines(head, file) else {
        return false;
    };
    let removed = file_lines(base, file).filter(|_| removal);
    let Some(mut on_line) = line.and_then(|l| lines.get(&l)).cloned() else {
        return false;
    };
    let mut whole: String = lines.values().cloned().collect::<Vec<_>>().join(" ");
    if let Some(removed) = removed {
        whole = format!(
            "{whole} {}",
            removed.values().cloned().collect::<Vec<_>>().join(" ")
        );
        if let Some(gone) = line.and_then(|l| removed.get(&l)) {
            on_line = format!("{on_line} {gone}");
        }
    }
    let spans: Vec<String> = description
        .split('`')
        .skip(1)
        .step_by(2)
        .map(norm)
        .filter(|s| s.len() >= 3 && !s.contains(".rs:"))
        .collect();
    !spans.is_empty()
        && spans.iter().all(|s| whole.contains(s.as_str()))
        && spans.iter().any(|s| on_line.contains(s.as_str()))
}

/// Whether a review with no survivor let a withheld finding block it or shape
/// its grade. AQ-7t (Bob 2026-10-05): an approving verdict keeps the grade its
/// band gives an empty survivor set; UNKNOWN carries none. #9310 (Architect
/// ruling 2026-10-06 16:50Z): a `suppressed_reject` review is REQUEST_CHANGES
/// on the reviewer's own rejection, not on a withheld finding, and carries the
/// best grade of that band. `verdict` and `status` are as `run --json` prints
/// them.
pub fn withheld_shapes_verdict(
    survivors: usize,
    withheld: usize,
    verdict: &str,
    status: Option<&str>,
    grade: Option<&str>,
) -> bool {
    let suppressed = status == Some("suppressed_reject") && verdict == "REQUEST_CHANGES";
    let survivorless_grade = match verdict {
        "APPROVE" => Some("A+"),
        "APPROVE*" => Some("C+"),
        "REQUEST_CHANGES" if suppressed => Some("D+"),
        _ => None,
    };
    let blocks = matches!(verdict, "REQUEST_CHANGES" | "BLOCK") && !suppressed;
    survivors == 0 && withheld > 0 && (blocks || grade != survivorless_grade)
}
