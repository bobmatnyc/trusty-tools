//! Launch-time `ws/<session-name>` workstream-activity label ensure (issue
//! #3726, PM-brief decision).
//!
//! Why: workstream activity is tracked on GitHub via a `ws/<session-name>`
//! label — explicitly NOT milestones, which stay reserved for epics/releases
//! (a GitHub repo allows only ONE milestone per issue, a slot epics/releases
//! already need; milestones are also a heavyweight lifecycle object, while a
//! workstream is ephemeral and freely renamable; labels, by contrast, are
//! multi-valued, cheap to create, and filterable exactly like the existing
//! `trusty-mpm` convention label). For the PM brief's `--label ws/<name>`
//! convention to work on the FIRST issue/PR a session ever files, the label
//! must already exist in the repo — `gh issue edit --add-label` fails on an
//! unknown label. This module makes label creation part of session launch
//! itself, so no PM ever hits that failure.
//! What: [`ensure_workstream_label`] is the launch-time entry point — given
//! the (optional) `repo_url` a session was spawned from, its workspace
//! directory, and its tmux session name, it derives the target
//! `<owner>/<repo>` (preferring the passed `repo_url`, falling back to the
//! workspace's own `git remote get-url origin`), and idempotently creates
//! `ws/<session-name>` via `gh label create --force` with a name-derived
//! stable color (see [`label_color_for`]). Every non-GitHub-remote case (no
//! origin, non-GitHub host, `gh` missing/unauthenticated/timed-out) is a
//! logged, non-fatal skip — [`LabelOutcome`] is returned for callers/tests
//! that want to observe the branch taken, but production call sites (session
//! launch) MUST treat every variant as fire-and-forget: this must NEVER block
//! or fail a session launch.
//! Test: `owner_repo_from_ssh_origin`, `owner_repo_from_https_origin`,
//! `owner_repo_rejects_non_github_host`, `label_color_is_stable_and_valid_hex`,
//! `label_color_differs_across_names`, `ensure_skips_when_no_origin`,
//! `ensure_skips_when_non_github`, `ensure_skips_when_name_blank`,
//! `ensure_creates_label_via_runner`, `ensure_reports_runner_failure` drive the
//! pure derivation + a scripted [`GhLabelRunner`] fake — no live `gh`/network.

use std::path::Path;
use std::time::Duration;

use trusty_common::github_path::GithubPath;

use crate::core::gh_account::run_bounded;

/// Bound for the `gh label create` call this module makes at session launch.
///
/// Why: a launch must never hang on a wedged/slow `gh` (stuck credential
/// helper, offline network). 5s mirrors `gh_account`'s `GH_ENFORCE_TIMEOUT` —
/// generous enough for a real call, short enough to never meaningfully delay
/// a launch that already performs a git clone and multiple deploy steps.
const GH_LABEL_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of one [`ensure_workstream_label`] call.
///
/// Why: production call sites only log this, but the unit tests (and any
/// future `tm doctor` surfacing) need a typed way to assert which branch was
/// taken instead of scraping log text.
/// What: one variant per skip reason, plus `Ensured` (the `gh` call
/// succeeded) and `GhFailed` (a `gh` call was attempted and failed/timed out).
/// Test: every variant is asserted by name in this module's tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelOutcome {
    /// The label was created/updated via `gh label create --force`.
    Ensured,
    /// `session_name` was empty/whitespace — nothing to label with.
    SkippedBlankName,
    /// Neither the supplied `repo_url` nor the workspace's own git origin
    /// resolved to a remote URL.
    SkippedNoOrigin,
    /// The resolved origin is not a `github.com` remote.
    SkippedNonGithub,
    /// A `gh` call was attempted but failed (non-zero exit, spawn error, or
    /// timed out past [`GH_LABEL_TIMEOUT`]) — covers "gh missing",
    /// "unauthenticated", and "offline" alike, since all three surface as an
    /// unsuccessful `gh` invocation.
    GhFailed,
}

/// Ensure the `ws/<session_name>` label exists on the session's GitHub repo.
///
/// Why: see the module docs — this is the launch-time hook that makes the PM
/// brief's `--label ws/<name>` convention usable from a session's very first
/// issue/PR.
/// What: derives `<owner>/<repo>` (via [`owner_repo_from_origin`] on
/// `repo_url` when present, else by reading `workspace_dir`'s
/// `remote.origin.url`), then runs `gh label create ws/<session_name> --repo
/// <owner>/<repo> --color <hex> --description "trusty-mpm workstream
/// <session_name>" --force` through [`RealGhRunner`], bounded by
/// [`GH_LABEL_TIMEOUT`]. Every failure path is a logged, non-fatal skip — see
/// [`LabelOutcome`].
/// Test: `ensure_creates_label_via_runner`, `ensure_skips_when_no_origin`,
/// `ensure_skips_when_non_github`, `ensure_skips_when_name_blank`.
pub fn ensure_workstream_label(
    repo_url: Option<&str>,
    workspace_dir: &Path,
    session_name: &str,
) -> LabelOutcome {
    ensure_workstream_label_with(&RealGhRunner, repo_url, workspace_dir, session_name)
}

