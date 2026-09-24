//! Doctor probe: this binary's EMBEDDED skill assets against the repo they
//! were compiled from (issue #8482).
//!
//! Why: bundled skill assets are `include_str!`-embedded at compile time
//! (`core::bundle_tm_skills`), so an installed binary ships the asset text as
//! of its build and the deploy path faithfully writes that text. On 2026-09-23
//! `/Users/mac/.cargo/bin/tm` was built at 07:26:46Z; a fix to
//! `tm-epic/references/manual-procedure.md` merged at 16:32Z; for nine hours
//! the daemon kept deploying the pre-fix text, and at 22:06:01Z it REVERTED six
//! files a newer build had already deployed correctly. The pre-fix text carried
//! a recipe that silently empties an epic body, so the lag was not cosmetic.
//!
//! Nothing detected it, and one row actively denied it. `skill_staleness`
//! (`doctor_skill_drift.rs`) compares deployed files against THIS binary's
//! embedded assets — by design, per #4604 — so it is structurally blind here:
//! both sides of its comparison come from the same stale source.
//! `binary_provenance` compares a semver against cargo's registry ledger and
//! said "the binary is NOT stale" while it lagged the source tree by nine hours;
//! #8482 narrowed that sentence to what it actually checked and pointed it here.
//!
//! What: [`check_bundled_asset_lag`] resolves the project's `origin` remote
//! through the same derivation `doctor_rust_build_env::gather` uses
//! (`trusty_common::github_path::derive_github_path`) and SKIPS unless it is
//! `bobmatnyc/trusty-tools` — the comparison is meaningless anywhere else. In
//! this repo it hashes every `skills/*` entry of [`bundle::ALL`] with the same
//! `<rel_path>\0<contents>\n` construction [`skill_bundle_stamp`] folds over,
//! reads the same paths out of `origin/main`, and Warns when any key differs.
//! READ-ONLY: it never fetches, writes, installs, or deploys.
//!
//! Fail-closed (the Fail-Open Check): every arm that could not read the source
//! — no `origin/main`, no git, not a repo, an empty asset tree, a non-UTF-8
//! blob — reports [`CheckStatus::Unknown`], which ranks above `Warn` and never
//! renders as healthy. A staleness detector that passes when it cannot read the
//! source is worse than no detector, because it converts "unknown" into "fine".
//!
//! Test: `doctor_bundled_asset_lag_tests.rs`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use trusty_common::github_path::GithubPath;

use crate::core::agent_manifest::checksum;
use crate::core::build_identity::build_id;
use crate::core::bundle;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "bundled_asset_lag";

/// The only repo whose source tree this binary's assets can be compared against.
const REPO_OWNER: &str = "bobmatnyc";
/// See [`REPO_OWNER`].
const REPO_NAME: &str = "trusty-tools";

/// The ref the comparison reads. `origin/main` and not `HEAD`: the question is
/// whether the binary lags what MERGED, not what the operator has checked out.
const REF: &str = "origin/main";

/// Repo-relative directory holding the assets `bundle::ALL`'s `skills/*` entries
/// embed. A bundle key `<k>` is the repo path `<ASSET_DIR>/<k>`.
const ASSET_DIR: &str = "crates/trusty-mpm/src/assets/skills";

/// How many lagging keys the message names before summarising the rest.
const MAX_NAMED: usize = 5;

/// What reading the repo side of the comparison produced.
///
/// Why: the two outcomes carry different verdicts — a read that failed is
/// [`CheckStatus::Unknown`], never a pass — so they are separate variants rather
/// than an empty map standing in for both.
/// What: `Read` carries one content hash per repo asset key plus the newest
/// commit touching the asset tree; `Unreadable` carries why the read failed.
/// Test: `lag_is_unknown_when_origin_main_is_absent`.
enum RepoAssets {
    /// Key → content hash, for every asset present at [`REF`].
    Read {
        /// Per-key hash, built with [`asset_stamp`].
        hashes: BTreeMap<String, String>,
        /// ISO-8601 committer date of the newest commit touching [`ASSET_DIR`].
        newest_commit: Option<String>,
    },
    /// The source tree could not be read; the string says why.
    Unreadable(String),
}

