//! Tests for [`super`] — the `build:` config shape and its defaults (#6868).
//!
//! Why: every branch here must be provable without reading the operator's real
//! `~/.trusty-tools/trusty-mpm/config.yaml` and without depending on the test
//! host's core count, so `home`, `identity` and `cores` are all parameters.
//! Test: this file.

use super::*;

/// A `GithubPath` for the repo this workspace lives in.
fn identity() -> GithubPath {
    GithubPath {
        owner: "bobmatnyc".to_string(),
        repo: "trusty-tools".to_string(),
    }
}

/// The default target dir nests `<owner>/<repo>` under `~/.trusty-tools`.
///
/// Why: two checkouts of the same repo must land on ONE directory (that is the
/// warm-cache win) while two different repos must not (that would put unrelated
/// workspaces behind one cargo lock).
#[test]
fn rust_build_env_defaults_target_dir_from_git_remote() {
    let home = std::path::Path::new("/tmp/fake-home");
    let resolved = resolve_build_env(None, home, Some(&identity()), 16)
        .expect("a derivable identity must resolve");

    assert_eq!(
        resolved.cargo_target_dir,
        home.join(".trusty-tools")
            .join("cargo-target")
            .join("bobmatnyc")
            .join("trusty-tools"),
        "the default must be ~/.trusty-tools/cargo-target/<owner>/<repo>"
    );
    assert!(
        !resolved.target_dir_configured,
        "a derived path must not be reported as configured"
    );

    // A different repo gets a different directory — no shared cargo lock.
    let other = GithubPath {
        owner: "bobmatnyc".to_string(),
        repo: "other-repo".to_string(),
    };
    let other_resolved =
        resolve_build_env(None, home, Some(&other), 16).expect("second identity resolves");
    assert_ne!(
        resolved.cargo_target_dir, other_resolved.cargo_target_dir,
        "two repos must never share one target directory"
    );
}

/// Half the cores, floored at two.
///
/// Why: the floor is the whole point on a small host — `cores / 2` is 1 on a
/// 3-core box and 0 on a 1-core box, and a zero-job cargo invocation refuses to
/// run at all.
#[test]
fn rust_build_env_defaults_build_jobs_to_half_cores_min_two() {
    assert_eq!(default_build_jobs(16), 8, "16 cores → 8 jobs");
    assert_eq!(default_build_jobs(8), 4, "8 cores → 4 jobs");
    assert_eq!(default_build_jobs(4), 2, "4 cores → 2 jobs");
    assert_eq!(default_build_jobs(3), 2, "3 cores → the floor, not 1");
    assert_eq!(default_build_jobs(1), 2, "1 core → the floor, not 0");
    assert_eq!(
        default_build_jobs(0),
        2,
        "an unknown core count → the floor"
    );

    let resolved = resolve_build_env(None, std::path::Path::new("/tmp/h"), Some(&identity()), 16)
        .expect("resolves");
    assert_eq!(resolved.build_jobs, 8, "resolution must use the same rule");
}

/// The paste line is exactly the resolved values, in the documented order.
///
/// Why (#6868 closure condition 3): a PM pastes this verbatim. A drift between
/// what the row REPORTS and what the line SETS would hand an agent a build
/// environment nobody diagnosed.
#[test]
fn rust_build_env_paste_line_matches_resolved_values() {
    let env = ResolvedBuildEnv {
        cargo_target_dir: std::path::PathBuf::from("/shared/target"),
        build_jobs: 8,
        sccache: true,
        target_dir_configured: true,
    };

    assert_eq!(
        paste_line(&env),
        "CARGO_TARGET_DIR=/shared/target CARGO_BUILD_JOBS=8 RUSTC_WRAPPER=sccache SKIP_UI_BUILD=1"
    );

    // Every resolved value appears, so the line cannot silently drop one.
    let line = paste_line(&env);
    assert!(line.contains(&env.cargo_target_dir.display().to_string()));
    assert!(line.contains(&format!("CARGO_BUILD_JOBS={}", env.build_jobs)));
    assert!(
        line.ends_with("SKIP_UI_BUILD=1"),
        "the UI-build skip must close the line: {line}"
    );
}

/// No `RUSTC_WRAPPER=` when sccache is off — the default posture.
///
/// Why: handing an agent `RUSTC_WRAPPER=sccache` on a machine with no sccache
/// wired turns every cargo invocation into a hard failure.
#[test]
fn the_paste_line_omits_the_wrapper_when_sccache_is_off() {
    let env = ResolvedBuildEnv {
        cargo_target_dir: std::path::PathBuf::from("/shared/target"),
        build_jobs: 4,
        sccache: false,
        target_dir_configured: false,
    };
    let line = paste_line(&env);
    assert!(!line.contains("RUSTC_WRAPPER"), "{line}");
    assert_eq!(
        line,
        "CARGO_TARGET_DIR=/shared/target CARGO_BUILD_JOBS=4 SKIP_UI_BUILD=1"
    );
}

