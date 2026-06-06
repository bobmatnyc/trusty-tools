//! Keyword-based task signal extractor for PM inspection (#harness-test-suite).
//!
//! Why: The PM delegates to agents by matching capability signals (role,
//! languages, frameworks, tags) against each candidate's declared
//! capabilities. The real PM path uses an LLM to infer those signals, but
//! we want a zero-cost path for CI and harness tests that turns task text
//! directly into signals via cheap substring matching. This module owns
//! that heuristic so both the `inspect --dry-run` command and the static
//! unit tests share one implementation.
//! What: `TaskSignals::extract` scans lowercased task text for a curated
//! set of keywords and returns languages / frameworks / role / tags vectors.
//! Role resolution is priority-ordered (docs > research > planner > qa >
//! ops > engineer default) so that e.g. "document the new bash script"
//! picks `docs` not `ops`.
//! Test: See `#[cfg(test)]` block at the bottom of this file.

/// Keyword-extracted routing signals for a free-form task description.
///
/// Why: Keeps signal extraction and registry matching decoupled — the
/// extractor doesn't care what agents exist, and the registry doesn't care
/// how signals were derived. This lets us swap the extractor (keyword →
/// LLM → embeddings) without touching routing.
/// What: Parallel `Vec<String>` fields so callers can borrow slices into
/// `AgentRegistry::best_match` cheaply. `role` is `Option<String>` because
/// not every task implies a role (the extractor defaults to `engineer`).
/// Test: `extracts_python_language`, `prioritizes_docs_over_ops`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSignals {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub role: Option<String>,
    pub tags: Vec<String>,
}

