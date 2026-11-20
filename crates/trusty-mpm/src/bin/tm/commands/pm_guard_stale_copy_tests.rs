//! Tests for the stale-copy guard (#10574), fixture F2 variants included.

use std::path::PathBuf;

use serde_json::{Value, json};
use trusty_mpm::core::stale_copy::{ObservedStore, StaleCopyConfig, StaleCopyLists};

use super::{LOCKED, STALE_INPUT_HASH, apply_edits, evaluate_stale_copy, evaluate_with};

/// A project with `docs/scenarios.md` v1, `docs/*.md` registered and
/// `docs/rules.md` locked.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    cfg: StaleCopyConfig,
    store: ObservedStore,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("docs")).expect("mkdir");
        std::fs::write(root.join("docs/scenarios.md"), "S1\nS4 Result: ok\n").expect("v1");
        std::fs::write(root.join("docs/rules.md"), "R1\n").expect("rules");
        let lists = StaleCopyLists {
            paths: vec!["docs/*.md".into()],
            locked: vec!["docs/rules.md".into()],
        };
        let cfg = StaleCopyConfig::new(&root, &lists).expect("cfg");
        let store = ObservedStore::at(root.join(".trusty-mpm/stale-copy"));
        Self {
            _dir: dir,
            root,
            cfg,
            store,
        }
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).display().to_string()
    }

    fn call(&self, tool: &str, input: Value) -> Option<String> {
        evaluate_with(&self.cfg, &self.store, tool, Some(&input), &self.root).map(|d| d.reason)
    }

    fn read(&self, rel: &str) {
        assert!(
            self.call("Read", json!({ "file_path": self.path(rel) }))
                .is_none()
        );
    }

    fn write(&self, rel: &str, content: &str) -> Option<String> {
        self.call(
            "Write",
            json!({ "file_path": self.path(rel), "content": content }),
        )
    }

    fn bash(&self, command: &str) -> Option<String> {
        self.call("Bash", json!({ "command": command }))
    }

    /// The owner's out-of-band edit: the unmarked token lands in S4.
    fn owner_edit(&self) {
        std::fs::write(
            self.root.join("docs/scenarios.md"),
            "S1\nS4 Result: ok UNMARKED-7F3\n",
        )
        .expect("owner edit");
    }
}

fn assert_code(out: Option<String>, code: &str) {
    let reason = out.unwrap_or_else(|| panic!("expected a {code} deny"));
    assert!(reason.starts_with(code), "{reason}");
}

#[test]
fn f2_in_place_write_after_an_owner_edit_is_denied_stale() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    fx.owner_edit();
    assert_code(fx.write("docs/scenarios.md", "S1\nv2\n"), STALE_INPUT_HASH);
}

#[test]
fn f2_bash_redirect_over_an_owner_edit_is_denied_stale() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    fx.owner_edit();
    assert_code(fx.bash("cat new.md > docs/scenarios.md"), STALE_INPUT_HASH);
}

#[test]
fn a_write_to_a_never_read_artifact_is_denied_stale() {
    let fx = Fx::new();
    assert_code(fx.write("docs/scenarios.md", "x"), STALE_INPUT_HASH);
    assert_code(fx.bash("cat new.md > docs/scenarios.md"), STALE_INPUT_HASH);
}

#[test]
fn f2_locked_path_is_denied_whatever_the_hash() {
    let fx = Fx::new();
    fx.read("docs/rules.md");
    assert_code(fx.write("docs/rules.md", "R1\n"), LOCKED);
    assert_code(fx.bash("echo x >> docs/rules.md"), LOCKED);
}

#[test]
fn f2_late_edit_after_the_last_read_is_detected_by_rehash() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    assert!(fx.write("docs/scenarios.md", "S1\nv2\n").is_none());
    // The owner appends a line after the PM's write, before publish.
    std::fs::write(fx.root.join("docs/scenarios.md"), "S1\nv2\nlate line\n").expect("late");
    assert_code(fx.write("docs/scenarios.md", "S1\nv3\n"), STALE_INPUT_HASH);
}

#[test]
fn a_fresh_read_allows_the_write_and_records_its_content() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    assert!(fx.write("docs/scenarios.md", "v2\n").is_none());
    std::fs::write(fx.root.join("docs/scenarios.md"), "v2\n").expect("tool write");
    // The recorded version is the written one, so a second write passes.
    assert!(fx.write("docs/scenarios.md", "v3\n").is_none());
}

#[test]
fn a_fresh_edit_records_the_edited_text() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    let edit = json!({
        "file_path": fx.path("docs/scenarios.md"),
        "old_string": "ok",
        "new_string": "fine",
    });
    assert!(fx.call("Edit", edit).is_none());
    std::fs::write(fx.root.join("docs/scenarios.md"), "S1\nS4 Result: fine\n").expect("edit");
    assert!(fx.write("docs/scenarios.md", "v3\n").is_none());
}

