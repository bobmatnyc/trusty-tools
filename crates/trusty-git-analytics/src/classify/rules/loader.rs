//! Rule file loader and built-in default ruleset.

use std::path::Path;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::types::{Rule, RuleSet};

/// Load a [`RuleSet`] from a YAML or JSON file.
///
/// Why: deployments often need to layer project-specific rules on top of the
/// built-in ruleset; loading from disk decouples the binary from the rule
/// definitions.
/// What: detects format by extension (`.json` → JSON, anything else → YAML),
/// deserializes into [`RuleSet`], and rejects empty rule lists so config
/// mistakes are surfaced loudly.
/// Test: see `tga::classify::tests` (round-trips serialization).
///
/// # Errors
///
/// - [`ClassifyError::Io`] if the file cannot be read.
/// - [`ClassifyError::Yaml`] / [`ClassifyError::Json`] on parse failure.
pub fn load_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path)?;
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let set: RuleSet = if is_json {
        serde_json::from_str(&text)?
    } else {
        serde_yaml::from_str(&text)?
    };

    if set.rules.is_empty() {
        return Err(ClassifyError::RuleLoad(format!(
            "rule file {} contained no rules",
            path.display()
        )));
    }
    Ok(set)
}

/// Return the built-in default ruleset.
///
/// Why: a comprehensive baseline ruleset keeps the "uncategorized" rate low
/// without an LLM. The set is assembled from named category helpers so each
/// group can be audited or revised in isolation.
/// What: concatenates rule lists from the per-category builders below and
/// wraps them in a [`RuleSet`] with `extend_defaults = true`.
/// Test: `crate::classify::tests::default_rules_is_non_empty` and the
/// corpus smoke test `corpus_uncategorized_below_1_percent` cover behaviour.
///
/// Covers (in order of inclusion):
///
/// - Conventional commit prefixes (`feat:`, `fix:`, …) — see
///   [`conventional_commit_rules`].
/// - Breaking-change marker — see [`breaking_change_rules`].
/// - GitHub merge / `Revert "..."` headers — see [`merge_plumbing_rules`].
/// - Initial / bootstrap / version-bump conventions —
///   see [`initial_and_release_rules`].
/// - Dependency, infrastructure (Docker / k8s / Terraform / GitHub Actions),
///   and code-review keywords — see [`dependency_rules`],
///   [`infra_rules`], [`code_review_and_cleanup_rules`].
/// - JIRA-style ticket patterns (`PROJ-123`) and GitHub issue refs (`#123`) —
///   see [`ticket_reference_rules`].
/// - Lower-priority keyword fallbacks for `bug`, `security`, `performance`,
///   etc. — see [`generic_keyword_rules`], [`generic_prose_rules`],
///   [`catch_all_rule`].
pub fn default_rules() -> RuleSet {
    let mut rules: Vec<Rule> = Vec::new();
    rules.extend(conventional_commit_rules());
    rules.extend(breaking_change_rules());
    rules.extend(merge_plumbing_rules());
    rules.extend(initial_and_release_rules());
    rules.extend(dependency_rules());
    rules.extend(code_review_and_cleanup_rules());
    rules.extend(infra_rules());
    rules.extend(generic_keyword_rules());
    rules.extend(cloud_platform_rules());
    rules.extend(observability_rules());
    rules.extend(datastore_rules());
    rules.extend(messaging_rules());
    rules.extend(networking_rules());
    rules.extend(language_tooling_rules());
    rules.extend(pr_hygiene_rules());
    rules.extend(experiment_and_rollback_rules());
    rules.extend(auto_generated_plumbing_rules());
    rules.extend(translation_rules());
    rules.extend(documentation_meta_rules());
    rules.extend(content_and_assets_rules());
    rules.extend(generic_prose_rules());
    rules.extend(ticket_reference_rules());
    rules.push(catch_all_rule());

    RuleSet {
        version: Some("1.0".into()),
        extend_defaults: true,
        rules,
    }
}

