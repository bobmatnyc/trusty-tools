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
//!
//! **Invariant — conflict resolution never yields fewer bytes than it was
//! given.** Every branch returns either a real merge or one agent's *full*
//! version, so a git failure or an LLM failure degrades to first-writer-wins
//! rather than to an empty file. This is enforced structurally at a single
//! seam (`resolve_conflict`) rather than trusted branch-by-branch, so a new
//! resolution path cannot violate it silently.
//!
//! Degrading is never silent: the resolution mode is recorded per file and
//! rendered into `merge-report.md`, which is the only channel downstream
//! phases actually read (`engine::executor::dispatch` appends it to the
//! phase's `AgentOutput`). A `tracing::warn!` alone would be invisible there.
//!
//! Test: `resolver_tests.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::workflow::parallel::ParallelPhaseResult;

/// Default OpenRouter chat-completions endpoint.
const OPENROUTER_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Merges file trees produced by parallel sub-agents into a single output dir.
pub struct ConflictResolver {
    /// OpenRouter API key for the LLM fallback. Empty string disables LLM
    /// resolution (we fall back to first-agent-wins in that case).
    pub api_key: String,
    /// Chat-completions endpoint. Per-instance rather than a global/env
    /// lookup so tests can point at a local stub without a process-wide
    /// mutation that races other tests in the same binary.
    completions_url: String,
}

/// How a conflicted path was *actually* resolved.
///
/// Why: `merge()` used to stamp `[MERGED from a+b]` on every conflicted file
/// regardless of whether a merge happened at all, so "we combined both agents'
/// work" and "we threw one agent's work away because git crashed" were
/// indistinguishable downstream. The mode is carried out to the report.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolutionMode {
    /// Every version was byte-identical; there was nothing to decide.
    Identical,
    /// `git merge-file` combined the versions cleanly.
    GitMerged,
    /// The LLM produced a merged body from git's conflicted output.
    LlmMerged,
    /// No merge happened. One agent's full version was kept and the others
    /// were discarded. `reason` is rendered verbatim into the merge report.
    Degraded { kept: String, reason: String },
}

/// Bytes chosen for a conflicted path, plus how they were chosen.
struct Resolution {
    bytes: Vec<u8>,
    mode: ResolutionMode,
}