#[test]
fn an_allowed_bash_write_drops_the_record() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    assert!(fx.bash("cat new.md > docs/scenarios.md").is_none());
    assert_code(fx.write("docs/scenarios.md", "v3"), STALE_INPUT_HASH);
}

#[test]
fn creating_a_registered_file_is_allowed() {
    let fx = Fx::new();
    assert!(fx.write("docs/new.md", "fresh").is_none());
}

#[test]
fn unregistered_paths_are_never_judged() {
    let fx = Fx::new();
    assert!(fx.write("notes.md", "x").is_none());
    assert!(fx.bash("echo x > notes.md").is_none());
}

#[test]
fn an_unreadable_store_denies_writes_to_registered_paths() {
    let fx = Fx::new();
    fx.read("docs/scenarios.md");
    let store_dir = fx.root.join(".trusty-mpm/stale-copy");
    for entry in std::fs::read_dir(&store_dir).expect("ls").flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            std::fs::write(entry.path(), "garbage").expect("corrupt");
        }
    }
    let reason = fx.write("docs/scenarios.md", "v2").expect("deny");
    assert!(reason.starts_with(STALE_INPUT_HASH), "{reason}");
    assert!(reason.contains("cannot be judged"), "{reason}");
}

#[test]
fn a_rejected_config_allows_and_stays_inactive() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[stale_copy]\npaths = 3\n",
    )
    .expect("cfg");
    std::fs::write(dir.path().join("x.md"), "x").expect("file");
    let input =
        json!({ "file_path": dir.path().join("x.md").display().to_string(), "content": "y" });
    assert!(evaluate_stale_copy("Write", Some(&input), dir.path()).is_none());
}

#[test]
fn an_absent_or_empty_config_is_inert() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input =
        json!({ "file_path": dir.path().join("x.md").display().to_string(), "content": "y" });
    std::fs::write(dir.path().join("x.md"), "x").expect("file");
    assert!(evaluate_stale_copy("Write", Some(&input), dir.path()).is_none());
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[stale_copy]\npaths = []\n",
    )
    .expect("cfg");
    assert!(evaluate_stale_copy("Write", Some(&input), dir.path()).is_none());
}

#[test]
fn apply_edits_follows_replace_all_and_misses() {
    let edit = json!({ "old_string": "a", "new_string": "b" });
    assert_eq!(apply_edits("a a", "Edit", &edit).as_deref(), Some("b a"));
    let all = json!({ "old_string": "a", "new_string": "b", "replace_all": true });
    assert_eq!(apply_edits("a a", "Edit", &all).as_deref(), Some("b b"));
    assert_eq!(apply_edits("c", "Edit", &edit), None);
    let multi = json!({ "edits": [edit, { "old_string": "b", "new_string": "c" }] });
    assert_eq!(apply_edits("a", "MultiEdit", &multi).as_deref(), Some("c"));
}

impl Fx {
    /// A symlink at `link` (relative to the root) pointing at `target`.
    fn symlink(&self, target: &str, link: &str) {
        let link = self.root.join(link);
        std::fs::create_dir_all(link.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }
}

// #10574: APFS is case-insensitive, so a case variant names the locked file.
#[cfg(target_os = "macos")]
#[test]
fn a_locked_path_is_denied_through_a_case_variant() {
    let fx = Fx::new();
    assert_code(fx.write("Docs/RULES.md", "R1\n"), LOCKED);
    assert_code(fx.bash("echo x >> Docs/Rules.md"), LOCKED);
}

#[test]
fn a_locked_path_is_denied_through_a_symlink() {
    let fx = Fx::new();
    fx.symlink("../docs/rules.md", "other/l.md");
    assert_code(fx.write("other/l.md", "R1\n"), LOCKED);
    assert_code(fx.bash("echo x >> other/l.md"), LOCKED);
}

#[test]
fn a_stale_write_is_denied_through_a_dotdot_after_a_symlink() {
    let fx = Fx::new();
    fx.symlink("../docs", "other/d");
    fx.read("docs/scenarios.md");
    fx.owner_edit();
    // Lexically `other/docs/scenarios.md`; the kernel opens `docs/scenarios.md`.
    assert_code(
        fx.write("other/d/../docs/scenarios.md", "v2\n"),
        STALE_INPUT_HASH,
    );
}

#[test]
fn a_read_through_one_spelling_keeps_a_write_through_another_fresh() {
    let fx = Fx::new();
    fx.symlink("../docs/scenarios.md", "other/s.md");
    fx.read("other/s.md");
    assert!(fx.write("docs/scenarios.md", "v2\n").is_none());
}
