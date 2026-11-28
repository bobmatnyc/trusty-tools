//! #12077 row 11: the legacy global `trusty-memory` hook strip.
//!
//! What: the strip never writes `<home>/.claude/settings.json`; in the
//! tm-owned config dir it removes only a group equal to one tm wrote, and keeps
//! a group that differs in any field. A read failure is an error, not a pass.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;

use super::settings::{clean_global_trusty_memory_hooks, remove_global_trusty_memory_hooks};

/// tm's own old canonical `UserPromptSubmit` group, as it wrote it.
fn tm_prompt_context_group() -> Value {
    json!({"matcher": "", "hooks": [
        {"type": "command", "command": "trusty-memory prompt-context", "timeout": 60}
    ]})
}

/// tm's own pre-#1270 `Stop` group, as it wrote it.
fn tm_legacy_stop_group() -> Value {
    json!({"matcher": "", "hooks": [
        {"type": "command", "command": "trusty-memory hooks fire claude.stop", "timeout": 60}
    ]})
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, serde_json::to_string_pretty(value).expect("json")).expect("write");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

/// #12077: with no config dir named, the strip leaves the generic
/// `~/.claude/settings.json` byte-identical, even when it holds tm's exact old
/// group.
#[test]
fn the_strip_never_writes_the_home_settings_file_12077() {
    let home = tempdir().expect("home");
    let settings = home.path().join(".claude").join("settings.json");
    write_json(
        &settings,
        &json!({"hooks": {"UserPromptSubmit": [tm_prompt_context_group()]}}),
    );
    let before = std::fs::read(&settings).expect("read");

    remove_global_trusty_memory_hooks(Some(home.path()), None).expect("no error");

    assert_eq!(std::fs::read(&settings).expect("read"), before);
}

/// #12077: in the tm-owned config dir, tm's exact old groups are removed.
#[test]
fn tm_exact_old_groups_are_removed_from_the_config_dir_12077() {
    let home = tempdir().expect("home");
    let config = tempdir().expect("config");
    let settings = config.path().join("settings.json");
    write_json(
        &settings,
        &json!({"theme": "dark", "hooks": {
            "UserPromptSubmit": [tm_prompt_context_group()],
            "Stop": [tm_legacy_stop_group()],
        }}),
    );

    remove_global_trusty_memory_hooks(Some(home.path()), Some(config.path())).expect("strip");

    let after = read_json(&settings);
    assert_eq!(after["theme"], "dark");
    assert_eq!(after["hooks"], json!({}), "{after}");
}

/// #12077: a group that differs from tm's old group in any field is the
/// operator's and is kept.
#[test]
fn a_user_hook_that_differs_from_tm_old_hook_is_kept_12077() {
    let tmp = tempdir().expect("dir");
    let settings = tmp.path().join("settings.json");
    let mut timeout = tm_prompt_context_group();
    timeout["hooks"][0]["timeout"] = json!(30);
    let mut matcher = tm_prompt_context_group();
    matcher["matcher"] = json!("*");
    let mut extra = tm_legacy_stop_group();
    extra["hooks"]
        .as_array_mut()
        .expect("hooks")
        .push(json!({"type": "command", "command": "bash mine.sh"}));
    let doc = json!({"hooks": {
        "UserPromptSubmit": [timeout, matcher],
        "Stop": [extra],
    }});
    write_json(&settings, &doc);

    clean_global_trusty_memory_hooks(&settings).expect("clean");

    assert_eq!(read_json(&settings), doc, "every differing group must stay");
}

/// #12077 fail-open: a settings path that exists and cannot be read is an
/// error, never a silent pass.
#[test]
fn an_unreadable_settings_file_is_an_error_12077() {
    let tmp = tempdir().expect("dir");
    let settings = tmp.path().join("settings.json");
    std::fs::create_dir_all(&settings).expect("a directory where the file belongs");

    assert!(clean_global_trusty_memory_hooks(&settings).is_err());
}