impl ConflictResolver {
    /// Construct a resolver with a given OpenRouter API key.
    ///
    /// Why: Passing the key at construction time keeps the merge call site
    /// free of env-var lookups. An empty string disables LLM fallback.
    /// What: Plain struct literal.
    /// Test: Implicit via `merge_*` tests.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            completions_url: OPENROUTER_COMPLETIONS_URL.to_string(),
        }
    }

    /// Point the LLM fallback at a different OpenRouter-compatible endpoint.
    ///
    /// Why: lets a caller route through a self-hosted gateway, and lets tests
    /// drive the real HTTP error paths (401/429/malformed body) against a
    /// local stub. Per-instance, so concurrent tests cannot interfere.
    /// What: builder-style override of `completions_url`.
    /// Test: `llm_resolve_rejects_http_error_bodies`.
    pub fn with_completions_url(mut self, url: String) -> Self {
        self.completions_url = url;
        self
    }

    /// Merge file trees from all parallel sub-agent results into `out_dir`.
    ///
    /// Why: The engine needs a single consolidated output per phase so
    /// downstream phases (QA, observe) see one set of files, not several.
    /// What: Collects files from each `result.out_dir`, writes unique files
    /// through directly, resolves conflicts via `resolve_conflict`, and
    /// writes `merge-report.md` to `out_dir` summarizing the result —
    /// distinguishing real merges from degraded first-agent-wins fallbacks,
    /// and leading with an explicit warning block when anything degraded.
    /// Test: `merge_single_owner_files_pass_through`,
    /// `merge_report_flags_degraded_resolution`.
    pub async fn merge(
        &self,
        results: &[ParallelPhaseResult],
        out_dir: &Path,
    ) -> anyhow::Result<String> {
        tokio::fs::create_dir_all(out_dir).await?;

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

        let mut file_lines: Vec<String> = Vec::new();
        let mut degraded_lines: Vec<String> = Vec::new();
        let mut conflicts = 0usize;
        let mut merged_ok = 0usize;

        for (rel_path, versions) in &file_map {
            let dest = out_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            let Some((first_label, first_bytes)) = versions.first() else {
                continue;
            };

            if versions.len() == 1 {
                tokio::fs::write(&dest, first_bytes).await?;
                file_lines.push(format!("  [{}] {}", first_label, rel_path.display()));
                continue;
            }

            conflicts += 1;
            let resolution = self.resolve_conflict(rel_path, versions).await?;
            tokio::fs::write(&dest, &resolution.bytes).await?;

            let labels = versions
                .iter()
                .map(|(l, _)| l.as_str())
                .collect::<Vec<_>>()
                .join("+");

            match &resolution.mode {
                ResolutionMode::Identical | ResolutionMode::GitMerged => {
                    merged_ok += 1;
                    file_lines.push(format!("  [MERGED from {labels}] {}", rel_path.display()));
                }
                ResolutionMode::LlmMerged => {
                    merged_ok += 1;
                    file_lines.push(format!(
                        "  [LLM-MERGED from {labels}] {}",
                        rel_path.display()
                    ));
                }
                ResolutionMode::Degraded { kept, reason } => {
                    file_lines.push(format!(
                        "  [DEGRADED from {labels}] {} — kept '{kept}', discarded the rest ({reason})",
                        rel_path.display()
                    ));
                    degraded_lines.push(format!(
                        "  - {}: kept '{kept}', discarded the rest ({reason})",
                        rel_path.display()
                    ));
                }
            }
        }

        // HashMap iteration order is arbitrary; sort so the report is
        // reproducible across runs and diffable between phases.
        file_lines.sort();
        degraded_lines.sort();

        let mut report_lines = vec![
            "# Parallel Phase Merge Report".to_string(),
            format!("Merged {} sub-agent outputs", results.len()),
            String::new(),
        ];

        if !degraded_lines.is_empty() {
            report_lines.push(format!(
                "!! DEGRADED: {} file(s) were NOT merged. One agent's version was kept \
                 and the other agent's work was discarded:",
                degraded_lines.len()
            ));
            report_lines.extend(degraded_lines.iter().cloned());
            report_lines.push(String::new());
        }

        report_lines.extend(file_lines);
        report_lines.push(String::new());
        report_lines.push(format!(
            "Total files: {}, conflicts: {} (merged: {}, degraded: {})",
            file_map.len(),
            conflicts,
            merged_ok,
            degraded_lines.len()
        ));
        let report = report_lines.join("\n");

        tokio::fs::write(out_dir.join("merge-report.md"), &report).await?;
        Ok(report)
    }

    /// Resolve a single-file conflict between N >= 2 versions.
    ///
    /// Why: This function's output is written straight over the merged tree,
    /// so returning nothing destroys both agents' work. Rather than trust each
    /// branch to honour that, this is the **single seam** where the invariant
    /// is enforced: whatever `resolve_conflict_inner` decided, an empty result
    /// for non-empty input is rejected here and downgraded to first-agent-wins
    /// with a report-visible reason. A future resolution path therefore cannot
    /// truncate a file even if it forgets the rule.
    /// What: Delegates to `resolve_conflict_inner`, then applies the non-empty
    /// guard. An empty result is only legitimate when the agents genuinely
    /// produced empty files.
    /// Test: `resolve_conflict_seam_rejects_empty_result`,
    /// `resolve_conflict_allows_genuinely_empty_versions`.
    async fn resolve_conflict(
        &self,
        path: &Path,
        versions: &[(String, Vec<u8>)],
    ) -> anyhow::Result<Resolution> {
        debug_assert!(
            versions.len() >= 2,
            "resolve_conflict is only called for paths with >= 2 versions"
        );
        let Some((first_label, first)) = versions.first() else {
            anyhow::bail!("no versions to resolve for {}", path.display());
        };

        let resolution = self
            .resolve_conflict_inner(path, versions, first_label, first)
            .await?;

        if let Some(rescued) = enforce_non_empty(&resolution, first_label, first) {
            tracing::warn!(
                path = %path.display(),
                mode = ?resolution.mode,
                "conflict resolution produced an empty result for non-empty input; \
                 falling back to first agent's version"
            );
            return Ok(rescued);
        }

        Ok(resolution)
    }

    /// Pick bytes for a conflicted path. Callers must go through
    /// `resolve_conflict`, which enforces the non-empty invariant on the way
    /// out.
    ///
    /// Why: We prefer structural merge (git merge-file) over LLM so the
    /// common case (non-overlapping edits) costs nothing. LLM is the last
    /// line of defense when git leaves `<<<<<<<` markers.
    /// What: Identical versions short-circuit to those bytes. For exactly 2
    /// differing versions: 3-way merge with an empty base, classified by git's
    /// exit status via `decide_merge_bytes` — a clean merge returns git's
    /// output, a conflicted merge goes to the LLM, and a git *failure* degrades
    /// to the first agent's version. For 3+ versions: degrade to first.
    /// Test: `merge_two_identical_is_noop`,
    /// `merge_conflicting_versions_falls_back_to_first_agent`,
    /// `merge_clean_git_merge_combines_both_versions`.
    async fn resolve_conflict_inner(
        &self,
        path: &Path,
        versions: &[(String, Vec<u8>)],
        first_label: &str,
        first: &[u8],
    ) -> anyhow::Result<Resolution> {
        // Merging N byte-identical copies is a no-op by definition. Answering
        // it here keeps the common case (both agents left an input file
        // untouched) free of a subprocess spawn, and independent of whether
        // `git merge-file` is usable at all.
        if versions.iter().all(|(_, bytes)| bytes.as_slice() == first) {
            return Ok(Resolution {
                bytes: first.to_vec(),
                mode: ResolutionMode::Identical,
            });
        }

        // Slice pattern instead of `versions[0]` / `versions[1]`: the 2-version
        // case is expressed in the type system, so there is no index to panic.
        let [(_, a_bytes), (_, b_bytes)] = versions else {
            tracing::warn!(
                path = %path.display(),
                count = versions.len(),
                "3+ versions in conflict; taking first agent's version"
            );
            return Ok(Resolution {
                bytes: first.to_vec(),
                mode: ResolutionMode::Degraded {
                    kept: first_label.to_string(),
                    reason: format!(
                        "{} conflicting versions; only 2-way merge is supported",
                        versions.len()
                    ),
                },
            });
        };

        let tmp = tempfile_dir();
        tokio::fs::create_dir_all(&tmp).await?;
        let base_path = tmp.join("base");
        let a_path = tmp.join("a");
        let b_path = tmp.join("b");
        tokio::fs::write(&base_path, b"").await?;
        tokio::fs::write(&a_path, a_bytes).await?;
        tokio::fs::write(&b_path, b_bytes).await?;

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

        let conflicted = match decide_merge_bytes(out.status.code(), out.stdout, first) {
            MergeOutcome::Merged(bytes) => {
                return Ok(Resolution {
                    bytes,
                    mode: ResolutionMode::GitMerged,
                });
            }
            MergeOutcome::Conflicted {
                conflicted,
                regions,
            } => {
                tracing::debug!(
                    path = %path.display(),
                    regions,
                    "git merge-file left conflict markers; escalating"
                );
                conflicted
            }
            MergeOutcome::Failed { fallback, status } => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(
                    path = %path.display(),
                    status = ?status,
                    stderr = %stderr.trim(),
                    "git merge-file failed; falling back to first agent's version"
                );
                return Ok(Resolution {
                    bytes: fallback,
                    mode: ResolutionMode::Degraded {
                        kept: first_label.to_string(),
                        reason: format!("git merge-file failed ({})", describe_status(status)),
                    },
                });
            }
        };

        if self.api_key.is_empty() {
            return Ok(Resolution {
                bytes: first.to_vec(),
                mode: ResolutionMode::Degraded {
                    kept: first_label.to_string(),
                    reason: "git left conflict markers and no OpenRouter key is configured \
                             for LLM merge"
                        .to_string(),
                },
            });
        }

        match self.llm_resolve(path, a_bytes, b_bytes, &conflicted).await {
            Ok(resolved) => Ok(Resolution {
                bytes: resolved.into_bytes(),
                mode: ResolutionMode::LlmMerged,
            }),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "LLM conflict resolution failed; falling back to first version"
                );
                Ok(Resolution {
                    bytes: first.to_vec(),
                    mode: ResolutionMode::Degraded {
                        kept: first_label.to_string(),
                        reason: format!("LLM merge failed: {e}"),
                    },
                })
            }
        }
    }

    /// LLM-driven conflict resolution via OpenRouter.
    ///
    /// Why: Semantic conflicts (both agents added different helper functions
    /// to the same file) need a model that understands the code, not just
    /// line-based merge.
    /// What: Sends a prompt with both versions + the git-merge output (with
    /// markers) to a cheap model (Haiku) and asks for the merged file body.
    /// `error_for_status` is mandatory here: `reqwest::send` resolves happily
    /// on 4xx/5xx, and OpenRouter's error payload is well-formed JSON, so
    /// without it an expired key or a rate limit parsed cleanly into a
    /// *missing* `choices` key and produced an empty file. See
    /// `extract_llm_content`.
    /// Test: `extract_llm_content_*` (parsing); the HTTP layer is exercised by
    /// `llm_resolve_rejects_http_error_bodies`.
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
            .post(&self.completions_url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": "anthropic/claude-haiku-3-5",
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": prompt}]
            }))
            .send()
            .await?
            // Without this, a 401/402/429 body parses fine and yields "".
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        extract_llm_content(&resp)
    }
}

