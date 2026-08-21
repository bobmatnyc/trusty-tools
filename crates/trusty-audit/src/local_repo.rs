//! Auditing a repository that is already on the operator's disk (#6001).
//!
//! Why: [`crate::clone`] only ever ran `gh repo clone` against a remote, so an
//! engagement whose corpus is a checkout on disk had no way in. That is not a
//! hypothetical shape: the engagement this landed for has a 1.4 GB checkout with
//! full history and 109 of its 111 sibling repositories no longer reachable from
//! the machine's GitHub credential. The data was there and the tool could not
//! read it.
//!
//! **The invariant: the source checkout is never modified.** Not a fetch, not a
//! checkout, not a stash, not a config write. It matters concretely rather than
//! theoretically — a git stash list is shared across every worktree of a
//! repository, so one stash against a client's checkout is real damage to
//! something someone else is using. Two things hold it:
//!
//! - Acquisition is `git clone <source> <staging>`, which reads the source and
//!   writes only under the working directory. Everything downstream then sees a
//!   checkout indistinguishable from a remotely-cloned one, so no later stage
//!   needs to know where it came from.
//! - The only other command run against the source is `git rev-parse`, which
//!   reads refs and configuration and writes nothing.
//!
//! Test: `super::local_repo_tests::the_source_checkout_is_byte_identical_afterwards`.
//!
//! **Clone-from-local rather than operate-in-place.** A local clone hardlinks
//! the object store, so it costs inodes rather than bytes and finishes in about
//! the time a directory walk takes. Operating in place would be faster still and
//! is exactly what the invariant forbids. The cost is that only COMMITTED
//! history crosses over — uncommitted work in the source is invisible to the
//! audit, which is what tga consumes anyway.
//! Test: `super::local_repo_tests::a_local_clone_hardlinks_the_object_store`.
//!
//! ## What makes a spec a path rather than an `owner/repo`
//!
//! [`is_local_spec`]: the spec is ABSOLUTE. Nothing else — see that function for
//! why the rule is syntactic rather than "an existing directory".
//!
//! Test: `super::local_repo_tests`.
//!
//! ## Trust boundary
//!
//! A local-path line in `engagement.toml` or `repos.txt` is as trusted as a
//! shell command the operator runs directly: it can name any readable git
//! repository on the machine, including another engagement's checkout or
//! anything else the operator's own account can read. There is no sandbox
//! that confines a local target to "under this engagement's source
//! directory" — [`is_local_spec`] decides the spec's MEANING syntactically,
//! and deciding its REACH was a separate question this feature deliberately
//! left unanswered. The operator who can type a path into `repos.txt` already
//! had that same filesystem access before this feature existed; this module
//! adds a way to point the audit at it, not a new privilege. State this
//! plainly so a future reviewer sees the boundary was considered, not missed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::AuditError;

/// The owner segment every locally-sourced checkout is acquired under.
///
/// A local path has no owner, and [`crate::clone::destination`] builds
/// `repos/<owner>/<name>` — so one is supplied. See [`derive_name`].
pub const LOCAL_OWNER: &str = "local";

/// Is this spec a local path rather than a GitHub `owner/repo`?
///
/// Why: `foo/bar` is a valid `owner/repo` AND a valid relative path, and a rule
/// that guesses wrong audits a different repository than the operator named —
/// the failure class this crate has already shipped once (#5896, fixed by
/// #5963). So the rule is one syntactic property with no overlap: an
/// `owner/repo` can never be absolute, because [`crate::clone::split_name`]
/// refuses an empty owner and a leading `/` produces one.
///
/// It is deliberately NOT `trusty-mpm`'s `is_local_workdir` rule — "an absolute
/// path that EXISTS as a directory" (`daemon/managed_routes/lifecycle.rs:394`).
/// That rule asks the filesystem what a spec means, so the same `repos.txt` line
/// means different things on two machines, and a mistyped absolute path silently
/// demotes to an `owner/repo` that is then refused for the wrong reason. Making
/// the meaning syntactic and the USABILITY a separate, loud check ([`inspect`])
/// is what keeps a refusal pointed at the real problem.
///
/// No `file://` or `path:` prefix is accepted either: a second accepted
/// spelling is a second rule to keep in step, and absoluteness already
/// disambiguates completely.
/// Test: `super::local_repo_tests::only_an_absolute_path_reads_as_a_local_repository`.
pub fn is_local_spec(spec: &str) -> bool {
    Path::new(spec.trim()).is_absolute()
}

