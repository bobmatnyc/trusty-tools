//! Agentic-detection catch rate measured against real commit history (#5249).
//!
//! Why: every detection test before #5249 was a synthetic one-liner, so the
//! 41.6-point undercount was invisible to the suite — each individual pattern
//! worked, and the set as a whole missed 978 commits. This measures detection
//! the way the report consumes it: as a share of a real corpus.
//!
//! What this corpus can and cannot prove: trusty-tools' history contains
//! exactly two agentic markers — the trusty-mpm footer and Claude
//! `Co-Authored-By:` trailers. The Devin, OpenHands, Aider, Copilot and Cursor
//! markers have ZERO occurrences here, so no assertion below exercises them;
//! they are covered only by the synthetic unit tests in
//! `collect::ai_markers::tests`. The run prints the active tool list beside the
//! observed per-tool counts so that gap is visible in the output rather than
//! implied by a passing test.
//!
//! Test: this file. Ignored by default because it depends on the surrounding
//! checkout's history; run with
//! `cargo test -p tga --test agentic_detection_corpus -- --include-ignored --nocapture`.

use std::collections::BTreeMap;

use git2::Repository;
use regex::Regex;
use tga::collect::ai_attribution::AgenticMode;
use tga::collect::ai_markers::{detect, detection_disclosure, CommitSignals};

struct Commit {
    message: String,
    author_email: String,
    committer_email: String,
}

/// The detector exactly as it stood before #5249, frozen.
///
/// Why: the "before" number has to come from the old behaviour, and the old
/// code no longer exists to call. This is a transcription of the seven
/// patterns in `ai_attribution.rs` at commit 31ea3250 — a historical constant,
/// not a second implementation of anything shipping.
struct LegacyDetector {
    trailer_line: Regex,
    claude_trailer: Regex,
    ide_trailer: Regex,
    generated_with_claude_code: Regex,
    x_ai: Regex,
}

impl LegacyDetector {
    fn new() -> Self {
        Self {
            trailer_line: Regex::new(r"(?im)^[Cc]o-[Aa]uthored-[Bb]y:\s*(.+)$").expect("compiles"),
            claude_trailer: Regex::new(r"(?i)\bclaude\b").expect("compiles"),
            ide_trailer: Regex::new(r"(?i)\bcopilot\b|GitHub\s+Copilot|@cursor\.sh|\bCursor\b")
                .expect("compiles"),
            generated_with_claude_code: Regex::new(r"(?i)Generated\s+with\s+Claude\s+Code")
                .expect("compiles"),
            x_ai: Regex::new(r"(?im)^X-AI-(?:Tokens-(?:In|Out)|Model):\s*\S").expect("compiles"),
        }
    }

    fn detects(&self, message: &str) -> bool {
        for caps in self.trailer_line.captures_iter(message) {
            let v = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if self.claude_trailer.is_match(v) || self.ide_trailer.is_match(v) {
                return true;
            }
        }
        self.generated_with_claude_code.is_match(message) || self.x_ai.is_match(message)
    }
}

/// The two markers this corpus actually contains, counted independently of the
/// detector.
///
/// This is a KNOWN-MARKER BASELINE, not a ground truth: it says nothing about
/// commits whose markers were stripped, and it is blind to the ten markers
/// with no occurrences in this history. It bounds how much of the detector's
/// output this corpus can explain, nothing more.
fn matches_known_marker(c: &Commit) -> bool {
    let lower = c.message.to_lowercase();
    lower.contains("generated with trusty-mpm")
        || lower
            .lines()
            .any(|l| l.starts_with("co-authored-by:") && l.contains("claude"))
}

fn history() -> Option<Vec<Commit>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = Repository::open(&root).ok()?;
    let mut walk = repo.revwalk().ok()?;
    walk.push_head().ok()?;
    let mut out = Vec::new();
    for oid in walk {
        let oid = oid.ok()?;
        let c = repo.find_commit(oid).ok()?;
        out.push(Commit {
            message: c.message().unwrap_or("").to_string(),
            author_email: c.author().email().unwrap_or("").to_string(),
            committer_email: c.committer().email().unwrap_or("").to_string(),
        });
    }
    Some(out)
}

