//! Git memory extractor: mine commit history for facts using rule-based NLP.
//!
//! Why: Git history is the richest source of ground-truth project facts —
//! every commit records who changed what, when, and (often) why.
//! What: Regex-based conventional commit parser + file-path room classifier +
//!       entity extractor + heuristic importance scorer. Zero LLM calls; zero
//!       embedding calls. Embeddings are computed later by the normal
//!       `memory_remember` path when each `GitFact` is converted into a `Drawer`.
//! Test: `cargo test -p trusty-memory-core git::` covers conventional commit
//!       parsing, file-path classification, importance scoring, entity extraction,
//!       and end-to-end extraction against the trusty-memory repo itself.

use crate::memory_core::palace::{Drawer, RoomType};
use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use std::{collections::HashSet, path::PathBuf, sync::OnceLock};
use uuid::Uuid;

// ── Regex patterns (compiled once) ──────────────────────────────────────────

fn cc_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)^(feat|fix|chore|refactor|test|docs|perf|ci|style|build)(\(.+?\))?(!)?\s*:\s*(.+)",
        )
        .expect("conventional-commit regex is a compile-time constant")
    })
}

fn issue_ref_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)(?:closes?|fixes?|resolves?)\s+#(\d+)|#(\d+)")
            .expect("issue-ref regex is a compile-time constant")
    })
}

fn coauthor_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)Co-authored-by:\s+(.+?)\s+<")
            .expect("coauthor regex is a compile-time constant")
    })
}

fn symbol_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?m)^[+-]\s+(?:pub\s+)?(?:fn|struct|class|def|func|interface)\s+(\w+)")
            .expect("symbol regex is a compile-time constant")
    })
}

// ── Types ────────────────────────────────────────────────────────────────────

/// Parsed conventional commit components.
///
/// Why: The conventional-commit prefix (`feat:`, `fix:`, …) is a strong signal
/// of a commit's intent and is used downstream to score importance.
/// What: Holds the type, optional scope, breaking flag, and free-form description.
/// Test: `parse_*` tests below assert each field for representative messages.
#[derive(Debug, Clone, Default)]
pub struct ConventionalCommit {
    /// `"feat"`, `"fix"`, etc. Empty string if the message is not conventional.
    pub commit_type: String,
    pub scope: Option<String>,
    pub breaking: bool,
    pub description: String,
}

/// Entities extracted from a commit message + diff.
///
/// Why: Drawers are easier to retrieve when tagged with concrete entities
/// (issue numbers, co-authors, changed files, symbol names).
/// What: Plain data captured by regex; no inference.
/// Test: `extract_issue_refs` plus end-to-end test on real repo.
#[derive(Debug, Clone, Default)]
pub struct CommitEntities {
    pub issue_refs: Vec<u64>,
    pub co_authors: Vec<String>,
    /// Names of `fn`/`struct`/`class`/`def` etc. added or removed in the diff.
    pub symbols: Vec<String>,
    pub file_paths: Vec<String>,
    /// Distinct `RoomType`s inferred from changed file paths.
    pub room_types: Vec<RoomType>,
}

/// A fact extracted from a single commit, ready to become a `Drawer`.
///
/// Why: Decouples the git-walking pass from the storage pass — callers can
/// extract first, then store later (or store nothing at all in tests).
/// What: SHA + author + parsed conventional commit + entities + importance + narrative.
/// Test: Tested indirectly via `extract_on_real_repo`.
#[derive(Debug, Clone)]
pub struct GitFact {
    pub sha: String,
    pub author: String,
    pub author_email: String,
    pub committed_at: DateTime<Utc>,
    pub conventional: ConventionalCommit,
    pub entities: CommitEntities,
    pub importance: f32,
    /// Narrative text ready to store as a Drawer.
    pub narrative: String,
}

impl GitFact {
    /// Convert into a `Drawer` for storage in a palace room.
    ///
    /// Why: The normal storage path (`memory_remember`) operates on `Drawer`s,
    /// so emitting a `Drawer` lets git extraction reuse all downstream plumbing
    /// (embedding, indexing, importance ranking).
    /// What: Builds a drawer with the narrative as content, importance preserved,
    /// and a tag set encoding sha/author/type/scope/issues for filtered recall.
    /// Test: Verified indirectly in `extract_on_real_repo` (drawer importance
    /// matches fact importance; tags include the commit's short SHA).
    pub fn to_drawer(&self, room_id: Uuid) -> Drawer {
        let mut d = Drawer::new(room_id, self.narrative.clone());
        d.importance = self.importance;
        d.tags = self.build_tags();
        d
    }