/// The spec as a path, with trailing separators trimmed.
///
/// `/srv/apex/` and `/srv/apex` are one checkout, and leaving them as two
/// strings would make them two registry entries that then collide on
/// [`derive_name`]. Pure — nothing here touches the filesystem.
pub fn normalize(spec: &str) -> PathBuf {
    let trimmed = spec.trim();
    let cut = trimmed.trim_end_matches('/');
    PathBuf::from(if cut.is_empty() { "/" } else { cut })
}

/// `local/<basename>` — the `owner/name` a local checkout is audited under.
///
/// Why: output directories, `extract/<repo>.db` and every report section key on
/// a repository name, and a path is not one. The basename is what an operator
/// recognises, and a `.git` suffix comes off because `git clone` itself strips
/// it — a bare `apex.git` would otherwise be audited under a name its own clone
/// does not use.
///
/// **A git remote is deliberately NOT consulted.** Reading `remote.origin.url`
/// would name the repository after somewhere it cannot be fetched from, which
/// is the whole reason this path exists; and for the mirror-of-a-dead-org case
/// it would resurrect an owner that no longer resolves. The path is the identity
/// the operator has. That holds for the NAME; the repository's GitHub ISSUE
/// identity is a different question, answered separately by
/// [`github_slug`] (#6130).
///
/// **On collision**, two paths whose basenames match derive one name and would
/// share `repos/local/<name>` — the second finding the first's checkout and
/// reporting it as reused, so one repository is audited twice under two
/// registrations. That is refused rather than disambiguated, at both gates: at
/// registration by [`crate::session`], and at acquisition by
/// [`crate::clone::clone_all`], which refuses any request whose entries resolve
/// to one destination.
/// Test: `super::local_repo_tests::the_name_is_the_basename_without_a_git_suffix`,
/// `crate::clone::clone_tests::two_paths_with_one_basename_are_refused_together`.
///
/// # Errors
///
/// [`AuditError::LocalRepoUnusable`] when the path has no basename, or when its
/// basename holds no character a directory name may carry.
pub fn derive_name(path: &Path) -> Result<String, AuditError> {
    let unusable = |reason: &str| AuditError::LocalRepoUnusable {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    };
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| unusable("it has no directory name to audit it under"))?;
    let base = base.strip_suffix(".git").unwrap_or(base);
    // The same charset `clone::split_name` decides, applied by mapping rather
    // than by refusing: a path is the operator's, not a name they chose for us.
    let name: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        return Err(unusable(
            "its directory name holds no character a checkout can be named after",
        ));
    }
    Ok(format!("{LOCAL_OWNER}/{name}"))
}

/// The GitHub host whose `owner/repo` the audit's issue collection can address.
///
/// Only `github.com`: tga's issue client is hard-wired to `api.github.com`, so a
/// GitHub Enterprise remote names a repository this audit still cannot fetch —
/// accepting it would trade a wrong slug for a slug that 404s, which is the
/// same defect wearing a better hostname.
const GITHUB_HOST: &str = "github.com";

