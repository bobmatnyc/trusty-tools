//! Unit tests for `config::mapreduce` (Phase 1, #690 / #680).
//!
//! Why: kept in a separate file to honour the 500-line cap on
//! `mapreduce.rs` (CLAUDE.md §"500-line file size hard cap").
//! What: covers `MapMode` parsing, `MapReduceConfig` defaults/env/fallback,
//! and the `select_review_mode` decision table.
//! Test: this file is the test module itself.

use super::*;
use serial_test::serial;

fn clear_map_env() {
    unsafe {
        std::env::remove_var(ENV_MAP_MODE);
        std::env::remove_var(ENV_FILE_THRESHOLD);
        std::env::remove_var(ENV_PER_FILE_CHARS);
        std::env::remove_var(ENV_CONCURRENCY);
        std::env::remove_var(ENV_TOTAL_CHAR_BUDGET);
        std::env::remove_var(ENV_MAX_CALLS);
        std::env::remove_var(ENV_MAX_FINDINGS);
        std::env::remove_var(ENV_SYNTHESIS);
        std::env::remove_var(ENV_PER_FILE_SEARCH);
    }
}

// ── MapMode parsing ───────────────────────────────────────────────────────────

#[test]
fn map_mode_parse_auto() {
    assert_eq!(MapMode::from_str_opt("auto"), Some(MapMode::Auto));
    assert_eq!(MapMode::from_str_opt("AUTO"), Some(MapMode::Auto));
    assert_eq!(MapMode::from_str_opt("Auto"), Some(MapMode::Auto));
}

#[test]
fn map_mode_parse_always() {
    assert_eq!(MapMode::from_str_opt("always"), Some(MapMode::Always));
    assert_eq!(MapMode::from_str_opt("ALWAYS"), Some(MapMode::Always));
}

#[test]
fn map_mode_parse_never() {
    assert_eq!(MapMode::from_str_opt("never"), Some(MapMode::Never));
    assert_eq!(MapMode::from_str_opt("NEVER"), Some(MapMode::Never));
}

#[test]
fn map_mode_parse_garbage_returns_none() {
    assert_eq!(MapMode::from_str_opt("yes"), None);
    assert_eq!(MapMode::from_str_opt("1"), None);
    assert_eq!(MapMode::from_str_opt("mapreduce"), None);
    assert_eq!(MapMode::from_str_opt(""), None);
}

#[test]
#[serial]
fn map_mode_env_unset_is_auto() {
    clear_map_env();
    assert_eq!(MapMode::from_env(), MapMode::Auto);
}

#[test]
#[serial]
fn map_mode_env_always() {
    clear_map_env();
    unsafe { std::env::set_var(ENV_MAP_MODE, "always") };
    assert_eq!(MapMode::from_env(), MapMode::Always);
    clear_map_env();
}

#[test]
#[serial]
fn map_mode_env_never() {
    clear_map_env();
    unsafe { std::env::set_var(ENV_MAP_MODE, "never") };
    assert_eq!(MapMode::from_env(), MapMode::Never);
    clear_map_env();
}

#[test]
#[serial]
fn map_mode_env_garbage_falls_back_to_auto() {
    clear_map_env();
    unsafe { std::env::set_var(ENV_MAP_MODE, "banana") };
    assert_eq!(MapMode::from_env(), MapMode::Auto);
    clear_map_env();
}

#[test]
#[serial]
fn map_mode_env_empty_is_auto() {
    clear_map_env();
    unsafe { std::env::set_var(ENV_MAP_MODE, "") };
    assert_eq!(MapMode::from_env(), MapMode::Auto);
    clear_map_env();
}

// ── MapReduceConfig defaults ──────────────────────────────────────────────────

#[test]
fn mapreduce_config_defaults() {
    let cfg = MapReduceConfig::default();
    assert_eq!(cfg.mode, MapMode::Auto, "default mode must be auto");
    assert_eq!(cfg.file_threshold, 12);
    assert_eq!(cfg.per_file_chars, 120_000);
    assert_eq!(cfg.concurrency, 4);
    assert_eq!(cfg.total_char_budget, 1_000_000);
    assert_eq!(cfg.max_calls, 40);
    assert_eq!(cfg.max_findings, 50);
    assert!(cfg.synthesis, "synthesis must default to true");
    assert!(
        !cfg.per_file_search,
        "per_file_search must default to false"
    );
}