/// Why: conventional-commit prefixes (`feat:`, `fix:`, etc.) are the
/// strongest classification signal in modern repos; matching them at high
/// priority keeps a leading `feat(scope)!:` from being beaten by a stray
/// later "bug" word.
/// What: returns the Tier-1/Tier-2 rules for the standard
/// `feat|fix|chore|docs|refactor|test|ci|perf|style|build|revert` prefixes
/// plus a few extras (`security:`, `deps:`, `i18n:`, `release:`, `wip:`)
/// commonly seen in practice. Each rule combines exact-substring keywords
/// with an anchored regex variant for `feat(scope)!:` forms.
/// Test: covered by `cc_prefix_variants_with_scope_and_bang` and
/// `cc_additional_prefixes` in `classify::tests`.
fn conventional_commit_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "cc-feat".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec!["feat:".into(), "feature:".into()],
            patterns: vec![r"(?i)^\s*feat(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-fix".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["fix:".into(), "bugfix:".into(), "hotfix".into()],
            patterns: vec![r"(?i)^\s*fix(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-chore".into(),
            category: "chore".into(),
            subcategory: None,
            keywords: vec!["chore:".into()],
            patterns: vec![r"(?i)^\s*chore(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-docs".into(),
            category: "documentation".into(),
            subcategory: None,
            keywords: vec!["docs:".into(), "doc:".into()],
            patterns: vec![r"(?i)^\s*docs?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-refactor".into(),
            category: "refactor".into(),
            subcategory: None,
            keywords: vec!["refactor:".into(), "refactoring:".into()],
            patterns: vec![r"(?i)^\s*refactor(ing)?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-test".into(),
            category: "test".into(),
            subcategory: None,
            keywords: vec!["test:".into(), "tests:".into()],
            patterns: vec![r"(?i)^\s*tests?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-ci".into(),
            category: "ci".into(),
            subcategory: None,
            keywords: vec!["ci:".into()],
            patterns: vec![r"(?i)^\s*ci(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-perf".into(),
            category: "performance".into(),
            subcategory: None,
            keywords: vec!["perf:".into(), "performance:".into()],
            patterns: vec![r"(?i)^\s*perf(ormance)?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-style".into(),
            category: "style".into(),
            subcategory: None,
            keywords: vec!["style:".into()],
            patterns: vec![r"(?i)^\s*style(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-build".into(),
            category: "build".into(),
            subcategory: None,
            keywords: vec!["build:".into()],
            patterns: vec![r"(?i)^\s*build(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-revert".into(),
            category: "revert".into(),
            subcategory: None,
            // Include the leading word with trailing space so that an
            // auto-generated `Revert "feat: ..."` message wins the Tier-1
            // race against the inner `feat:` keyword.
            keywords: vec!["revert:".into(), "revert \"".into()],
            patterns: vec![
                r"(?i)^\s*revert(\([^)]*\))?!?:".into(),
                r#"(?i)^\s*revert\s+""#.into(), // "Revert "feat: ..."" auto-generated
                r"(?i)^\s*this reverts commit".into(),
            ],
            // Above `cc-feat` (100) and `cc-fix` (100) so that
            // `Revert "feat: ..."` is classified as a revert, not a feature.
            priority: 115,
            confidence: 0.9,
        },
        // Additional conventional-style prefixes seen in the wild.
        Rule {
            id: "cc-security".into(),
            category: "security".into(),
            subcategory: None,
            keywords: vec!["security:".into(), "sec:".into()],
            patterns: vec![r"(?i)^\s*(security|sec)(\([^)]*\))?!?:".into()],
            priority: 95,
            confidence: 0.9,
        },
        Rule {
            id: "cc-deps".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec!["deps:".into(), "dep:".into(), "dependencies:".into()],
            patterns: vec![r"(?i)^\s*dep(s|endencies)?(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-i18n".into(),
            category: "localization".into(),
            subcategory: None,
            keywords: vec!["i18n:".into(), "l10n:".into()],
            patterns: vec![r"(?i)^\s*(i18n|l10n)(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-release".into(),
            category: "release".into(),
            subcategory: None,
            keywords: vec!["release:".into()],
            patterns: vec![r"(?i)^\s*release(\([^)]*\))?!?:".into()],
            priority: 85,
            confidence: 0.9,
        },
        Rule {
            id: "cc-wip".into(),
            category: "wip".into(),
            subcategory: None,
            keywords: vec!["wip:".into()],
            patterns: vec![
                r"(?i)^\s*wip(\([^)]*\))?!?:".into(),
                r"(?i)^\s*\[wip\]".into(),
            ],
            priority: 85,
            confidence: 0.85,
        },
    ]
}

/// Why: the breaking-change marker must outrank ordinary conventional-commit
/// rules so `feat(api)!: drop v1` classifies as `breaking`, not `feature`.
/// What: returns a single rule matching the explicit `BREAKING CHANGE`
/// trailer and the `!:` shorthand at the start of any conventional prefix.
/// Test: covered by `cc_prefix_variants_with_scope_and_bang`
/// (`feat(api)!: drop v1` → `"breaking"`).
fn breaking_change_rules() -> Vec<Rule> {
    vec![Rule {
        id: "breaking-change".into(),
        category: "breaking".into(),
        subcategory: Some("api".into()),
        keywords: vec!["breaking change".into(), "breaking-change".into()],
        patterns: vec![
            r"(?i)breaking[\s-]change".into(),
            // Conventional-commit `!:` breaking marker
            // e.g. `feat(api)!: drop v1`, `refactor!: rename module`.
            r"(?i)^\s*(feat|fix|chore|refactor|perf|build|docs|ci|style|test)(\([^)]*\))?!:".into(),
        ],
        priority: 110,
        confidence: 0.9,
    }]
}

/// Why: GitHub-generated `Merge pull request #N from …` and `Merge branch …`
/// commit messages are noise on activity reports; catching them here at high
/// priority with high confidence stops the fuzzy tier from having to handle
/// them and tags them deterministically.
/// What: returns two rules for `merge pull request` / `merge branch` /
/// `merge tag` headers with subcategory routing.
/// Test: covered by `merge_patterns_classify_to_merge`.
fn merge_plumbing_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "merge-pr".into(),
            category: "merge".into(),
            subcategory: Some("pull-request".into()),
            keywords: vec!["merge pull request".into(), "merge remote-tracking".into()],
            patterns: vec![r"(?i)^\s*merge pull request #\d+".into()],
            priority: 105,
            confidence: 0.95,
        },
        Rule {
            id: "merge-branch".into(),
            category: "merge".into(),
            subcategory: Some("branch".into()),
            keywords: vec!["merge branch".into()],
            patterns: vec![
                r"(?i)^\s*merge branch ".into(),
                r"(?i)^\s*merge tag ".into(),
            ],
            priority: 105,
            confidence: 0.95,
        },
    ]
}

/// Why: bootstrap commits ("Initial commit", "Bootstrap repo") and
/// version-bump commits ("Release v1.2.3") are categorically distinct from
/// feature work and should not contaminate developer activity metrics.
/// What: returns two rules — one for initial/bootstrap headers (→ `chore`),
/// one for version-bump / release-tagging prose (→ `release`).
/// Test: covered by `initial_commit_classifies_to_chore` and
/// `version_bump_classifies_to_release`.
fn initial_and_release_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "initial-commit".into(),
            category: "chore".into(),
            subcategory: Some("initial".into()),
            keywords: vec!["initial commit".into(), "first commit".into()],
            patterns: vec![
                r"(?i)^\s*initial\s+commit\b".into(),
                r"(?i)^\s*first\s+commit\b".into(),
                r"(?i)^\s*initial\s+import\b".into(),
                r"(?i)^\s*bootstrap\s+repo".into(),
            ],
            priority: 95,
            confidence: 0.9,
        },
        Rule {
            id: "version-bump".into(),
            category: "release".into(),
            subcategory: Some("version-bump".into()),
            keywords: vec![
                "bump version".into(),
                "version bump".into(),
                "release version".into(),
                "prepare release".into(),
                "cut release".into(),
            ],
            patterns: vec![
                r"(?i)^\s*bump\s+(version|to\s+v?\d)".into(),
                r"(?i)^\s*release\s+v?\d+\.\d+".into(),
                r"(?i)^\s*v?\d+\.\d+\.\d+(\s*$|\s+release)".into(),
            ],
            priority: 90,
            confidence: 0.9,
        },
    ]
}

/// Why: Dependabot / Renovate / Snyk update commits are a major source of
/// otherwise-uncategorized output; tagging them as `maintenance/dependencies`
/// keeps developer activity reports clean.
/// What: returns two rules — one for prose ("update dependencies",
/// "bump foo from 1.0 to 2.0") and one for bot author markers.
/// Test: covered by `dependency_updates_classify_to_maintenance`.
fn dependency_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-deps-update".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec![
                "update deps".into(),
                "update dependencies".into(),
                "upgrade deps".into(),
                "upgrade dependencies".into(),
                "bump deps".into(),
                "bump dependencies".into(),
                "pin dependencies".into(),
                "lockfile".into(),
                "package-lock".into(),
                "yarn.lock".into(),
                "cargo.lock".into(),
                "poetry.lock".into(),
            ],
            patterns: vec![
                r"(?i)\bbump\s+\S+\s+from\s+\S+\s+to\s+\S+".into(), // Dependabot
                r"(?i)\bupdate\s+\S+\s+to\s+v?\d+\.\d+".into(),
            ],
            priority: 75,
            confidence: 0.9,
        },
        Rule {
            id: "kw-dependabot".into(),
            category: "maintenance".into(),
            subcategory: Some("dependencies".into()),
            keywords: vec!["dependabot".into(), "renovate".into(), "snyk".into()],
            patterns: vec![],
            priority: 75,
            confidence: 0.9,
        },
    ]
}

