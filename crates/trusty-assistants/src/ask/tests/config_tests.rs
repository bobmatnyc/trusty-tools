//! The `[ask]` table and its defaults (#11650).
//!
//! Test: this module IS the test surface.

use std::path::Path;

use crate::agents::FALLBACK_MODEL;
use crate::ask::{AskError, AskSettings, DEFAULT_MAX_TURNS, DEFAULT_TIMEOUT_SECS};
use crate::assistants::AssistantHomeConfig;

fn settings(toml_text: &str) -> AskSettings {
    toml::from_str::<AssistantHomeConfig>(toml_text)
        .expect("parse")
        .ask
}

#[test]
fn an_absent_table_resolves_to_the_defaults() {
    let limits = settings("id = \"izzie\"\n")
        .resolve(Path::new("config.toml"))
        .unwrap();
    assert_eq!(limits.model, FALLBACK_MODEL);
    assert_eq!(limits.max_turns, DEFAULT_MAX_TURNS);
    assert_eq!(limits.timeout_secs, DEFAULT_TIMEOUT_SECS);
}

#[test]
fn an_empty_model_resolves_to_the_default() {
    for raw in ["\"\"", "\"   \""] {
        let limits = settings(&format!("[ask]\nmodel = {raw}\n"))
            .resolve(Path::new("config.toml"))
            .unwrap();
        assert_eq!(limits.model, FALLBACK_MODEL, "{raw}");
        assert!(!limits.model.is_empty());
    }
}

#[test]
fn set_values_are_used() {
    let limits = settings("[ask]\nmodel = \"openai/gpt-4o\"\nmax_turns = 3\ntimeout_secs = 9\n")
        .resolve(Path::new("config.toml"))
        .unwrap();
    assert_eq!(limits.model, "openai/gpt-4o");
    assert_eq!(limits.max_turns, 3);
    assert_eq!(limits.timeout_secs, 9);
}

#[test]
fn zero_limits_are_typed_errors() {
    for (body, key) in [
        ("[ask]\nmax_turns = 0\n", "max_turns"),
        ("[ask]\ntimeout_secs = 0\n", "timeout_secs"),
    ] {
        match settings(body).resolve(Path::new("config.toml")) {
            Err(AskError::InvalidSetting { key: k, .. }) => assert_eq!(k, key),
            other => panic!("{body}: {other:?}"),
        }
    }
}
