//! The whole chain from the command line: select, clone, analyze (#5556).
//!
//! Why: every issue under #5473 closed its own slice, and nothing obligated
//! proving they compose. They did not. `taudit clone acme/api acme/web`
//! acquired both checkouts and `taudit run` then refused with "nothing to
//! audit", because the selection file `run` reads had no writer anywhere in
//! production — `run.rs`'s own documentation named #5487/#5497/#5215 as the
//! producers and none of them wrote it. Only a test that drives one stage into
//! the next catches that; every per-stage test passed throughout.
//!
//! What: a throwaway engagement driven through the real `taudit` binary, with a
//! stub `gh` on `PATH` standing in for the remote and stub binaries standing in
//! for the pinned triple. Nothing here reaches the network, so nothing is
//! `#[ignore]`d. The three closure conditions of #5556 map onto
//! `a_cli_chain_selects_clones_and_audits_every_repository` (multi-repo, no
//! window), `a_repository_that_fails_to_clone_never_reaches_the_sweep` (partial
//! acquisition) and `a_repository_that_fails_analysis_leaves_the_sweep_partial`
//! (partial analysis).
//! Test: this is the test.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const TAUDIT: &str = env!("CARGO_BIN_EXE_taudit");

const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

/// The directory name `--work-dir` is spelled with, relative to the engagement.
const WORK_DIR_NAME: &str = "trusty-audit-work";

struct Engagement {
    tmp: tempfile::TempDir,
    work: PathBuf,
    config: PathBuf,
    /// Directory prepended to the child's `PATH`, holding the stub `gh`.
    bin: PathBuf,
}

impl Engagement {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = tmp.path().join(WORK_DIR_NAME);
        let config = tmp.path().join("engagement.toml");
        let bin = tmp.path().join("bin");
        std::fs::write(&config, CONFIG).expect("write engagement config");
        std::fs::create_dir_all(&bin).expect("mkdir bin");
        for area in ["tools", "repos", "extract", "state", "out", "logs"] {
            std::fs::create_dir_all(work.join(area)).expect("mkdir area");
        }
        Self {
            tmp,
            work,
            config,
            bin,
        }
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// A `gh` that clones from nowhere: it builds the tree `gh repo clone` would
    /// have left, so the acquisition path runs whole without a remote.
    ///
    /// `fails` is a shell glob fragment; every repository whose `owner/name`
    /// contains it exits non-zero, the way a repository the credential cannot
    /// reach does.
    fn stub_gh(&self, fails: Option<&str>) {
        let fail = match fails {
            Some(pattern) => format!(
                "case \"$name\" in *{pattern}*) \
                 echo 'GraphQL: Could not resolve to a Repository' >&2; exit 1;; esac\n"
            ),
            None => String::new(),
        };
        let body = format!(
            "#!/bin/sh\n\
             [ \"$1\" = repo ] && [ \"$2\" = clone ] || {{ echo \"unexpected: $*\" >&2; exit 9; }}\n\
             name=\"$3\"\ndest=\"$4\"\n{fail}\
             mkdir -p \"$dest/.git/refs/heads\" || exit 1\n\
             printf 'ref: refs/heads/main\\n' > \"$dest/.git/HEAD\"\n\
             printf '1111111111111111111111111111111111111111\\n' > \"$dest/.git/refs/heads/main\"\n\
             printf 'fn main() {{}}\\n' > \"$dest/main.rs\"\n\
             exit 0\n"
        );
        Self::write_script(&self.bin.join("gh"), &body);
    }

    /// A stub `tga` that writes the manifest a real one would — a zero exit with
    /// no manifest is a FAILURE, so a test expecting success needs this.
    ///
    /// It also writes the `extract/<stem>.db` a real `tga audit` would (#5862),
    /// deriving the path from `$out` the same way the unit-test stubs in
    /// `chain.rs` and `cli/registration.rs` do: `out/` and `extract/` are
    /// always siblings under the work-dir root. Without this, every scenario
    /// here that reaches `package::assemble` trips
    /// `AuditError::MissingExtractDatabase` on a stub gap, not a real one.
    fn manifest_writer(exit_for: Option<&str>) -> String {
        let fail = match exit_for {
            Some(pattern) => format!("case \"$*\" in *{pattern}*) exit 4;; esac\n"),
            None => String::new(),
        };
        format!(
            "#!/bin/sh\n{fail}out=\"\"\nwhile [ $# -gt 0 ]; do\n  \
             case \"$1\" in --output) out=\"$2\"; shift;; esac\n  shift\ndone\n\
             mkdir -p \"$out\"\n\
             printf '[report]\\ntitle = \"Acme\"\\n\\n[[repositories]]\\n\
             name = \"acme\"\\npath = \"/r\"\\n' > \"$out/manifest.toml\"\n\
             work_root=$(dirname \"$(dirname \"$out\")\")\n\
             stem=$(basename \"$out\")\n\
             mkdir -p \"$work_root/extract\"\n\
             printf 'sqlite-stub' > \"$work_root/extract/$stem.db\"\n\
             exit 0\n"
        )
    }