/// Compare this binary's embedded skill assets against `origin/main`.
///
/// Why: see the module doc — this is the only row that can see a binary whose
/// embedded assets lag the repo, because it is the only one whose reference
/// point is not the binary itself.
/// What: derives the project's `owner/repo` with
/// [`trusty_common::github_path::derive_github_path`] (the derivation
/// `doctor_rust_build_env::gather` already uses) and hands it, plus a lazy
/// reader over the project directory, to [`report`]. The reader is never called
/// when the identity gate declines, so a foreign repo runs no git command.
/// Test: `doctor_bundled_asset_lag_tests.rs`.
pub(super) fn check_bundled_asset_lag(project_dir: Option<&Path>) -> DoctorCheck {
    let identity = project_dir.and_then(trusty_common::github_path::derive_github_path);
    report(identity.as_ref(), &|| match project_dir {
        Some(dir) => read_repo_assets(dir),
        None => RepoAssets::Unreadable("no project directory was supplied".to_string()),
    })
}

/// Pure-over-its-inputs core of [`check_bundled_asset_lag`].
///
/// Why: taking the identity and a lazy `repo` reader as parameters is what lets
/// a test prove the skip arm runs NO git command — the reader it passes panics
/// if called — without mocking `Command`.
/// What: `Ok` (not applicable) when the identity is absent or is not
/// `bobmatnyc/trusty-tools`; otherwise [`CheckStatus::Unknown`] for an
/// unreadable source tree and [`compare`]'s verdict for a readable one.
/// Test: `lag_skips_without_the_trusty_tools_remote`,
/// `lag_skips_without_any_remote`, `lag_is_unknown_when_the_source_is_unreadable`.
fn report(identity: Option<&GithubPath>, repo: &dyn Fn() -> RepoAssets) -> DoctorCheck {
    let applies = identity.is_some_and(|id| id.owner == REPO_OWNER && id.repo == REPO_NAME);
    if !applies {
        let seen = identity.map_or_else(
            || "no `origin` remote resolved".to_string(),
            |id| format!("`origin` resolves to `{}/{}`", id.owner, id.repo),
        );
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "not applicable — {seen}, and this binary's embedded assets can only be \
                 compared against the `{REPO_OWNER}/{REPO_NAME}` source tree they were \
                 compiled from (issue #8482)"
            ),
        );
    }

    match repo() {
        RepoAssets::Unreadable(why) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "the `{REPO_NAME}` source tree could not be read, so whether this binary's \
                 embedded assets lag it is UNKNOWN — {why}. Reported as unknown rather than \
                 clean deliberately: a staleness detector that passes when it cannot read \
                 the source converts \"unknown\" into \"fine\" (issue #8482)"
            ),
        ),
        RepoAssets::Read {
            hashes,
            newest_commit,
        } => compare(&hashes, newest_commit.as_deref()),
    }
}

