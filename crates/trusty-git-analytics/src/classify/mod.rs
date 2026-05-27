//! Stage 2 of the pipeline: classify each collected commit using a four-tier
//! cascade.
//!
//! ## Tiers
//!
//! 1. **Exact** — Aho-Corasick multi-keyword match (case-insensitive).
//! 2. **Regex** — pre-compiled regex patterns.
//! 3. **Fuzzy** — structural heuristics (merge/revert/ticket-prefix).
//! 4. **LLM** — optional async fallback via an OpenAI-compatible API.
//!
//! Tiers 1–3 are synchronous and run in parallel across commits via Rayon.
//! Tier 4 is async and serialized.

pub mod classifier;
pub mod errors;
pub mod pipeline;
pub mod rules;
pub mod sources;
pub mod taxonomy;
pub mod tiers;

pub use classifier::{ClassificationEngine, ClassificationEngineConfig};
pub use errors::{ClassifyError, Result};
pub use pipeline::{ClassificationPipeline, ClassificationStats};
pub use rules::{Rule, RuleSet};
pub use taxonomy::{SubcategoryDef, TaxonomyRegistry, TopLevelCategory};
pub use tiers::ClassificationResult;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::taxonomy::{SubcategoryDef, TaxonomyRegistry, TopLevelCategory};
    use crate::core::models::ClassificationMethod;

    // ---------- Taxonomy registry tests ----------

    #[test]
    fn registry_resolves_builtin_subcategories() {
        let reg = TaxonomyRegistry::with_builtins();
        assert_eq!(reg.resolve("feature"), Some(TopLevelCategory::Feature));
        assert_eq!(reg.resolve("bugfix"), Some(TopLevelCategory::Bugfix));
        assert_eq!(
            reg.resolve("performance"),
            Some(TopLevelCategory::PlatformWork)
        );
        assert_eq!(reg.resolve("ci"), Some(TopLevelCategory::Ktlo));
        assert_eq!(
            reg.resolve("documentation"),
            Some(TopLevelCategory::Content)
        );
        assert_eq!(reg.resolve("refactor"), Some(TopLevelCategory::Maintenance));
    }

    #[test]
    fn registry_merges_user_defined() {
        let user = vec![
            SubcategoryDef::new("payments", TopLevelCategory::Integrations),
            SubcategoryDef::new("auth", TopLevelCategory::Feature),
        ];
        let reg = TaxonomyRegistry::new(user);
        assert_eq!(
            reg.resolve("payments"),
            Some(TopLevelCategory::Integrations)
        );
        assert_eq!(reg.resolve("auth"), Some(TopLevelCategory::Feature));
        assert_eq!(reg.resolve("feature"), Some(TopLevelCategory::Feature));
    }

    #[test]
    fn user_cannot_override_top_level_enum() {
        // A user-defined subcategory with the same name as a built-in just
        // replaces it; it must not corrupt the registry or break unrelated
        // lookups.
        let user = vec![SubcategoryDef::new(
            "security",
            TopLevelCategory::PlatformWork,
        )];
        let reg = TaxonomyRegistry::new(user);
        assert_eq!(
            reg.resolve("security"),
            Some(TopLevelCategory::PlatformWork)
        );
        assert_eq!(reg.resolve("bugfix"), Some(TopLevelCategory::Bugfix));
        let dup_count = reg
            .all()
            .iter()
            .filter(|d| d.name.eq_ignore_ascii_case("security"))
            .count();
        assert_eq!(dup_count, 1);
    }

    #[test]
    fn classification_result_has_top_level() {
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig::default(),
        )
        .expect("engine");
        let r = engine
            .classify_sync("feat: add new login flow", false)
            .expect("classified");
        assert_eq!(r.category, "feature");
        assert_eq!(r.top_level, Some(TopLevelCategory::Feature));

        let r = engine
            .classify_sync("fix: null deref in user lookup", false)
            .expect("classified");
        assert_eq!(r.category, "bugfix");
        assert_eq!(r.top_level, Some(TopLevelCategory::Bugfix));
    }

    #[test]
    fn unknown_subcategory_returns_none_top_level() {
        let reg = TaxonomyRegistry::with_builtins();
        assert!(reg.resolve("definitely-not-registered-xyz").is_none());
    }

    #[test]
    fn default_rules_is_non_empty() {
        let rs = rules::default_rules();
        assert!(!rs.rules.is_empty());
        let ids: Vec<&str> = rs.rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"cc-feat"));
        assert!(ids.contains(&"cc-fix"));
        assert!(ids.contains(&"jira-ticket"));
    }

    #[test]
    fn exact_matcher_classifies_feat() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        let r = m.classify("feat: add login flow").expect("match");
        assert_eq!(r.category, "feature");
    }

    #[test]
    fn exact_matcher_classifies_fix() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        let r = m
            .classify("fix: null pointer in user lookup")
            .expect("match");
        assert_eq!(r.category, "bugfix");
    }

    #[test]
    fn exact_matcher_returns_none_for_unknown() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        assert!(m
            .classify("the rain in spain falls mainly on the plain")
            .is_none());
    }

    #[test]
    fn regex_matcher_classifies_jira_ticket() {
        let rs = rules::default_rules();
        let m = tiers::regex_tier::RegexMatcher::new(&rs.rules).expect("build");
        let r = m
            .classify("PROJ-123 implement payment flow")
            .expect("match");
        assert_eq!(r.category, "feature");
    }

    #[test]
    fn regex_matcher_extracts_ticket_id() {
        let id =
            tiers::regex_tier::RegexMatcher::extract_ticket_id("Implement PROJ-456 with new logic");
        assert_eq!(id.as_deref(), Some("PROJ-456"));
    }

    #[test]
    fn fuzzy_detects_merge_via_flag() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f.classify("arbitrary message", true).expect("match");
        assert_eq!(r.category, "merge");
        assert_eq!(r.method, ClassificationMethod::FuzzyMatch);
    }

    #[test]
    fn fuzzy_detects_merge_via_text() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f
            .classify("Merge pull request #42 from feature/x", false)
            .expect("match");
        assert_eq!(r.category, "merge");
    }

    #[test]
    fn fuzzy_detects_revert() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f
            .classify("Revert \"feat: add buggy feature\"", false)
            .expect("match");
        assert_eq!(r.category, "revert");
    }

    #[test]
    fn engine_classify_batch_does_not_panic() {
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig::default(),
        )
        .expect("engine");
        let pairs: Vec<(&str, bool)> = vec![
            ("feat: add login", false),
            ("fix: null deref", false),
            ("docs: update readme", false),
            ("Merge branch 'main' into x", true),
            ("PROJ-1 minor update", false),
            ("totally random text", false),
        ];
        let results = engine.classify_batch(&pairs);
        assert_eq!(results.len(), pairs.len());
        assert_eq!(results[0].category, "feature");
        assert_eq!(results[1].category, "bugfix");
        assert_eq!(results[2].category, "documentation");
        assert_eq!(results[3].category, "merge");
    }

    /// Build a sync engine from default rules for quick rule-coverage tests.
    fn engine() -> ClassificationEngine {
        ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: false,
                ..Default::default()
            },
        )
        .expect("engine")
    }

    fn classify_sync(msg: &str) -> ClassificationResult {
        engine()
            .classify_sync(msg, false)
            .unwrap_or_else(ClassificationResult::unclassified)
    }

    // ---------- Conventional-commit prefix coverage ----------

    #[test]
    fn cc_prefix_variants_with_scope_and_bang() {
        assert_eq!(classify_sync("feat(api)!: drop v1").category, "breaking");
        assert_eq!(classify_sync("fix(ui): broken modal").category, "bugfix");
        assert_eq!(
            classify_sync("perf(db): faster query").category,
            "performance"
        );
        assert_eq!(
            classify_sync("docs(readme): typo").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("test(auth): cover edge case").category,
            "test"
        );
        assert_eq!(classify_sync("ci(release): publish step").category, "ci");
        assert_eq!(classify_sync("build(deps): upgrade").category, "build");
        assert_eq!(classify_sync("style(lint): tabs").category, "style");
        assert_eq!(
            classify_sync("refactor(core): extract helper").category,
            "refactor"
        );
        assert_eq!(classify_sync("chore(deps): bump axios").category, "chore");
    }

    #[test]
    fn cc_additional_prefixes() {
        assert_eq!(
            classify_sync("security: patch CVE-2024-1234").category,
            "security"
        );
        assert_eq!(
            classify_sync("deps: bump tokio to 1.40").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("i18n: add Spanish translations").category,
            "localization"
        );
        assert_eq!(classify_sync("release: v1.2.0").category, "release");
        assert_eq!(classify_sync("wip: still thinking").category, "wip");
    }

    // ---------- Merge / revert ----------

    #[test]
    fn merge_patterns_classify_to_merge() {
        assert_eq!(
            classify_sync("Merge pull request #42 from foo/bar").category,
            "merge"
        );
        assert_eq!(
            classify_sync("Merge branch 'main' into dev").category,
            "merge"
        );
        assert_eq!(classify_sync("Merge tag 'v1.0.0'").category, "merge");
    }

    #[test]
    fn revert_patterns_classify_to_revert() {
        assert_eq!(
            classify_sync(r#"Revert "feat: add login""#).category,
            "revert"
        );
        assert_eq!(
            classify_sync("This reverts commit abcdef1234567890.").category,
            "revert"
        );
    }

    // ---------- Initial commit / version bump ----------

    #[test]
    fn initial_commit_classifies_to_chore() {
        let r = classify_sync("Initial commit");
        assert_eq!(r.category, "chore");
        assert_eq!(r.subcategory.as_deref(), Some("initial"));
    }

    #[test]
    fn version_bump_classifies_to_release() {
        assert_eq!(classify_sync("Bump version to 1.2.3").category, "release");
        assert_eq!(classify_sync("Prepare release 2.0").category, "release");
        assert_eq!(classify_sync("Release v3.4.0").category, "release");
    }

    // ---------- Dependency updates ----------

    #[test]
    fn dependency_updates_classify_to_maintenance() {
        assert_eq!(
            classify_sync("Update dependencies for security").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Bump axios from 0.21.0 to 1.0.0").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Dependabot bumps lodash").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Update yarn.lock after install").category,
            "maintenance"
        );
    }

    // ---------- Lint / format / cleanup ----------

    #[test]
    fn lint_and_format_classify_to_style() {
        assert_eq!(classify_sync("Fix lint warnings").category, "style");
        assert_eq!(classify_sync("Run prettier on src/").category, "style");
        assert_eq!(classify_sync("Reformat with rustfmt").category, "style");
        assert_eq!(
            classify_sync("Trailing whitespace removal").category,
            "style"
        );
    }

    #[test]
    fn cleanup_classifies_to_refactor() {
        assert_eq!(classify_sync("Clean up old helpers").category, "refactor");
        assert_eq!(classify_sync("Remove unused imports").category, "refactor");
        assert_eq!(classify_sync("Delete dead code").category, "refactor");
    }

    // ---------- Review / PR feedback ----------

    #[test]
    fn review_feedback_classifies_to_refactor() {
        let r = classify_sync("Address review comments");
        assert_eq!(r.category, "refactor");
        assert_eq!(r.subcategory.as_deref(), Some("review"));

        assert_eq!(
            classify_sync("Apply suggestions from code review").category,
            "refactor"
        );
        assert_eq!(
            classify_sync("Incorporate review feedback").category,
            "refactor"
        );
    }

    // ---------- Infrastructure ----------

    #[test]
    fn infra_keywords_classify_appropriately() {
        assert_eq!(
            classify_sync("Update Dockerfile base image").category,
            "build"
        );
        assert_eq!(
            classify_sync("Add Helm chart for staging").category,
            "build"
        );
        assert_eq!(
            classify_sync("Switch k8s to nginx ingress").category,
            "build"
        );
        assert_eq!(
            classify_sync("Refactor Terraform modules").category,
            "build"
        );
        assert_eq!(
            classify_sync("Update github workflow for release").category,
            "ci"
        );
    }

    // ---------- Bug / fix prose ----------

    #[test]
    fn bug_fix_prose_classifies_to_bugfix() {
        for msg in [
            "Fix crash on empty input",
            "Fixes #123: bad redirect",
            "Resolve race condition in worker",
            "Closes #456",
        ] {
            let r = classify_sync(msg);
            assert_eq!(r.category, "bugfix", "{msg:?} => {r:?}");
        }
    }

    // ---------- Security / perf / docs / tests ----------

    #[test]
    fn security_prose_classifies_to_security() {
        assert_eq!(
            classify_sync("Patch XSS in comment renderer").category,
            "security"
        );
        assert_eq!(
            classify_sync("Fix SQL injection in search").category,
            "security"
        );
        assert_eq!(classify_sync("Address CVE-2023-0001").category, "security");
    }

    #[test]
    fn performance_prose_classifies_to_performance() {
        assert_eq!(
            classify_sync("Speed up query parser").category,
            "performance"
        );
        assert_eq!(
            classify_sync("Reduce memory usage in cache").category,
            "performance"
        );
        assert_eq!(
            classify_sync("Fix memory leak in worker").category,
            "performance"
        );
    }

    #[test]
    fn docs_prose_classifies_to_documentation() {
        assert_eq!(
            classify_sync("Update README with install instructions").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("Update changelog for 1.0").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("Add docstring to extractor").category,
            "documentation"
        );
    }

    #[test]
    fn test_prose_classifies_to_test() {
        assert_eq!(classify_sync("Add unit tests for parser").category, "test");
        assert_eq!(classify_sync("Fix flaky integration test").category, "test");
        assert_eq!(classify_sync("Improve test coverage").category, "test");
    }

    // ---------- WIP / database / config ----------

    #[test]
    fn wip_prose_classifies_to_wip() {
        assert_eq!(classify_sync("[WIP] still hacking").category, "wip");
        assert_eq!(classify_sync("WIP refactor of auth").category, "wip");
    }

    #[test]
    fn database_migration_classifies_to_feature() {
        let r = classify_sync("Add migration for users table");
        assert_eq!(r.category, "feature");
        assert_eq!(r.subcategory.as_deref(), Some("database"));
    }

    // ---------- Coverage smoke test: a representative corpus ----------

    /// Smoke test: confirm that with the expanded ruleset + catch-all,
    /// **zero** messages from a 200+ real-world corpus fall through as
    /// `"uncategorized"`. The catch-all routes residual prose to
    /// `category="maintenance"` with `subcategory="uncategorized"` (so
    /// reports can still flag low-confidence verdicts) — but the top-level
    /// `category` should never be the literal string `"uncategorized"`.
    #[test]
    fn corpus_uncategorized_below_1_percent() {
        let corpus: &[(&str, bool)] = &[
            // --- Conventional-commit prefixes ---
            ("feat: add login flow", false),
            ("feat(api)!: drop deprecated /v1 routes", false),
            ("feat(ui): user avatars on profile page", false),
            ("fix: null deref in user lookup", false),
            ("fix(ui): modal close button", false),
            ("fix(api): handle 404 gracefully", false),
            ("chore: tidy imports", false),
            ("chore(deps): bump axios", false),
            ("docs: clarify install steps", false),
            ("docs(readme): typo", false),
            ("test: add cases for parser", false),
            ("test(auth): cover token refresh", false),
            ("ci: enable rust beta", false),
            ("ci(release): publish step", false),
            ("perf: cache hot path", false),
            ("perf(db): index hot table", false),
            ("style: rustfmt run", false),
            ("style(lint): tabs to spaces", false),
            ("build: upgrade webpack", false),
            ("build(deps): upgrade vite", false),
            ("refactor: extract auth helper", false),
            ("refactor(core): split module", false),
            ("revert: revert bad commit", false),
            ("Revert \"feat: add buggy thing\"", false),
            ("revert!: undo breaking change", false),
            ("security: patch CVE-2024-1234", false),
            ("i18n: add Spanish translations", false),
            ("l10n: French strings update", false),
            ("release: v1.2.0", false),
            ("wip: still thinking", false),
            ("deps: bump tokio to 1.40", false),
            // --- Merge / revert plumbing ---
            ("Merge pull request #42 from foo/bar", true),
            ("Merge pull request #99 from contrib/feature", true),
            ("Merge branch 'main' into dev", true),
            ("Merge branch 'feature/x' of github.com:org/repo", true),
            ("Merge tag 'v1.0.0'", true),
            ("Merge remote-tracking branch 'origin/main'", true),
            ("This reverts commit abcd1234.", false),
            ("Revert \"chore: deprecated\"", false),
            // --- Initial / bootstrap ---
            ("Initial commit", false),
            ("First commit", false),
            ("Initial import of legacy codebase", false),
            ("Bootstrap repo with starter template", false),
            // --- Version / release ---
            ("Bump version to 1.2.3", false),
            ("Release v2.0", false),
            ("Release v3.4.0", false),
            ("Prepare release 2.0", false),
            ("Cut release 4.5.0", false),
            ("v1.2.3", false),
            ("1.2.3", false),
            ("bump to 2.0.0", false),
            // --- Dependency updates ---
            ("Update dependencies", false),
            ("Update dependencies for security", false),
            ("Bump tokio from 1.30 to 1.40", false),
            ("Bump axios from 0.21.0 to 1.0.0", false),
            ("Dependabot weekly update", false),
            ("Dependabot bumps lodash", false),
            ("Renovate: update dependencies", false),
            ("Snyk: upgrade vulnerable package", false),
            ("Pin dependencies to known-good versions", false),
            ("Update yarn.lock after install", false),
            ("Update package-lock.json", false),
            ("Update Cargo.lock", false),
            ("Update poetry.lock", false),
            // --- Lint / format ---
            ("Fix lint warnings", false),
            ("Run prettier on src/", false),
            ("Reformat with rustfmt", false),
            ("Apply clippy fix", false),
            ("Fix linting errors", false),
            ("Trailing whitespace removal", false),
            ("Fix indentation in module", false),
            ("eslint fix run", false),
            ("gofmt the whole tree", false),
            ("black format pass", false),
            // --- Code review / PR hygiene ---
            ("Address review comments", false),
            ("Code review feedback", false),
            ("Apply suggestions from code review", false),
            ("Incorporate review feedback", false),
            ("Address PR feedback", false),
            ("Reviewer feedback applied", false),
            ("Per review: rename variable", false),
            ("nit: typo in comment", false),
            ("Remove debug logging", false),
            ("Remove console.log statements", false),
            ("Remove print statements", false),
            ("Remove TODO comment", false),
            // --- Cleanup ---
            ("Clean up dead code", false),
            ("Cleanup", false),
            ("Remove unused imports", false),
            ("Remove unused variables", false),
            ("Delete dead code paths", false),
            ("Tidy up the worker module", false),
            ("Housekeeping in core/", false),
            // --- Infra (Docker/k8s/terraform/CI) ---
            ("Update Dockerfile", false),
            ("Update Dockerfile base image", false),
            ("Tweak docker-compose for dev", false),
            ("Add Helm chart for staging", false),
            ("Update kubernetes manifests", false),
            ("Switch k8s to nginx ingress", false),
            ("Refactor Terraform modules", false),
            ("Add ansible playbook for deploy", false),
            ("Update github workflow", false),
            ("Update github action versions", false),
            ("Tweak circleci config", false),
            ("Update gitlab ci pipeline", false),
            ("Add jenkinsfile for builds", false),
            // --- Cloud platforms ---
            ("Add aws lambda for image resize", false),
            ("Update cloudformation stack", false),
            ("Tweak cloudfront caching", false),
            ("Adjust cloudwatch alarms", false),
            ("Provision s3 bucket for backups", false),
            ("Update iam role for runner", false),
            ("Add dynamodb table for sessions", false),
            ("Deploy to google cloud run", false),
            ("Migrate to gke cluster", false),
            ("Configure bigquery dataset", false),
            ("Add azure functions for webhook", false),
            ("Configure aks cluster", false),
            ("Provision blob storage container", false),
            // --- Monitoring / observability ---
            ("Add datadog dashboard", false),
            ("Wire up prometheus metrics", false),
            ("Add grafana dashboard for latency", false),
            ("Configure sentry alerts", false),
            ("Tweak pagerduty escalation", false),
            ("Add opentelemetry tracing", false),
            ("Add tracing spans to handler", false),
            ("Tune alert rule thresholds", false),
            ("Kibana dashboard for logs", false),
            // --- Databases ---
            ("Switch to postgresql for prod", false),
            ("Update mysql driver", false),
            ("Add redis cache for sessions", false),
            ("Migrate to mongodb cluster", false),
            ("Reindex elasticsearch nodes", false),
            ("Apply database schema change", false),
            ("Add index migration for users", false),
            // --- Messaging ---
            ("Wire up kafka consumer", false),
            ("Switch from rabbitmq to nats", false),
            ("Add sqs queue for webhooks", false),
            ("Publish events to pub/sub topic", false),
            ("Drop AMQP fallback path", false),
            ("Add event bus for orders", false),
            // --- Networking ---
            ("Tune nginx config", false),
            ("Add traefik routing rules", false),
            ("Switch load balancer to ALB", false),
            ("Renew tls certificate", false),
            ("Add letsencrypt cert manager", false),
            ("Configure cdn caching", false),
            ("Add istio sidecar injection", false),
            // --- Language tooling ---
            ("Update Cargo.toml deps", false),
            ("Run cargo clippy", false),
            ("Tidy npm scripts", false),
            ("Migrate to pnpm", false),
            ("Update pyproject metadata", false),
            ("Switch to poetry from pip", false),
            ("Update tsconfig strict flags", false),
            ("Migrate gradle to maven", false),
            ("Update go.mod requirements", false),
            ("Add golangci-lint config", false),
            // --- Bug / fix prose ---
            ("Fix crash on empty input", false),
            ("Resolves #123", false),
            ("Closes #456 properly", false),
            ("Fix race condition in worker", false),
            ("Fix deadlock in scheduler", false),
            ("Fix memory leak in worker", false),
            ("Fix segfault on shutdown", false),
            ("Fix flaky test", false),
            ("Correct error handling", false),
            ("Handle null response from API", false),
            ("Prevent double submission", false),
            // --- Security prose ---
            ("Patch XSS vulnerability", false),
            ("Fix SQL injection in search", false),
            ("Address CVE-2023-0001", false),
            ("Mitigate CSRF on form submit", false),
            ("Defend against SSRF in webhook", false),
            // --- Performance prose ---
            ("Speed up query parser", false),
            ("Optimize hot path", false),
            ("Improve performance of search", false),
            ("Reduce memory usage in cache", false),
            ("Reduce latency in handler", false),
            // --- Docs prose ---
            ("Update README", false),
            ("Update README with install instructions", false),
            ("Update changelog for 1.0", false),
            ("Update CONTRIBUTING guidelines", false),
            ("Add CODE_OF_CONDUCT", false),
            ("Update LICENSE file", false),
            ("Add SECURITY.md", false),
            ("Add docstring to extractor", false),
            ("Add swagger spec for endpoints", false),
            ("Generate openapi schema", false),
            ("Publish postman collection", false),
            ("Update API documentation", false),
            // --- Tests ---
            ("Add unit tests for parser", false),
            ("Add integration tests for auth", false),
            ("Add e2e tests for checkout", false),
            ("Improve test coverage", false),
            ("Fix flaky integration test", false),
            // --- WIP / experiments ---
            ("WIP: experimenting", false),
            ("[WIP] refactor", false),
            ("Spike: try alternative algorithm", false),
            ("POC: new caching strategy", false),
            ("Prototype dashboard layout", false),
            ("Experiment with new parser", false),
            ("Trying out new ORM", false),
            // --- Database migrations ---
            ("Add migration for users table", false),
            // --- Ticket-only / refs ---
            ("PROJ-123 implement payment flow", false),
            ("ENG-456", false),
            ("ABC-789 wire up dashboards", false),
            ("refs #123", false),
            ("see #456", false),
            // --- Translations / content ---
            ("Add Spanish translations", false),
            ("Update French locale file", false),
            ("Translate UI strings to German", false),
            ("Add new landing page copy", false),
            ("Update blog post draft", false),
            ("Refresh marketing copy", false),
            // --- Assets ---
            ("Update favicon", false),
            ("Replace logo svg", false),
            ("Add new icons set", false),
            // --- Rollback ---
            ("Rollback to previous deploy", false),
            ("Roll back risky change", false),
            ("Back out broken commit", false),
            ("Undo regression", false),
            ("Revert to v1.0 behavior", false),
            // --- Auto-generated plumbing ---
            ("Squashed commit of feature branch", false),
            ("Cherry pick from main", false),
            ("Cherry-pick fix to release branch", false),
            // --- Generic prose (catch-all candidates) ---
            ("Add new module", false),
            ("Create new package", false),
            ("Modify the worker config", false),
            ("Adjust default timeout", false),
            ("Tweak the retry policy", false),
            ("Replace old helper with new util", false),
            ("Rename internal field", false),
            ("Move types to shared crate", false),
            ("Improve error message", false),
            ("Enhance UX on form", false),
            ("Drop legacy compatibility shim", false),
            ("Strip stale flags", false),
            ("Purge old experiments", false),
            ("Deprecate old endpoint", false),
            ("Handle edge case", false),
            ("Prevent regression", false),
            // --- Single-word / minimal ---
            ("WIP", false),
            ("fix", false),
            ("update", false),
            ("changes", false),
            ("cleanup", false),
            ("misc", false),
            ("temp", false),
            ("minor", false),
            // --- Adversarial: prose that historically went uncategorized ---
            ("foo bar baz", false),
            ("the rain in spain", false),
            ("xyzzy plugh frobnicate", false),
            ("something something something", false),
            ("just a small change", false),
        ];

        assert!(
            corpus.len() >= 200,
            "corpus must have at least 200 entries to be representative, got {}",
            corpus.len()
        );

        let results = engine().classify_batch(corpus);
        let total = results.len();
        let uncategorized = results
            .iter()
            .filter(|r| r.category == "uncategorized")
            .count();
        let pct = (uncategorized as f64 / total as f64) * 100.0;

        // With the catch-all rule we expect *zero* uncategorized verdicts.
        // Anything else means a corpus entry slipped through every rule
        // *and* the catch-all regex — which would be a real bug.
        assert_eq!(
            uncategorized, 0,
            "expected 0 uncategorized, got {uncategorized}/{total} ({pct:.2}%)"
        );
    }

    #[tokio::test]
    async fn engine_classify_full_cascade_catches_residual_via_catch_all() {
        // With the catch-all rule installed, even adversarial nonsense
        // messages get a deterministic verdict (category="maintenance",
        // subcategory="uncategorized") at low confidence (0.3). Downstream
        // reports can filter on the subcategory or confidence to flag
        // commits for LLM review.
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: false,
                ..Default::default()
            },
        )
        .expect("engine");
        let r = engine
            .classify("xyzzy plugh frobnicate quux nonsense", false)
            .await;
        assert_eq!(r.category, "maintenance");
        assert_eq!(r.subcategory.as_deref(), Some("uncategorized"));
        assert!(
            r.confidence <= 0.5,
            "catch-all verdicts must have low confidence, got {}",
            r.confidence
        );
    }

    #[tokio::test]
    async fn llm_classify_only_returns_none_when_disabled() {
        // Issue #99 regression: `llm_classify_only` must NOT fall back to
        // `classify_sync`. When `use_llm: false`, calling it for a message
        // that the regex tier would classify as `maintenance / 0.3` (the
        // catch-all) must still return `None` — otherwise the pipeline's
        // overwrite-guard would see the same low-confidence verdict back
        // and the LLM tier would never run.
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: false,
                ..Default::default()
            },
        )
        .expect("engine");
        assert!(engine
            .llm_classify_only("xyzzy plugh frobnicate")
            .await
            .is_none());
        assert_eq!(engine.llm_has_api_key(), None);
    }

    /// Panic-safe env-var save/restore (mirrors the pattern in
    /// `core::config::validator::tests::EnvVarGuard`). Without this, a
    /// failing assertion between `remove_var` and the restore would leak
    /// the cleared state to other parallel tests in the same binary.
    struct EnvVarGuard {
        name: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn remove(name: &'static str) -> Self {
            let original = std::env::var(name).ok();
            // SAFETY: 2024-edition env mutation; cleanup guaranteed by Drop.
            unsafe { std::env::remove_var(name) };
            Self { name, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: see `EnvVarGuard::remove`.
            unsafe {
                match self.original.as_deref() {
                    Some(v) => std::env::set_var(self.name, v),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    #[tokio::test]
    async fn llm_has_api_key_signals_misconfiguration() {
        // Issue #99 follow-up: when use_llm is on but no env key is set,
        // the engine should expose `Some(false)` so the pipeline can warn
        // at startup instead of silently producing no LLM verdicts.
        let _guard = EnvVarGuard::remove("OPENAI_API_KEY");

        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: true,
                llm_provider: "openai".to_string(),
                ..Default::default()
            },
        )
        .expect("engine");
        assert_eq!(engine.llm_has_api_key(), Some(false));
    }

    #[tokio::test]
    async fn pipeline_runs_against_in_memory_db() {
        use crate::core::config::Config;
        use crate::core::db::Database;
        use rusqlite::params;

        let mut db = Database::open_in_memory().expect("open");
        {
            let conn = db.connection();
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, is_merge) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["aaa", "x", "x@example.com", "2024-01-01T00:00:00Z", "feat: add x", "r", 0],
            )
            .expect("insert");
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, is_merge) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["bbb", "x", "x@example.com", "2024-01-01T00:00:00Z", "Merge branch foo", "r", 1],
            )
            .expect("insert");
        }

        let pipeline = ClassificationPipeline::new(Config::default());
        let stats = pipeline.run(&mut db).await.expect("run");
        assert_eq!(stats.total_commits, 2);
        assert_eq!(stats.classified, 2);
    }
}
