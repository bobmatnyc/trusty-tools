//! Seed `outputStyle`/`statusLine` defaults into the tm-owned `CLAUDE_CONFIG_DIR`
//! `settings.json` (defense-in-depth, issue #2214).
//!
//! Why: managed sessions currently rely on the PROJECT-tier `settings.json`
//! (written by [`crate::core::session_launch::settings::write_output_style`] /
//! `write_status_line`) plus `claude --setting-sources project,local` to show
//! the `trusty-mpm` output style and live statusline. That is a single point of
//! failure: if the project-tier write is ever skipped, races with a resumed
//! workspace, or the `--setting-sources` flag drifts, the tm-owned config dir
//! itself carries neither key and the session silently falls back to Claude
//! Code's own defaults. Seeding the SAME two keys directly into the tm-owned
//! `CLAUDE_CONFIG_DIR/settings.json` makes that config dir self-sufficient,
//! independent of the project tier.
//! What: [`ensure_settings_defaults`] reads the tm-owned
//! `<claude_config_dir>/settings.json` (tolerating absent/malformed content by
//! starting from `{}`), sets `outputStyle` and `statusLine` ONLY when each key
//! is not already present — never overwriting a value the client (or a prior
//! `tm login`) already persisted there — and writes back atomically only when
//! something actually changed. The values reused are the exact same ones the
//! project-tier writer computes: [`crate::core::session_launch::OUTPUT_STYLE`]
//! (the default output-style id) and
//! [`crate::core::session_launch::resolve_statusline_command`] (the resolved
//! absolute `<tm-binary> statusline` command).
//! Test: `ensure_settings_defaults_seeds_fresh_file`,
//! `ensure_settings_defaults_preserves_existing_keys`,
//! `ensure_settings_defaults_does_not_overwrite_customized_values`,
//! `ensure_settings_defaults_is_idempotent`.

use std::path::Path;

use trusty_common::claude_config::write_json_atomic;

/// Seed `outputStyle` and `statusLine` into `<claude_config_dir>/settings.json`
/// when either key is absent, preserving every other key untouched.
///
/// Why: see the module-level doc comment — this is the defense-in-depth seed
/// called from [`super::global_config::ensure_global_config_dir`] on every
/// managed-driver bootstrap, so the tm-owned config dir never depends solely on
/// the project tier for these two keys.
/// What: reads the on-disk `settings.json` (or starts from `{}` if
/// absent/malformed/non-object), inserts `outputStyle` =
/// [`crate::core::session_launch::OUTPUT_STYLE`] only if the key is not already
/// present, inserts `statusLine` = `{ "type": "command", "command":
/// <resolved absolute path> statusline", "padding": 0 }` (via
/// [`crate::core::session_launch::resolve_statusline_command`]) only if the key
/// is not already present, then writes back via
/// [`trusty_common::claude_config::write_json_atomic`] only when the merged
/// value actually differs from what was on disk (idempotent — no spurious
/// rewrite / mtime bump on repeat calls).
/// Test: `ensure_settings_defaults_seeds_fresh_file`,
/// `ensure_settings_defaults_preserves_existing_keys`,
/// `ensure_settings_defaults_does_not_overwrite_customized_values`,
/// `ensure_settings_defaults_is_idempotent`.
pub(crate) fn ensure_settings_defaults(claude_config_dir: &Path) -> anyhow::Result<()> {
    let settings_path = claude_config_dir.join("settings.json");

    let existing_value: Option<serde_json::Value> = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object);

    let mut settings = existing_value
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = settings
        .as_object_mut()
        .expect("settings was constructed/filtered to be an object");

    obj.entry("outputStyle").or_insert_with(|| {
        serde_json::Value::String(crate::core::session_launch::OUTPUT_STYLE.to_string())
    });
    obj.entry("statusLine").or_insert_with(|| {
        serde_json::json!({
            "type": "command",
            "command": crate::core::session_launch::resolve_statusline_command(),
            "padding": 0
        })
    });

    // Structural comparison (not byte-wise) so formatting differences left by
    // editors or prior writes never trigger a spurious rewrite, matching the
    // idempotency pattern used by `global_config::ensure_mcp_config`.
    let needs_write = existing_value.as_ref() != Some(&settings);
    if needs_write {
        write_json_atomic(&settings_path, &settings)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_settings_defaults_seeds_fresh_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        ensure_settings_defaults(&cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["outputStyle"].as_str(),
            Some(crate::core::session_launch::OUTPUT_STYLE),
            "outputStyle must be seeded with the tm default id"
        );
        let status_line = &val["statusLine"];
        assert_eq!(status_line["type"].as_str(), Some("command"));
        assert_eq!(status_line["padding"].as_i64(), Some(0));
        assert!(
            status_line["command"]
                .as_str()
                .is_some_and(|c| c.ends_with(" statusline")),
            "statusLine.command must resolve to '<abs path> statusline', got {:?}",
            status_line["command"]
        );
    }

    #[test]
    fn ensure_settings_defaults_preserves_existing_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        std::fs::write(
            cfg.join("settings.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "theme": "dark",
                "model": "opusplan",
            }))
            .unwrap(),
        )
        .unwrap();

        ensure_settings_defaults(&cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["theme"].as_str(),
            Some("dark"),
            "unrelated key preserved"
        );
        assert_eq!(
            val["model"].as_str(),
            Some("opusplan"),
            "unrelated key preserved"
        );
        assert_eq!(
            val["outputStyle"].as_str(),
            Some(crate::core::session_launch::OUTPUT_STYLE)
        );
        assert!(val["statusLine"].is_object());
    }

    #[test]
    fn ensure_settings_defaults_does_not_overwrite_customized_values() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        std::fs::write(
            cfg.join("settings.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "outputStyle": "teaching",
                "statusLine": { "type": "command", "command": "my-custom-line", "padding": 2 },
            }))
            .unwrap(),
        )
        .unwrap();

        ensure_settings_defaults(&cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["outputStyle"].as_str(),
            Some("teaching"),
            "a pre-existing outputStyle must never be clobbered"
        );
        assert_eq!(
            val["statusLine"]["command"].as_str(),
            Some("my-custom-line"),
            "a pre-existing statusLine must never be clobbered"
        );
        assert_eq!(val["statusLine"]["padding"].as_i64(), Some(2));
    }

    #[test]
    fn ensure_settings_defaults_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        ensure_settings_defaults(&cfg).unwrap();
        let bytes_first = std::fs::read(cfg.join("settings.json")).unwrap();

        ensure_settings_defaults(&cfg).unwrap();
        let bytes_second = std::fs::read(cfg.join("settings.json")).unwrap();

        assert_eq!(
            bytes_first, bytes_second,
            "a second call must not rewrite the file when nothing changed"
        );
    }
}
