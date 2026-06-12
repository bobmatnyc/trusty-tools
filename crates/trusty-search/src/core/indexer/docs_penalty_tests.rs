//! Tests for the mode-based file-type filter in [`super`].
//!
//! Why: extracted from `docs_penalty.rs` to keep the main file under 500 SLOC.
//! What: unit tests for `is_allowed_for_mode` and `doc_score_penalty`.
//! Test: this file is the test suite.

use super::*;

// ---- code mode -------------------------------------------------------

#[test]
fn test_code_mode_allows_source_extensions() {
    // Why: code mode must admit every mainstream source-file extension.
    for path in &[
        "src/main.rs",
        "src/lib/auth.ts",
        "components/Button.tsx",
        "pkg/handler.go",
        "app/views.py",
        "src/index.js",
        "src/index.mjs",
        "Main.java",
        "kernel.c",
        "engine.cpp",
        "include/header.h",
        "App.swift",
        "Module.kt",
        "lib.scala",
        "build.zig",
        "scripts/deploy.sh",
        "lib/util.lua",
        "app.rb",
        "Component.fs",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::Code),
            "{path}: expected to be allowed in code mode"
        );
    }
}

#[test]
fn test_code_mode_rejects_prose_and_data() {
    // Why: hard filter — prose docs and config/data must not appear in
    // code-mode results, even when their BM25 score is high.
    for path in &[
        "README.md",
        "CHANGELOG.md",
        "docs/intro.rst",
        "guide.txt",
        "Cargo.toml",
        "package.json",
        "pnpm-lock.yaml",
        "schema.xml",
        "rates.csv",
        "LICENSE",
    ] {
        assert!(
            !is_allowed_for_mode(path, SearchMode::Code),
            "{path}: expected to be rejected in code mode"
        );
    }
}

// ---- text mode -------------------------------------------------------

#[test]
fn test_text_mode_allows_prose_extensions() {
    for path in &[
        "docs/intro.md",
        "docs/INTRO.MD",
        "guide.rst",
        "notes.txt",
        "manual.adoc",
        "docs/overview.html",
        "paper.tex",
        "diary.org",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::Text),
            "{path}: expected to be allowed in text mode"
        );
    }
}

#[test]
fn test_text_mode_allows_named_docs_without_extension() {
    // Why: many repos ship LICENSE / CHANGELOG / NOTICE with no
    // extension. The basename-prefix rule must catch them.
    for path in &[
        "LICENSE",
        "CHANGELOG",
        "README",
        "NOTICE",
        "CONTRIBUTING",
        "docs/CHANGELOG.rst",
        "subdir/license-policy",
        "ReadMe",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::Text),
            "{path}: expected to be allowed in text mode"
        );
    }
}

#[test]
fn test_text_mode_rejects_source_and_data() {
    for path in &[
        "src/main.rs",
        "src/lib/auth.ts",
        "pkg/handler.go",
        "Cargo.toml",
        "package.json",
        "config.yaml",
        "schema.xml",
        "rates.csv",
    ] {
        assert!(
            !is_allowed_for_mode(path, SearchMode::Text),
            "{path}: expected to be rejected in text mode"
        );
    }
}

// ---- data mode -------------------------------------------------------

#[test]
fn test_data_mode_allows_data_extensions() {
    for path in &[
        "Cargo.toml",
        "package.json",
        "data.jsonl",
        "config.yaml",
        "config.yml",
        "schema.xml",
        "rates.csv",
        "rates.TSV",
        "Cargo.lock",
        "pnpm-lock.yaml",
        "migration.sql",
        "schema.graphql",
        "service.proto",
        "data.parquet",
        "db.sqlite",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::Data),
            "{path}: expected to be allowed in data mode"
        );
    }
}