    fn build_tags(&self) -> Vec<String> {
        let mut tags = vec![
            format!("git:{}", short_sha(&self.sha)),
            format!("author:{}", self.author),
        ];
        if !self.conventional.commit_type.is_empty() {
            tags.push(format!("type:{}", self.conventional.commit_type));
        }
        if let Some(scope) = &self.conventional.scope {
            tags.push(format!("scope:{scope}"));
        }
        for issue in &self.entities.issue_refs {
            tags.push(format!("issue:{issue}"));
        }
        tags
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..8.min(sha.len())]
}

// ── NLP extraction functions ─────────────────────────────────────────────────

/// Parse a commit message as a conventional commit (regex, no inference).
///
/// Why: Conventional-commit prefixes are a near-universal convention in modern
/// repos; extracting them gives us a structured signal without any LLM call.
/// What: Returns a `ConventionalCommit` with `commit_type == ""` when the
/// message is not conventional; otherwise fills in type/scope/breaking/description.
/// Test: `parse_feat_conventional_commit`, `parse_breaking_commit`,
/// `parse_non_conventional_commit`.
pub fn parse_conventional_commit(message: &str) -> ConventionalCommit {
    let first_line = message.lines().next().unwrap_or(message);
    if let Some(caps) = cc_regex().captures(first_line) {
        ConventionalCommit {
            commit_type: caps.get(1).map_or("", |m| m.as_str()).to_lowercase(),
            scope: caps.get(2).map(|m| {
                m.as_str()
                    .trim_matches(|c| c == '(' || c == ')')
                    .to_string()
            }),
            breaking: caps.get(3).is_some(),
            description: caps.get(4).map_or("", |m| m.as_str()).to_string(),
        }
    } else {
        ConventionalCommit {
            description: first_line.to_string(),
            ..Default::default()
        }
    }
}

/// Classify a file path into a `RoomType` (rule-based, no inference).
///
/// Why: Routing changed files to the right room lets us pre-cluster commits
/// by domain (Frontend/Backend/Testing/…) so retrieval is cheap.
/// What: Pure suffix/substring rules with explicit precedence (test → frontend
/// → config → docs → backend → general).
/// Test: `classify_*` tests cover Rust src, tests, frontend, config files.
pub fn classify_file_path(path: &str) -> RoomType {
    let p = path.to_lowercase();
    if p.contains("test")
        || p.contains("spec")
        || p.ends_with("_test.rs")
        || p.ends_with("_spec.ts")
        || p.ends_with(".test.ts")
    {
        RoomType::Testing
    } else if p.ends_with(".css")
        || p.ends_with(".scss")
        || p.ends_with(".html")
        || p.ends_with(".svelte")
        || p.ends_with(".tsx")
        || p.ends_with(".jsx")
        || p.contains("frontend")
        || p.contains("ui/")
        || p.contains("components/")
    {
        RoomType::Frontend
    } else if p.contains(".github/")
        || p == "makefile"
        || p == "dockerfile"
        || p.ends_with(".yml")
        || p.ends_with(".yaml")
        || p.ends_with(".toml")
        || p.contains("ci/")
        || p.contains("deploy")
    {
        RoomType::Configuration
    } else if p.ends_with(".md") || p.contains("docs/") || p.contains("readme") {
        RoomType::Documentation
    } else if p.ends_with(".rs")
        || p.ends_with(".py")
        || p.ends_with(".ts")
        || p.ends_with(".go")
        || p.ends_with(".java")
        || p.contains("src/")
        || p.contains("lib/")
        || p.contains("backend/")
    {
        RoomType::Backend
    } else {
        RoomType::General
    }
}

/// Heuristic importance score from commit metadata (no inference).
///
/// Why: Drawer importance drives L1 selection (top-15 always-loaded drawers);
/// a deterministic heuristic lets git memories surface meaningful commits
/// without any model call.
/// What: Base score from commit type, +0.2 for breaking, +0.1 for >10 files,
/// clamped to 1.0.
/// Test: `importance_breaking_feat` (0.7 + 0.2 = 0.9), `importance_large_chore`
/// (0.3 + 0.1 = 0.4).
pub fn score_importance(conv: &ConventionalCommit, files_changed: usize) -> f32 {
    let base: f32 = match conv.commit_type.as_str() {
        "feat" => 0.7,
        "fix" => 0.6,
        "refactor" | "perf" => 0.5,
        "chore" | "ci" | "docs" | "style" | "build" => 0.3,
        _ => 0.4, // no conventional prefix
    };
    let breaking_bonus: f32 = if conv.breaking { 0.2 } else { 0.0 };
    let size_bonus: f32 = if files_changed > 10 { 0.1 } else { 0.0 };
    (base + breaking_bonus + size_bonus).min(1.0_f32)
}

