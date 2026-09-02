//! Unit tests for the `log_drain:` config section (#6535).
//!
//! Why: the section's whole contract is "a typo is an error, an absent section
//! is silence". Both halves need pinning, and both are pure functions over a
//! deserialised struct — no daemon, no network, no real home directory.
//! What: YAML round-trip, the default-fill path, and one test per
//! [`super::LogDrainConfigError`] variant.

use std::path::Path;

use super::*;

/// Parse a `log_drain:` YAML fragment into a full [`TrustyToolsConfig`].
fn config_from_yaml(yaml: &str) -> TrustyToolsConfig {
    serde_yaml::from_str(yaml).expect("fixture YAML parses")
}

/// The home every test resolves `~` and the built-in source against.
fn home() -> &'static Path {
    Path::new("/fixture-home")
}

#[test]
fn log_drain_config_yaml_round_trip() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "s3://logs-bucket/drain"
  interval_secs: 60
  max_file_bytes: 1024
  secrets: ["hunter2"]
  github_id: "octocat"
  session_id: "sess-1"
  sources:
    - crate_name: trusty-code
      root: "~/Library/Logs/trusty-code"
      include: ["*.log"]
      level: warn
      destination: "s3://owner-bucket/logs?profile=owner"
"#,
    );
    let section = config.log_drain.as_ref().expect("section present");
    assert_eq!(
        section.sources[0].destination.as_deref(),
        Some("s3://owner-bucket/logs?profile=owner")
    );
    assert_eq!(section.enabled, Some(true));
    assert_eq!(
        section.destination.as_deref(),
        Some("s3://logs-bucket/drain")
    );
    assert_eq!(section.secrets, vec!["hunter2".to_string()]);
    assert_eq!(section.sources.len(), 1);

    // Serialising back and re-reading must not lose a field.
    let yaml = serde_yaml::to_string(&config).expect("serialises");
    let round_tripped: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("re-parses");
    assert_eq!(round_tripped.log_drain, config.log_drain);
}

#[test]
fn resolve_disabled_when_section_absent() {
    let config = TrustyToolsConfig::default();
    assert!(matches!(
        resolve_log_drain(&config, home()).expect("absent section is not an error"),
        LogDrainSetting::Disabled
    ));
}

#[test]
fn resolve_disabled_when_enabled_is_false() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: false
  destination: "file:///tmp/drain"
"#,
    );
    assert!(matches!(
        resolve_log_drain(&config, home()).expect("a well-formed disabled section is not an error"),
        LogDrainSetting::Disabled
    ));
}

#[test]
fn resolve_fills_defaults() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
"#,
    );
    let LogDrainSetting::Enabled(plan) = resolve_log_drain(&config, home()).expect("resolves")
    else {
        panic!("expected an enabled plan");
    };
    assert_eq!(plan.interval.as_secs(), DEFAULT_INTERVAL_SECS);
    assert_eq!(plan.max_file_bytes, DEFAULT_MAX_FILE_BYTES);
    assert_eq!(plan.max_wire_bytes, DEFAULT_MAX_WIRE_BYTES);
    assert_eq!(plan.destinations.len(), 1);
    assert_eq!(plan.destinations[0].scheme(), "file");
    assert_eq!(plan.github_id, None);
    assert_eq!(plan.session_id, None);

    // The built-in source is the daemon's own rolling file log.
    let sources = &plan.destinations[0].sources;
    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(source.crate_name, "trusty-mpm");
    assert_eq!(source.root, home().join(".trusty-mpm").join("logs"));
    assert_eq!(source.include, vec!["trusty-mpm.log*".to_string()]);
    assert_eq!(source.level_filter, Some(Level::Info));
}

#[test]
fn resolve_uses_configured_sources() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "s3://bucket/prefix"
  sources:
    - crate_name: trusty-agents
      root: "~/Library/Logs/trusty-agents"
      include: ["daemon-*.log"]
      level: WARN
    - crate_name: trusty-code
      root: "/var/log/trusty-code"