/// The `owner/repo` a remote URL addresses on [`GITHUB_HOST`], if it does.
///
/// Why (#6130): [`derive_name`] deliberately does not consult a remote, and
/// that stays right — the on-disk identity is the operator's path. But its
/// output is `local/<name>`, and `crate::run` also handed that string to tga as
/// `github.repo`, so the self-audit asked `api.github.com/repos/local/trusty-tools`
/// for 3152 work items and got 3152 404s. The repository's ISSUE identity and
/// its on-disk identity are two different questions; this answers the second
/// one only.
///
/// **Why not [`trusty_common::github_path::parse_github_path`].** That helper
/// SLUGIFIES both segments for filesystem safety — lower-cased, `_` collapsed
/// to `-` — which is correct for a directory name and wrong for an API path:
/// `Acme/Cool_App` would be queried as `acme/cool-app`, a different repository
/// or none. It also accepts any host, and this must not. A slug that addresses
/// the wrong repository is the failure mode #6130 already is.
///
/// What: strips the scheme and any userinfo, requires the host to be exactly
/// [`GITHUB_HOST`], takes the two path segments verbatim (only a trailing
/// `.git` comes off, as `git clone` itself strips it), and returns them only if
/// [`crate::clone::split_name`] accepts the pair — so a slug from here is
/// always one the rest of this crate can carry.
/// Test: `super::local_repo_tests::a_github_remote_yields_its_owner_and_repo`,
/// `super::local_repo_tests::a_non_github_remote_yields_nothing`.
pub fn parse_github_slug(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let after_scheme = match trimmed.find("://") {
        Some(idx) => &trimmed[idx + 3..],
        None => trimmed,
    };
    // scp-syntax and URL userinfo both put credentials before the host, and
    // both put them ahead of the first `/` — so only an `@` there is userinfo,
    // never one inside a path segment.
    let head_len = after_scheme.find('/').unwrap_or(after_scheme.len());
    let locator = match after_scheme[..head_len].find('@') {
        Some(idx) => &after_scheme[idx + 1..],
        None => after_scheme,
    };
    // The host ends at the scp `:` or the first `/`, whichever comes first.
    let split = locator.find([':', '/'])?;
    if !locator[..split].eq_ignore_ascii_case(GITHUB_HOST) {
        return None;
    }
    let path = locator[split + 1..].trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo] = segments.as_slice() else {
        return None;
    };
    let slug = format!("{owner}/{repo}");
    crate::clone::split_name(&slug).ok()?;
    Some(slug)
}

/// The `owner/repo` this checkout's `origin` remote addresses on GitHub.
///
/// Why: the one I/O entry point for [`parse_github_slug`], kept here so the
/// module's read-only invariant covers it — `git config --get` reads
/// configuration and writes nothing, the same guarantee [`inspect`]'s
/// `rev-parse` calls carry.
/// What: `None` when git cannot be run, the path is not a repository, there is
/// no `origin`, or its URL does not address [`GITHUB_HOST`]. Every one of those
/// is the same answer to the caller — this audit has no GitHub identity for
/// this checkout — and the caller turns it into a declared gap, never a
/// failure.
/// Test: `super::local_repo_tests::a_checkout_with_a_github_origin_resolves_its_slug`,
/// `super::local_repo_tests::a_checkout_with_no_remote_resolves_nothing`.
pub async fn github_slug(path: &Path) -> Option<String> {
    let argv: Vec<OsString> = vec![
        "-C".into(),
        path.as_os_str().to_os_string(),
        "config".into(),
        "--get".into(),
        "remote.origin.url".into(),
    ];
    let output = git(argv).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_slug(&String::from_utf8_lossy(&output.stdout))
}