    /// Stand in for a verified install: four executables plus the record that
    /// says this client placed them.
    fn install_stubs(&self, script: &str) {
        for name in ["tga", "trusty-search", "trusty-analyze", "trusty-review"] {
            Self::write_script(&self.work.join("tools").join(name), script);
        }
        let record = format!(
            "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{d}/tga\"\n\
             [[tools]]\ncrate_name = \"trusty-search\"\nversion = \"0.47.0\"\nbinary = \"{d}/s\"\n\
             [[tools]]\ncrate_name = \"trusty-analyze\"\nversion = \"0.9.2\"\nbinary = \"{d}/a\"\n\
             [[tools]]\ncrate_name = \"trusty-review\"\nversion = \"0.15.1\"\nbinary = \"{d}/r\"\n",
            d = self.work.join("tools").display()
        );
        std::fs::write(self.work.join("state/tool-versions.toml"), record).expect("record");
    }

    /// One `taudit` invocation, with the stub `gh` ahead of the real one.
    fn taudit(&self, args: &[&str]) -> (String, String, Option<i32>) {
        let path = match std::env::var("PATH") {
            Ok(existing) => format!("{}:{existing}", self.bin.display()),
            Err(_) => self.bin.display().to_string(),
        };
        let out = Command::new(TAUDIT)
            .args(args)
            .arg("--work-dir")
            .arg(&self.work)
            .arg("--config")
            .arg(&self.config)
            .env("PATH", path)
            .current_dir(self.tmp.path())
            .output()
            .expect("taudit runs");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    }

    fn clone_repos(&self, repos: &[&str]) -> (String, String, Option<i32>) {
        let mut args = vec!["clone"];
        args.extend_from_slice(repos);
        self.taudit(&args)
    }

    fn run(&self) -> (String, String, Option<i32>) {
        self.taudit(&["run"])
    }

    /// The one-shot chain: install, materialize, collect, package (#5824).
    fn audit(&self) -> (String, String, Option<i32>) {
        self.taudit(&["audit"])
    }

    fn selection(&self) -> String {
        std::fs::read_to_string(self.work.join("state/selected-repos.toml")).unwrap_or_default()
    }

    /// Register repository targets the way `taudit add repo` would.
    ///
    /// Written directly rather than driven through `add`, because `add`
    /// validates each target with `gh repo view` and the stub `gh` answers only
    /// `repo clone`. What this test needs from the registry is that the
    /// materialize phase READS it, which is the same whichever writer filled it.
    fn register_repos(&self, repos: &[&str]) {
        let mut document = format!("count = {}\n", repos.len());
        for name_with_owner in repos {
            document.push_str(&format!(
                "\n[[targets]]\nkind = \"repo\"\nname_with_owner = \"{name_with_owner}\"\n"
            ));
        }
        std::fs::write(self.work.join("state/audit-targets.toml"), document).expect("registry");
    }

    /// One generated member of the return package, as the recipient reads it.
    fn package_entry(&self, entry: &str) -> String {
        let path = self.work.join("audit-return-package.zip");
        let file =
            std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        let mut archive = zip::ZipArchive::new(file).expect("a readable zip");
        let mut member = archive.by_name(entry).expect("entry present");
        let mut text = String::new();
        std::io::Read::read_to_string(&mut member, &mut text).expect("read entry");
        text
    }
}

