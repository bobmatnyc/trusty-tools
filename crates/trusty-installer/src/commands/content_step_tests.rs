//! Tests for the `tctl` content step (#9396). [`FakeRunner`] stands in for
//! the subprocess, so no test runs `tm` or reaches the network.

use std::cell::RefCell;

use super::*;

/// A [`CommandRunner`] that records each call and answers with `answer`.
pub(crate) struct FakeRunner {
    calls: RefCell<Vec<(PathBuf, Vec<String>)>>,
    answer: fn() -> std::io::Result<RunOutput>,
}

impl FakeRunner {
    /// `tm content update` succeeds, printing what it pinned.
    pub(crate) fn succeeding() -> Self {
        Self::answering(|| {
            Ok(RunOutput {
                success: true,
                code: Some(0),
                stdout: "pinned content-v0.3.0 (sha256 abc)\n".to_owned(),
                stderr: String::new(),
            })
        })
    }

    /// `tm content update` exits 1, unable to reach the release.
    pub(crate) fn failing() -> Self {
        Self::answering(|| {
            Ok(RunOutput {
                success: false,
                code: Some(1),
                stdout: String::new(),
                stderr: "Error: could not reach https://api.github.com: timed out\n".to_owned(),
            })
        })
    }

    pub(crate) fn answering(answer: fn() -> std::io::Result<RunOutput>) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            answer,
        }
    }

    /// Every `(program, args)` run so far.
    pub(crate) fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &Path, args: &[&str]) -> std::io::Result<RunOutput> {
        self.calls.borrow_mut().push((
            program.to_path_buf(),
            args.iter().map(|a| (*a).to_owned()).collect(),
        ));
        (self.answer)()
    }
}

/// `placed` rows for trusty-search and trusty-mpm under `dir`, with a `tm`
/// beside `trusty-mpm` as a real install places one.
pub(crate) fn placed_with_mpm(dir: &Path) -> Vec<(String, PathBuf)> {
    std::fs::write(dir.join("tm"), b"").expect("tm");
    vec![
        ("trusty-search".to_owned(), dir.join("trusty-search")),
        (MPM_CRATE.to_owned(), dir.join("trusty-mpm")),
    ]
}

/// #9396: with trusty-mpm not placed, nothing runs.
#[test]
fn no_content_step_without_trusty_mpm() {
    let runner = FakeRunner::succeeding();
    let placed = vec![(
        "trusty-search".to_owned(),
        PathBuf::from("/x/trusty-search"),
    )];
    assert_eq!(run_if_mpm_placed(&placed, &runner, true), None);
    assert!(runner.calls().is_empty());
}

/// #9396: a `tm` that cannot even start is a failed outcome naming the
/// remedy, never a panic.
#[test]
fn a_tm_that_cannot_start_is_a_failed_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = FakeRunner::answering(|| Err(std::io::Error::other("exec format error")));
    let out = run_if_mpm_placed(&placed_with_mpm(dir.path()), &runner, true).expect("the step ran");
    assert!(!out.ok);
    assert!(out.detail.contains("exec format error"), "{}", out.detail);
    assert!(out.detail.contains(REMEDY), "{}", out.detail);
}

/// #9396: the `tm` beside the placed `trusty-mpm` is the one run.
#[test]
fn tm_beside_prefers_the_sibling_tm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let placed = dir.path().join("trusty-mpm");
    assert_eq!(tm_beside(&placed), placed, "no sibling: the alias itself");
    std::fs::write(dir.path().join("tm"), b"").expect("tm");
    assert_eq!(tm_beside(&placed), dir.path().join("tm"));
}
