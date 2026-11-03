//! The VERIFY evidence scanner (§3.5): which observed text proves a task done.
//!
//! Why: the scanner is the trust boundary of the goal Done gate. #10721 found it
//! accepted a failed test run, "wrote nothing" and any PR URL as proof, so a
//! failed delegation closed its goal DONE. The rules here are deterministic and
//! fail closed: text the scanner cannot tie to a real, successful result is not
//! evidence.
//! What: [`TaskExpectation`] (what the delegated task asked for) and
//! [`scan_evidence`] (observed text + expectation → evidence snippet or `None`).
//! Test: `observe_tests.rs` — the positive scans plus the #10721 negatives.

/// What a delegated task asked for, used to tie observed evidence to that task.
///
/// Why: a PR URL proves nothing unless it is the PR this delegation was meant to
/// open or touch (#10721 W1), and text the SM itself typed into the pane (the
/// delivered task) must never count as the session's own output.
/// What: the delivered task text and the PR references it names. `Default` is
/// "no task known": no PR URL can verify and nothing is filtered as an echo.
/// Test: `observe_tests.rs::pr_url_matching_the_task_pr_is_verified`,
/// `pr_url_echoed_from_the_task_is_not_verified`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskExpectation {
    task: String,
    expected_prs: Vec<PrRef>,
}

impl TaskExpectation {
    /// Build the expectation for a task from its delivered prompt text.
    pub fn from_task(task: &str) -> Self {
        Self {
            task: task.to_string(),
            expected_prs: pr_urls(task)
                .iter()
                .filter_map(|u| PrRef::parse(u))
                .collect(),
        }
    }

    /// The delivered task text (empty when unknown).
    pub(super) fn task(&self) -> &str {
        &self.task
    }
}

/// One pull/merge request identity: host, project path and number, lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrRef {
    host: String,
    project: String,
    number: u64,
}

impl PrRef {
    /// Parse a PR-shaped URL (`…/owner/repo/pull/N…` or `…/-/merge_requests/N…`).
    fn parse(url: &str) -> Option<Self> {
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        let (project, tail) = path
            .split_once("/pull/")
            .or_else(|| path.split_once("/merge_requests/"))?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        Some(Self {
            host: host.to_ascii_lowercase(),
            project: project.trim_end_matches("/-").to_ascii_lowercase(),
            number: digits.parse().ok()?,
        })
    }
}

/// Scan observed pane/record text for gate-satisfying evidence (§3.5).
///
/// Why: the gate forbids `Verified` without OBSERVED evidence; #10721 made each
/// rule demand a real, successful result tied to this task.
/// What: returns `None` when any test summary in `observed` reports a failure (a
/// failure marker beats evidence). Otherwise returns the first match, in priority
/// order: (1) a PR URL that is a PR the task named, or one `gh pr create` reports
/// creating in the observed text; (2) a test summary with a positive pass count
/// and zero failures; (3) a `diff --git` header or a write phrase that names a
/// written path. Undecidable text → `None`. The caller removes task echoes first
/// ([`drop_task_echo`]).
/// Test: `observe_tests.rs::scan_finds_pr_url`, `scan_finds_test_pass`,
/// `scan_finds_diff`, `scan_finds_nothing`,
/// `zero_passed_three_failed_is_not_test_evidence`,
/// `wrote_nothing_is_not_write_evidence`, `failing_summary_vetoes_other_evidence`.
pub fn scan_evidence(observed: &str, expect: &TaskExpectation) -> Option<String> {
    // #10721: a failing test summary anywhere in the observed text wins.
    if observed
        .lines()
        .any(|l| classify_test_summary(l) == Some(TestSummary::Fail))
    {
        return None;
    }
    if let Some(url) = find_pr_url(observed, expect) {
        return Some(format!("PR opened: {url}"));
    }
    if let Some(pass) = observed
        .lines()
        .find(|l| classify_test_summary(l) == Some(TestSummary::Pass))
    {
        return Some(format!("tests pass: {}", pass.trim()));
    }
    if let Some(diff) = observed.lines().find(|l| names_a_write(l)) {
        return Some(format!("edit made: {}", diff.trim()));
    }
    None
}