"#,
    );
    let LogDrainSetting::Enabled(plan) = resolve_log_drain(&config, home()).expect("resolves")
    else {
        panic!("expected an enabled plan");
    };
    // Both sources inherit the section destination, so they share one group.
    assert_eq!(plan.destinations.len(), 1);
    assert_eq!(plan.destinations[0].scheme(), "s3");
    let sources = &plan.destinations[0].sources;
    assert_eq!(sources.len(), 2);
    // `~` expands against the supplied home, and the level name is
    // case-insensitive.
    assert_eq!(sources[0].root, home().join("Library/Logs/trusty-agents"));
    assert_eq!(sources[0].level_filter, Some(Level::Warn));
    // An absent `include` collects everything under the root; an absent `level`
    // uploads every line.
    assert_eq!(sources[1].include, vec!["**/*".to_string()]);
    assert_eq!(sources[1].level_filter, None);
    assert_eq!(sources[1].root, Path::new("/var/log/trusty-code"));
}

#[test]
fn resolve_groups_sources_by_destination() {
    // #6657: a source's own `destination` wins; a source without one inherits
    // the section default. Sources sharing a destination share one pass.
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "s3://default-bucket/prefix"
  sources:
    - crate_name: trusty-mpm
      root: "/var/log/trusty-mpm"
    - crate_name: trusty-code
      root: "/var/log/trusty-code"
      destination: "s3://owner-bucket/logs?profile=owner"
    - crate_name: trusty-agents
      root: "/var/log/trusty-agents"
    - crate_name: trusty-search
      root: "/var/log/trusty-search"
      destination: "s3://owner-bucket/logs?profile=owner"
"#,
    );
    let LogDrainSetting::Enabled(plan) = resolve_log_drain(&config, home()).expect("resolves")
    else {
        panic!("expected an enabled plan");
    };

    // Two distinct destinations, in first-appearance order.
    assert_eq!(plan.destinations.len(), 2);
    assert_eq!(
        plan.destinations[0].destination_display,
        "s3://default-bucket/prefix"
    );
    assert_eq!(
        plan.destinations[1].destination_display,
        "s3://owner-bucket/logs?profile=owner"
    );

    // The two inheriting sources landed on the default; the two overriding
    // ones collapsed onto one group rather than opening a pass each.
    let names = |index: usize| {
        plan.destinations[index]
            .sources
            .iter()
            .map(|s| s.crate_name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(0), vec!["trusty-mpm", "trusty-agents"]);
    assert_eq!(names(1), vec!["trusty-code", "trusty-search"]);
}

#[test]
fn resolve_needs_no_section_destination_when_every_source_names_one() {
    // The section default is required only by a source that still needs it.
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  sources:
    - crate_name: trusty-code
      root: "/var/log/trusty-code"
      destination: "file:///tmp/drain-a"
"#,
    );
    let LogDrainSetting::Enabled(plan) = resolve_log_drain(&config, home()).expect("resolves")
    else {
        panic!("expected an enabled plan");
    };
    assert_eq!(plan.destinations.len(), 1);
    assert_eq!(
        plan.destinations[0].destination_display,
        "file:///tmp/drain-a"
    );
}

#[test]
fn resolve_rejects_a_malformed_source_destination() {
    // A per-source override is validated exactly as the section's own is: a
    // typo must not fall back to the default, which is the wrong account.
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  sources:
    - crate_name: trusty-code
      root: "/var/log/trusty-code"
      destination: "s3://bucket/logs?porfile=owner"
"#,
    );
    let err = resolve_log_drain(&config, home())
        .expect_err("a malformed source destination is a hard error");
    match err {
        LogDrainConfigError::SourceDestination { index, ref reason } => {
            assert_eq!(index, 0);
            assert!(reason.contains("porfile"), "unhelpful reason: {reason}");
        }
        other => panic!("expected SourceDestination, got {other:?}"),
    }
}