/// Case-fold a derived name or destination path for a collision comparison.
///
/// Why: on a case-insensitive, case-preserving filesystem — APFS's default
/// mode, and the filesystem this feature runs on — `repos/local/Apex` and
/// `repos/local/apex` are ONE directory, but [`derive_name`] preserves case on
/// purpose (the operator's basename is what an operator recognises). A
/// collision gate that compares the derived names or destinations as plain
/// strings sees two different values and misses the collision: the second
/// registration or clone lands in the first's checkout and is reported as
/// independently audited, having actually read the first repository's
/// history. Folding only the *comparison*, not [`derive_name`]'s output,
/// keeps the on-disk directory name exactly what the operator typed.
///
/// **Unconditional and ASCII-only.** Applied to every comparison in both
/// collision gates — [`crate::clone::refuse_collisions`] and
/// [`crate::session::Session::refuse_shared_checkout`] — not only when the
/// two paths are known to share a filesystem: a false-positive refusal of a
/// genuinely distinct `Apex`/`apex` pair on a case-sensitive filesystem costs
/// far less than a silently misattributed audit. The fold is
/// [`str::to_ascii_lowercase`], which folds `A`–`Z` only; a non-ASCII case
/// pair (Turkish İ/i, German ẞ/ß) is not folded further by this function.
/// That is a narrower guarantee than a full Unicode case fold, not a
/// regression — those pairs were compared byte-for-byte before this function
/// existed, so this can only catch more collisions, never fewer.
/// Test: `super::local_repo_tests::case_folding_is_ascii_only`,
/// `crate::clone::clone_tests::two_paths_that_differ_only_by_case_are_refused_together`,
/// `crate::session::session_tests::a_second_local_path_that_differs_only_by_case_is_refused`.
pub(crate) fn case_fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Prove this path is a repository the audit can read, without changing it.
///
/// Why: a remote target is validated by a GitHub probe; a local one has no such
/// authority to ask, so the checks are the ones that decide whether the sweep
/// would produce a report worth anything. Every one of them is a refusal rather
/// than a degraded run — this crate's recent history is a run of fail-open
/// defects (#5215, #5916, #5896), and "accepted, then silently empty" is that
/// same shape.
///
/// What: in order, so the first thing that is wrong is the thing named —
/// exists, is a directory, is readable, is a git repository, is that
/// repository's root, is not shallow, has at least one commit.
///
/// **Shallow is refused; bare is accepted.** A `--depth=1` source has exactly
/// one commit, so tga would report one commit by one author over a zero-length
/// period and every line attributed to whoever last touched it — measured, and
/// the reason [`crate::clone`] carries no depth knob (#5916). A misleading
/// report is worse than none, so it is refused loudly. A BARE repository has the
/// full history and `git clone` turns it into an ordinary checkout, so refusing
/// it would reject a perfectly good corpus — a mirror of an org that no longer
/// answers is exactly the shape this feature exists for.
///
/// **A subdirectory is refused.** `git rev-parse` inside `/srv/apex/services`
/// answers about `/srv/apex`, so accepting it would audit a repository the
/// operator did not name — silently, and with a plausible-looking report.
///
/// Returns the bare reason rather than an [`AuditError`] because the two callers
/// wrap it differently: registration refuses the operator, and the sweep records
/// a gap.
/// Test: `super::local_repo_tests::every_unusable_source_is_refused_by_name`,
/// `super::local_repo_tests::a_bare_repository_is_a_usable_source`.
pub async fn inspect(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => "it does not exist".to_owned(),
        _ => format!("it could not be read: {source}"),
    })?;
    if !meta.is_dir() {
        return Err("it is not a directory".to_owned());
    }
    std::fs::read_dir(path).map_err(|source| format!("it is not readable: {source}"))?;

    // Folded into one round trip rather than a bare-only follow-up call for
    // `--show-toplevel`: a bare repository has no working tree, so that flag
    // alone fails the whole invocation even though the other two still print —
    // `rev_parse_lenient` keeps that partial answer instead of discarding it.
    let lines = rev_parse_lenient(
        path,
        &[
            "--is-bare-repository",
            "--is-shallow-repository",
            "--show-toplevel",
        ],
    )
    .await;
    if lines.len() < 2 {
        return Err("it is not a git repository".to_owned());
    }
    let bare = lines[0] == "true";
    let shallow = lines[1] == "true";

    if !bare {
        let top = lines
            .get(2)
            .ok_or_else(|| "it is not a git repository".to_owned())?;
        if !same_directory(path, Path::new(top)) {
            return Err(format!(
                "it is a subdirectory of the repository at {top} — register that path instead"
            ));
        }
    }
    if shallow {
        return Err(
            "it is a shallow clone, so its history is truncated and the audit would report \
             one commit by one author — audit a full clone instead"
                .to_owned(),
        );
    }
    rev_parse(path, &["--verify", "HEAD"])
        .await
        .map_err(|_| "it has no commits".to_owned())?;
    Ok(())
}

/// Whether two paths name one directory, comparing what the filesystem resolves.
///
/// `git rev-parse --show-toplevel` reports the resolved path, so a source
/// reached through a symlink or a `/var` → `/private/var` alias would otherwise
/// read as a subdirectory of itself. Falls back to a literal comparison when
/// either side cannot be canonicalised, which fails CLOSED — an unresolvable
/// path is refused rather than accepted.
fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(l), Ok(r)) => l == r,
        _ => left == right,
    }
}

/// The clone invocation, as argv rather than a run.
///
/// Why: the argv is where #5916's defect lived — one `--depth=1` emptied the
/// whole git-analytics leg — so it is asserted rather than trusted. Nothing here
/// truncates history, and nothing disables the local-clone optimisation that
/// makes this cheap.
/// What: `clone <source> <into>`, and no more.
/// Test: `super::local_repo_tests::a_local_clone_asks_git_for_the_whole_history`.
pub fn clone_argv(source: &Path, into: &Path) -> Vec<OsString> {
    vec![
        "clone".into(),
        source.as_os_str().to_os_string(),
        into.as_os_str().to_os_string(),
    ]
}