/// The defect #5852's review found: a registered repository that never cloned
/// vanished from the engagement and the command reported success.
///
/// The clone failure is recorded only on `CloneReport::gaps`. `record_selection`
/// keeps unusable checkouts out of the selection, so the sweep never sees the
/// repository, `RunStatus` is `AllSucceeded`, and `package::exclusions` — which
/// derives entirely from the sweep's failures — never learns it existed. Before
/// the fix this exits 0 with a package whose README reads "Every repository the
/// sweep was asked to audit is in this package."
///
/// Both halves matter and neither substitutes for the other: the exit status is
/// what stops `taudit audit && send-it`, and the package's own README and
/// metadata are what a third party opening the zip has instead of this console.
#[test]
fn a_registered_repository_that_never_cloned_is_named_in_the_package() {
    let e = Engagement::new();
    e.stub_gh(Some("gone"));
    e.install_stubs(&Engagement::manifest_writer(None));
    e.register_repos(&["acme/api", "acme/gone"]);

    let (stdout, stderr, code) = e.audit();

    // 2 is EXIT_INCOMPLETE, and pinning it proves WHICH arm of
    // `Outcome::exit_code` fired. The sweep audited every repository it was
    // given, so the partial-sweep arm (1) cannot have run: the only thing left
    // to make this non-zero is the materialize-phase gap.
    assert_eq!(
        code,
        Some(2),
        "a registered repository that never cloned must not exit zero\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("Audited 1 repository"), "{stdout}");

    // Each section states the fact it owns, and no section repeats another's
    // sentence: the acquisition failed it, and the package does not include it.
    assert!(stdout.contains("Gap: acme/gone"), "{stdout}");
    assert!(stdout.contains("Not included: acme/gone"), "{stdout}");
    assert!(
        !stdout.contains("Not audited: acme/gone"),
        "the chain roll-up repeated the acquisition section verbatim:\n{stdout}"
    );

    let readme = e.package_entry("README.md");
    assert!(
        readme.contains("acme/gone"),
        "the package README must name the repository it does not cover:\n{readme}"
    );
    assert!(readme.contains("Not in this package"), "{readme}");

    let metadata = e.package_entry("package.toml");
    assert!(
        metadata.contains("acme/gone"),
        "package.toml must name the repository it does not cover:\n{metadata}"
    );
}

/// #5556 closure conditions 1 and 2: one CLI-driven sequence selects SEVERAL
/// repositories, clones them, and audits them, with nothing but the two
/// commands — no hand-written selection file, no window.
#[test]
fn a_cli_chain_selects_clones_and_audits_every_repository() {
    let e = Engagement::new();
    e.stub_gh(None);
    e.install_stubs(&Engagement::manifest_writer(None));

    let (stdout, stderr, code) = e.clone_repos(&["acme/api", "acme/web"]);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("2 repositories on disk"), "{stdout}");
    assert!(
        e.work.join("repos/acme/api/.git").is_dir(),
        "the checkout must be promoted into repos/"
    );

    // Nothing between the two commands: the clone IS the selection.
    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 2 repositories"), "{stdout}");
    assert!(stdout.contains("ok      acme/api"), "{stdout}");
    assert!(stdout.contains("ok      acme/web"), "{stdout}");
}

/// #5556 closure condition 3, at the acquisition stage: one repository of three
/// cannot be reached. The clone reports the gap and does not exit 0, and the
/// sweep then audits the two that landed — never the one that did not.
#[test]
fn a_repository_that_fails_to_clone_never_reaches_the_sweep() {
    let e = Engagement::new();
    e.stub_gh(Some("gone"));
    e.install_stubs(&Engagement::manifest_writer(None));

    let (stdout, stderr, code) = e.clone_repos(&["acme/api", "acme/gone", "acme/web"]);
    assert_eq!(code, Some(2), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("FAILED"), "{stdout}");
    assert!(stdout.contains("Gap: acme/gone"), "{stdout}");
    assert!(stdout.contains("2 repositories on disk"), "{stdout}");

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 2 repositories"), "{stdout}");
    assert!(
        !stdout.contains("acme/gone"),
        "a repository that never cloned must not be audited or reported on:\n{stdout}"
    );
}

/// #5556 closure condition 3, at the analysis stage: both repositories are on
/// disk and one fails `tga audit`. The sweep completes, says so, and the process
/// does not exit 0.
#[test]
fn a_repository_that_fails_analysis_leaves_the_sweep_partial() {
    let e = Engagement::new();
    e.stub_gh(None);
    e.install_stubs(&Engagement::manifest_writer(Some("acme-web")));

    let (stdout, stderr, code) = e.clone_repos(&["acme/api", "acme/web"]);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("PARTIAL"), "{stdout}");
    assert!(stdout.contains("ok      acme/api"), "{stdout}");
    assert!(stdout.contains("FAILED  acme/web"), "{stdout}");
}

/// The failure path of the selection write itself: an acquisition in which
/// EVERY repository failed is refused, and a refusal must not take the previous
/// selection with it — the operator's next `taudit run` still has the set that
/// did land.
#[test]
fn an_acquisition_that_fails_entirely_leaves_the_previous_selection_intact() {
    let e = Engagement::new();
    e.stub_gh(None);
    e.install_stubs(&Engagement::manifest_writer(None));
    let (stdout, stderr, code) = e.clone_repos(&["acme/api"]);
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    let recorded = e.selection();
    assert!(recorded.contains("acme/api"), "{recorded}");

    // A second acquisition where nothing can be reached at all.
    e.stub_gh(Some("web"));
    let (stdout, stderr, code) = e.clone_repos(&["acme/web"]);
    assert_ne!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        e.selection(),
        recorded,
        "a refused acquisition rewrote the selection"
    );

    let (stdout, stderr, code) = e.run();
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("Audited 1 repository"), "{stdout}");
}
