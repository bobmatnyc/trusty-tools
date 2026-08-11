//! The same corpus, measured with an operator marker file active (#5414).
//!
//! Why: `agentic_detection_corpus.rs` proves the shipped set holds at ~91% on
//! this repo's real history. It cannot prove the configurable half, because the
//! marker set is a process-global `OnceLock` — one process observes one
//! configuration. This binary is the second process.
//! What: points `TGA_AI_MARKERS` at a file carrying one deliberately synthetic
//! probe marker, walks the full history, and checks that every commit the probe
//! claims is one the shipped markers did not already account for. That is the
//! "+N" half of #5414's acceptance: the share stays >= 91% and rises by a
//! measurable, attributable N.
//! Test: this file. Ignored by default — it walks the surrounding checkout.
//!
//! The probe is labelled `synthetic-probe` and keys on a human co-author
//! trailer, which is NOT a claim that those commits are agentic. It is a
//! measurable string in this corpus chosen so N is verifiable; a real operator
//! would write their own house footer here.

use tga::collect::ai_markers::{detect, detection_disclosure, CommitSignals};

struct Commit {
    message: String,
    author_email: String,
    committer_email: String,
}

/// The two markers this corpus actually contains, counted independently of the
/// detector — the same known-marker baseline `agentic_detection_corpus.rs`
/// uses, and no more a ground truth here than it is there.
fn matches_known_marker(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("generated with trusty-mpm")
        || lower.contains("generated with [trusty-mpm")
        || lower
            .lines()
            .any(|l| l.starts_with("co-authored-by:") && l.contains("claude"))
}

fn history() -> Option<Vec<Commit>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = git2::Repository::open(root).ok()?;
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

#[test]
#[ignore = "walks the surrounding trusty-tools checkout; run with --include-ignored"]
fn a_configured_marker_raises_the_catch_rate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ai-markers.yaml");
    std::fs::write(
        &path,
        "markers:\n\
         \x20 - tool: synthetic-probe\n\
         \x20   mode: full_agentic\n\
         \x20   scope: trailer\n\
         \x20   pattern: '(?i)Bob\\s+Matsuoka'\n",
    )
    .expect("writes the marker file");
    std::env::set_var("TGA_AI_MARKERS", &path);

    let Some(commits) = history() else {
        panic!("expected a git checkout at the workspace root");
    };
    assert!(
        commits.len() > 2000,
        "expected the full trusty-tools history, got {} commits",
        commits.len()
    );

    let mut baseline = 0usize;
    let mut detected = 0usize;
    let mut probe = 0usize;

    for c in &commits {
        let known = matches_known_marker(&c.message);
        if known {
            baseline += 1;
        }
        let d = detect(&CommitSignals {
            message: &c.message,
            author_email: &c.author_email,
            committer_email: &c.committer_email,
        });
        if let Some(tool) = d.tool {
            detected += 1;
            if tool == "synthetic-probe" {
                probe += 1;
                // The point of the exercise: the operator marker only ever
                // claims commits the shipped set left unclassified. Appending
                // rather than interleaving is what guarantees this.
                assert!(
                    !known,
                    "an operator marker must not take a commit the shipped markers already own"
                );
            }
        }
    }

    let total = commits.len();
    let pct = |n: usize| n as f64 * 100.0 / total as f64;
    println!("{}", detection_disclosure());
    println!(
        "commits={total} known_marker_baseline={baseline} ({:.2}%) \
         configured={detected} ({:.2}%) added_by_operator_marker={probe}",
        pct(baseline),
        pct(detected)
    );

    assert!(
        pct(detected) >= 91.0,
        "configured share must hold at or above the shipped 91%; got {:.2}%",
        pct(detected)
    );
    assert!(
        probe > 0,
        "the operator marker must contribute detections the shipped set did not have"
    );
    assert!(
        detected > baseline,
        "configured detection must exceed the known-marker baseline: {detected} vs {baseline}"
    );
}