/// An absent section is the shipped defaults, never an error about a missing
/// file.
#[test]
fn an_absent_section_resolves_defaults() {
    let resolved = resolve_build_env(None, std::path::Path::new("/h"), Some(&identity()), 12)
        .expect("an absent section resolves");
    assert_eq!(resolved.build_jobs, 6);
    assert!(!resolved.sccache, "sccache is opt-in, never a default");
}

/// A configured directory wins, and a leading `~` expands against `home`.
#[test]
fn a_configured_target_dir_wins_over_the_remote_default() {
    let home = std::path::Path::new("/tmp/fake-home");
    let config = BuildConfig {
        cargo_target_dir: Some("~/elsewhere/target".to_string()),
        build_jobs: Some(3),
        sccache: Some(true),
    };
    let resolved = resolve_build_env(Some(&config), home, Some(&identity()), 16).expect("resolves");

    assert_eq!(resolved.cargo_target_dir, home.join("elsewhere/target"));
    assert!(resolved.target_dir_configured);
    assert_eq!(resolved.build_jobs, 3, "a configured job count wins");
    assert!(resolved.sccache);
}

/// No config and no remote is UNDETERMINED, never a guessed shared path.
///
/// Why: a fallback bucket shared by every remote-less checkout would put two
/// unrelated workspaces behind one cargo target lock, which is the failure the
/// per-repo keying exists to prevent.
#[test]
fn resolution_without_a_remote_or_a_config_is_an_error() {
    let err = resolve_build_env(None, std::path::Path::new("/h"), None, 8)
        .expect_err("no identity and no config must not resolve");
    assert_eq!(err, BuildEnvError::NoRepoIdentity);
    assert!(
        err.to_string().contains("origin"),
        "the error must name what could not be read: {err}"
    );

    // A configured directory removes the need for an identity entirely.
    let config = BuildConfig {
        cargo_target_dir: Some("/explicit/target".to_string()),
        ..BuildConfig::default()
    };
    assert!(
        resolve_build_env(Some(&config), std::path::Path::new("/h"), None, 8).is_ok(),
        "an explicit directory must not need a remote"
    );
}

/// A configured `0` is treated as absent.
///
/// Why: `CARGO_BUILD_JOBS=0` is a refusal, not a slow build, so it cannot be a
/// preference an operator held on purpose.
#[test]
fn a_zeroed_build_jobs_falls_back_to_the_default() {
    let config = BuildConfig {
        build_jobs: Some(0),
        ..BuildConfig::default()
    };
    let resolved = resolve_build_env(
        Some(&config),
        std::path::Path::new("/h"),
        Some(&identity()),
        16,
    )
    .expect("resolves");
    assert_eq!(resolved.build_jobs, 8);
}

/// The YAML shape round-trips, and an absent key stays absent on the wire.
#[test]
fn build_config_yaml_round_trips() {
    let config = BuildConfig {
        cargo_target_dir: Some("/shared/target".to_string()),
        build_jobs: Some(8),
        sccache: Some(false),
    };
    let yaml = serde_yaml::to_string(&config).expect("serialises");
    let back: BuildConfig = serde_yaml::from_str(&yaml).expect("round-trips");
    assert_eq!(back, config);

    let empty = serde_yaml::to_string(&BuildConfig::default()).expect("serialises");
    assert!(
        !empty.contains("cargo_target_dir"),
        "an absent key must not be emitted: {empty}"
    );
}

/// The seeded block parses back to exactly the values it was rendered from.
///
/// Why: the repair appends this text to a YAML file rather than re-serialising
/// the whole config, so nothing else proves the block is valid YAML.
#[test]
fn the_seeded_block_parses_back_to_the_resolved_values() {
    let env = ResolvedBuildEnv {
        cargo_target_dir: std::path::PathBuf::from("/shared/target: with colon"),
        build_jobs: 8,
        sccache: false,
        target_dir_configured: false,
    };
    let block = default_build_section_yaml(&env);

    #[derive(serde::Deserialize)]
    struct Doc {
        build: BuildConfig,
    }
    let doc: Doc = serde_yaml::from_str(&block).expect("the seeded block must be valid YAML");
    assert_eq!(
        doc.build.cargo_target_dir.as_deref(),
        Some("/shared/target: with colon"),
        "a path containing a colon must survive quoting"
    );
    assert_eq!(doc.build.build_jobs, Some(8));
    assert_eq!(doc.build.sccache, Some(false));
}