/// #5249 acceptance: detected agentic share must rise from ~48% toward the
/// ~91% the two known markers account for.
#[test]
#[ignore = "walks the surrounding trusty-tools checkout; run with --include-ignored"]
fn catch_rate_on_trusty_tools_history() {
    let Some(commits) = history() else {
        panic!("expected a git checkout at the workspace root");
    };
    assert!(
        commits.len() > 2000,
        "expected the full trusty-tools history, got {} commits",
        commits.len()
    );

    let legacy = LegacyDetector::new();
    let mut baseline = 0usize;
    let mut before = 0usize;
    let mut after = 0usize;
    let mut beyond_baseline = 0usize;
    // #5250: how much of the marker-less remainder is a message git or the
    // forge composed, rather than one an author wrote.
    let mut unknown = 0usize;
    let mut by_tool: BTreeMap<&str, usize> = BTreeMap::new();

    for c in &commits {
        let signals = CommitSignals {
            message: &c.message,
            author_email: &c.author_email,
            committer_email: &c.committer_email,
        };
        let known = matches_known_marker(c);
        let d = detect(&signals);
        if known {
            baseline += 1;
        }
        if legacy.detects(&c.message) {
            before += 1;
        }
        if let Some(tool) = d.tool {
            after += 1;
            *by_tool.entry(tool).or_default() += 1;
            if !known {
                beyond_baseline += 1;
            }
        }
        if d.mode == AgenticMode::Unknown {
            unknown += 1;
        }
    }

    let total = commits.len();
    let pct = |n: usize| n as f64 * 100.0 / total as f64;

    // The active tool list printed beside the observed counts is what makes
    // this corpus's reach legible: a tool named in the first line with no
    // entry in the second has zero occurrences here and is proven only by the
    // synthetic unit tests.
    println!("{}", detection_disclosure());
    println!(
        "commits={total} known_marker_baseline={baseline} ({:.2}%) \
         before={before} ({:.2}%) after={after} ({:.2}%) beyond_baseline={beyond_baseline}",
        pct(baseline),
        pct(before),
        pct(after)
    );
    let observed: Vec<String> = by_tool.iter().map(|(t, n)| format!("{t}={n}")).collect();
    println!("detections by tool: {}", observed.join(" "));
    println!(
        "#5250 unknown (rewrite fingerprint, no marker) = {unknown} ({:.2}% of all commits, \
         {:.2}% of the {} with no marker)",
        pct(unknown),
        unknown as f64 * 100.0 / (total - after) as f64,
        total - after
    );

    // #5250: `unknown` is a refinement of the marker-less remainder, so it can
    // never exceed it. A predicate that started claiming a large share of the
    // whole corpus would have stopped being the narrow, mechanism-backed rule
    // this issue asked for.
    assert!(
        unknown <= total - after,
        "unknown must be a subset of the marker-less commits: {unknown} of {}",
        total - after
    );
    assert!(
        pct(unknown) < 15.0,
        "the rewrite-fingerprint predicate must stay narrow; got {:.2}% of all commits",
        pct(unknown)
    );

    assert!(
        pct(after) >= 85.0,
        "detected share must approach the known-marker baseline; got {:.2}% (before {:.2}%)",
        pct(after),
        pct(before)
    );
    assert!(
        after > before,
        "the shipped set must catch strictly more than the pre-#5249 set"
    );
    // Detections outside the baseline are markers it does not model (the
    // `Generated with Claude Code` footer, `X-AI-*` trailers). They must stay a
    // small tail, not a flood.
    assert!(
        pct(beyond_baseline) < 10.0,
        "detections beyond the two known markers must stay marginal; got {:.2}%",
        pct(beyond_baseline)
    );
    // The house footer is the marker this issue is about; if the corpus stops
    // containing it, the headline number above stops meaning anything.
    assert!(
        by_tool.get("trusty-mpm").copied().unwrap_or(0) > 900,
        "the corpus must still contain the house footer this issue is about: {by_tool:?}"
    );
}