#[test]
#[serial]
fn mapreduce_env_overrides() {
    clear_map_env();
    unsafe {
        std::env::set_var(ENV_MAP_MODE, "always");
        std::env::set_var(ENV_FILE_THRESHOLD, "20");
        std::env::set_var(ENV_PER_FILE_CHARS, "80000");
        std::env::set_var(ENV_CONCURRENCY, "8");
        std::env::set_var(ENV_TOTAL_CHAR_BUDGET, "500000");
        std::env::set_var(ENV_MAX_CALLS, "20");
        std::env::set_var(ENV_MAX_FINDINGS, "30");
        std::env::set_var(ENV_SYNTHESIS, "false");
        std::env::set_var(ENV_PER_FILE_SEARCH, "true");
    }
    let cfg = MapReduceConfig::from_env();
    assert_eq!(cfg.mode, MapMode::Always);
    assert_eq!(cfg.file_threshold, 20);
    assert_eq!(cfg.per_file_chars, 80_000);
    assert_eq!(cfg.concurrency, 8);
    assert_eq!(cfg.total_char_budget, 500_000);
    assert_eq!(cfg.max_calls, 20);
    assert_eq!(cfg.max_findings, 30);
    assert!(!cfg.synthesis, "env false must disable synthesis");
    assert!(cfg.per_file_search, "env true must enable per_file_search");
    clear_map_env();
}

#[test]
#[serial]
fn mapreduce_malformed_usize_falls_back_to_default() {
    clear_map_env();
    unsafe {
        std::env::set_var(ENV_FILE_THRESHOLD, "not_a_number");
        std::env::set_var(ENV_CONCURRENCY, "-1");
        std::env::set_var(ENV_MAX_CALLS, "1.5");
    }
    let cfg = MapReduceConfig::from_env();
    assert_eq!(cfg.file_threshold, DEFAULT_MAP_FILE_THRESHOLD);
    assert_eq!(cfg.concurrency, DEFAULT_MAP_CONCURRENCY);
    assert_eq!(cfg.max_calls, DEFAULT_MAP_MAX_CALLS);
    clear_map_env();
}

#[test]
#[serial]
fn mapreduce_malformed_bool_falls_back_to_default() {
    clear_map_env();
    unsafe {
        std::env::set_var(ENV_SYNTHESIS, "maybe");
        std::env::set_var(ENV_PER_FILE_SEARCH, "2");
    }
    let cfg = MapReduceConfig::from_env();
    // Defaults: synthesis=true, per_file_search=false
    assert!(
        cfg.synthesis,
        "malformed synthesis must fall back to default true"
    );
    assert!(
        !cfg.per_file_search,
        "malformed per_file_search must fall back to default false"
    );
    clear_map_env();
}

#[test]
#[serial]
fn mapreduce_synthesis_env_toggle() {
    clear_map_env();
    for (val, expected) in [
        ("true", true),
        ("1", true),
        ("yes", true),
        ("on", true),
        ("false", false),
        ("0", false),
        ("no", false),
        ("off", false),
    ] {
        unsafe { std::env::set_var(ENV_SYNTHESIS, val) };
        let cfg = MapReduceConfig::from_env();
        assert_eq!(
            cfg.synthesis, expected,
            "TRUSTY_REVIEW_MAP_SYNTHESIS={val:?} → expected {expected}"
        );
    }
    clear_map_env();
}

// ── select_review_mode decision table ─────────────────────────────────────────

fn default_cfg() -> MapReduceConfig {
    MapReduceConfig::default()
}

#[test]
fn select_never_always_returns_unified() {
    let mut cfg = default_cfg();
    cfg.mode = MapMode::Never;
    // Large diff, many files — Never always wins.
    let stats = DiffStats {
        diff_chars: MAX_DIFF_CHARS + 1_000,
        file_count: 100,
    };
    assert_eq!(select_review_mode(stats, &cfg), ReviewPath::Unified);
}

#[test]
fn select_always_forces_mapreduce() {
    let mut cfg = default_cfg();
    cfg.mode = MapMode::Always;
    // Small diff, few files — Always always wins.
    let stats = DiffStats {
        diff_chars: 100,
        file_count: 1,
    };
    assert_eq!(select_review_mode(stats, &cfg), ReviewPath::MapReduce);
}