#[test]
fn resolve_rejects_a_malformed_destination() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "not-a-uri"
"#,
    );
    let err = resolve_log_drain(&config, home()).expect_err("a malformed URI is a hard error");
    assert!(
        matches!(err, LogDrainConfigError::Destination { .. }),
        "expected Destination, got {err:?}"
    );
}

#[test]
fn resolve_rejects_a_reserved_scheme() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "gs://bucket/prefix"
"#,
    );
    // `gs://` is recognised-and-reserved by the core parser; it must surface as
    // a config error rather than an enabled plan pointing nowhere.
    let err = resolve_log_drain(&config, home()).expect_err("a reserved scheme is a hard error");
    assert!(
        matches!(err, LogDrainConfigError::Destination { .. }),
        "expected Destination, got {err:?}"
    );
}

#[test]
fn resolve_validates_even_while_disabled() {
    // #6535: finding the typo before the operator flips `enabled` is the point.
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: false
  destination: "s3:/missing-a-slash"
"#,
    );
    let err = resolve_log_drain(&config, home())
        .expect_err("a malformed destination is an error even while disabled");
    assert!(
        matches!(err, LogDrainConfigError::Destination { .. }),
        "expected Destination, got {err:?}"
    );
}

#[test]
fn resolve_rejects_enabled_with_no_destination() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("enabled with no destination is an error"),
        LogDrainConfigError::MissingDestination
    );
}

#[test]
fn resolve_rejects_a_zero_interval() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  interval_secs: 0
"#,
    );
    // A zero interval would busy-loop the scheduler, so it is refused rather
    // than clamped to the default — silently changing an operator's number is
    // how a "why is it draining every second?" bug survives a config review.
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("zero interval is an error"),
        LogDrainConfigError::NonPositive {
            field: "interval_secs"
        }
    );
}

#[test]
fn resolve_rejects_a_zero_max_file_bytes() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  max_file_bytes: 0
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("zero ceiling is an error"),
        LogDrainConfigError::NonPositive {
            field: "max_file_bytes"
        }
    );
}

#[test]
fn resolve_rejects_a_zero_max_wire_bytes() {
    // #6547: a zero wire cap skips every file with a recorded decision, which
    // reads exactly like a working drain with nothing to send.
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  max_wire_bytes: 0
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("zero wire cap is an error"),
        LogDrainConfigError::NonPositive {
            field: "max_wire_bytes"
        }
    );
}

#[test]
fn resolve_carries_a_configured_max_wire_bytes() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  max_wire_bytes: 4096
"#,
    );
    let LogDrainSetting::Enabled(plan) = resolve_log_drain(&config, home()).expect("resolves")
    else {
        panic!("expected an enabled plan");
    };
    assert_eq!(plan.max_wire_bytes, 4096);
}

#[test]
fn resolve_rejects_a_source_with_no_root() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  sources:
    - crate_name: trusty-code
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("a source with no root is an error"),
        LogDrainConfigError::SourceField {
            index: 0,
            field: "root"
        }
    );
}

#[test]
fn resolve_rejects_a_source_with_no_crate_name() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  sources:
    - root: "/var/log/x"
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("a source with no crate_name is an error"),
        LogDrainConfigError::SourceField {
            index: 0,
            field: "crate_name"
        }
    );
}

#[test]
fn resolve_rejects_an_unknown_level() {
    let config = config_from_yaml(
        r#"
log_drain:
  enabled: true
  destination: "file:///tmp/drain"
  sources:
    - crate_name: trusty-code
      root: "/var/log/x"
      level: verbose
"#,
    );
    assert_eq!(
        resolve_log_drain(&config, home()).expect_err("an unknown level is an error"),
        LogDrainConfigError::SourceLevel {
            index: 0,
            value: "verbose".to_string()
        }
    );
}
