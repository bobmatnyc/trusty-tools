//! Conflict resolver — merges file trees from parallel sub-agents (#75).
//!
//! Why: When multiple parallel sub-agents emit their own file trees, we need
//! a deterministic way to combine them into a single `out_dir`. Most files
//! come from exactly one agent (no conflict); for files that appear in more
//! than one tree we try a `git merge-file` 3-way merge with an empty base,
//! and fall back to an LLM-driven resolution when the merge has markers.
//! What: `ConflictResolver::merge` walks each `ParallelPhaseResult::out_dir`,
//! collects a map of `rel_path -> Vec<(label, bytes)>`, writes single-owner
//! files through unchanged, resolves conflicts via git / LLM / first-writer-
//! wins, and emits a `merge-report.md` in the target dir.
//! Test: `merge_single_owner_files_pass_through` and `merge_two_identical_is_noop`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::workflow::parallel::ParallelPhaseResult;

/// Merges file trees produced by parallel sub-agents into a single output dir.
pub struct ConflictResolver {
    /// OpenRouter API key for the LLM fallback. Empty string disables LLM
    /// resolution (we fall back to first-agent-wins in that case).
    pub api_key: String,
}

impl ConflictResolver {
    /// Construct a resolver with a given OpenRouter API key.
    ///
    /// Why: Passing the key at construction time keeps the merge call site
    /// free of env-var lookups. An empty string disables LLM fallback.
    /// What: Plain struct literal.
    /// Test: Implicit via `merge_*` tests.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Merge file trees from all parallel sub-agent results into `out_dir`.
    ///
    /// Why: The engine needs a single consolidated output per phase so
    /// downstream phases (QA, observe) see one set of files, not several.
    /// What: Collects files from each `result.out_dir`, writes unique files
    /// through directly, resolves conflicts via `resolve_conflict`, and
    /// writes `merge-report.md` to `out_dir` summarizing the result.
    /// Test: `merge_single_owner_files_pass_through`.
    pub async fn merge(
        &self,
        results: &[ParallelPhaseResult],
        out_dir: &Path,
    ) -> anyhow::Result<String> {
        tokio::fs::create_dir_all(out_dir).await?;

        let mut report_lines = vec![
            "# Parallel Phase Merge Report".to_string(),
            format!("Merged {} sub-agent outputs", results.len()),
            String::new(),
        ];

        let mut file_map: HashMap<PathBuf, Vec<(String, Vec<u8>)>> = HashMap::new();

        for result in results {
            collect_files_recursive(
                &result.out_dir,
                &result.out_dir,
                &mut file_map,
                &result.label,
            )
            .await?;
        }

        let mut conflicts = 0usize;
        for (rel_path, versions) in &file_map {
            let dest = out_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            if versions.len() == 1 {
                tokio::fs::write(&dest, &versions[0].1).await?;
                report_lines.push(format!("  [{}] {}", versions[0].0, rel_path.display()));
            } else {
                conflicts += 1;
                let resolved = self.resolve_conflict(rel_path, versions).await?;
                tokio::fs::write(&dest, &resolved).await?;
                let labels = versions
                    .iter()
                    .map(|(l, _)| l.as_str())
                    .collect::<Vec<_>>()
                    .join("+");
                report_lines.push(format!("  [MERGED from {}] {}", labels, rel_path.display()));
            }
        }

        report_lines.push(String::new());
        report_lines.push(format!(
            "Total files: {}, Conflicts resolved: {}",
            file_map.len(),
            conflicts
        ));
        let report = report_lines.join("\n");

        tokio::fs::write(out_dir.join("merge-report.md"), &report).await?;
        Ok(report)
    }

    /// Resolve a single-file conflict between N >= 2 versions.
    ///
    /// Why: We prefer structural merge (git merge-file) over LLM so the
    /// common case (non-overlapping edits) costs nothing. LLM is the last
    /// line of defense when git leaves `<<<<<<<` markers.
    /// What: For exactly 2 versions: 3-way merge with empty base. If no
    /// markers, return the merge output; otherwise call LLM; otherwise
    /// return first agent's version. For 3+ versions: take first, log.
    async fn resolve_conflict(
        &self,
        path: &Path,
        versions: &[(String, Vec<u8>)],
    ) -> anyhow::Result<Vec<u8>> {
        if versions.len() == 2 {
            let tmp = tempfile_dir();
            tokio::fs::create_dir_all(&tmp).await?;
            let base_path = tmp.join("base");
            let a_path = tmp.join("a");
            let b_path = tmp.join("b");
            tokio::fs::write(&base_path, b"").await?;
            tokio::fs::write(&a_path, &versions[0].1).await?;
            tokio::fs::write(&b_path, &versions[1].1).await?;

            let out = Command::new("git")
                .args([
                    "merge-file",
                    "-p",
                    a_path.to_str().unwrap_or("."),
                    base_path.to_str().unwrap_or("."),
                    b_path.to_str().unwrap_or("."),
                ])
                .output()
                .await?;

            // Best-effort cleanup.
            let _ = tokio::fs::remove_dir_all(&tmp).await;

            let merged = out.stdout;
            let has_conflicts = merged.windows(7).any(|w| w == b"<<<<<<<");

            if !has_conflicts {
                return Ok(merged);
            }

            if !self.api_key.is_empty() {
                match self
                    .llm_resolve(path, &versions[0].1, &versions[1].1, &merged)
                    .await
                {
                    Ok(resolved) => return Ok(resolved.into_bytes()),
                    Err(e) => tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "LLM conflict resolution failed; falling back to first version"
                    ),
                }
            }

            return Ok(versions[0].1.clone());
        }