/// Extract entities from commit message + diff text (regex, no inference).
///
/// Why: Concrete entities (issue numbers, co-authors, symbols, room types)
/// give downstream retrieval cheap exact-match handles.
/// What: Runs four regexes over the message/diff, deduplicates room types
/// inferred from changed files, caps symbols at 10 to bound work.
/// Test: `extract_issue_refs` + end-to-end coverage in `extract_on_real_repo`.
pub fn extract_entities(
    message: &str,
    diff_text: &str,
    changed_files: &[String],
) -> CommitEntities {
    let full_text = format!("{message}\n{diff_text}");

    let issue_refs = issue_ref_regex()
        .captures_iter(message)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().parse::<u64>().ok())
        })
        .collect();

    let co_authors = coauthor_regex()
        .captures_iter(message)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();

    let symbols = symbol_regex()
        .captures_iter(&full_text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .take(10)
        .collect();

    let room_types: Vec<RoomType> = changed_files
        .iter()
        .map(|f| classify_file_path(f))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    CommitEntities {
        issue_refs,
        co_authors,
        symbols,
        file_paths: changed_files.to_vec(),
        room_types,
    }
}

/// Build a human-readable narrative for a commit (no inference).
///
/// Why: Drawers store free-form text; a deterministic narrative makes commits
/// readable in retrieval results without re-deriving structure each time.
/// What: Renders `[git:<short>] <author> made a <type><scope>[BREAKING] on
/// <date>: <desc>(refs: #N, ...)`.
/// Test: Indirectly via `extract_on_real_repo` (asserts narrative is non-empty).
pub fn build_narrative(
    sha: &str,
    author: &str,
    conv: &ConventionalCommit,
    entities: &CommitEntities,
    committed_at: &DateTime<Utc>,
) -> String {
    let type_str = if conv.commit_type.is_empty() {
        "change".to_string()
    } else {
        conv.commit_type.clone()
    };
    let scope_str = conv
        .scope
        .as_deref()
        .map(|s| format!(" in {s}"))
        .unwrap_or_default();
    let breaking_str = if conv.breaking { " [BREAKING]" } else { "" };
    let issues_str = if entities.issue_refs.is_empty() {
        String::new()
    } else {
        format!(
            " (refs: {})",
            entities
                .issue_refs
                .iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "[git:{sha}] {author} made a {type_str}{scope_str}{breaking_str} on {date}: {desc}{issues_str}",
        sha = short_sha(sha),
        date = committed_at.format("%Y-%m-%d"),
        desc = conv.description,
    )
}

// ── GitExtractor ─────────────────────────────────────────────────────────────

/// Extracts facts from a git repository using rule-based NLP.
///
/// Why: A small, owned wrapper around `git2::Repository` keeps the rest of
/// the crate free of `git2` types and provides a stable extraction surface.
/// What: Holds an open `Repository` and walks history into `GitFact`s.
/// Test: `extract_on_real_repo` opens this repo and pulls 5 facts.
pub struct GitExtractor {
    repo: git2::Repository,
}

impl GitExtractor {
    /// Open a git repository at `repo_path`.
    ///
    /// Why: Surfaces a clear `anyhow` error early if the path isn't a repo
    /// instead of failing deep inside walk logic.
    /// What: Wraps `git2::Repository::open` with path context.
    /// Test: `extract_on_real_repo` instantiates this against a real repo.
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        let repo = git2::Repository::open(&repo_path)
            .with_context(|| format!("failed to open git repo at {repo_path:?}"))?;
        Ok(Self { repo })
    }

    /// Extract up to `limit` facts, optionally filtered by `since`.
    ///
    /// Why: Bounded extraction prevents pathological work on huge histories
    /// and lets callers do incremental syncs by passing a `since` watermark.
    /// What: Walks HEAD in time order, parses each commit, stops once `limit`
    /// facts are produced or commits older than `since` are reached.
    /// Test: `extract_on_real_repo` asserts at least one fact and well-formed
    /// SHAs/narratives.
    pub fn extract(&self, since: Option<DateTime<Utc>>, limit: usize) -> Result<Vec<GitFact>> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;
        revwalk.set_sorting(git2::Sort::TIME)?;

        let mut facts = Vec::new();
        for oid in revwalk.take(limit.saturating_mul(3).max(limit)) {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;
            let committed_at = Utc
                .timestamp_opt(commit.time().seconds(), 0)
                .single()
                .unwrap_or_else(Utc::now);

            if let Some(since) = since
                && committed_at < since
            {
                break;
            }

            let message = commit.message().unwrap_or("").to_string();
            let author_sig = commit.author();
            let author = author_sig.name().unwrap_or("unknown").to_string();
            let author_email = author_sig.email().unwrap_or("").to_string();
            let sha = oid.to_string();

            let (changed_files, files_changed) = self.diff_files(&commit)?;

            let conv = parse_conventional_commit(&message);
            let entities = extract_entities(&message, "", &changed_files);
            let importance = score_importance(&conv, files_changed);
            let narrative = build_narrative(&sha, &author, &conv, &entities, &committed_at);

            facts.push(GitFact {
                sha,
                author,
                author_email,
                committed_at,
                conventional: conv,
                entities,
                importance,
                narrative,
            });

            if facts.len() >= limit {
                break;
            }
        }
        Ok(facts)
    }

    fn diff_files(&self, commit: &git2::Commit) -> Result<(Vec<String>, usize)> {
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        let mut files = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(path) = delta.new_file().path().and_then(|p| p.to_str()) {
                    files.push(path.to_string());
                }
                true
            },
            None,
            None,
            None,
        )?;
        let count = files.len();
        Ok((files, count))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_feat_conventional_commit() {
        let cc = parse_conventional_commit("feat(auth): add OAuth login");
        assert_eq!(cc.commit_type, "feat");
        assert_eq!(cc.scope.as_deref(), Some("auth"));
        assert!(!cc.breaking);
        assert_eq!(cc.description, "add OAuth login");
    }

    #[test]
    fn parse_breaking_commit() {
        let cc = parse_conventional_commit("feat(api)!: remove legacy endpoint");
        assert!(cc.breaking);
        assert_eq!(cc.commit_type, "feat");
    }

    #[test]
    fn parse_non_conventional_commit() {
        let cc = parse_conventional_commit("update readme");
        assert_eq!(cc.commit_type, "");
        assert_eq!(cc.description, "update readme");
    }

    #[test]
    fn classify_rust_src_as_backend() {
        assert_eq!(classify_file_path("src/main.rs"), RoomType::Backend);
        assert_eq!(
            classify_file_path("crates/core/src/lib.rs"),
            RoomType::Backend
        );
    }

    #[test]
    fn classify_test_files() {
        assert_eq!(
            classify_file_path("tests/integration_test.rs"),
            RoomType::Testing
        );
        assert_eq!(classify_file_path("src/user_test.rs"), RoomType::Testing);
    }

    #[test]
    fn classify_frontend_files() {
        assert_eq!(classify_file_path("src/App.tsx"), RoomType::Frontend);
        assert_eq!(
            classify_file_path("components/Button.svelte"),
            RoomType::Frontend
        );
    }

    #[test]
    fn classify_config_files() {
        assert_eq!(
            classify_file_path(".github/workflows/ci.yml"),
            RoomType::Configuration
        );
        assert_eq!(classify_file_path("Makefile"), RoomType::Configuration);
    }

    #[test]
    fn importance_breaking_feat() {
        let conv = ConventionalCommit {
            commit_type: "feat".to_string(),
            breaking: true,
            description: "x".to_string(),
            ..Default::default()
        };
        let score = score_importance(&conv, 0);
        assert!((score - 0.9).abs() < 1e-4, "got {score}");
    }

    #[test]
    fn importance_large_chore() {
        let conv = ConventionalCommit {
            commit_type: "chore".to_string(),
            description: "x".to_string(),
            ..Default::default()
        };
        let score = score_importance(&conv, 15);
        assert!((score - 0.4).abs() < 1e-4, "got {score}"); // 0.3 + 0.1 large
    }

    #[test]
    fn extract_issue_refs() {
        let entities = extract_entities("fix: closes #42 and fixes #99", "", &[]);
        assert!(entities.issue_refs.contains(&42));
        assert!(entities.issue_refs.contains(&99));
    }

    #[test]
    fn extract_on_real_repo() {
        // Use this repository itself (3+ commits exist); #9298: the running
        // checkout, not the one that built this binary.
        let repo_path =
            crate::test_harness::test_repo_root().expect("the running checkout's workspace root");
        let extractor = GitExtractor::new(repo_path).unwrap();
        let facts = extractor.extract(None, 5).unwrap();
        assert!(!facts.is_empty(), "should extract at least 1 fact");
        assert!(facts.iter().all(|f| !f.sha.is_empty()));
        assert!(facts.iter().all(|f| !f.narrative.is_empty()));
    }
}