/// What `git merge-file` printed, and whether it can be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeOutcome {
    /// Clean merge; these bytes are the merged file.
    Merged(Vec<u8>),
    /// Git merged but left conflict markers. `conflicted` feeds the LLM
    /// prompt; `regions` is git's reported conflict count.
    Conflicted { conflicted: Vec<u8>, regions: i32 },
    /// Git itself failed and its stdout is meaningless. `fallback` is the
    /// first agent's full version — never git's output.
    Failed {
        fallback: Vec<u8>,
        status: Option<i32>,
    },
}

/// Decide which bytes to use from a `git merge-file` run.
///
/// Why: git reports *three* distinct outcomes through one integer, and the
/// failure outcome comes with empty stdout. Treating that empty stdout as a
/// clean merge silently truncates both agents' work to zero bytes — which is
/// exactly what happened whenever git could not do repository discovery
/// (corrupt worktree, `safe.directory` refusal, bad `GIT_DIR`). Keeping this
/// decision in a pure function is what makes the failure path testable at all:
/// the previous code could only be reached by corrupting the ambient git
/// environment, which no test can do safely under parallel execution.
/// What: Per `git-merge-file(1)`, exit 0 is a clean merge, 1..=127 is the
/// number of conflict regions, and anything else (128/255, or `None` for a
/// signal-killed child) is an error. On error the caller's `first` version is
/// returned so the function is total — every arm yields real bytes.
/// Test: `decide_merge_bytes_*`.
fn decide_merge_bytes(code: Option<i32>, stdout: Vec<u8>, first: &[u8]) -> MergeOutcome {
    match code {
        Some(0) => MergeOutcome::Merged(stdout),
        Some(regions @ 1..=127) => MergeOutcome::Conflicted {
            conflicted: stdout,
            regions,
        },
        status => MergeOutcome::Failed {
            fallback: first.to_vec(),
            status,
        },
    }
}

