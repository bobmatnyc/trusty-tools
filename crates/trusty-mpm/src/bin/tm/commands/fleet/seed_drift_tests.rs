//! Tests for the #11773 drift classifier, its report and the `up --strict` gate.

use std::collections::BTreeMap;
use std::path::Path;

use trusty_mpm::core::content_source::Fetch;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::super::seed::{self, refresh};
use super::{Report, State, classify, classify_with, provenance_key};
use crate::commands::architect_drift::{json_value, render, strict_check_in, strict_refusal};

/// The checkout's seed, read once.
fn seed() -> &'static ArchitectSeed {
    static SEED: std::sync::OnceLock<ArchitectSeed> = std::sync::OnceLock::new();
    SEED.get_or_init(|| {
        let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
        ArchitectSeed::resolve_in(&root, None, None, Fetch::Never).expect("the checkout seed")
    })
}

/// A directory holding every managed file at its current shipped bytes.
fn seeded_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("scratch dir");
    for file in seed::files(seed()) {
        write(tmp.path(), &file.dest, file.contents.as_bytes());
    }
    for agent in &seed().agents {
        let dest = format!(".claude/agents/{}.md", agent.stem);
        write(tmp.path(), &dest, agent.contents.as_bytes());
    }
    tmp
}

fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Every path and its state.
fn states(report: &Report) -> BTreeMap<String, State> {
    report
        .files
        .iter()
        .map(|e| (e.path.clone(), e.state))
        .collect()
}

/// Every regular file below `dir` with its bytes, for a no-write check.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                go(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    go(dir, dir, &mut out);
    out
}

/// A directory init just wrote reads current throughout and exits 0.
#[test]
fn an_untouched_init_reads_current() {
    let tmp = seeded_dir();
    let report = classify(tmp.path(), seed()).unwrap();
    let not_current: Vec<_> = report
        .files
        .iter()
        .filter(|e| e.state != State::Current)
        .collect();
    assert!(not_current.is_empty(), "{not_current:?}");
    assert!(report.files.len() > 100, "{} files", report.files.len());
    assert!(report.files.iter().any(|e| e.path == "CLAUDE.md"));
    assert!(
        report
            .files
            .iter()
            .any(|e| e.path == ".claude/agents/runner.md")
    );
    assert_eq!(report.exit(), 0);
}

/// One case per state: edited, absent, instance, owner, extra.
#[test]
fn each_state_is_classified() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    write(dir, "scripts/datg.py", b"print('my edit')\n");
    write(dir, "schemas/dashboard/items.schema.json", b"{}\n");
    std::fs::remove_file(dir.join("scripts/fleet-poll.py")).unwrap();
    write(dir, "local/state.md", b"# my state\n");
    write(dir, "AGENTS.md", b"# owner rules\n");
    write(dir, "scripts/mine.py", b"print('mine')\n");
    let report = classify(dir, seed()).unwrap();
    let s = states(&report);
    assert_eq!(s["scripts/datg.py"], State::Edited);
    assert_eq!(s["schemas/dashboard/items.schema.json"], State::Edited);
    assert_eq!(s["scripts/fleet-poll.py"], State::Absent);
    assert_eq!(s["local/state.md"], State::Instance);
    assert_eq!(s["AGENTS.md"], State::Owner);
    assert_eq!(s["scripts/mine.py"], State::Extra);
    assert_eq!(s["CLAUDE.md"], State::Current);
    assert_eq!(report.count(State::Edited), 2);
    assert_eq!(report.exit(), 1);
    let absent = report
        .files
        .iter()
        .find(|e| e.path == "scripts/fleet-poll.py")
        .unwrap();
    assert!(absent.disk.is_none() && absent.embedded.is_some());
}

/// Bytes the history lists for that path read stale and exit 0; the same
/// bytes at another path read edited.
#[test]
fn a_stale_history_copy_reads_stale() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    let old = b"# an earlier shipped CLAUDE.md\n";
    write(dir, "CLAUDE.md", old);
    write(dir, ".claude/skills/tm-fleet-check/SKILL.md", old);
    let hash = refresh::sha256_hex(old);
    let history = |d: &str, b: &[u8]| d == "CLAUDE.md" && refresh::sha256_hex(b) == hash;
    let report = classify_with(dir, seed(), &history).unwrap();
    let s = states(&report);
    assert_eq!(s["CLAUDE.md"], State::Stale);
    assert_eq!(s[".claude/skills/tm-fleet-check/SKILL.md"], State::Edited);
    std::fs::remove_file(dir.join(".claude/skills/tm-fleet-check/SKILL.md")).unwrap();
    let report = classify_with(dir, seed(), &history).unwrap();
    assert_eq!(report.exit(), 0, "{:?}", states(&report));
}

