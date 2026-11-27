//! #12026 end-to-end: the `level_lint.py` recordings replay clean against a Rust binary.
//!
//! The recordings in `tests/fixtures/replay/level_lint/` were made from the
//! Architect's `level_lint.py` and replay clean against it (see the fixture
//! README); this test replays the same files against the Rust stand-in
//! `examples/replay_standin_level_lint.rs`, the target P1 (#12028) replaces.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

const NAME: &str = "replay_standin_level_lint";

/// Builds the stand-in example into this test binary's own target dir and
/// profile, once; `CARGO_BIN_EXE_*` covers `[[bin]]` targets only (the
/// pattern of trusty-code's `mcp_loader_e2e.rs`, #7951). A failed build fails.
fn standin() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let exe = std::env::current_exe().expect("the test binary has a path");
        // `<profile>/deps/<bin>`, or `<profile>/build/<crate>/<hash>/out/<bin>` (cargo#16807).
        fn named(d: &Path, n: &str) -> bool {
            d.file_name().is_some_and(|f| f == n)
        }
        let profile_dir = exe
            .ancestors()
            .find(|d| named(d, "deps"))
            .or_else(|| exe.ancestors().find(|d| named(d, "build")))
            .and_then(Path::parent)
            .expect("a cargo profile directory above the test binary")
            .to_path_buf();
        let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") => "dev".to_owned(),
            Some(other) => other.to_owned(),
            None => panic!("no profile directory in {}", exe.display()),
        };
        let target_dir = profile_dir.parent().expect("a profile dir has a parent");
        let out = std::process::Command::new(env!("CARGO"))
            .args(["build", "--example", NAME, "--profile", &profile])
            .arg("--manifest-path")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .arg("--target-dir")
            .arg(target_dir)
            .output()
            .expect("`cargo build --example` runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let path = profile_dir
            .join("examples")
            .join(format!("{NAME}{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "{} not built", path.display());
        path
    })
}

#[test]
fn level_lint_recordings_replay_clean_against_the_rust_stand_in() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/level_lint");
    let target = Target {
        program: standin().to_path_buf(),
        prefix: Vec::new(),
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        assert!(found.is_empty(), "{}: {found:?}", path.display());
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    names.sort();
    assert_eq!(
        names,
        [
            "engineer-quiet.json",
            "pm-strict-file.json",
            "unknown-level-stdin.json"
        ]
    );
    let strict = load(&dir.join("pm-strict-file.json")).unwrap();
    assert_eq!(
        strict.reads,
        Some(vec!["note.md".to_owned()]),
        "the probe saw the file read"
    );
    assert_eq!(strict.exit, 1);
}