/// Why: linting, formatting, review-feedback, and cleanup commits all share
/// the property of being maintenance-style rather than feature work; grouping
/// their rules together keeps the classification policy easy to audit.
/// What: returns rules for lint runs (rustfmt / prettier / clippy / eslint),
/// generic format passes, code-review-feedback markers, and cleanup keywords
/// (`remove unused`, `dead code`).
/// Test: covered by `lint_and_format_classify_to_style`,
/// `cleanup_classifies_to_refactor`, `review_feedback_classifies_to_refactor`.
fn code_review_and_cleanup_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-lint".into(),
            category: "style".into(),
            subcategory: Some("lint".into()),
            keywords: vec![
                "fix lint".into(),
                "lint fix".into(),
                "fix linting".into(),
                "fix linter".into(),
                "satisfy lint".into(),
                "clippy fix".into(),
                "fix clippy".into(),
                "eslint fix".into(),
                "rubocop".into(),
                "prettier".into(),
                "gofmt".into(),
                "rustfmt".into(),
                "black format".into(),
            ],
            patterns: vec![r"(?i)\bfix(es|ed|ing)?\s+lint(ing|er)?\b".into()],
            priority: 75,
            confidence: 0.85,
        },
        Rule {
            id: "kw-format".into(),
            category: "style".into(),
            subcategory: Some("format".into()),
            keywords: vec![
                "reformat".into(),
                "code formatting".into(),
                "fix formatting".into(),
                "fix whitespace".into(),
                "trailing whitespace".into(),
                "fix indentation".into(),
            ],
            patterns: vec![],
            priority: 65,
            confidence: 0.8,
        },
        Rule {
            id: "kw-review".into(),
            category: "refactor".into(),
            subcategory: Some("review".into()),
            keywords: vec![
                "address review".into(),
                "address feedback".into(),
                "address comments".into(),
                "review feedback".into(),
                "review comments".into(),
                "pr feedback".into(),
                "code review".into(),
                "apply suggestions".into(),
                "incorporate review".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.8,
        },
        Rule {
            id: "kw-cleanup".into(),
            category: "refactor".into(),
            subcategory: Some("cleanup".into()),
            keywords: vec![
                "clean up".into(),
                "cleanup".into(),
                "dead code".into(),
                "remove unused".into(),
                "delete unused".into(),
                "tidy up".into(),
                "housekeeping".into(),
            ],
            patterns: vec![r"(?i)\bremove\s+(unused|dead|stale|obsolete)\b".into()],
            priority: 60,
            confidence: 0.8,
        },
    ]
}