/// Remove pane lines that are an echo of the delivered task text.
///
/// Why: #10721 W1 provenance — the SM types the task into the pane, so a PR URL
/// (or any marker) in the task would otherwise prove itself.
/// What: strips prompt decoration (`>`, `│`, `❯`, whitespace) from each line and
/// drops it when the remainder is non-empty and occurs inside the task text, so
/// a wrapped echo is caught too. A bare-URL line equal to the task's URL is
/// therefore dropped as well (fail closed). With no task known, the text is
/// returned unchanged.
/// Test: `observe_tests.rs::pr_url_echoed_from_the_task_is_not_verified`.
pub(super) fn drop_task_echo(text: &str, task: &str) -> String {
    if task.trim().is_empty() {
        return text.to_string();
    }
    let decoration = |c: char| c.is_whitespace() || matches!(c, '>' | '│' | '❯' | '›' | '|');
    text.lines()
        .filter(|line| {
            let core = line.trim_matches(decoration);
            core.is_empty() || !task.contains(core)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every PR/MR-shaped URL in `text`, in order.
///
/// Why: both the task (expected PRs) and the observed text (claimed PRs) are
/// scanned the same way, so the two sides compare like with like.
/// What: finds each `http` span (bounded by whitespace or JSON/sentence framing,
/// trailing `.` stripped) carrying a `github.com …/pull/` or
/// `gitlab …/merge_requests/` path.
/// Test: `observe_tests.rs::scan_finds_pr_url`, `scan_pr_url_strips_json_framing`.
fn pr_urls(text: &str) -> Vec<String> {
    let is_boundary = |c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '}' | '{' | ']' | '[' | '(' | ')' | ',')
    };
    let mut urls = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find("http") {
        let start = search_from + rel;
        let len = text[start..]
            .find(is_boundary)
            .unwrap_or(text.len() - start);
        let end = start + len;
        let candidate = text[start..end].trim_end_matches('.');
        if (candidate.contains("github.com") && candidate.contains("/pull/"))
            || (candidate.contains("gitlab") && candidate.contains("/merge_requests/"))
        {
            urls.push(candidate.to_string());
        }
        search_from = end.max(start + 1);
    }
    urls
}

/// Find a PR URL that this task is entitled to claim.
///
/// Why: #10721 W1 — a URL is evidence only when it identifies the PR the
/// delegation was meant to open or touch, never an arbitrary or different PR.
/// What: returns the first observed PR URL whose identity equals a PR the task
/// named, or that a `gh pr create` line in the observed text reports creating.
/// Test: `observe_tests.rs::pr_url_for_a_different_pr_is_not_verified`,
/// `pr_url_matching_the_task_pr_is_verified`, `pr_created_in_session_is_verified`.
fn find_pr_url(observed: &str, expect: &TaskExpectation) -> Option<String> {
    let created = created_pr_projects(observed);
    pr_urls(observed).into_iter().find(|url| {
        PrRef::parse(url).is_some_and(|pr| {
            expect.expected_prs.contains(&pr)
                || (pr.host == "github.com" && created.contains(&pr.project))
        })
    })
}

/// Repos (`owner/repo`, lowercased) that `gh pr create` reports opening a PR in.
///
/// Why: a PR the session creates has no number the task could name in advance;
/// `gh pr create`'s own status line is the in-session provenance for it.
/// What: collects the trailing `owner/repo` of each
/// `Creating pull request for <head> into <base> in <owner/repo>` line.
/// Test: `observe_tests.rs::pr_created_in_session_is_verified`.
fn created_pr_projects(observed: &str) -> Vec<String> {
    observed
        .lines()
        .filter_map(|line| {
            let lower = line.trim().to_ascii_lowercase();
            let rest = lower.strip_prefix("creating pull request for ")?;
            let (_, repo) = rest.rsplit_once(" in ")?;
            let repo = repo.trim().trim_end_matches('.');
            repo.contains('/').then(|| repo.to_string())
        })
        .collect()
}

/// The verdict a test-run summary line carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestSummary {
    /// A positive pass count with zero failures.
    Pass,
    /// Any failure count above zero, or a failure word with no count.
    Fail,
}