#[test]
fn test_data_mode_rejects_source_and_prose() {
    for path in &[
        "src/main.rs",
        "src/lib/auth.ts",
        "pkg/handler.go",
        "README.md",
        "CHANGELOG.md",
        "LICENSE",
        "docs/intro.rst",
        "notes.txt",
    ] {
        assert!(
            !is_allowed_for_mode(path, SearchMode::Data),
            "{path}: expected to be rejected in data mode"
        );
    }
}

// ---- all mode --------------------------------------------------------

#[test]
fn test_all_mode_allows_everything() {
    // Why: `all` is the escape hatch — no filter, return whatever the
    // index produced.
    for path in &[
        "src/main.rs",
        "README.md",
        "Cargo.toml",
        "LICENSE",
        "rates.csv",
        "schema.xml",
        "weird-file-no-extension",
        "",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::All),
            "{path}: expected to be allowed in all mode"
        );
    }
}

// ---- xml routing -----------------------------------------------------

#[test]
fn test_xml_is_data_not_text() {
    // Why: `.xml` appears in both buckets historically; the spec assigns
    // it to `data` (structured markup) and excludes it from `text`.
    assert!(is_allowed_for_mode("schema.xml", SearchMode::Data));
    assert!(!is_allowed_for_mode("schema.xml", SearchMode::Text));
    assert!(!is_allowed_for_mode("schema.xml", SearchMode::Code));
}

#[test]
fn test_toml_is_data_not_text() {
    // Why: `.toml` is structured config; spec routes it to `data`.
    assert!(is_allowed_for_mode("Cargo.toml", SearchMode::Data));
    assert!(!is_allowed_for_mode("Cargo.toml", SearchMode::Text));
    assert!(!is_allowed_for_mode("Cargo.toml", SearchMode::Code));
}

// ---- issue #78: cross-project path exclusion in code mode ----------

#[test]
fn test_code_mode_excludes_claude_mpm_patch_paths() {
    // Why: `claude-mpm-patch/` is a vendored Python project under the
    // trusty-tools workspace root. Its `.py` / `.md` chunks routinely
    // out-ranked real Rust code in code-mode results because they
    // BM25-match identifier names. Issue #78 hard-filters them out.
    for path in &[
        "claude-mpm-patch/src/main.py",
        "claude-mpm-patch/docs/intro.md",
        "claude-mpm-patch/CHANGELOG.md",
        "CLAUDE-MPM-PATCH/src/foo.py",
        "some/nested/claude-mpm-patch/file.py",
    ] {
        assert!(
            !is_allowed_for_mode(path, SearchMode::Code),
            "{path}: expected to be excluded from code mode"
        );
    }
}

#[test]
fn test_code_mode_exclusion_does_not_affect_other_modes() {
    // Why: the exclusion is code-mode only. A user querying `text` mode
    // for prose in the vendored project should still see those docs.
    assert!(is_allowed_for_mode(
        "claude-mpm-patch/docs/intro.md",
        SearchMode::Text
    ));
    assert!(is_allowed_for_mode(
        "claude-mpm-patch/config.json",
        SearchMode::Data
    ));
    assert!(is_allowed_for_mode(
        "claude-mpm-patch/src/main.py",
        SearchMode::All
    ));
}

#[test]
fn test_sql_allowed_in_code_and_data() {
    // Why: SQL is both executable logic (migrations, stored procs, queries)
    // and structured/queryable data — it belongs in both `code` and `data`
    // mode results. It is not prose, so `text` mode correctly excludes it.
    for path in &[
        "migrations/0001_init.sql",
        "db/schema.SQL",
        "queries/users.sql",
    ] {
        assert!(
            is_allowed_for_mode(path, SearchMode::Code),
            "{path}: expected to be allowed in code mode"
        );
        assert!(
            is_allowed_for_mode(path, SearchMode::Data),
            "{path}: expected to be allowed in data mode"
        );
        assert!(
            !is_allowed_for_mode(path, SearchMode::Text),
            "{path}: expected to be rejected in text mode"
        );
    }
}