/// Unseeded files under a managed directory and an orphaned `.shipped` are
/// extra; a `.shipped` beside a kept edit, caches and the skills' deploy
/// record are not listed.
#[test]
fn extras_and_orphaned_shipped_siblings_read_extra() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    write(dir, "scripts/datg.py", b"print('my edit')\n");
    write(dir, "scripts/datg.py.shipped", b"shipped\n");
    write(dir, "scripts/fleet-poll.py.shipped", b"shipped\n");
    write(dir, "CLAUDE.md.shipped", b"shipped\n");
    write(dir, "scripts/__pycache__/datg.cpython-312.pyc", b"\0");
    write(
        dir,
        ".claude/skills/.trusty-mpm-architect-skills.json",
        b"{}",
    );
    write(dir, ".claude/skills/my-skill/SKILL.md", b"# mine\n");
    let s = states(&classify(dir, seed()).unwrap());
    assert_eq!(s["scripts/fleet-poll.py.shipped"], State::Extra);
    assert_eq!(s["CLAUDE.md.shipped"], State::Extra);
    assert_eq!(s[".claude/skills/my-skill/SKILL.md"], State::Extra);
    for unlisted in [
        "scripts/datg.py.shipped",
        "scripts/__pycache__/datg.cpython-312.pyc",
        ".claude/skills/.trusty-mpm-architect-skills.json",
    ] {
        assert!(!s.contains_key(unlisted), "{unlisted} listed: {s:?}");
    }
}

/// `--json` carries the dir, the exit, a count per state and three shas per path.
#[test]
fn the_json_report_names_three_shas_per_path() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    write(dir, "scripts/datg.py", b"print('my edit')\n");
    let report = classify(dir, seed()).unwrap();
    let v = json_value(&report);
    assert_eq!(v["exit"], 1);
    assert_eq!(v["summary"]["edited"], 1);
    for state in State::ALL {
        assert!(v["summary"].get(state.name()).is_some(), "{state:?}");
    }
    let files = v["files"].as_array().unwrap();
    let datg = files
        .iter()
        .find(|f| f["path"] == "scripts/datg.py")
        .unwrap();
    assert_eq!(datg["state"], "edited");
    assert_eq!(
        datg["disk_sha256"],
        refresh::sha256_hex(b"print('my edit')\n")
    );
    let installed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(refresh::PROVENANCE_DEST)).unwrap())
            .unwrap();
    assert_eq!(
        datg["provenance_sha256"],
        installed["files"]["scripts/datg.py"]
    );
    assert!(datg["embedded_sha256"].is_string());
    assert_eq!(
        provenance_key(".claude/skills/tm-fleet-check/SKILL.md").as_deref(),
        Some("skills/tm-fleet-check.md")
    );
    assert_eq!(provenance_key(refresh::PROVENANCE_DEST), None);
}

/// The text report lists each non-current path and the counts.
#[test]
fn the_text_report_lists_every_non_current_path() {
    let tmp = seeded_dir();
    write(tmp.path(), "scripts/datg.py", b"print('my edit')\n");
    let text = render(&classify(tmp.path(), seed()).unwrap());
    assert!(text.contains("  edited   scripts/datg.py\n"), "{text}");
    assert!(!text.contains("CLAUDE.md"), "{text}");
    assert!(text.contains(", 1 edited,"), "{text}");
}

/// An edited managed script refuses `--strict` and the gate writes nothing.
#[test]
fn strict_refuses_an_edited_script_and_writes_nothing() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    write(dir, "scripts/datg.py", b"print('my edit')\n");
    let before = snapshot(dir);
    let report = classify(dir, seed()).unwrap();
    let msg = strict_refusal(&report).expect("an edited script refuses");
    assert!(msg.contains("edited   scripts/datg.py"), "{msg}");
    assert_eq!(report.edited_scripts(), vec!["scripts/datg.py"]);
    assert_eq!(snapshot(dir), before);
}

