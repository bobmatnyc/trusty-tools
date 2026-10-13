//! Tests for owner-layer resolution: every fallback arm.

use super::*;
use std::collections::BTreeSet;
use tempfile::TempDir;

struct Fx {
    _tmp: TempDir,
    repos: PathBuf,
    state: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repos = base.join("repos");
        let project = repos.join("acme").join("widget");
        std::fs::create_dir_all(&project).unwrap();
        Self {
            state: base.join("state"),
            _tmp: tmp,
            repos,
            project,
        }
    }
    fn inputs(&self) -> OwnerInputs<'_> {
        OwnerInputs {
            cwd: &self.project,
            repos_root: &self.repos,
            state_root: &self.state,
        }
    }
    fn file(&self, body: &str) {
        std::fs::write(owner_file_path(&self.repos, "acme"), body).unwrap();
    }
    fn delete(&self) {
        std::fs::remove_file(owner_file_path(&self.repos, "acme")).unwrap();
    }
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_good_file_is_recorded_as_last_good() {
    let fx = Fx::new();
    fx.file("[mcp]\ndeny = [\"heygen\"]\n");
    let resolution = resolve_and_record(&fx.inputs());
    assert!(matches!(resolution.outcome, Outcome::Loaded(_)));
    let record = read_last_good(&fx.state, "acme").unwrap().unwrap();
    assert_eq!(record.policy.deny, set(&["heygen"]));
}

#[test]
fn resolve_alone_writes_nothing() {
    let fx = Fx::new();
    fx.file("[mcp]\ndeny = [\"heygen\"]\n");
    let _ = resolve(&fx.inputs());
    assert!(!fx.state.exists(), "the doctor read must not write");
}

#[test]
fn a_bad_file_with_a_last_good_uses_it_with_the_floor_reapplied() {
    let fx = Fx::new();
    fx.file("[mcp]\ndeny = [\"heygen\"]\n");
    resolve_and_record(&fx.inputs());
    // A record whose policy names a floor server, as an older floor allowed.
    let path = last_good::last_good_path(&fx.state, "acme");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replace("\"heygen\"", "\"heygen\", \"trusty-search\""),
    )
    .unwrap();
    fx.file("[mcp]\ndeny = [\"trusty-search\"]\n");

    let resolution = resolve(&fx.inputs());

    assert!(matches!(resolution.outcome, Outcome::Fallback { .. }));
    assert_eq!(resolution.effective().unwrap().deny, set(&["heygen"]));
}

#[test]
fn a_bad_file_without_a_last_good_skips_the_layer() {
    let fx = Fx::new();
    fx.file("[mcp\n");
    let resolution = resolve_and_record(&fx.inputs());
    assert!(matches!(
        resolution.outcome,
        Outcome::Skipped {
            last_good_error: None,
            ..
        }
    ));
    assert!(resolution.effective().is_none());
}

#[test]
fn a_bad_file_with_a_corrupt_last_good_skips_the_layer_and_says_why() {
    let fx = Fx::new();
    let path = last_good::last_good_path(&fx.state, "acme");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{").unwrap();
    fx.file("[mcp\n");
    let resolution = resolve(&fx.inputs());
    assert!(matches!(
        resolution.outcome,
        Outcome::Skipped {
            last_good_error: Some(_),
            ..
        }
    ));
}

#[test]
fn a_bad_file_never_overwrites_the_last_good_record() {
    let fx = Fx::new();
    fx.file("[mcp]\ndeny = [\"heygen\"]\n");
    resolve_and_record(&fx.inputs());
    let path = last_good::last_good_path(&fx.state, "acme");
    let before = std::fs::read(&path).unwrap();
    for bad in [
        "[tools]\n",
        "[mcp]\ndeny = [\"*\"]\n",
        "extends = \"ghost\"\n",
    ] {
        fx.file(bad);
        let resolution = resolve_and_record(&fx.inputs());
        assert!(
            matches!(resolution.outcome, Outcome::Fallback { .. }),
            "{bad:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{bad:?}");
    }
}

/// A deleted file turns the layer off; the retained record is never used.
#[test]
fn a_deleted_file_turns_the_layer_off_even_with_a_last_good() {
    let fx = Fx::new();
    fx.file("[mcp]\ndeny = [\"heygen\"]\n");
    resolve_and_record(&fx.inputs());
    fx.delete();
    let resolution = resolve_and_record(&fx.inputs());
    assert!(matches!(
        resolution.outcome,
        Outcome::NoFile { retained: Some(_) }
    ));
    assert!(resolution.effective().is_none());
}

#[test]
fn a_cwd_with_no_owner_resolves_to_no_owner() {
    let fx = Fx::new();
    let inputs = OwnerInputs {
        cwd: &fx.repos,
        ..fx.inputs()
    };
    let resolution = resolve(&inputs);
    assert!(matches!(resolution.outcome, Outcome::NoOwner));
    assert!(resolution.owner.is_none());
}