/// Grade the embedded bundle against the repo hashes.
///
/// Why: a key missing on EITHER side is lag — an asset edited on `main` and an
/// asset ADDED to `main` after this build both mean the binary deploys text the
/// repo no longer (or does not yet) carry.
/// What: folds the union of bundle keys and repo keys; `Ok` when every key
/// agrees, otherwise `Warn` naming up to [`MAX_NAMED`] lagging keys, this
/// binary's build timestamp, the newest asset commit's, and the remedy.
/// Test: `lag_warns_and_names_the_one_differing_asset`,
/// `lag_warns_for_an_asset_the_binary_does_not_embed`,
/// `lag_passes_when_every_asset_matches`.
fn compare(repo: &BTreeMap<String, String>, newest_commit: Option<&str>) -> DoctorCheck {
    let bundled = bundled_key_stamps();
    let mut lagging: Vec<&str> = Vec::new();
    for (key, stamp) in &bundled {
        if repo.get(key) != Some(stamp) {
            lagging.push(key);
        }
    }
    for key in repo.keys() {
        if !bundled.contains_key(key) {
            lagging.push(key);
        }
    }
    lagging.sort_unstable();

    let built = build_timestamp();
    if lagging.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "all {} embedded skill assets match `{REF}` — this binary (built {built}) \
                 deploys the merged text (issue #8482)",
                bundled.len()
            ),
        );
    }

    let named: Vec<&str> = lagging.iter().take(MAX_NAMED).copied().collect();
    let rest = lagging.len().saturating_sub(named.len());
    let more = if rest == 0 {
        String::new()
    } else {
        format!(" (+{rest} more)")
    };
    let merged = newest_commit.unwrap_or("unknown");
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "{} embedded skill asset(s) differ from `{REF}`: {}{more}. This binary was built \
             {built}; the newest commit touching `{ASSET_DIR}` is {merged}. Every deploy from \
             this binary writes the OLDER text, and `skill_staleness` cannot see it — it \
             compares deployed files against these same embedded assets (#4604). Remedy: \
             `cargo install --path <clean-checkout>/crates/trusty-mpm --locked` from a \
             checkout whose `git status --porcelain` is empty — never `cp` a release binary \
             on macOS, the next exec is SIGKILL'd as an invalid signature — then `tm \
             restart`, then redeploy, which happens automatically on the next managed spawn \
             or explicitly via `tm reinstall` or `tm doctor --fix-skills` (issue #8482)",
            lagging.len(),
            named.join(", "),
        ),
    )
}

/// Hash every `skills/*` entry of [`bundle::ALL`], keyed by its path under
/// [`ASSET_DIR`].
///
/// Why: [`skill_bundle_stamp`](crate::core::skill_source::skill_bundle_stamp)
/// folds the whole table into ONE digest, which answers "did anything change"
/// and cannot name what. This reuses that function's exact per-entry
/// construction so the two schemes cannot drift, and keeps the entries apart so
/// the Warn can name them.
/// What: `<key> → checksum("skills/<key>\0<contents>\n")`.
/// Test: `bundled_key_stamps_covers_every_skill_entry`.
fn bundled_key_stamps() -> BTreeMap<String, String> {
    bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
        .filter_map(|a| {
            let key = a.rel_path.strip_prefix("skills/")?;
            Some((key.to_string(), asset_stamp(a.rel_path, a.contents)))
        })
        .collect()
}

/// One asset's stamp, in `skill_bundle_stamp`'s own construction.
///
/// Why: the repo side must be hashed identically to the bundle side, so both
/// call this rather than each spelling the concatenation out.
/// What: `checksum(format!("{rel_path}\0{contents}\n"))` — `rel_path` is the
/// BUNDLE path (`skills/<key>`), never the repo path, so the two sides agree.
/// Test: `bundled_key_stamps_covers_every_skill_entry`.
fn asset_stamp(rel_path: &str, contents: &str) -> String {
    checksum(&format!("{rel_path}\0{contents}\n"))
}

/// This binary's `TRUSTY_MPM_BUILD_ID`, rendered as a UTC timestamp.
///
/// Why: the Warn has to let an operator see the nine-hour gap at a glance, and
/// a raw nanosecond counter does not.
/// What: `build.rs` emits the id as nanoseconds since the Unix epoch
/// (`build.rs:67-72`); anything that does not parse into that range renders as
/// the raw id rather than a wrong date.
/// Test: `build_timestamp_is_an_iso_instant`.
fn build_timestamp() -> String {
    let raw = build_id();
    let Ok(nanos) = raw.parse::<i64>() else {
        return format!("build id {raw}");
    };
    chrono::DateTime::from_timestamp_nanos(nanos)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Read every asset under [`ASSET_DIR`] at [`REF`].
///
/// Why: the whole row rests on this being the SOURCE tree's text, not the
/// working tree's — a dirty checkout must not mask or manufacture lag.
/// What: verifies the ref resolves, lists the tree, reads every blob through a
/// single `git cat-file --batch` (143 assets ship today; one `git show` each
/// would be 143 processes on every `tm doctor`, reading the very same
/// `origin/main:<path>` objects), and takes the newest commit date for the
/// directory. Any step that fails yields [`RepoAssets::Unreadable`].
/// Test: `lag_is_unknown_when_origin_main_is_absent`,
/// `lag_is_unknown_outside_a_git_repo`, `lag_reads_the_committed_tree`.
fn read_repo_assets(dir: &Path) -> RepoAssets {
    let commit_ref = format!("{REF}^{{commit}}");
    if let Err(why) = git_stdout(dir, &["rev-parse", "--verify", "--quiet", &commit_ref]) {
        return RepoAssets::Unreadable(format!(
            "`{REF}` does not resolve in `{}` ({why}) — run `git fetch origin`",
            dir.display()
        ));
    }

    let listing = match git_stdout(
        dir,
        &["ls-tree", "-r", "-z", "--name-only", REF, "--", ASSET_DIR],
    ) {
        Ok(out) => out,
        Err(why) => {
            return RepoAssets::Unreadable(format!(
                "`git ls-tree` failed in `{}`: {why}",
                dir.display()
            ));
        }
    };
    let paths: Vec<String> = String::from_utf8_lossy(&listing)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return RepoAssets::Unreadable(format!("`{REF}` carries no files under `{ASSET_DIR}`"));
    }

    let hashes = match read_blobs(dir, &paths) {
        Ok(hashes) => hashes,
        Err(why) => return RepoAssets::Unreadable(why),
    };
    let newest_commit = git_stdout(dir, &["log", "-1", "--format=%cI", REF, "--", ASSET_DIR])
        .ok()
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .filter(|s| !s.is_empty());
    RepoAssets::Read {
        hashes,
        newest_commit,
    }
}

