//! Language-tooling, PR-hygiene, experiment, translation, documentation, content, generic-prose, ticket-reference, and catch-all rules.

use crate::classify::rules::types::Rule;

/// Why: language-specific tooling churn (cargo / npm / pip / maven / go)
/// is tooling work rather than product work and should be tagged for
/// activity reports.
/// What: returns one rule per major language ecosystem.
/// Test: smoke-covered by the broad corpus.
pub(super) fn language_tooling_rules() -> Vec<Rule> {
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
pub(super) fn pr_hygiene_rules() -> Vec<Rule> {
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
pub(super) fn experiment_and_rollback_rules() -> Vec<Rule> {
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
pub(super) fn auto_generated_plumbing_rules() -> Vec<Rule> {
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
pub(super) fn translation_rules() -> Vec<Rule> {
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
pub(super) fn documentation_meta_rules() -> Vec<Rule> {
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
pub(super) fn content_and_assets_rules() -> Vec<Rule> {
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
pub(super) fn generic_prose_rules() -> Vec<Rule> {
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
pub(super) fn ticket_reference_rules() -> Vec<Rule> {
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
pub(super) fn catch_all_rule() -> Rule {
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
