//! Detecting a collection artifact stamped with an older `trusty-audit` than
//! the one assembling the package (#7133).
//!
//! Why: a multi-day audit engagement's collection step (`crate::run::run_one`)
//! runs under whatever `trusty-audit` was installed when it started. The
//! package step stamps `generated_by` with ITS OWN version
//! (`crate::package::generated`), and nothing compared the two — so a client
//! self-reporting a fixed version could still ship a manifest a bug fixed in
//! that very version never touched, because the manifest was collected before
//! the fix landed. See #6783's regression comment for the reproduced case: a
//! package self-reported `trusty-audit 0.14.0` while 57 of 59 repositories
//! still carried the pre-fix gap wording.
//!
//! What: [`detect`] compares each audited repository's
//! [`crate::run::RepoRun::collected_by_version`] against `env!("CARGO_PKG_VERSION")`
//! and returns one [`StaleArtifact`] per repository that is older, or that
//! never recorded a version at all (a legacy artifact, from before this field
//! existed). Equal and NEWER-than-running are both silent: this function
//! answers "is the collected data missing a fix the running version has",
//! never "does every artifact match exactly."
//!
//! This is a warning, not a refusal (closure condition 1 of #7133) — the
//! repository still packages. [`StaleArtifact::line`] is the one string that
//! reaches both `package.toml`'s `stale_artifacts` array (closure condition 2)
//! and the CLI's own console output, so the two cannot disagree.
//!
//! Test: `stale_artifacts_tests`.

use semver::Version;

use crate::run::RepoRun;

/// One repository whose collection artifact is stale against the running
/// version, or whose collection version cannot be confirmed at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaleArtifact {
    repo: String,
    collected_version: Option<String>,
    running_version: String,
}

impl StaleArtifact {
    /// The line printed on the console and written into `package.toml`.
    ///
    /// # Postconditions
    /// Names the repository, the running version, and — when known — the
    /// collected version, so a recipient can decide whether the gap between
    /// them matters without opening the manifest themselves.
    pub(super) fn line(&self) -> String {
        match &self.collected_version {
            Some(collected) => format!(
                "{}: collected by trusty-audit {collected}, this package is trusty-audit {} — \
                 re-collect to pick up any fixes since {collected}",
                self.repo, self.running_version
            ),
            None => format!(
                "{}: collected by a trusty-audit version this record does not name (a legacy \
                 artifact, from before this client tracked one), this package is trusty-audit \
                 {} — re-collect to confirm it reflects the current tool",
                self.repo, self.running_version
            ),
        }
    }
}

/// Compare every audited repository's recorded collection version against the
/// version assembling this package.
///
/// # Postconditions
/// One entry per repository in `audited` whose `collected_by_version` is
/// strictly older than the running version, or absent. A repository collected
/// at the same version, or at a newer one, is never returned.
pub(super) fn detect(audited: &[&RepoRun]) -> Vec<StaleArtifact> {
    let running_version = env!("CARGO_PKG_VERSION");
    // #7133: this crate's own Cargo.toml version, always valid semver — the
    // `unwrap_or` arm exists so a future parse change elsewhere cannot panic
    // the package step, not because it is reachable today.
    let running = Version::parse(running_version).unwrap_or_else(|_| Version::new(0, 0, 0));
    audited
        .iter()
        .filter(|run| is_stale(run.collected_by_version.as_deref(), &running))
        .map(|run| StaleArtifact {
            repo: run.repo.name.clone(),
            collected_version: run.collected_by_version.clone(),
            running_version: running_version.to_owned(),
        })
        .collect()
}

/// Whether a recorded collection version is older than `running`, or absent.
///
/// An unparsable version string is one this client cannot vouch for — treated
/// the same as absent, since neither can be confirmed current.
fn is_stale(collected: Option<&str>, running: &Version) -> bool {
    match collected.map(Version::parse) {
        None => true,
        Some(Ok(collected)) => collected < *running,
        Some(Err(_)) => true,
    }
}

#[cfg(test)]
mod stale_artifacts_tests {
    use super::*;
    use crate::run::SelectedRepo;

    fn repo(name: &str, version: Option<&str>) -> RepoRun {
        RepoRun {
            repo: SelectedRepo {
                name: name.to_owned(),
                path: name.into(),
                github_slug: None,
                github_absent: None,
            },
            output: "/o".into(),
            log: "/l.log".into(),
            gaps: Vec::new(),
            resumed: false,
            duration_ms: None,
            finished_at: None,
            collected_by_version: version.map(str::to_owned),
            result: crate::run::RepoResult::Succeeded,
        }
    }

    /// The comparison matrix #7133 asks for: equal, older patch, older minor,
    /// a missing version field (a legacy artifact), and newer-than-running.
    #[test]
    fn compares_every_case_in_the_matrix() {
        let running = Version::parse("0.14.4").expect("valid semver");
        assert!(!is_stale(Some("0.14.4"), &running), "equal is not stale");
        assert!(is_stale(Some("0.14.3"), &running), "older patch is stale");
        assert!(is_stale(Some("0.13.9"), &running), "older minor is stale");
        assert!(
            is_stale(None, &running),
            "a missing version is a legacy artifact"
        );
        assert!(!is_stale(Some("0.15.0"), &running), "newer is not stale");
    }

    /// An unparsable version string cannot be vouched for, so it is treated as
    /// unknown rather than trusted at face value.
    #[test]
    fn an_unparsable_version_is_treated_as_unknown() {
        let running = Version::parse("0.14.4").expect("valid semver");
        assert!(is_stale(Some("not-a-version"), &running));
    }

    /// `detect` reports the stale and the legacy repository, in order, and
    /// says nothing about the one collected at the running version.
    #[test]
    fn detect_reports_stale_and_legacy_but_not_current() {
        let running_version = env!("CARGO_PKG_VERSION");
        let stale = repo("acme-legacy-collector", Some("0.13.0"));
        let legacy = repo("acme-untracked", None);
        let current = repo("acme-current", Some(running_version));
        let audited = vec![&stale, &legacy, &current];

        let found = detect(&audited);
        assert_eq!(
            found.iter().map(|a| a.repo.as_str()).collect::<Vec<_>>(),
            vec!["acme-legacy-collector", "acme-untracked"],
            "{found:?}"
        );
        assert_eq!(
            found[0].line(),
            format!(
                "acme-legacy-collector: collected by trusty-audit 0.13.0, this package is \
                 trusty-audit {running_version} — re-collect to pick up any fixes since 0.13.0"
            )
        );
        assert_eq!(
            found[1].line(),
            format!(
                "acme-untracked: collected by a trusty-audit version this record does not name \
                 (a legacy artifact, from before this client tracked one), this package is \
                 trusty-audit {running_version} — re-collect to confirm it reflects the current \
                 tool"
            )
        );
    }
}
