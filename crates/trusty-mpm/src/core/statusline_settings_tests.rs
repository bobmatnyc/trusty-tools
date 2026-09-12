//! Tests for [`super::ensure_statusline_entry_in`] (#7617).
//!
//! Why: this is the one writer every settings tier goes through, so its
//! seed / repair / keep / refuse quadrants are what guarantee the `💸` segment
//! is wired on a fresh install and stays wired after an upgrade.
//! Test: this file IS the test module.

use super::*;

/// A settings file holding `value` under `statusLine`, in a fresh tempdir.
fn settings_with(value: serde_json::Value) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "statusLine": value })).unwrap(),
    )
    .expect("seed settings");
    (dir, path)
}

/// Read `statusLine.command` back off disk.
fn command_in(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["statusLine"]["command"].as_str().map(str::to_owned)
}

/// Why (#7617 closure 1): a fresh install must come away with the entry, which
/// is the whole "core setup, guaranteed by the framework" ruling.
/// Test: itself.
#[test]
fn a_fresh_file_is_seeded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("settings.json");

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Seeded);
    assert!(outcome.wrote());
    assert!(
        command_in(&path).is_some_and(|cmd| cmd.ends_with(" statusline")),
        "the seeded command must invoke the statusline subcommand"
    );
}

/// Why (#2229, #7262): the disappearance class this rule exists for is a command
/// pointing at a binary that no longer exists — a Cargo build tree that was
/// cleaned, or an install that moved.
/// Test: itself.
#[test]
fn a_stale_entry_is_repaired() {
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": "/definitely/not/here/tm statusline",
        "padding": 0
    }));

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Repaired);
    assert_ne!(
        command_in(&path).as_deref(),
        Some("/definitely/not/here/tm statusline"),
        "a command whose binary is gone must be repointed"
    );
}

/// Why: the rule must never cost an operator their own status bar. A command
/// pointing at a binary that EXISTS is theirs, whatever it runs.
/// Test: itself.
#[test]
fn a_customized_entry_is_kept() {
    let mine = format!("{} --custom", std::env::current_exe().unwrap().display());
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": mine.clone(),
        "padding": 0
    }));

    let outcome = ensure_statusline_entry_in(&path);

    assert_eq!(outcome, StatuslineWrite::Unchanged);
    assert!(!outcome.wrote());
    assert_eq!(command_in(&path).as_deref(), Some(mine.as_str()));
}

/// Why (Fail-Open Check): a settings file that cannot be parsed is not an empty
/// one. Starting from `{}` would silently delete every other key the operator
/// has — hooks, permissions, MCP servers — to fix a status bar.
/// Test: itself.
#[test]
fn an_unparseable_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ this is not json").expect("seed");

    let outcome = ensure_statusline_entry_in(&path);

    assert!(
        matches!(outcome, StatuslineWrite::Refused(_)),
        "got {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ this is not json",
        "a refused file must be left byte-for-byte alone"
    );
}

/// Why: this runs on every launch and every resume. A second call must not
/// rewrite the file, or every session bumps the mtime of the operator's
/// settings for nothing.
///
/// #7689: the enum alone let the real contract drift — assert the BYTES and the
/// mtime too, plus the absence of the `<path>.bak` that `write_json_atomic`
/// takes before every publish. Those three are what "wrote nothing" actually
/// means; `StatuslineWrite::Unchanged` is only the report of it.
/// Test: itself.
#[test]
fn seeding_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Seeded);
    let seeded_bytes = std::fs::read(&path).expect("read after seed");
    let seeded_mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .expect("mtime after seed");

    assert_eq!(
        ensure_statusline_entry_in(&path),
        StatuslineWrite::Unchanged
    );
    assert_eq!(
        std::fs::read(&path).expect("read after second call"),
        seeded_bytes,
        "a second call must leave the file byte-for-byte as the seed wrote it"
    );
    assert_eq!(
        std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime after second call"),
        seeded_mtime,
        "a second call must not bump the mtime of the operator's settings"
    );
    assert!(
        !backup_of(&path).is_file(),
        "a second call must not publish, so it must take no backup either"
    );
}

/// Why (#7689): `resolve_statusline_command` degrades to the bare
/// literal `tm statusline` when `current_exe()` is an ephemeral build path and
/// no `tm`/`trusty-mpm` is installed — and that literal is one of
/// `KNOWN_BARE_STATUSLINE_COMMANDS`, so `is_stale_statusline_command` claims the
/// seeder's own output and the next call reports `Repaired` forever. That holds
/// on a CI runner and not on a developer machine, which is why
/// `seeding_is_idempotent` was green locally and red in CI for every run of
/// shard 5. Injecting the resolver pins the degraded case on any host.
/// FAILS BEFORE THIS CHANGE: the second call returned `Repaired`.
/// Test: itself.
#[test]
fn an_unresolvable_seed_is_not_repaired_on_the_next_call() {
    let degraded = || "tm statusline".to_string();
    let mut obj = serde_json::Map::new();

    assert_eq!(
        apply_statusline_entry_with(&mut obj, degraded),
        StatuslineWrite::Seeded
    );
    let seeded = obj.clone();

    assert_eq!(
        apply_statusline_entry_with(&mut obj, degraded),
        StatuslineWrite::Unchanged,
        "the resolution has not changed, so there is nothing to repoint to"
    );
    assert_eq!(obj, seeded, "nothing may be mutated on the second pass");
}

