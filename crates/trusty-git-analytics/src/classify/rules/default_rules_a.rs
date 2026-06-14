//! Conventional-commit, breaking-change, merge, initial/release, and dependency rules.

use crate::classify::rules::types::Rule;

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
pub(super) fn conventional_commit_rules() -> Vec<Rule> {
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
pub(super) fn breaking_change_rules() -> Vec<Rule> {
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
pub(super) fn merge_plumbing_rules() -> Vec<Rule> {
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
pub(super) fn initial_and_release_rules() -> Vec<Rule> {
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
pub(super) fn dependency_rules() -> Vec<Rule> {
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