/// Hash every blob named by `paths`, in one `git cat-file --batch`.
///
/// Why: one process instead of one per asset. `--batch` also reports a missing
/// object explicitly rather than by exit status, which keeps a partial read from
/// looking like a clean one.
/// What: feeds `<REF>:<path>` per line, parses the `<oid> blob <size>` header +
/// body framing, and stamps each body with [`asset_stamp`] under its bundle key.
/// A missing entry is skipped (it becomes a lagging key in [`compare`]); a
/// non-UTF-8 body is an error, because it cannot be stamped honestly.
/// Test: `lag_reads_the_committed_tree`, `lag_warns_and_names_the_one_differing_asset`.
fn read_blobs(dir: &Path, paths: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("`git cat-file` could not be spawned: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "`git cat-file` gave no stdin".to_string())?;
        for path in paths {
            writeln!(stdin, "{REF}:{path}")
                .map_err(|e| format!("`git cat-file` stdin write failed: {e}"))?;
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("`git cat-file` did not complete: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git cat-file` exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let mut hashes = BTreeMap::new();
    let mut cursor = &out.stdout[..];
    for path in paths {
        let split = cursor
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| format!("`git cat-file` output ended before `{path}`"))?;
        let header = String::from_utf8_lossy(&cursor[..split]).into_owned();
        cursor = &cursor[split + 1..];
        let Some(size) = header
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            // `<request> missing` / `<request> ambiguous` — no body follows.
            continue;
        };
        if cursor.len() < size + 1 {
            return Err(format!("`git cat-file` body for `{path}` was truncated"));
        }
        let body = std::str::from_utf8(&cursor[..size])
            .map_err(|e| format!("`{path}` at `{REF}` is not UTF-8: {e}"))?;
        cursor = &cursor[size + 1..];
        let Some(key) = path
            .strip_prefix(ASSET_DIR)
            .map(|k| k.trim_start_matches('/'))
        else {
            continue;
        };
        hashes.insert(key.to_string(), asset_stamp(&format!("skills/{key}"), body));
    }
    Ok(hashes)
}

/// Run one git command in `dir` and return its stdout, or why it failed.
///
/// Why: three call sites need the same "git is absent / this is not a repo /
/// the command failed" collapse, and every one of them must produce a REASON
/// rather than an `Option`, because the reason is what the Unknown reports.
/// What: `Err` when git cannot be spawned or exits non-zero, carrying stderr.
/// Test: `lag_is_unknown_outside_a_git_repo`.
fn git_stdout(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git could not be run: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git exited {}", out.status)
        } else {
            stderr
        });
    }
    Ok(out.stdout)
}

#[cfg(test)]
#[path = "doctor_bundled_asset_lag_tests.rs"]
mod tests;