/// Why: Dockerfile / k8s / Terraform / GitHub Actions changes are
/// infrastructure work that should be reported separately from product
/// development.
/// What: returns four rules for the major infra ecosystems (Docker,
/// Kubernetes/Helm, Terraform/Ansible, CI runners).
/// Test: covered by `infra_keywords_classify_appropriately`.
fn infra_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-docker".into(),
            category: "build".into(),
            subcategory: Some("docker".into()),
            keywords: vec![
                "dockerfile".into(),
                "docker-compose".into(),
                "docker compose".into(),
                "docker image".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-k8s".into(),
            category: "build".into(),
            subcategory: Some("kubernetes".into()),
            keywords: vec![
                "kubernetes".into(),
                "k8s".into(),
                "helm chart".into(),
                "kustomize".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-terraform".into(),
            category: "build".into(),
            subcategory: Some("terraform".into()),
            keywords: vec![
                "terraform".into(),
                "tf module".into(),
                "tflint".into(),
                "ansible playbook".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-github-actions".into(),
            category: "ci".into(),
            subcategory: Some("github-actions".into()),
            keywords: vec![
                "github action".into(),
                "github actions".into(),
                "github workflow".into(),
                "gh action".into(),
                ".github/workflows".into(),
                "circleci".into(),
                "gitlab ci".into(),
                "jenkinsfile".into(),
                "azure pipeline".into(),
                "azure pipelines".into(),
                "travis".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
    ]
}

/// Why: prose commit messages without conventional-commit prefixes still need
/// a category; these mid-priority keyword rules cover the most common verbs
/// and topics so the catch-all only fires on truly unstructured prose.
/// What: returns rules for "add/implement" (→ feature), "fix/resolve"
/// (→ bugfix), bug/regression prose, security/CVE keywords, performance,
/// docs, tests, config, database, and WIP markers.
/// Test: covered by `bug_fix_prose_classifies_to_bugfix`,
/// `security_prose_classifies_to_security`, `performance_prose_classifies_to_performance`,
/// `docs_prose_classifies_to_documentation`, `test_prose_classifies_to_test`,
/// `wip_prose_classifies_to_wip`, and `database_migration_classifies_to_feature`.
fn generic_keyword_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-add-implement".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec![
                "implement".into(),
                "introduce".into(),
                "add support".into(),
                "add feature".into(),
                "new feature".into(),
                "initial implementation".into(),
            ],
            patterns: vec![r"(?i)^\s*add\s+(new\s+)?(support\s+for|feature|the)\b".into()],
            priority: 45,
            confidence: 0.7,
        },
        Rule {
            id: "kw-fix-resolve".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec![
                "fix bug".into(),
                "fix issue".into(),
                "fix crash".into(),
                "fix regression".into(),
                "fix race".into(),
                "fix deadlock".into(),
                "fix leak".into(),
                "fix segfault".into(),
                "fix panic".into(),
                "fix error".into(),
                "resolve issue".into(),
                "resolve bug".into(),
                "resolve race".into(),
                "resolve deadlock".into(),
                "fixes #".into(),
                "fixes:".into(),
                "closes #".into(),
                "patch bug".into(),
                "correct behavior".into(),
                "correct handling".into(),
            ],
            patterns: vec![r"(?i)\b(fix(es|ed)?|resolves?|closes?)\s+#\d+".into()],
            priority: 60,
            confidence: 0.85,
        },
        Rule {
            id: "kw-bug".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["defect".into(), "regression".into()],
            patterns: vec![r"(?i)\b(bug|bugs)\b".into()],
            priority: 40,
            confidence: 0.7,
        },
        Rule {
            id: "kw-security".into(),
            category: "security".into(),
            subcategory: None,
            keywords: vec![
                "security patch".into(),
                "security fix".into(),
                "vulnerability".into(),
                "cve-".into(),
                "xss".into(),
                "csrf".into(),
                "sql injection".into(),
                "rce".into(),
                "ssrf".into(),
            ],
            patterns: vec![r"(?i)\bCVE-\d{4}-\d+".into()],
            priority: 80,
            confidence: 0.9,
        },
        Rule {
            id: "kw-performance".into(),
            category: "performance".into(),
            subcategory: None,
            keywords: vec![
                "speed up".into(),
                "speedup".into(),
                "optimize".into(),
                "optimization".into(),
                "improve performance".into(),
                "reduce latency".into(),
                "reduce memory".into(),
                "memory leak".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-docs".into(),
            category: "documentation".into(),
            subcategory: None,
            keywords: vec![
                "readme".into(),
                "changelog".into(),
                "update docs".into(),
                "documentation".into(),
                "javadoc".into(),
                "rustdoc".into(),
                "docstring".into(),
                "doc comment".into(),
            ],
            patterns: vec![r"(?i)\bupdate\s+(the\s+)?(readme|changelog|docs)\b".into()],
            priority: 50,
            confidence: 0.8,
        },
        Rule {
            id: "kw-test-add".into(),
            category: "test".into(),
            subcategory: None,
            keywords: vec![
                "add test".into(),
                "add tests".into(),
                "unit test".into(),
                "unit tests".into(),
                "integration test".into(),
                "e2e test".into(),
                "snapshot test".into(),
                "test coverage".into(),
                "test suite".into(),
                "fix test".into(),
                "fix tests".into(),
                "fix flaky".into(),
                "flaky test".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.85,
        },
        Rule {
            id: "kw-config".into(),
            category: "chore".into(),
            subcategory: Some("config".into()),
            keywords: vec![
                "update config".into(),
                "config change".into(),
                "configuration".into(),
                ".gitignore".into(),
                ".editorconfig".into(),
                "tsconfig".into(),
                "pyproject".into(),
                "package.json".into(),
                "cargo.toml".into(),
            ],
            patterns: vec![],
            priority: 50,
            confidence: 0.75,
        },
        Rule {
            id: "kw-database".into(),
            category: "feature".into(),
            subcategory: Some("database".into()),
            keywords: vec![
                "db migration".into(),
                "database migration".into(),
                "schema migration".into(),
                "add migration".into(),
                "new migration".into(),
                "alter table".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.8,
        },
        Rule {
            id: "kw-wip".into(),
            category: "wip".into(),
            subcategory: None,
            keywords: vec![
                "work in progress".into(),
                "todo:".into(),
                "fixme:".into(),
                "checkpoint".into(),
            ],
            patterns: vec![r"(?i)^\s*wip\b".into(), r"(?i)^\s*\[wip\]".into()],
            priority: 40,
            confidence: 0.65,
        },
    ]
}

/// Why: cloud-provider commits (AWS / GCP / Azure) cluster naturally as a
/// "cloud" category for organisations doing infrastructure work, distinct
/// from generic build/CI churn.
/// What: returns three rules, one per provider, with cloud-service keywords
/// (S3 / EC2 / Lambda for AWS, BigQuery / Cloud Run for GCP, etc.).
/// Test: smoke-tested via the broad corpus in `corpus_uncategorized_below_1_percent`.
fn cloud_platform_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-cloud-aws".into(),
            category: "cloud".into(),
            subcategory: Some("aws".into()),
            keywords: vec![
                "aws ".into(),
                " aws".into(),
                "amazon web services".into(),
                "cloudformation".into(),
                "cloudfront".into(),
                "cloudwatch".into(),
                " s3 ".into(),
                "s3 bucket".into(),
                " ec2".into(),
                " lambda".into(),
                "aws lambda".into(),
                " eks".into(),
                " ecs".into(),
                " rds".into(),
                " sqs".into(),
                " sns".into(),
                "iam role".into(),
                "iam policy".into(),
                "route53".into(),
                "route 53".into(),
                "dynamodb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-gcp".into(),
            category: "cloud".into(),
            subcategory: Some("gcp".into()),
            keywords: vec![
                " gcp".into(),
                "google cloud".into(),
                "bigquery".into(),
                "cloud run".into(),
                "cloud functions".into(),
                " gke".into(),
                "pub/sub".into(),
                "pubsub".into(),
                "gcs bucket".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-azure".into(),
            category: "cloud".into(),
            subcategory: Some("azure".into()),
            keywords: vec![
                " azure".into(),
                "azure functions".into(),
                "azure devops".into(),
                " aks".into(),
                "app service".into(),
                "blob storage".into(),
                "cosmos db".into(),
                "cosmosdb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
    ]
}

/// Why: monitoring / observability commits (Datadog dashboards, Prometheus
/// metrics, Sentry alerts) are a distinct slice of platform work worth
/// reporting separately from features.
/// What: returns a single rule with the major SaaS vendors and OSS tools
/// (OpenTelemetry, ELK stack, etc.).
/// Test: covered indirectly by `corpus_uncategorized_below_1_percent`.
fn observability_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-monitoring".into(),
        category: "monitoring".into(),
        subcategory: None,
        keywords: vec![
            "datadog".into(),
            "prometheus".into(),
            "grafana".into(),
            "sentry".into(),
            "newrelic".into(),
            "new relic".into(),
            "pagerduty".into(),
            "splunk".into(),
            "opentelemetry".into(),
            "otel".into(),
            "tracing".into(),
            "metrics".into(),
            "alerting".into(),
            "alert rule".into(),
            "dashboard".into(),
            "log aggregation".into(),
            "elk stack".into(),
            "kibana".into(),
            "logstash".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: database-engine commits cluster naturally as their own category,
/// useful for organisations tracking data-platform work.
/// What: returns a single rule listing the major relational, key-value, and
/// document databases plus schema-migration markers.
/// Test: covered indirectly by the corpus smoke test.
fn datastore_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-database".into(),
        category: "database".into(),
        subcategory: None,
        keywords: vec![
            "postgresql".into(),
            "postgres".into(),
            " mysql".into(),
            "mariadb".into(),
            "sqlite".into(),
            " redis".into(),
            "mongodb".into(),
            "mongo db".into(),
            "elasticsearch".into(),
            "cassandra".into(),
            "dynamodb".into(),
            "schema change".into(),
            "schema migration".into(),
            "db schema".into(),
            "database schema".into(),
            "index migration".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: message-bus and queueing work (Kafka, RabbitMQ, NATS, SQS) is a
/// distinct platform slice.
/// What: returns a single rule covering the major brokers and queue
/// abstractions.
/// Test: smoke-covered by the broad corpus.
fn messaging_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-messaging".into(),
        category: "messaging".into(),
        subcategory: None,
        keywords: vec![
            " kafka".into(),
            "rabbitmq".into(),
            "rabbit mq".into(),
            " sqs".into(),
            " sns".into(),
            "pub/sub".into(),
            "pubsub".into(),
            "nats".into(),
            "amqp".into(),
            "message queue".into(),
            "event bus".into(),
            "event stream".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: networking / web-infra commits (nginx, load balancers, TLS, service
/// meshes) belong together as a platform category.
/// What: returns a single rule with proxy, load-balancer, TLS, and
/// service-mesh keywords.
/// Test: smoke-covered by the broad corpus.
fn networking_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-networking".into(),
        category: "networking".into(),
        subcategory: None,
        keywords: vec![
            " nginx".into(),
            "traefik".into(),
            "load balancer".into(),
            " cdn".into(),
            " ssl".into(),
            " tls".into(),
            "certificate".into(),
            "cert manager".into(),
            "letsencrypt".into(),
            "let's encrypt".into(),
            "reverse proxy".into(),
            "ingress controller".into(),
            "service mesh".into(),
            " istio".into(),
            "linkerd".into(),
            "envoy proxy".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: language-specific tooling churn (cargo / npm / pip / maven / go)
/// is tooling work rather than product work and should be tagged for
/// activity reports.
/// What: returns one rule per major language ecosystem.
/// Test: smoke-covered by the broad corpus.
fn language_tooling_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-rust-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("rust".into()),
            keywords: vec![
                " cargo ".into(),
                "cargo run".into(),
                "cargo test".into(),
                "cargo build".into(),
                "cargo clippy".into(),
                " clippy".into(),
                "rustfmt".into(),
                "cargo.toml".into(),
                "rust crate".into(),
                "rust workspace".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-js-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("javascript".into()),
            keywords: vec![
                " npm ".into(),
                "npm install".into(),
                "npm run".into(),
                " yarn ".into(),
                " pnpm".into(),
                "package.json".into(),
                "node_modules".into(),
                "webpack".into(),
                " vite ".into(),
                " vitest".into(),
                "eslint".into(),
                "prettier".into(),
                "tsconfig".into(),
                "tsc build".into(),
                "babel".into(),
                "rollup".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-python-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("python".into()),
            keywords: vec![
                " poetry ".into(),
                " pip ".into(),
                "pip install".into(),
                "pyproject".into(),
                "virtualenv".into(),
                " venv".into(),
                " conda".into(),
                "requirements.txt".into(),
                "setup.py".into(),
                " ruff".into(),
                " mypy".into(),
                " pytest".into(),
                " tox".into(),
                " uv ".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-java-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("java".into()),
            keywords: vec![
                " maven".into(),
                " gradle".into(),
                "pom.xml".into(),
                "build.gradle".into(),
                "spring boot".into(),
                "springboot".into(),
                " jvm".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-go-tooling".into(),
            category: "tooling".into(),
            subcategory: Some("go".into()),
            keywords: vec![
                "go.mod".into(),
                "go.sum".into(),
                "goroutine".into(),
                "gofmt".into(),
                "go modules".into(),
                "go vet".into(),
                "golangci".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
    ]
}

/// Why: PR-hygiene commits (removing console.log, addressing review nits)
/// should be tagged as refactor/cleanup, not feature work.
/// What: returns a single rule for "remove debug", "nit:", "per review",
/// etc.
/// Test: smoke-covered by the broad corpus.
fn pr_hygiene_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-pr-hygiene".into(),
        category: "refactor".into(),
        subcategory: Some("cleanup".into()),
        keywords: vec![
            "remove debug".into(),
            "remove console.log".into(),
            "remove console log".into(),
            "remove print".into(),
            "remove todo".into(),
            "remove fixme".into(),
            "remove commented".into(),
            "remove logging".into(),
            "drop debug".into(),
            "strip debug".into(),
            "nit:".into(),
            " nits".into(),
            "per review".into(),
            "per cr".into(),
            "reviewer feedback".into(),
            "suggested changes".into(),
        ],
        patterns: vec![],
        priority: 60,
        confidence: 0.8,
    }]
}

/// Why: exploratory work (spikes, POCs, prototypes) and rollback commits
/// have distinct semantics — surfacing them keeps reports honest about how
/// much "real" work shipped.
/// What: returns two rules — one for experiment / spike / POC keywords, one
/// for rollback / undo prose.
/// Test: smoke-covered by the broad corpus.
fn experiment_and_rollback_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-experiment".into(),
            category: "experiment".into(),
            subcategory: None,
            keywords: vec![
                "experiment".into(),
                "experimental".into(),
                " spike ".into(),
                " spike:".into(),
                "proof of concept".into(),
                " poc ".into(),
                " poc:".into(),
                "prototype".into(),
                "prototyping".into(),
                "try out".into(),
                "trying out".into(),
            ],
            patterns: vec![],
            priority: 50,
            confidence: 0.75,
        },
        Rule {
            id: "kw-rollback".into(),
            category: "rollback".into(),
            subcategory: None,
            keywords: vec![
                "rollback".into(),
                "roll back".into(),
                " undo ".into(),
                "revert to".into(),
                "back out".into(),
                "backed out".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
    ]
}

/// Why: automatically generated git plumbing (squashed commits, cherry-picks,
/// auto-merges) is bookkeeping rather than development.
/// What: returns a single rule with the common plumbing markers.
/// Test: smoke-covered by the broad corpus.
fn auto_generated_plumbing_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-auto-generated".into(),
        category: "maintenance".into(),
        subcategory: Some("auto-generated".into()),
        keywords: vec![
            "squashed commit".into(),
            "cherry pick".into(),
            "cherry-pick".into(),
            "cherry-picked".into(),
            "auto-merge".into(),
            "automerge".into(),
            "auto generated".into(),
            "auto-generated".into(),
        ],
        patterns: vec![],
        priority: 80,
        confidence: 0.9,
    }]
}

/// Why: translation / localisation work has its own reporting category;
/// surfacing it deters under-counting i18n contributions.
/// What: returns a single rule with localisation prose keywords.
/// Test: smoke-covered by the broad corpus.
fn translation_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-translation".into(),
        category: "translation".into(),
        subcategory: None,
        keywords: vec![
            "translation".into(),
            "translations".into(),
            "translate".into(),
            "translated".into(),
            "localize".into(),
            "localization".into(),
            "localisation".into(),
            " locale".into(),
            "locale file".into(),
            " i18n".into(),
            " l10n".into(),
            "language file".into(),
        ],
        patterns: vec![],
        priority: 60,
        confidence: 0.85,
    }]
}