/// Classify one line as a passing or failing test summary, or neither.
///
/// Why: #10721 W2 — "a digit plus `passed`" accepted `0 passed, 3 failed`. The
/// counts must be parsed: a pass needs a positive count and zero failures.
/// What: a line is a summary when it carries `test result:` or a `passed` word.
/// `test result: failed`, a failure word (`failed`, `failures`, `errors`, …)
/// bound to a count above zero, or one with no count at all → `Fail`. Otherwise a
/// summed `passed` count above zero → `Pass`; zero passes → `None` (no evidence,
/// no veto, so cargo's empty `0 passed; 0 failed` targets stay neutral).
/// Test: `observe_tests.rs::zero_passed_three_failed_is_not_test_evidence`,
/// `scan_finds_test_pass`, `failing_summary_vetoes_other_evidence`.
fn classify_test_summary(line: &str) -> Option<TestSummary> {
    let lower = line.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if !lower.contains("test result:") && !tokens.contains(&"passed") {
        return None;
    }
    if lower.contains("test result: failed") {
        return Some(TestSummary::Fail);
    }
    // The count bound to the word at `i`: the token before it, or before `tests`.
    let count_at = |i: usize| -> Option<u64> {
        let prev = tokens.get(i.checked_sub(1)?)?;
        if matches!(*prev, "test" | "tests") {
            return tokens.get(i.checked_sub(2)?)?.parse().ok();
        }
        prev.parse().ok()
    };
    let mut passed = 0u64;
    for (i, tok) in tokens.iter().enumerate() {
        match *tok {
            "passed" => passed += count_at(i).unwrap_or(0),
            "failed" | "failures" | "failure" | "failing" | "errors" | "error" | "errored"
                if count_at(i).is_none_or(|n| n > 0) =>
            {
                return Some(TestSummary::Fail);
            }
            _ => {}
        }
    }
    (passed > 0).then_some(TestSummary::Pass)
}

/// Whether `line` reports a real write: a diff header or a write naming a path.
///
/// Why: #10721 W3 — any `wrote ` accepted "wrote nothing" and "I never wrote the
/// requested file". A write claim must name the change it made.
/// What: `diff --git` → true. Otherwise for each `wrote ` / `updated file` /
/// `created file` phrase not negated in the three words before it, true when the
/// next token is a path, or is a positive count followed within two tokens by
/// `to <path>` (Claude Code's `Wrote 42 lines to src/x.rs`).
/// Test: `observe_tests.rs::scan_finds_diff`, `wrote_nothing_is_not_write_evidence`,
/// `write_naming_a_path_is_write_evidence`.
fn names_a_write(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("diff --git") {
        return true;
    }
    ["wrote ", "updated file", "created file"]
        .iter()
        .any(|phrase| {
            lower.match_indices(phrase).any(|(idx, _)| {
                let negated = lower[..idx].split_whitespace().rev().take(3).any(|w| {
                    matches!(w, "never" | "not" | "no" | "without" | "unable") || w.ends_with("n't")
                });
                let tokens: Vec<&str> = lower[idx + phrase.len()..]
                    .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
                    .split_whitespace()
                    .collect();
                !negated && names_written_target(&tokens)
            })
        })
}

/// Whether the tokens after a write phrase name the written path.
fn names_written_target(tokens: &[&str]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    if is_path_like(first) {
        return true;
    }
    let positive_count = first.parse::<u64>().is_ok_and(|n| n > 0);
    positive_count
        && tokens
            .windows(2)
            .skip(1)
            .take(2)
            .any(|w| w[0] == "to" && is_path_like(w[1]))
}

/// Whether a token looks like a file path (`a/b`, `name.ext`), not a prose word.
fn is_path_like(token: &str) -> bool {
    let t = token
        .trim_start_matches(['(', '`', '"', '\''])
        .trim_end_matches(['.', ',', ':', ';', ')', '`', '"', '\'']);
    if t.is_empty() || t.starts_with("http") {
        return false;
    }
    if t.contains('/') {
        return t.chars().any(|c| c.is_ascii_alphanumeric());
    }
    t.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=8).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    })
}