impl TaskSignals {
    /// Extract signals from task text via keyword matching.
    ///
    /// Why: No LLM cost, fully deterministic, fast enough to run on every
    /// CI job. The keyword list is intentionally narrow — it only covers
    /// routing-impacting terms we've already seen in the harness test
    /// suite. Adding a new keyword should be accompanied by a new test.
    /// What: Lowercases once, scans for language / framework / role / tag
    /// keywords in priority order. Role priority matters: "document the
    /// new CLI" should resolve to `docs` even though "cli" might trigger
    /// `engineer` heuristics elsewhere. The final `else` branch defaults
    /// to `engineer` so generic "write code" tasks still get routed.
    /// Test: `extracts_python_language`, `prioritizes_docs_over_ops`,
    /// `defaults_to_engineer_role`.
    pub fn extract(task: &str) -> Self {
        let lower = task.to_lowercase();
        let mut languages: Vec<String> = Vec::new();
        let mut frameworks: Vec<String> = Vec::new();
        let mut tags: Vec<String> = Vec::new();

        // ── Languages ────────────────────────────────────────────────
        if lower.contains("python") || lower.contains(".py") {
            languages.push("python".into());
        }
        if lower.contains("rust") || lower.contains("cargo") {
            languages.push("rust".into());
        }
        if lower.contains("typescript") || lower.contains(".ts") {
            languages.push("typescript".into());
        }
        if lower.contains("javascript") || lower.contains(".js") {
            languages.push("javascript".into());
        }
        // Go language detection. Use word-boundary-ish checks for "go" to
        // avoid false positives on words like "going" or "argo". The bare
        // " go " (with surrounding spaces) plus Go-specific tokens cover the
        // common harness phrasings without polluting other tasks.
        if lower.contains("golang")
            || lower.contains(" go ")
            || lower.starts_with("go ")
            || lower.ends_with(" go")
            || lower.contains("net/http")
            || lower.contains("fmt.errorf")
            || lower.contains("httptest")
            || lower.contains("goroutine")
            || lower.contains(".go ")
            || lower.ends_with(".go")
        {
            languages.push("go".into());
        }
        if lower.contains("java ")
            || lower.contains(" java")
            || lower.contains("spring boot")
            || lower.contains("springboot")
            || lower.contains(".java")
        {
            // Avoid double-matching "javascript" (handled above) — the
            // contiguous substring "javascript" won't trigger any of the
            // patterns above (no leading/trailing space, no .java extension).
            // But a defensive check guards against future edits.
            if !lower.contains("javascript") || lower.contains("spring") || lower.contains(".java")
            {
                languages.push("java".into());
            }
        }
        if lower.contains("ruby")
            || lower.contains(".rb ")
            || lower.contains("rspec")
            || lower.contains(" rails ")
            || lower.contains("sinatra")
        {
            languages.push("ruby".into());
        }

        // ── Frameworks ───────────────────────────────────────────────
        if lower.contains("fastapi") {
            frameworks.push("fastapi".into());
        }
        if lower.contains("flask") {
            frameworks.push("flask".into());
        }
        if lower.contains("django") {
            frameworks.push("django".into());
        }
        // "pytest" and "test" both hint at testing; "pytest" is the
        // precise framework signal, "test" adds a tag for skills.
        if lower.contains("pytest") {
            frameworks.push("pytest".into());
        }
        if lower.contains("test") {
            tags.push("testing".into());
        }

        // ── Tags ─────────────────────────────────────────────────────
        if lower.contains("docker") || lower.contains("dockerfile") {
            tags.push("docker".into());
        }
        if lower.contains("git ") || lower.contains("git log") || lower.contains("git commit") {
            tags.push("git-operations".into());
        }
        if lower.contains("wave") || lower.contains("multi-file") || lower.contains("decompose") {
            tags.push("wave-planning".into());
        }
        if lower.contains("readme") || lower.contains("documentation") || lower.contains("api doc")
        {
            tags.push("documentation".into());
        }

        // ── Role (priority ordered; first match wins) ────────────────
        //
        // Why: Higher-priority roles (docs, research, planner) are
        // lexically narrower than ops/engineer, so they need to win ties.
        // Without this ordering, "document the bash script" would be
        // routed to ops because "bash"/"script" matches first.
        let role = if lower.contains("document")
            || lower.contains("readme")
            || lower.contains("api doc")
        {
            Some("docs".into())
        } else if lower.contains("research")
            || lower.contains("check if")
            || lower.contains("accessible")
            || lower.contains("http status")
            || lower.contains("compare")
                && (lower.contains("best practice") || lower.contains("tokio"))
        {
            Some("research".into())
        } else if lower.contains("plan a ")
            || lower.contains("decompose")
            || lower.contains("multi-file")
            || lower.contains("multi file")
        {
            Some("planner".into())
        } else if lower.contains("run test")
            || lower.contains("run the pytest")
            || lower.contains("pytest suite")
            || lower.contains("failing test")
            || (lower.contains("identify") && lower.contains("test"))
        {
            Some("qa".into())
        } else if lower.contains("bash")
            || lower.contains("shell script")
            || lower.contains("dockerfile")
            || lower.contains("docker-compose")
            || lower.contains("deploy")
            || lower.contains("backup")
            || lower.contains("tar.gz")
        {
            Some("ops".into())
        } else {
            Some("engineer".into())
        };

        TaskSignals {
            languages,
            frameworks,
            role,
            tags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_python_language() {
        let s = TaskSignals::extract("Write a Python script that reads a CSV file");
        assert!(s.languages.contains(&"python".to_string()));
        assert_eq!(s.role.as_deref(), Some("engineer"));
    }

    #[test]
    fn extracts_rust_language() {
        let s = TaskSignals::extract("Write a Rust CLI tool using clap");
        assert!(s.languages.contains(&"rust".to_string()));
    }

    #[test]
    fn extracts_fastapi_framework() {
        let s = TaskSignals::extract("Create a FastAPI REST API with CRUD endpoints");
        assert!(s.frameworks.contains(&"fastapi".to_string()));
    }

    #[test]
    fn prioritizes_docs_over_ops() {
        let s = TaskSignals::extract("Update README.md to document the new CLI commands");
        assert_eq!(
            s.role.as_deref(),
            Some("docs"),
            "docs role should beat default engineer"
        );
    }

    #[test]
    fn detects_research_role_for_web_check() {
        let s = TaskSignals::extract("Check if https://httpbin.org/get is accessible");
        assert_eq!(s.role.as_deref(), Some("research"));
    }

    #[test]
    fn detects_planner_role_for_multi_file() {
        let s = TaskSignals::extract("Plan a multi-file Python project with API client and tests");
        assert_eq!(s.role.as_deref(), Some("planner"));
        assert!(s.tags.contains(&"wave-planning".to_string()));
    }

    #[test]
    fn detects_qa_role_for_pytest_suite() {
        let s = TaskSignals::extract(
            "Run the pytest suite in ./out/weather-app/ and identify failing tests",
        );
        assert_eq!(s.role.as_deref(), Some("qa"));
    }

    #[test]
    fn detects_ops_role_for_bash_script() {
        let s = TaskSignals::extract(
            "Write a bash script that backs up a directory to a timestamped tar.gz archive",
        );
        assert_eq!(s.role.as_deref(), Some("ops"));
    }

    #[test]
    fn detects_ops_role_for_docker_setup() {
        let s = TaskSignals::extract(
            "Write a Dockerfile and docker-compose.yml for a FastAPI application",
        );
        assert_eq!(s.role.as_deref(), Some("ops"));
        assert!(s.tags.contains(&"docker".to_string()));
    }

    #[test]
    fn defaults_to_engineer_role() {
        let s = TaskSignals::extract(
            "Create TypeScript interfaces and Zod schemas for a REST API response",
        );
        assert_eq!(s.role.as_deref(), Some("engineer"));
        assert!(s.languages.contains(&"typescript".to_string()));
    }

    #[test]
    fn pytest_framework_implies_testing_tag() {
        let s = TaskSignals::extract("pytest suite for FastAPI");
        assert!(s.frameworks.contains(&"pytest".to_string()));
        assert!(s.tags.contains(&"testing".to_string()));
    }

    #[test]
    fn extracts_go_language_from_golang_keyword() {
        let s = TaskSignals::extract("Write a Go HTTP client with net/http and httptest tests");
        assert!(
            s.languages.contains(&"go".to_string()),
            "expected go in {:?}",
            s.languages
        );
    }

    #[test]
    fn extracts_go_language_from_net_http() {
        let s = TaskSignals::extract("Build a service using net/http and goroutines");
        assert!(s.languages.contains(&"go".to_string()));
    }

    #[test]
    fn extracts_java_language() {
        let s = TaskSignals::extract("Write a Java Spring Boot REST API");
        assert!(s.languages.contains(&"java".to_string()));
    }

    #[test]
    fn extracts_ruby_language() {
        let s = TaskSignals::extract("Write a Ruby service object with RSpec");
        assert!(s.languages.contains(&"ruby".to_string()));
    }

    #[test]
    fn javascript_does_not_pull_in_java() {
        let s = TaskSignals::extract("Write a JavaScript module with fetch");
        assert!(s.languages.contains(&"javascript".to_string()));
        assert!(
            !s.languages.contains(&"java".to_string()),
            "javascript should not also match java: {:?}",
            s.languages
        );
    }

    #[test]
    fn git_log_adds_git_operations_tag() {
        let s = TaskSignals::extract(
            "Write a Python script that analyzes git log output and summarizes commits",
        );
        assert!(s.tags.contains(&"git-operations".to_string()));
    }
}