/// Why: repository-meta documentation (CONTRIBUTING, LICENSE, CODE_OF_CONDUCT)
/// and API documentation (Swagger / OpenAPI / docstrings) are both
/// documentation but have distinct subcategories for reporting.
/// What: returns two rules — one for repo-meta files, one for API/spec docs.
/// Test: smoke-covered by the broad corpus.
fn documentation_meta_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-repo-meta".into(),
            category: "documentation".into(),
            subcategory: Some("repo-meta".into()),
            keywords: vec![
                "contributing".into(),
                "code_of_conduct".into(),
                "code of conduct".into(),
                "license file".into(),
                "license.md".into(),
                "license.txt".into(),
                "security.md".into(),
                "support.md".into(),
                "authors.md".into(),
                "maintainers.md".into(),
                "history.md".into(),
                "news.md".into(),
                "releases.md".into(),
            ],
            patterns: vec![],
            priority: 60,
            confidence: 0.85,
        },
        Rule {
            id: "kw-api-docs".into(),
            category: "documentation".into(),
            subcategory: Some("api".into()),
            keywords: vec![
                "swagger".into(),
                "openapi".into(),
                "open api".into(),
                "postman".into(),
                "api docs".into(),
                "api documentation".into(),
                "jsdoc".into(),
                "tsdoc".into(),
                "rustdoc".into(),
                "javadoc".into(),
                "docstring".into(),
                "doc comment".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.85,
        },
    ]
}