/// Enforce the module invariant: a resolution must never be emptier than the
/// input it was derived from.
///
/// Why: this is the *single seam* protecting the merged tree. Every historical
/// instance of this bug — git failing with empty stdout (#3652), an OpenRouter
/// error body extracting to `""` — reached the filesystem because some branch
/// returned zero bytes and nothing checked. Keeping the check in one pure
/// function means a newly added resolution path is covered by construction,
/// and means the guard itself is testable without having to manufacture a
/// broken branch.
/// What: returns `Some(replacement)` when the resolution is empty but the
/// first agent's version was not — the caller should use the replacement and
/// report it as degraded. Returns `None` when the resolution is acceptable,
/// including the legitimate case where the agents genuinely produced empty
/// files.
/// Test: `enforce_non_empty_rescues_empty_resolution`,
/// `enforce_non_empty_allows_genuine_empty`,
/// `enforce_non_empty_passes_through_good_resolution`.
fn enforce_non_empty(
    resolution: &Resolution,
    first_label: &str,
    first: &[u8],
) -> Option<Resolution> {
    if resolution.bytes.is_empty() && !first.is_empty() {
        return Some(Resolution {
            bytes: first.to_vec(),
            mode: ResolutionMode::Degraded {
                kept: first_label.to_string(),
                reason: "resolution returned no content".to_string(),
            },
        });
    }
    None
}

/// Render an exit status for the merge report.
fn describe_status(status: Option<i32>) -> String {
    match status {
        Some(code) => format!("exit {code}"),
        None => "killed by signal".to_string(),
    }
}

/// Extract the assistant message body from an OpenRouter chat completion.
///
/// Why: the old code did `...["content"].as_str().unwrap_or("")`, which turns
/// *any* unexpected response shape into an empty string. Combined with a
/// missing `error_for_status`, an OpenRouter error body (`{"error":{...}}`,
/// returned for an expired key, a rate limit, or an unavailable model) parsed
/// cleanly, produced `""`, and was written over both agents' work. Routine
/// operational failures must surface as `Err` so the caller can degrade to a
/// real version, never as valid-looking empty content.
/// What: requires `choices[0].message.content` to be a string and to be
/// non-blank; anything else is an error naming what was actually received.
/// Test: `extract_llm_content_accepts_normal_completion`,
/// `extract_llm_content_rejects_error_body`,
/// `extract_llm_content_rejects_empty_choices`,
/// `extract_llm_content_rejects_blank_content`.
fn extract_llm_content(resp: &serde_json::Value) -> anyhow::Result<String> {
    let content = resp["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| {
            let summary = resp.to_string();
            anyhow::anyhow!(
                "OpenRouter response has no choices[0].message.content string; got: {}",
                &summary[..summary.len().min(400)]
            )
        })?;
    anyhow::ensure!(
        !content.trim().is_empty(),
        "OpenRouter returned blank content"
    );
    Ok(content.to_string())
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
#[path = "resolver_tests.rs"]
mod tests;