/// Why (#7689): the equality check must not cost the `--fix` repair its job. An
/// entry that names a DIFFERENT binary from the one resolution offers is still
/// stale, and still gets repointed.
/// Test: itself.
#[test]
fn a_divergent_entry_is_still_repaired_against_the_resolution() {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "statusLine".to_string(),
        serde_json::json!({
            "type": "command",
            "command": "/definitely/not/here/tm statusline",
            "padding": 0
        }),
    );

    assert_eq!(
        apply_statusline_entry_with(&mut obj, || "/opt/tm statusline".to_string()),
        StatuslineWrite::Repaired
    );
    assert_eq!(obj["statusLine"]["command"], "/opt/tm statusline");
}

/// Why (critic CRITICAL): this writes `~/.claude/settings.json`, the file class
/// `write_json_atomic` exists for — a half-written one bricks Claude Code, and
/// `ensure_status_line` runs on every launch AND every resume, so two sessions
/// race. A bare `std::fs::write` leaves no recovery artifact when the publish
/// goes wrong; the backup is the observable half of routing through the atomic
/// writer.
/// FAILS BEFORE THIS CHANGE: `f8a8a2fb8` wrote with `std::fs::write`, so
/// `<path>.bak` never existed.
/// Test: itself.
#[test]
fn a_repair_backs_up_the_prior_bytes() {
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": "/definitely/not/here/tm statusline",
        "padding": 0
    }));

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Repaired);

    let backup = backup_of(&path);
    assert!(
        backup.is_file(),
        "the prior bytes must be recoverable from {}",
        backup.display()
    );
    assert!(
        std::fs::read_to_string(&backup)
            .unwrap()
            .contains("/definitely/not/here/tm statusline"),
        "the backup must carry what was replaced"
    );
}

/// Why (critic MEDIUM 3): a repair is a REPOINT. An operator who set
/// `padding: 2` — or any key Claude Code adds to this entry that this code has
/// never heard of — must not lose it because the binary the command named moved.
/// The pre-#7617 project-tier writer patched only `command` for this reason.
/// FAILS BEFORE THIS CHANGE: `f8a8a2fb8` replaced the whole `statusLine` object,
/// resetting `padding` to 0 and dropping unknown keys entirely.
/// Test: itself.
#[test]
fn a_repair_preserves_operator_fields() {
    let (_dir, path) = settings_with(serde_json::json!({
        "type": "command",
        "command": "/definitely/not/here/tm statusline",
        "padding": 2,
        "someFutureClaudeCodeKey": "keep me"
    }));

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Repaired);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        value["statusLine"]["padding"], 2,
        "an operator's padding must survive a repoint"
    );
    assert_eq!(
        value["statusLine"]["someFutureClaudeCodeKey"], "keep me",
        "a key this code does not know must survive a repoint"
    );
    assert!(
        value["statusLine"]["command"].as_str().is_some_and(
            |cmd| cmd.ends_with(" statusline") && cmd != "/definitely/not/here/tm statusline"
        ),
        "the command itself must still be repointed: {}",
        value["statusLine"]["command"]
    );
}

/// Why (critic re-review): the repair's wholesale-insert branch was documented
/// as "reachable only for a non-object `statusLine`", and it is not reachable at
/// all — `is_stale_statusline_command` reads `entry.get("type")`, and
/// `serde_json::Value::get` answers `None` for a non-object, so such a value is
/// never claimed as stale. This pins where a non-object actually lands: the
/// `Unchanged` arm, with the value left exactly as the operator wrote it. Were
/// the predicate ever widened, this test is what would notice the destination
/// changing.
/// Test: itself.
#[test]
fn a_non_object_statusline_is_left_alone() {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "statusLine".to_string(),
        serde_json::Value::String("garbage-string".to_string()),
    );

    assert_eq!(
        apply_statusline_entry(&mut obj),
        StatuslineWrite::Unchanged,
        "a non-object statusLine is never claimed as stale, so nothing is written"
    );
    assert_eq!(
        obj["statusLine"],
        serde_json::Value::String("garbage-string".to_string()),
        "the value must be left exactly as it was found"
    );
}

/// Why: the seed writes the whole file back, so every other key has to survive
/// the round trip — this is the assertion that a repair is not a reset.
/// Test: itself.
#[test]
fn other_keys_survive_the_seed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "outputStyle": "trusty-mpm",
            "permissions": { "allow": ["Bash(ls:*)"] }
        }))
        .unwrap(),
    )
    .expect("seed");

    assert_eq!(ensure_statusline_entry_in(&path), StatuslineWrite::Seeded);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["outputStyle"], "trusty-mpm");
    assert_eq!(value["permissions"]["allow"][0], "Bash(ls:*)");
}