/// Why: marketing copy, landing pages, and asset updates (icons, fonts, logos)
/// are categorisable distinctly from product code, useful for reports on
/// design / marketing throughput.
/// What: returns two rules — one for content/marketing prose, one for asset
/// file extensions.
/// Test: smoke-covered by the broad corpus.
fn content_and_assets_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-content".into(),
            category: "content-docs".into(),
            subcategory: None,
            keywords: vec![
                "landing page".into(),
                "blog post".into(),
                "blogpost".into(),
                "announcement".into(),
                "marketing copy".into(),
                "copy update".into(),
                "ui text".into(),
                "ui copy".into(),
                "microcopy".into(),
            ],
            patterns: vec![],
            priority: 55,
            confidence: 0.8,
        },
        Rule {
            id: "kw-assets".into(),
            category: "assets".into(),
            subcategory: None,
            keywords: vec![
                " svg".into(),
                " png".into(),
                " jpg".into(),
                " jpeg".into(),
                " gif".into(),
                " webp".into(),
                "favicon".into(),
                " icons".into(),
                "icon set".into(),
                " font ".into(),
                "fonts/".into(),
                "logo".into(),
            ],
            patterns: vec![],
            priority: 40,
            confidence: 0.7,
        },
    ]
}

/// Why: very short or generic prose ("Add new module", "update X", "remove Y",
/// "fix Z") still benefits from a low-confidence verdict so the catch-all
/// doesn't see it; these rules also handle minimal single-word messages
/// like "wip", "fix.", "update".
/// What: returns five rules — generic-add, generic-update, generic-remove,
/// generic-fix, and single-word minimal patterns. All run at low priority
/// (≤ 25) so structured commit prefixes win first.
/// Test: smoke-covered by `corpus_uncategorized_below_1_percent`.
fn generic_prose_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-generic-add".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*add(s|ed|ing)?\b".into(),
                r"(?i)^\s*create(s|d|ing)?\b".into(),
                r"(?i)^\s*introduce(s|d|ing)?\b".into(),
                r"(?i)^\s*support(s|ed|ing)?\b".into(),
                r"(?i)^\s*enable(s|d|ing)?\b".into(),
                r"(?i)^\s*allow(s|ed|ing)?\b".into(),
            ],
            priority: 20,
            confidence: 0.55,
        },
        Rule {
            id: "kw-generic-update".into(),
            category: "maintenance".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*update(s|d|ing)?\b".into(),
                r"(?i)^\s*modif(y|ies|ied|ying)\b".into(),
                r"(?i)^\s*change(s|d)?\b".into(),
                r"(?i)^\s*adjust(s|ed|ing)?\b".into(),
                r"(?i)^\s*tweak(s|ed|ing)?\b".into(),
                r"(?i)^\s*tune(s|d|ing)?\b".into(),
                r"(?i)^\s*edit(s|ed|ing)?\b".into(),
                r"(?i)^\s*rename(s|d|ing)?\b".into(),
                r"(?i)^\s*move(s|d|ing)?\b".into(),
                r"(?i)^\s*replace(s|d|ing)?\b".into(),
                r"(?i)^\s*switch(es|ed|ing)?\b".into(),
                r"(?i)^\s*upgrade(s|d|ing)?\b".into(),
                r"(?i)^\s*bump(s|ed|ing)?\b".into(),
                r"(?i)^\s*improve(s|d|ing)?\b".into(),
                r"(?i)^\s*enhance(s|d|ing)?\b".into(),
                r"(?i)^\s*polish(es|ed|ing)?\b".into(),
            ],
            priority: 18,
            confidence: 0.55,
        },
        Rule {
            id: "kw-generic-remove".into(),
            category: "refactor".into(),
            subcategory: Some("cleanup".into()),
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*remove(s|d|ing)?\b".into(),
                r"(?i)^\s*delete(s|d|ing)?\b".into(),
                r"(?i)^\s*drop(s|ped|ping)?\b".into(),
                r"(?i)^\s*strip(s|ped|ping)?\b".into(),
                r"(?i)^\s*purge(s|d|ing)?\b".into(),
                r"(?i)^\s*deprecate(s|d|ing)?\b".into(),
            ],
            priority: 18,
            confidence: 0.6,
        },
        Rule {
            id: "kw-generic-fix".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*fix(es|ed|ing)?\b".into(),
                r"(?i)^\s*correct(s|ed|ing)?\b".into(),
                r"(?i)^\s*repair(s|ed|ing)?\b".into(),
                r"(?i)^\s*patch(es|ed|ing)?\b".into(),
                r"(?i)^\s*handle(s|d|ing)?\b".into(),
                r"(?i)^\s*prevent(s|ed|ing)?\b".into(),
                r"(?i)^\s*avoid(s|ed|ing)?\b".into(),
            ],
            priority: 22,
            confidence: 0.6,
        },
        Rule {
            id: "kw-single-word".into(),
            category: "maintenance".into(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![
                r"(?i)^\s*wip\s*\.?\s*$".into(),
                r"(?i)^\s*fix\s*\.?\s*$".into(),
                r"(?i)^\s*update\s*\.?\s*$".into(),
                r"(?i)^\s*updates\s*\.?\s*$".into(),
                r"(?i)^\s*changes?\s*\.?\s*$".into(),
                r"(?i)^\s*cleanup\s*\.?\s*$".into(),
                r"(?i)^\s*tweak\s*\.?\s*$".into(),
                r"(?i)^\s*edit\s*\.?\s*$".into(),
                r"(?i)^\s*minor\s*\.?\s*$".into(),
                r"(?i)^\s*misc\s*\.?\s*$".into(),
                r"(?i)^\s*temp\s*\.?\s*$".into(),
                r"(?i)^\s*testing\s*\.?\s*$".into(),
            ],
            priority: 25,
            confidence: 0.5,
        },
    ]
}