        tracing::warn!(
            path = %path.display(),
            count = versions.len(),
            "3+ versions in conflict; taking first agent's version"
        );
        Ok(versions[0].1.clone())
    }

    /// LLM-driven conflict resolution via OpenRouter.
    ///
    /// Why: Semantic conflicts (both agents added different helper functions
    /// to the same file) need a model that understands the code, not just
    /// line-based merge.
    /// What: Sends a prompt with both versions + the git-merge output (with
    /// markers) to a cheap model (Haiku) and asks for the merged file body.
    async fn llm_resolve(
        &self,
        path: &Path,
        a: &[u8],
        b: &[u8],
        conflicted: &[u8],
    ) -> anyhow::Result<String> {
        let prompt = format!(
            "Two agents produced conflicting versions of `{}`. \
             Produce a single merged version that incorporates the best of both. \
             Return ONLY the file content with no explanation.\n\n\
             === VERSION A ===\n{}\n\n=== VERSION B ===\n{}\n\n\
             === GIT MERGE OUTPUT (with conflict markers) ===\n{}",
            path.display(),
            String::from_utf8_lossy(&a[..a.len().min(3000)]),
            String::from_utf8_lossy(&b[..b.len().min(3000)]),
            String::from_utf8_lossy(&conflicted[..conflicted.len().min(2000)]),
        );

        let client = reqwest::Client::new();
        let resp = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "anthropic/claude-haiku-3-5",
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": prompt}]
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        Ok(resp["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }
}

/// Process-unique temp dir for a single merge invocation.
fn tempfile_dir() -> PathBuf {
    std::env::temp_dir().join(format!("trusty-agents-merge-{}", uuid::Uuid::new_v4()))
}

/// Walk `dir` and push every regular file into `map`, keyed by path relative
/// to `root`. `label` tags which sub-agent the file came from so merge
/// reports + conflict resolution know the provenance.
///
/// Why: We need the relative path (not absolute) so outputs from multiple
/// sub-agents collate correctly — `src/a.py` from agent-A and agent-B should
/// end up in the same bucket.
/// What: Recursive async walk via `Box::pin` (async recursion requires it).
/// Skips `.git` directories and any `merge-report.md` left over from prior
/// runs to avoid recursive merging of our own outputs.
fn collect_files_recursive<'a>(
    root: &'a Path,
    dir: &'a Path,
    map: &'a mut HashMap<PathBuf, Vec<(String, Vec<u8>)>>,
    label: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if !dir.exists() {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name == ".git" {
                continue;
            }
            if path.is_dir() {
                collect_files_recursive(root, &path, map, label).await?;
            } else {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                let rel_str = rel.to_string_lossy().to_string();
                if rel_str.starts_with(".git") || rel_str == "merge-report.md" {
                    continue;
                }
                let content = tokio::fs::read(&path).await?;
                map.entry(rel)
                    .or_default()
                    .push((label.to_string(), content));
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::perf::TokenUsage;
    use crate::tools::traits::AgentOutput;

    fn mk_result(label: &str, out_dir: PathBuf) -> ParallelPhaseResult {
        ParallelPhaseResult {
            label: label.to_string(),
            output: AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            },
            out_dir,
        }
    }

    #[tokio::test]
    async fn merge_single_owner_files_pass_through() {
        // Each sub-agent writes distinct files; no conflicts.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let out = tmp.path().join("out");
        tokio::fs::create_dir_all(a.join("src")).await.unwrap();
        tokio::fs::create_dir_all(b.join("src")).await.unwrap();
        tokio::fs::write(a.join("src/one.py"), b"# from a")
            .await
            .unwrap();
        tokio::fs::write(b.join("src/two.py"), b"# from b")
            .await
            .unwrap();

        let results = vec![mk_result("a", a), mk_result("b", b)];
        let resolver = ConflictResolver::new(String::new());
        let report = resolver.merge(&results, &out).await.unwrap();

        assert!(out.join("src/one.py").exists());
        assert!(out.join("src/two.py").exists());
        assert!(out.join("merge-report.md").exists());
        assert!(report.contains("Total files: 2"));
        assert!(report.contains("Conflicts resolved: 0"));
    }

    #[tokio::test]
    async fn merge_two_identical_is_noop() {
        // Both sub-agents emit the same bytes for the same path. Git
        // merge-file produces the merged content without markers.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let out = tmp.path().join("out");
        tokio::fs::create_dir_all(&a).await.unwrap();
        tokio::fs::create_dir_all(&b).await.unwrap();
        tokio::fs::write(a.join("f.txt"), b"hello\n").await.unwrap();
        tokio::fs::write(b.join("f.txt"), b"hello\n").await.unwrap();

        let results = vec![mk_result("a", a), mk_result("b", b)];
        let resolver = ConflictResolver::new(String::new());
        let report = resolver.merge(&results, &out).await.unwrap();

        assert!(out.join("f.txt").exists());
        // Either it resolved cleanly (no conflicts) OR took version A.
        let body = tokio::fs::read(out.join("f.txt")).await.unwrap();
        assert_eq!(&body[..], b"hello\n");
        assert!(report.contains("Total files: 1"));
    }
}