/// [`ensure_workstream_label`] with the `gh` invocation seam injected, so
/// tests can drive it with a scripted [`GhLabelRunner`] instead of a live
/// `gh`.
fn ensure_workstream_label_with<R: GhLabelRunner>(
    runner: &R,
    repo_url: Option<&str>,
    workspace_dir: &Path,
    session_name: &str,
) -> LabelOutcome {
    let name = session_name.trim();
    if name.is_empty() {
        tracing::debug!("workstream label: blank session name — skipping");
        return LabelOutcome::SkippedBlankName;
    }

    let origin = repo_url
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| origin_url_from_workspace(workspace_dir));
    let Some(origin) = origin else {
        tracing::debug!(session = %name, "workstream label: no git origin resolvable — skipping");
        return LabelOutcome::SkippedNoOrigin;
    };

    let Some(gh_path) = owner_repo_from_origin(&origin) else {
        tracing::debug!(
            session = %name,
            origin = %origin,
            "workstream label: origin is not a github.com remote — skipping"
        );
        return LabelOutcome::SkippedNonGithub;
    };

    let label = label_name_for(name);
    let color = label_color_for(name);
    let description = format!("trusty-mpm workstream {name}");
    let repo = gh_path.rel_path();

    if runner.create_label(&repo, &label, &color, &description) {
        tracing::info!(session = %name, repo = %repo, label = %label, "workstream label ensured");
        LabelOutcome::Ensured
    } else {
        tracing::debug!(
            session = %name,
            repo = %repo,
            label = %label,
            "workstream label: `gh label create` failed/unavailable — skipping (non-fatal)"
        );
        LabelOutcome::GhFailed
    }
}

/// Fire-and-forget [`ensure_workstream_label`] on the blocking thread pool.
///
/// Why: [`ensure_workstream_label`] shells out to `gh` (bounded by
/// [`GH_LABEL_TIMEOUT`], but still up to a few seconds on a slow/offline
/// host); the daemon's `spawn_managed_*` branches call this at the identical
/// point (right before launching the runtime) across all three spawn shapes
/// (cloned, in-project, local-path-redirected). Detaching via
/// `tokio::task::spawn` + `spawn_blocking` — rather than `.await`ing inline —
/// guarantees it adds ZERO latency to the launch critical path, matching
/// `daemon::managed_routes::lifecycle`'s existing convention for blocking
/// `gh` calls made from async handlers (see that module's `resolve_gh_env`
/// doc comment).
/// What: clones the three owned inputs, spawns a detached task that runs
/// [`ensure_workstream_label`] on the blocking thread pool, and logs (at
/// debug) if the spawned task itself could not be joined — the ensure call
/// already logs its own outcome internally. Must be called from inside a
/// tokio runtime (every call site is an async `spawn_managed_*` handler).
/// Test: [`ensure_workstream_label`]'s own unit tests cover the pure logic
/// this wraps; this wrapper is a thin, side-effect-only dispatch with
/// nothing pure to assert beyond "doesn't block", which the
/// `handler_spawn_*`/`resume_managed_*` integration tests already exercise
/// by not timing out.
pub fn spawn_workstream_label_ensure(
    repo_url: Option<String>,
    workspace_dir: std::path::PathBuf,
    session_name: String,
) {
    tokio::task::spawn(async move {
        if let Err(e) = tokio::task::spawn_blocking(move || {
            ensure_workstream_label(repo_url.as_deref(), &workspace_dir, &session_name)
        })
        .await
        {
            tracing::debug!("workstream label ensure task failed to join: {e}");
        }
    });
}

/// The `ws/<name>` label name for a session.
fn label_name_for(session_name: &str) -> String {
    format!("ws/{session_name}")
}