/// Why: ticket-identifier references (JIRA `PROJ-123`, GitHub `#123`) signal
/// trackable work; classifying them as `feature/ticketed` keeps the report
/// pipeline's ticketed-stats accurate.
/// What: returns three rules — bare ticket-only messages, generic JIRA
/// ticket references inside messages, and GitHub issue refs (`refs #123`).
/// Test: covered by `regex_matcher_classifies_jira_ticket` and
/// `regex_matcher_extracts_ticket_id`.
fn ticket_reference_rules() -> Vec<Rule> {
    vec![
        // Bare ticket-only message (e.g. "PROJ-123" or "PROJ-456 some work").
        // The standalone "jira-ticket" rule below also matches, but this one
        // has explicit subcategory routing through "maintenance".
        Rule {
            id: "bare-ticket-prefix".into(),
            category: "maintenance".into(),
            subcategory: Some("ticketed".into()),
            keywords: vec![],
            patterns: vec![r"(?i)^\s*[A-Z][A-Z0-9]+-\d+([:\s].*)?$".into()],
            priority: 15,
            confidence: 0.5,
        },
        Rule {
            id: "jira-ticket".into(),
            category: "feature".into(),
            subcategory: Some("ticketed".into()),
            keywords: vec![],
            patterns: vec![r"\b[A-Z][A-Z0-9]+-\d+\b".into()],
            priority: 30,
            confidence: 0.7,
        },
        Rule {
            id: "github-issue-ref".into(),
            category: "feature".into(),
            subcategory: Some("issue".into()),
            keywords: vec![],
            patterns: vec![r"(?i)(^|\s)(refs?|references|see|for)\s+#\d+\b".into()],
            priority: 25,
            confidence: 0.6,
        },
    ]
}

/// Why: residual prose that escapes every other rule still needs a
/// deterministic verdict so the pipeline never falls through to the slow
/// fuzzy or LLM tiers when they are unavailable.
/// What: returns the lowest-priority catch-all rule that matches any
/// non-empty message and routes it to `category="maintenance",
/// subcategory="uncategorized"` at low confidence (0.3). Downstream reports
/// can filter on subcategory/confidence to flag commits for LLM review.
/// Test: covered by `corpus_uncategorized_below_1_percent` (asserts zero
/// `"uncategorized"` top-level verdicts even on adversarial prose).
fn catch_all_rule() -> Rule {
    Rule {
        id: "catch-all".into(),
        category: "maintenance".into(),
        subcategory: Some("uncategorized".into()),
        keywords: vec![],
        patterns: vec![r"(?s).+".into()],
        priority: 1,
        confidence: 0.3,
    }
}