#[test]
fn select_auto_small_diff_is_unified() {
    let cfg = default_cfg();
    // diff_chars <= MAX_DIFF_CHARS and file_count <= file_threshold.
    let stats = DiffStats {
        diff_chars: MAX_DIFF_CHARS,
        file_count: cfg.file_threshold,
    };
    assert_eq!(
        select_review_mode(stats, &cfg),
        ReviewPath::Unified,
        "diff exactly at cap and files exactly at threshold must be Unified"
    );
}

#[test]
fn select_auto_would_truncate_triggers_mapreduce() {
    let cfg = default_cfg();
    // diff_chars > MAX_DIFF_CHARS → would truncate.
    let stats = DiffStats {
        diff_chars: MAX_DIFF_CHARS + 1,
        file_count: 1,
    };
    assert_eq!(
        select_review_mode(stats, &cfg),
        ReviewPath::MapReduce,
        "diff one char over cap must trigger map-reduce"
    );
}

#[test]
fn select_auto_many_files_triggers_mapreduce() {
    let cfg = default_cfg();
    // diff_chars fine but file_count > threshold.
    let stats = DiffStats {
        diff_chars: 1_000,
        file_count: cfg.file_threshold + 1,
    };
    assert_eq!(
        select_review_mode(stats, &cfg),
        ReviewPath::MapReduce,
        "file count one over threshold must trigger map-reduce"
    );
}

#[test]
fn select_auto_boundary_at_exactly_max_chars() {
    let cfg = default_cfg();
    // Exactly MAX_DIFF_CHARS → NOT truncated (≤ cap).
    let at = DiffStats {
        diff_chars: MAX_DIFF_CHARS,
        file_count: 1,
    };
    assert_eq!(select_review_mode(at, &cfg), ReviewPath::Unified);

    // One over → truncated.
    let over = DiffStats {
        diff_chars: MAX_DIFF_CHARS + 1,
        file_count: 1,
    };
    assert_eq!(select_review_mode(over, &cfg), ReviewPath::MapReduce);
}

#[test]
fn select_auto_boundary_at_exactly_threshold() {
    let cfg = default_cfg();
    // Exactly file_threshold → NOT over threshold.
    let at = DiffStats {
        diff_chars: 1_000,
        file_count: cfg.file_threshold,
    };
    assert_eq!(select_review_mode(at, &cfg), ReviewPath::Unified);

    // One over → over threshold.
    let over = DiffStats {
        diff_chars: 1_000,
        file_count: cfg.file_threshold + 1,
    };
    assert_eq!(select_review_mode(over, &cfg), ReviewPath::MapReduce);
}

#[test]
fn select_auto_both_conditions_triggers_mapreduce() {
    let cfg = default_cfg();
    // Both conditions true → map-reduce.
    let stats = DiffStats {
        diff_chars: MAX_DIFF_CHARS + 1,
        file_count: cfg.file_threshold + 1,
    };
    assert_eq!(select_review_mode(stats, &cfg), ReviewPath::MapReduce);
}

#[test]
fn select_auto_zero_files_zero_chars_is_unified() {
    let cfg = default_cfg();
    let stats = DiffStats {
        diff_chars: 0,
        file_count: 0,
    };
    assert_eq!(select_review_mode(stats, &cfg), ReviewPath::Unified);
}

#[test]
fn select_auto_custom_threshold_respects_config() {
    let mut cfg = default_cfg();
    cfg.file_threshold = 3;
    let small = DiffStats {
        diff_chars: 1_000,
        file_count: 3,
    };
    assert_eq!(select_review_mode(small, &cfg), ReviewPath::Unified);
    let large = DiffStats {
        diff_chars: 1_000,
        file_count: 4,
    };
    assert_eq!(select_review_mode(large, &cfg), ReviewPath::MapReduce);
}

// ── Default / Display ─────────────────────────────────────────────────────────

#[test]
fn map_mode_default_is_auto() {
    assert_eq!(MapMode::default(), MapMode::Auto);
}

#[test]
fn map_mode_display() {
    assert_eq!(MapMode::Auto.to_string(), "auto");
    assert_eq!(MapMode::Always.to_string(), "always");
    assert_eq!(MapMode::Never.to_string(), "never");
}

#[test]
fn review_path_equality() {
    assert_eq!(ReviewPath::Unified, ReviewPath::Unified);
    assert_eq!(ReviewPath::MapReduce, ReviewPath::MapReduce);
    assert_ne!(ReviewPath::Unified, ReviewPath::MapReduce);
}