/// Read `remote.origin.url` from `dir` via `git config`, best-effort.
///
/// Why: the fallback path when no `repo_url` was threaded through the spawn
/// (mirrors [`trusty_common::github_path::derive_github_path`]'s own
/// `git config --get remote.origin.url` call, but this module needs the RAW
/// URL string first so it can apply its own github.com-host check before
/// parsing owner/repo).
/// What: `None` on any git failure (not a repo, no origin, git absent) or an
/// empty result.
fn origin_url_from_workspace(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// Parse a git remote URL into an `owner/repo` pair, but ONLY when its host is
/// `github.com` — unlike [`trusty_common::github_path::parse_github_path`],
/// which accepts any host.
///
/// Why: the PM-brief convention and `gh label create` both assume a
/// `github.com` repo; a GitLab/Bitbucket/self-hosted origin must skip
/// cleanly rather than have `gh` (silently pointed at the wrong host by its
/// own defaults) fail confusingly.
/// What: extracts the host from either SSH scp-syntax
/// (`git@github.com:owner/repo.git`) or a URL
/// (`https://github.com/owner/repo`, `ssh://git@github.com/owner/repo`),
/// case-insensitively compares it to `github.com`, and on a match delegates
/// owner/repo extraction to [`trusty_common::github_path::parse_github_path`].
/// Returns `None` for any other host or an unparseable input.
/// Test: `owner_repo_from_ssh_origin`, `owner_repo_from_https_origin`,
/// `owner_repo_from_https_origin_without_dot_git`,
/// `owner_repo_rejects_non_github_host`, `owner_repo_rejects_empty`.
fn owner_repo_from_origin(origin: &str) -> Option<GithubPath> {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_scheme = trimmed
        .find("://")
        .map(|i| &trimmed[i + 3..])
        .unwrap_or(trimmed);
    let host_source = after_scheme
        .find('@')
        .map(|i| &after_scheme[i + 1..])
        .unwrap_or(after_scheme);
    let colon = host_source.find(':');
    let slash = host_source.find('/');
    let host = match (colon, slash) {
        (Some(c), s) if s.is_none_or(|s| c < s) => &host_source[..c],
        (_, Some(s)) => &host_source[..s],
        _ => host_source,
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    trusty_common::github_path::parse_github_path(trimmed)
}

/// Derive a stable, name-specific 6-hex label color.
///
/// Scheme: FNV-1a (64-bit, chosen over `std`'s `DefaultHasher` because its
/// algorithm is explicitly not guaranteed stable across Rust versions — this
/// color must stay identical for a given name across every future rebuild)
/// hashes `session_name`; the hash mod 360 becomes an HSL hue, with fixed
/// saturation (55%) and lightness (45%) chosen to stay readable as GitHub
/// label text on both light and dark repo themes. The hue spread means
/// distinct workstream names get visually distinct, deterministic colors.
/// Test: `label_color_is_stable_and_valid_hex`,
/// `label_color_differs_across_names`.
fn label_color_for(session_name: &str) -> String {
    let hue = (fnv1a_hash(session_name) % 360) as f64;
    hsl_to_hex(hue, 0.55, 0.45)
}

/// FNV-1a 64-bit hash — deterministic across Rust versions/platforms, unlike
/// `std::collections::hash_map::DefaultHasher`.
fn fnv1a_hash(input: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Convert an HSL color (`h` in `[0, 360)`, `s`/`l` in `[0, 1]`) to an
/// uppercase 6-hex RGB string with no leading `#` (matching `gh label
/// create --color`'s expected form).
fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h_prime as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_byte = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("{:02X}{:02X}{:02X}", to_byte(r1), to_byte(g1), to_byte(b1))
}

/// A seam for the one `gh` invocation this module makes, so tests can script
/// the outcome instead of shelling out to a live `gh`.
///
/// Why: mirrors the `CommandRunner` seam `tm ticket`/`watch` use
/// (`bin/tm/commands/ticket/runner.rs`) — that trait lives in the `tm` binary
/// crate and is not reachable from this library module, so this is a small,
/// purpose-built equivalent scoped to the one call this module needs.
/// What: `create_label` returns whether the `gh` call succeeded; callers never
/// see the raw stdout/stderr since label-create has nothing worth surfacing on
/// success and every failure is already a non-fatal skip.
/// Test: driven by `FakeGhRunner` in this module's tests.
trait GhLabelRunner {
    /// Run `gh label create <name> --repo <repo> --color <color> --description
    /// <description> --force`, returning whether it succeeded.
    fn create_label(&self, repo: &str, name: &str, color: &str, description: &str) -> bool;
}

/// Production [`GhLabelRunner`] — spawns real `gh`, bounded by
/// [`GH_LABEL_TIMEOUT`].
struct RealGhRunner;

impl GhLabelRunner for RealGhRunner {
    fn create_label(&self, repo: &str, name: &str, color: &str, description: &str) -> bool {
        let repo = repo.to_string();
        let name = name.to_string();
        let color = color.to_string();
        let description = description.to_string();
        run_bounded(GH_LABEL_TIMEOUT, move || {
            std::process::Command::new("gh")
                .args([
                    "label",
                    "create",
                    &name,
                    "--repo",
                    &repo,
                    "--color",
                    &color,
                    "--description",
                    &description,
                    "--force",
                ])
                .output()
                .ok()
                .map(|out| out.status.success())
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ── owner/repo host-scoped derivation ───────────────────────────────

    #[test]
    fn owner_repo_from_ssh_origin() {
        let gp = owner_repo_from_origin("git@github.com:bobmatnyc/trusty-tools.git")
            .expect("github ssh origin parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_from_https_origin() {
        let gp = owner_repo_from_origin("https://github.com/bobmatnyc/trusty-tools.git")
            .expect("github https origin parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_from_https_origin_without_dot_git() {
        let gp = owner_repo_from_origin("https://github.com/bobmatnyc/trusty-tools")
            .expect("github https origin without .git parses");
        assert_eq!(gp.owner, "bobmatnyc");
        assert_eq!(gp.repo, "trusty-tools");
    }

    #[test]
    fn owner_repo_rejects_non_github_host() {
        assert!(owner_repo_from_origin("git@gitlab.com:acme/widget.git").is_none());
        assert!(owner_repo_from_origin("https://bitbucket.org/acme/widget.git").is_none());
    }

    #[test]
    fn owner_repo_rejects_empty() {
        assert!(owner_repo_from_origin("").is_none());
        assert!(owner_repo_from_origin("   ").is_none());
    }

    // ── color derivation ────────────────────────────────────────────────

    #[test]
    fn label_color_is_stable_and_valid_hex() {
        let a = label_color_for("tm-tcode-01");
        let b = label_color_for("tm-tcode-01");
        assert_eq!(a, b, "same name must always derive the same color");
        assert_eq!(a.len(), 6);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, a.to_ascii_uppercase(), "color must be uppercase hex");
    }

    #[test]
    fn label_color_differs_across_names() {
        let a = label_color_for("tm-tcode-01");
        let b = label_color_for("tm-dogfood-relaunch-01");
        assert_ne!(a, b, "distinct names should (overwhelmingly likely) differ");
    }

    // ── ensure_workstream_label skip-cleanly paths ──────────────────────

    struct FakeGhRunner {
        succeed: bool,
        calls: RefCell<Vec<(String, String, String, String)>>,
    }

    impl FakeGhRunner {
        fn new(succeed: bool) -> Self {
            Self {
                succeed,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl GhLabelRunner for FakeGhRunner {
        fn create_label(&self, repo: &str, name: &str, color: &str, description: &str) -> bool {
            self.calls.borrow_mut().push((
                repo.to_string(),
                name.to_string(),
                color.to_string(),
                description.to_string(),
            ));
            self.succeed
        }
    }

    #[test]
    fn ensure_skips_when_name_blank() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "   ",
        );
        assert_eq!(outcome, LabelOutcome::SkippedBlankName);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_skips_when_no_origin() {
        let runner = FakeGhRunner::new(true);
        // A path that is (almost certainly) not inside any git repo and no
        // repo_url supplied.
        let outcome = ensure_workstream_label_with(&runner, None, Path::new("/"), "tm-tcode-01");
        assert_eq!(outcome, LabelOutcome::SkippedNoOrigin);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_skips_when_non_github() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("git@gitlab.com:acme/widget.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::SkippedNonGithub);
        assert!(runner.calls.borrow().is_empty(), "gh must not be invoked");
    }

    #[test]
    fn ensure_creates_label_via_runner() {
        let runner = FakeGhRunner::new(true);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::Ensured);
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        let (repo, name, color, description) = &calls[0];
        assert_eq!(repo, "bobmatnyc/trusty-tools");
        assert_eq!(name, "ws/tm-tcode-01");
        assert_eq!(color.len(), 6);
        assert_eq!(description, "trusty-mpm workstream tm-tcode-01");
    }

    #[test]
    fn ensure_reports_runner_failure() {
        let runner = FakeGhRunner::new(false);
        let outcome = ensure_workstream_label_with(
            &runner,
            Some("https://github.com/bobmatnyc/trusty-tools.git"),
            Path::new("/nonexistent"),
            "tm-tcode-01",
        );
        assert_eq!(outcome, LabelOutcome::GhFailed);
    }
}
