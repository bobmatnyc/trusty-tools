//! Tests for the `tctl` content step (#9396). [`FakeRunner`] stands in for
//! the subprocess, so no test runs `tm` or reaches the network.

use std::cell::RefCell;

use super::*;

/// What one fake run answers, given the program and its arguments.
type Answer = Box<dyn Fn(&Path, &[&str]) -> std::io::Result<RunOutput>>;

/// A [`CommandRunner`] that records each call and answers with `answer`.
pub(crate) struct FakeRunner {
    calls: RefCell<Vec<(PathBuf, Vec<String>)>>,
    answer: Answer,
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
        Self::by_program(move |_, _| answer())
    }

    /// #10588: answers per program, so `tm` and `trusty-review` can differ.
    pub(crate) fn by_program(
        answer: impl Fn(&Path, &[&str]) -> std::io::Result<RunOutput> + 'static,
    ) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    /// #10588: `tm content update` pins `content-v2.3.0`, and the placed
    /// trusty-review — newer than that release — answers `content check`
    /// with `review`.
    pub(crate) fn review_check(review: fn() -> RunOutput) -> Self {
        Self::by_program(move |program, args| {
            if program.file_name() == Some(std::ffi::OsStr::new("trusty-review")) {
                assert_eq!(args, ["content", "check"], "the check verb");
                return Ok(review());
            }
            Ok(RunOutput {
                success: true,
                code: Some(0),
                stdout: "pinned content-v2.3.0 (sha256 aa0c)\n".to_owned(),
                stderr: String::new(),
            })
        })
    }

    /// #10588: the deployed release predates the principles addendum.
    pub(crate) fn review_newer_than_content() -> Self {
        Self::review_check(|| {
            RunOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: format!("Error: content content-v2.3.0 does not serve trusty-review 0.39.0: missing or unusable {PRINCIPLES}; run `tm content update`\n"),
        }
        })
    }

    /// #10588: the deployed release serves the placed trusty-review.
    pub(crate) fn review_served() -> Self {
        Self::review_check(|| RunOutput {
            success: true,
            code: Some(0),
            stdout: "content content-v2.3.0 has all 7 files trusty-review 0.39.0 reads\n"
                .to_owned(),
            stderr: String::new(),
        })
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
        (self.answer)(program, args)
    }
}

/// The bundle path the #10588 fakes report missing.
pub(crate) const PRINCIPLES: &str = "instructions/trusty-review/prompts/principles_addendum.md";

/// `placed` rows for trusty-search and trusty-mpm under `dir`, with a `tm`
/// beside `trusty-mpm` as a real install places one.
pub(crate) fn placed_with_mpm(dir: &Path) -> Vec<(String, PathBuf)> {
    std::fs::write(dir.join("tm"), b"").expect("tm");
    vec![
        ("trusty-search".to_owned(), dir.join("trusty-search")),
        (MPM_CRATE.to_owned(), dir.join("trusty-mpm")),
    ]
}

/// #10588: [`placed_with_mpm`] plus trusty-review under `dir`.
pub(crate) fn placed_with_mpm_and_review(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut placed = placed_with_mpm(dir);
    placed.push(("trusty-review".to_owned(), dir.join("trusty-review")));
    placed
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

/// #10588: a released trusty-review older than `content check` answers with
/// clap's usage error; that is reported as not checked, never as a failure,
/// and any other exit 2 still fails.
#[test]
fn a_trusty_review_without_the_check_verb_is_not_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let placed = vec![("trusty-review".to_owned(), dir.path().join("trusty-review"))];
    let old = FakeRunner::review_check(|| RunOutput {
        success: false,
        code: Some(2),
        stdout: String::new(),
        stderr: "error: unrecognized subcommand 'content'\n\nUsage: trusty-review\n".to_owned(),
    });
    let out = run_content_steps(&placed, &old, true).expect("the check ran");
    assert!(out.ok, "{out:?}");
    assert!(out.detail.contains("not checked"), "{out:?}");

    let broken = FakeRunner::review_check(|| RunOutput {
        success: false,
        code: Some(2),
        stdout: String::new(),
        stderr: "Error: content content-v2.3.0 is unreadable\n".to_owned(),
    });
    let out = run_content_steps(&placed, &broken, true).expect("the check ran");
    assert!(!out.ok, "{out:?}");
    assert!(out.detail.contains("is unreadable"), "{out:?}");
}