/// Unedited scripts pass `--strict`; an edited skill or `CLAUDE.md` does not
/// fail it, though `drift` exits 1 on them.
#[test]
fn strict_passes_unedited_scripts_and_an_edited_skill() {
    let tmp = seeded_dir();
    assert_eq!(strict_refusal(&classify(tmp.path(), seed()).unwrap()), None);
    write(tmp.path(), "CLAUDE.md", b"# my CLAUDE.md\n");
    write(
        tmp.path(),
        ".claude/skills/tm-fleet-check/SKILL.md",
        b"# mine\n",
    );
    let report = classify(tmp.path(), seed()).unwrap();
    assert_eq!(report.exit(), 1);
    assert_eq!(strict_refusal(&report), None);
}

/// The checkout root the seed resolves from.
fn repo_root() -> std::path::PathBuf {
    trusty_common::test_harness::test_repo_root().expect("resolve the checkout")
}

/// `f` on another thread, or a panic naming a hang after 20 s.
///
/// Why: #11773 — the pre-fix classifier blocked forever in `fs::read` on a
/// FIFO; a bounded wait turns that hang into a test failure.
fn within_20s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(20))
        .expect("hung: the drift read blocked on a non-regular file")
}

/// Replace `rel` under `dir` with a FIFO.
#[cfg(unix)]
fn fifo_at(dir: &Path, rel: &str) {
    let path = dir.join(rel);
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
}

/// #11773: a FIFO at a managed script path is an error, not a hang, and
/// `up --strict`'s gate refuses on it.
#[cfg(unix)]
#[test]
fn a_fifo_at_a_managed_path_fails_closed_without_hanging() {
    let tmp = seeded_dir();
    fifo_at(tmp.path(), "scripts/datg.py");
    let dir = tmp.path().to_path_buf();
    let (classified, strict) = within_20s(move || {
        let classified = classify(&dir, seed())
            .map(|_| ())
            .map_err(|e| format!("{e:#}"));
        (classified, strict_check_in(&dir, Some(&repo_root()), None))
    });
    let err = classified.expect_err("a FIFO is not readable");
    assert!(err.contains("not a regular file"), "{err}");
    assert!(err.contains("scripts/datg.py"), "{err}");
    let msg = strict.expect_err("--strict fails closed");
    assert!(msg.contains("not a regular file"), "{msg}");
}

/// #11773: a FIFO under a managed directory is skipped from the extras, and
/// a symlink to a file is still read, at a managed path and as an extra.
#[cfg(unix)]
#[test]
fn a_fifo_extra_is_skipped_without_hanging() {
    let tmp = seeded_dir();
    let dir = tmp.path();
    fifo_at(dir, "scripts/pipe.py");
    let real = dir.join("datg-real.py");
    std::fs::rename(dir.join("scripts/datg.py"), &real).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("scripts/datg.py")).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("scripts/linked.py")).unwrap();
    let owned = dir.to_path_buf();
    let report = within_20s(move || classify(&owned, seed()).map_err(|e| format!("{e:#}")))
        .expect("a FIFO extra is not an error");
    let s = states(&report);
    assert!(!s.contains_key("scripts/pipe.py"), "{s:?}");
    assert_eq!(s["scripts/datg.py"], State::Current);
    assert_eq!(s["scripts/linked.py"], State::Extra);
}

/// #11773 critic: an unreadable managed script makes the strict gate return
/// `Err`, never a pass.
#[cfg(unix)]
#[test]
fn strict_fails_closed_on_an_unreadable_script() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root reads a mode-000 file");
        return;
    }
    let tmp = seeded_dir();
    let path = tmp.path().join("scripts/datg.py");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = strict_check_in(tmp.path(), Some(&repo_root()), None);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let msg = result.expect_err("an unreadable script fails closed");
    assert!(msg.contains("cannot read"), "{msg}");
    assert!(msg.contains("scripts/datg.py"), "{msg}");
}

/// #11773 critic: with no checkout and no content cache the seed cannot
/// resolve, and the strict gate returns `Err`.
#[test]
fn strict_fails_closed_when_the_seed_cannot_resolve() {
    let tmp = seeded_dir();
    let msg = strict_check_in(tmp.path(), None, None).expect_err("no seed fails closed");
    assert!(msg.starts_with("tm architect up --strict: "), "{msg}");
}