/// Copy the source into `into` with `git clone`, reading the source only.
///
/// Returns `git`'s own stderr on failure, so a gap line says what git said.
///
/// # Errors
///
/// The reason as one line — `git` missing, or a non-zero exit.
pub async fn clone_into(source: &Path, into: &Path) -> Result<(), String> {
    let output = git(clone_argv(source, into))
        .output()
        .await
        .map_err(|source| format!("`git clone` could not be run: {source}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git clone` exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// `git -C <path> rev-parse <args>`, as an argv.
fn rev_parse_argv(path: &Path, args: &[&str]) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["-C".into(), path.as_os_str().to_os_string()];
    argv.push("rev-parse".into());
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// `stdout` split into trimmed lines.
fn rev_parse_lines(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| line.trim().to_owned())
        .collect()
}

/// `git rev-parse <args>` against `path`, as trimmed stdout lines.
///
/// Read-only in every form used here: `rev-parse` resolves refs and reads
/// configuration, and writes nothing — which is what keeps the module's
/// invariant true for the validation half as well as the clone.
async fn rev_parse(path: &Path, args: &[&str]) -> Result<Vec<String>, ()> {
    let output = git(rev_parse_argv(path, args))
        .output()
        .await
        .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    Ok(rev_parse_lines(&output.stdout))
}

/// `git rev-parse <args>` against `path`, tolerating a non-zero exit.
///
/// Why: [`inspect`] folds `--is-bare-repository`, `--is-shallow-repository`
/// and `--show-toplevel` into one invocation, and `--show-toplevel` fails on
/// its own for a bare repository (no working tree) even though the two flags
/// ahead of it already printed. [`rev_parse`]'s success gate would discard
/// that partial stdout; this variant keeps it.
/// What: empty only when the process could not be spawned — the same failure
/// [`rev_parse`] maps to `Err(())`.
async fn rev_parse_lenient(path: &Path, args: &[&str]) -> Vec<String> {
    match git(rev_parse_argv(path, args)).output().await {
        Ok(output) => rev_parse_lines(&output.stdout),
        Err(_) => Vec::new(),
    }
}

/// A `git` child with the ambient repository environment cleared.
///
/// An inherited `GIT_DIR` or `GIT_WORK_TREE` redirects a clone at whatever the
/// parent shell was pointed at, which for an agent-run or hook-run invocation is
/// somebody else's repository. `GIT_TERMINAL_PROMPT=0` keeps a source that
/// unexpectedly wants credentials from hanging an unattended sweep.
fn git(argv: Vec<OsString>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .args(argv)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

#[cfg(test)]
pub(crate) mod local_repo_tests {
    use super::*;

    /// Everything a source-checkout fixture needs, built with a real `git`.
    ///
    /// Every fixture is created under a `tempfile::tempdir`, so nothing here
    /// names a path outside the repository and the suite passes on a fresh
    /// clone on any machine.
    pub(crate) fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("the fixture directory");
        run_git(path, &["init", "-q"]);
        run_git(path, &["config", "user.email", "fixture@example.invalid"]);
        run_git(path, &["config", "user.name", "Fixture"]);
    }

    pub(crate) fn commit(path: &Path, file: &str, body: &str) {
        std::fs::write(path.join(file), body).expect("write the fixture file");
        run_git(path, &["add", "-A"]);
        run_git(path, &["commit", "-q", "-m", "fixture"]);
    }

    /// A source repository with two commits, ready to clone from.
    pub(crate) fn source_repo(path: &Path) {
        init_repo(path);
        commit(path, "a.txt", "first\n");
        commit(path, "a.txt", "first\nsecond\n");
    }

    fn pathstr(path: &Path) -> &str {
        path.to_str().expect("a UTF-8 fixture path")
    }

    /// Run `git -C <cwd> <args>`, failing the test with git's own message.
    pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git is on PATH for this suite");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// 🔴 The disambiguation rule, stated as the pairs it has to keep apart.
    ///
    /// `foo/bar` is a valid `owner/repo` AND a valid relative path, so the rule
    /// has to be one property with no overlap. Against a rule that also accepts
    /// a relative path, the first two entries register a directory instead of a
    /// GitHub repository.
    #[test]
    fn only_an_absolute_path_reads_as_a_local_repository() {
        for remote in ["acme/api", "./acme/api", "../api", "acme/api.git", "a/b"] {
            assert!(!is_local_spec(remote), "{remote} must stay an owner/repo");
        }
        for local in ["/srv/apex", "  /srv/apex  ", "/srv/apex/", "/"] {
            assert!(is_local_spec(local), "{local} must read as a path");
        }
    }

    #[test]
    fn a_trailing_separator_is_not_a_second_repository() {
        assert_eq!(normalize("/srv/apex/"), normalize("/srv/apex"));
        assert_eq!(normalize("  /srv/apex  "), PathBuf::from("/srv/apex"));
        assert_eq!(normalize("/"), PathBuf::from("/"));
    }

    #[test]
    fn the_name_is_the_basename_without_a_git_suffix() {
        for (path, expected) in [
            ("/srv/apex", "local/apex"),
            ("/srv/apex.git", "local/apex"),
            ("/srv/APEX-2", "local/APEX-2"),
            // Anything outside the charset a checkout directory may carry
            // becomes `-`, the same substitution `run::sanitize` makes.
            ("/srv/my repo", "local/my-repo"),
        ] {
            assert_eq!(
                derive_name(Path::new(path)).expect("a name"),
                expected,
                "{path}"
            );
        }
        // And what it produces is a name `clone::split_name` accepts, which is
        // what stops a derived name reaching a path builder that refuses it.
        let name = derive_name(Path::new("/srv/my repo")).expect("a name");
        crate::clone::split_name(&name).expect("the derived name is a usable owner/name");
    }

    /// 🔴 The fix for the case-insensitive-filesystem collision (#6001 follow-up):
    /// `Apex` and `apex` fold to the same string, which is what makes both
    /// collision gates catch them. Also documents the stated ASCII-only scope —
    /// a non-ASCII case pair is left exactly as case-sensitive as it was before
    /// this function existed, never worse.
    #[test]
    fn case_folding_is_ascii_only() {
        assert_eq!(case_fold("Apex"), "apex");
        assert_eq!(case_fold("local/Apex"), "local/apex");
        assert_eq!(case_fold("apex"), case_fold("APEX"));
        // Non-ASCII: no further folding is claimed or attempted.
        assert_ne!(case_fold("İstanbul"), case_fold("istanbul"));
    }

    /// 🔴 #6130: both remote spellings resolve to the SAME slug the GitHub API
    /// takes, with case and punctuation preserved. Against
    /// `trusty_common::github_path::parse_github_path` the last row fails —
    /// that helper slugifies to `acme/cool-app`, a repository this one is not.
    #[test]
    fn a_github_remote_yields_its_owner_and_repo() {
        for url in [
            "git@github.com:bobmatnyc/trusty-tools.git",
            "git@github.com:bobmatnyc/trusty-tools",
            "https://github.com/bobmatnyc/trusty-tools.git",
            "https://github.com/bobmatnyc/trusty-tools",
            "ssh://git@github.com/bobmatnyc/trusty-tools.git",
            "https://x-access-token:ghp_token@github.com/bobmatnyc/trusty-tools.git",
            "  https://GitHub.com/bobmatnyc/trusty-tools/  ",
        ] {
            assert_eq!(
                parse_github_slug(url).as_deref(),
                Some("bobmatnyc/trusty-tools"),
                "{url}"
            );
        }
        assert_eq!(
            parse_github_slug("git@github.com:Acme/Cool_App.git").as_deref(),
            Some("Acme/Cool_App"),
            "the API slug is verbatim; slugifying it names a different repository"
        );
    }

    /// Everything that is not a `github.com` `owner/repo` answers "no identity"
    /// — which the caller turns into a declared gap, never into a guess.
    /// A GitHub Enterprise host is refused on purpose: tga's issue client only
    /// speaks to `api.github.com`, so accepting it would trade a wrong slug for
    /// one that 404s.
    #[test]
    fn a_non_github_remote_yields_nothing() {
        for url in [
            "git@gitlab.com:acme/api.git",
            "https://bitbucket.org/acme/api.git",
            "https://github.example.com/acme/api.git",
            "https://github.com.evil.test/acme/api.git",
            "/srv/mirrors/apex.git",
            "https://github.com/bobmatnyc",
            "https://github.com/acme/group/api.git",
            "https://github.com/",
            "",
        ] {
            assert_eq!(parse_github_slug(url), None, "{url}");
        }
    }

    /// The I/O half, against a real checkout: an `origin` that names GitHub
    /// resolves, and one that names nothing at all resolves to nothing.
    #[tokio::test]
    async fn a_checkout_with_a_github_origin_resolves_its_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("trusty-tools");
        source_repo(&repo);
        run_git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
        );

        assert_eq!(
            github_slug(&repo).await.as_deref(),
            Some("bobmatnyc/trusty-tools")
        );
        // And the on-disk identity is untouched by it — the two answers stay
        // independent.
        assert_eq!(derive_name(&repo).expect("a name"), "local/trusty-tools");
    }

    #[tokio::test]
    async fn a_checkout_with_no_remote_resolves_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("apex");
        source_repo(&repo);
        assert_eq!(github_slug(&repo).await, None, "no origin, no identity");

        run_git(&repo, &["remote", "add", "origin", "/srv/mirrors/apex.git"]);
        assert_eq!(
            github_slug(&repo).await,
            None,
            "a filesystem origin names no GitHub repository"
        );

        let absent = tmp.path().join("nowhere");
        assert_eq!(github_slug(&absent).await, None, "not a repository at all");
    }

    #[test]
    fn a_path_with_no_usable_basename_is_refused() {
        for path in ["/", "/.."] {
            let err = derive_name(Path::new(path)).expect_err("{path} must be refused");
            assert!(
                matches!(err, AuditError::LocalRepoUnusable { .. }),
                "{err:?}"
            );
        }
    }

    /// 🔴 Every refusal names the condition that failed, rather than accepting
    /// the path and producing an empty audit.
    #[tokio::test]
    async fn every_unusable_source_is_refused_by_name() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let missing = tmp.path().join("nowhere");
        assert!(
            inspect(&missing)
                .await
                .expect_err("absent")
                .contains("does not exist"),
            "an absent path must say so"
        );

        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"not a repo").expect("write");
        assert!(
            inspect(&file)
                .await
                .expect_err("a file")
                .contains("not a directory"),
            "a file must say so"
        );

        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).expect("mkdir");
        assert!(
            inspect(&plain)
                .await
                .expect_err("not a repo")
                .contains("not a git repository"),
            "a plain directory must say so"
        );

        let empty = tmp.path().join("empty");
        init_repo(&empty);
        assert!(
            inspect(&empty)
                .await
                .expect_err("commitless")
                .contains("no commits"),
            "a commitless repository must say so"
        );
    }

    /// 🔴 A `--depth=1` source produces the exact degenerate database #5916
    /// measured — one commit, one author, a zero-length period. Refused loudly
    /// rather than audited thinly.
    #[tokio::test]
    async fn a_shallow_source_is_refused_rather_than_audited_thinly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let full = tmp.path().join("full");
        source_repo(&full);
        let shallow = tmp.path().join("shallow");
        run_git(
            tmp.path(),
            &[
                "clone",
                "-q",
                "--depth=1",
                &format!("file://{}", pathstr(&full)),
                pathstr(&shallow),
            ],
        );

        inspect(&full)
            .await
            .expect("a full clone is a usable source");
        let reason = inspect(&shallow)
            .await
            .expect_err("a shallow source must be refused");
        assert!(reason.contains("shallow"), "{reason}");
        assert!(
            reason.contains("full clone"),
            "the remedy must be named: {reason}"
        );
    }

    /// A bare mirror carries the whole history and clones into an ordinary
    /// checkout, so refusing it would reject the mirror-of-a-dead-org case this
    /// feature exists for.
    #[tokio::test]
    async fn a_bare_repository_is_a_usable_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let full = tmp.path().join("full");
        source_repo(&full);
        let bare = tmp.path().join("mirror.git");
        run_git(
            tmp.path(),
            &["clone", "-q", "--bare", pathstr(&full), pathstr(&bare)],
        );

        inspect(&bare)
            .await
            .expect("a bare repository is a usable source");
        assert_eq!(derive_name(&bare).expect("a name"), "local/mirror");

        let into = tmp.path().join("acquired");
        clone_into(&bare, &into)
            .await
            .expect("a bare source clones");
        assert_eq!(run_git(&into, &["rev-list", "--count", "HEAD"]), "2");
    }

    /// 🔴 Naming a subdirectory would audit the enclosing repository — a
    /// different corpus than the operator asked for, reported as though it were
    /// theirs.
    #[tokio::test]
    async fn a_subdirectory_is_refused_rather_than_silently_widened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("apex");
        source_repo(&repo);
        let sub = repo.join("services");
        std::fs::create_dir(&sub).expect("mkdir");

        let reason = inspect(&sub)
            .await
            .expect_err("a subdirectory must be refused");
        assert!(reason.contains("subdirectory"), "{reason}");
        assert!(
            reason.contains(repo.to_str().expect("utf-8")),
            "the real root must be named: {reason}"
        );
    }

    /// #5916's contract for this acquisition path: no depth flag can reach git.
    #[test]
    fn a_local_clone_asks_git_for_the_whole_history() {
        let argv = clone_argv(Path::new("/srv/apex"), Path::new("/w/stage/local/apex"));
        let rendered: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered, ["clone", "/srv/apex", "/w/stage/local/apex"]);
        assert!(
            !rendered.iter().any(|a| a.contains("--depth")),
            "{rendered:?}"
        );
    }

    /// The "cheap" claim, verified rather than asserted: a local clone hardlinks
    /// the object store, so the copy costs inodes and not bytes.
    #[tokio::test]
    async fn a_local_clone_hardlinks_the_object_store() {
        use std::os::unix::fs::MetadataExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("apex");
        source_repo(&src);
        let into = tmp.path().join("acquired");
        clone_into(&src, &into)
            .await
            .expect("a local source clones");

        let loose: Vec<PathBuf> = walk(&src.join(".git/objects/")).into_iter().collect();
        assert!(!loose.is_empty(), "the fixture must have loose objects");
        for object in &loose {
            let links = std::fs::metadata(object).expect("stat").nlink();
            assert!(
                links >= 2,
                "{} was copied rather than hardlinked (nlink {links})",
                object.display()
            );
        }
    }

    /// Every regular file under `dir`, recursively.
    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else if path.is_file() {
                out.push(path);
            }
        }
        out
    }

    /// 🔴 THE invariant. The audit must never modify the source checkout — and a
    /// git stash list is shared across every worktree of a repository, so this
    /// is real damage to something someone else is using, not a theoretical one.
    ///
    /// Working tree, HEAD and reflog are all recorded before and compared after.
    /// Against an operate-in-place design — a fetch, a checkout, a stash — every
    /// one of these three assertions fails.
    #[tokio::test]
    async fn the_source_checkout_is_byte_identical_afterwards() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("apex");
        source_repo(&src);
        // Uncommitted work in the tree: the state an in-place fetch or stash
        // would disturb, and the state this must leave exactly alone.
        std::fs::write(src.join("dirty.txt"), b"work in progress\n").expect("write");

        let snapshot = |what: &str| -> String {
            match what {
                "status" => run_git(&src, &["status", "--porcelain"]),
                "head" => run_git(&src, &["rev-parse", "HEAD"]),
                "stash" => run_git(&src, &["stash", "list"]),
                _ => run_git(&src, &["reflog", "--format=%H %gd %gs"]),
            }
        };
        let before = ["status", "head", "reflog", "stash"].map(snapshot);

        inspect(&src).await.expect("the fixture is a usable source");
        clone_into(&src, &tmp.path().join("acquired"))
            .await
            .expect("the source clones");

        let after = ["status", "head", "reflog", "stash"].map(snapshot);
        assert_eq!(before[0], after[0], "the working tree changed");
        assert_eq!(before[1], after[1], "HEAD moved");
        assert_eq!(before[2], after[2], "the reflog grew");
        assert_eq!(before[3], after[3], "a stash was pushed onto a shared list");
        assert!(
            !src.join(".git/shallow").exists(),
            "the source was truncated"
        );
    }
}
