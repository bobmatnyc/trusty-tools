//! Detecting a home directory a user changed from outside (#4325).
//!
//! Why: #4325's "Design Rationale: Visibility and Resilience" makes external
//! modification EXPECTED, not an error condition — users will edit, move,
//! rename and delete inside a home that is deliberately browsable. The system's
//! two obligations there are asymmetric and this module implements the first:
//! it is NOT required to auto-migrate or relocate anything, but it IS required
//! to DETECT missing or malformed files. The second obligation — the concierge
//! surfacing the problem CONVERSATIONALLY and walking the user through the fix
//! — needs a narration seam that #4320 owns; what it needs FROM here is a
//! structured finding with a reason and a remedy, never a raw error dump.
//!
//! What: [`inspect`] walks the five entries [`super::home`] defines and returns
//! a [`HomeHealth`] — zero or more [`HomeIssue`]s, each naming the entry, the
//! path, WHAT is wrong ([`HomeIssueKind`]) and WHAT WOULD FIX IT. Inspection
//! never fails and never repairs: an unreadable file is a finding, not an
//! `Err`, because a caller that only wanted to report must not be blocked by
//! the very condition it was reporting on. A missing home short-circuits to a
//! single finding rather than five, since "you have no home directory" and
//! "your okg directory is missing" are the same fact told once or five times.
//!
//! Test: `super::tests::health_tests` — the whole module.

use std::fmt;
use std::path::{Path, PathBuf};

use super::error::AssistantError;
use super::home::{
    AGENTS_DIR, ATTACHMENTS_DIR, AssistantHome, AssistantHomeConfig, CONFIG_FILE,
    INSTRUCTIONS_FILE, OKG_DIR, STORES_DIR,
};
use super::project::{PROJECT_DIR_KEY, canonical_project_dir, duplicate_claims, project_claims};

/// What is wrong with one entry of an assistant's home.
///
/// Why: the concierge phrases "it is gone" differently from "it is there but I
/// cannot parse it"; a single "broken" verdict would collapse that distinction
/// and force the narration layer to sniff strings.
/// What: the five conditions [`inspect`] can distinguish on a user-owned tree.
/// Test: `super::tests::health_tests` — the whole module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeIssueKind {
    /// The whole home directory is absent.
    HomeMissing,
    /// One expected entry inside an existing home is absent.
    Missing,
    /// An entry that must be a directory is something else (usually a file).
    NotADirectory,
    /// An entry that must be a file is something else (usually a directory).
    NotAFile,
    /// The entry exists but could not be read (permissions, a broken symlink).
    Unreadable,
    /// The entry was read but its content is not what it must be.
    Malformed,
    /// #4325: the app tried to create the entry at startup and could not
    /// (read-only filesystem, permission denied, no space, a file sitting where
    /// a directory belongs). Startup continues regardless — see
    /// [`super::provision`](mod@crate::assistants::provision).
    NotCreatable,
    /// #11649: two homes claim the same project directory.
    Conflict,
}

impl HomeIssueKind {
    /// A short human phrase for this condition.
    ///
    /// Test: `super::tests::health_tests::narration_names_every_issue`.
    pub fn describe(self) -> &'static str {
        match self {
            Self::HomeMissing => "the home directory is missing",
            Self::Missing => "missing",
            Self::NotADirectory => "not a directory",
            Self::NotAFile => "not a file",
            Self::Unreadable => "unreadable",
            Self::Malformed => "malformed",
            Self::NotCreatable => "could not be created",
            Self::Conflict => "claimed by two assistants",
        }
    }
}

/// One detected problem with an assistant's home directory.
///
/// Why/What/Test: see this module's doc comment. `remedy` is the whole point of
/// the struct — a finding a user cannot act on is the raw error dump #4325
/// (via #4320) rules out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeIssue {
    /// What is wrong.
    pub kind: HomeIssueKind,
    /// The layout entry this concerns (`okg`, `config.toml`, …), or `""` for
    /// the home itself.
    pub entry: &'static str,
    /// The path the finding is about.
    pub path: PathBuf,
    /// The specific reason, phrased for a person.
    pub detail: String,
    /// What would fix it, phrased as an action the user can take.
    pub remedy: String,
}

impl fmt::Display for HomeIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} — {}. {}",
            self.path.display(),
            self.detail,
            self.remedy
        )
    }
}

/// The result of inspecting one assistant's home directory.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeHealth {
    /// The home that was inspected.
    pub home: PathBuf,
    /// Every problem found, in layout order. Empty means healthy.
    pub issues: Vec<HomeIssue>,
}

impl HomeHealth {
    /// Whether the home is intact.
    ///
    /// Test: `super::tests::health_tests::a_freshly_ensured_home_is_healthy`.
    pub fn is_healthy(&self) -> bool {
        self.issues.is_empty()
    }

    /// The findings as text a concierge can say out loud.
    ///
    /// Why: this is the seam #4325 places here — the narration layer (#4320)
    /// consumes a REASON plus a REMEDY, so it never has to render a stack of
    /// `Debug` output at a user. It is a plain rendering, not a decision: the
    /// concierge is free to rephrase it.
    /// What: `None` when healthy; otherwise one line per finding, each already
    /// carrying its own remedy.
    /// Test: `super::tests::health_tests::narration_names_every_issue`,
    /// `super::tests::health_tests::a_freshly_ensured_home_is_healthy`.
    pub fn narration(&self) -> Option<String> {
        if self.is_healthy() {
            return None;
        }
        let mut out = format!(
            "The assistant home at {} needs attention:",
            self.home.display()
        );
        for issue in &self.issues {
            out.push_str("\n- ");
            out.push_str(&issue.to_string());
        }
        Some(out)
    }
}

/// Inspect an assistant's home for missing or malformed entries.
///
/// Why/What/Test: see this module's doc comment.
pub fn inspect(home: &AssistantHome) -> HomeHealth {
    let root = home.path().to_path_buf();
    if !root.is_dir() {
        let kind = if root.exists() {
            HomeIssueKind::NotADirectory
        } else {
            HomeIssueKind::HomeMissing
        };
        return HomeHealth {
            issues: vec![HomeIssue {
                kind,
                entry: "",
                path: root.clone(),
                detail: kind.describe().to_string(),
                remedy: format!(
                    "recreate it (the app regenerates the standard layout), or move the \
                     `{}` directory back if you relocated it",
                    home.id()
                ),
            }],
            home: root,
        };
    }

    let mut issues = Vec::new();
    check_instructions(home, &mut issues);
    check_config(home, &mut issues);
    for (entry, path) in [
        (AGENTS_DIR, home.agents_dir()),
        (OKG_DIR, home.okg_dir()),
        (ATTACHMENTS_DIR, home.attachments_dir()),
        // #4325: `stores/` gets the same detection as every other layout
        // directory (owner, 2026-08-01).
        (STORES_DIR, home.stores_dir()),
    ] {
        check_dir(entry, &path, &mut issues);
    }
    HomeHealth { home: root, issues }
}

/// `instructions.md` must exist, be a file, and carry something.
fn check_instructions(home: &AssistantHome, issues: &mut Vec<HomeIssue>) {
    let path = home.instructions_path();
    if let Some(issue) = missing_or_wrong_kind(INSTRUCTIONS_FILE, &path, Kind::File) {
        issues.push(issue);
        return;
    }
    match std::fs::read_to_string(&path) {
        Err(err) => issues.push(HomeIssue {
            kind: HomeIssueKind::Unreadable,
            entry: INSTRUCTIONS_FILE,
            path,
            detail: format!("could not be read ({err})"),
            remedy: "check its permissions, or replace it with a readable text file".to_string(),
        }),
        Ok(body) if body.trim().is_empty() => issues.push(HomeIssue {
            kind: HomeIssueKind::Malformed,
            entry: INSTRUCTIONS_FILE,
            path,
            detail: "is empty, so this assistant has no instructions of its own".to_string(),
            remedy: "write what you want this assistant to do, or delete the file to have \
                     the standard one regenerated"
                .to_string(),
        }),
        Ok(_) => {}
    }
}

/// `config.toml` must exist, be a file, and parse as TOML.
fn check_config(home: &AssistantHome, issues: &mut Vec<HomeIssue>) {
    let path = home.config_path();
    if let Some(issue) = missing_or_wrong_kind(CONFIG_FILE, &path, Kind::File) {
        issues.push(issue);
        return;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            issues.push(HomeIssue {
                kind: HomeIssueKind::Unreadable,
                entry: CONFIG_FILE,
                path,
                detail: format!("could not be read ({err})"),
                remedy: "check its permissions, or replace it with a readable TOML file"
                    .to_string(),
            });
            return;
        }
    };
    let config = match toml::from_str::<AssistantHomeConfig>(&raw) {
        Ok(config) => config,
        Err(err) => {
            issues.push(malformed_config(path, &err));
            return;
        }
    };
    if let Some(declared) = config.project_dir {
        check_project_dir(&declared, issues);
    }
}

/// #11649: a declared `project_dir` must still be an absolute directory.
/// Only the path is reported; no other config value reaches the finding.
fn check_project_dir(declared: &Path, issues: &mut Vec<HomeIssue>) {
    let Err(err) = canonical_project_dir(declared) else {
        return;
    };
    let kind = if declared.is_absolute() && !declared.exists() {
        HomeIssueKind::Missing
    } else {
        HomeIssueKind::Malformed
    };
    let reason = match err {
        AssistantError::InvalidProjectDir { reason, .. } => reason,
        other => other.to_string(),
    };
    issues.push(HomeIssue {
        kind,
        entry: PROJECT_DIR_KEY,
        path: declared.to_path_buf(),
        detail: format!("`{PROJECT_DIR_KEY}` is not usable: {reason}"),
        remedy: format!(
            "restore the directory, or set `{PROJECT_DIR_KEY}` in config.toml to the \
             absolute path of an existing directory"
        ),
    });
}

/// Every pair of homes under `root` that claim the same project directory.
///
/// Why: a duplicate claim makes project resolution refuse to pick an instance
/// (#11649), so the user must hear which two homes collide. Per-home
/// [`inspect`] cannot see it.
/// What: one [`HomeIssueKind::Conflict`] per duplicate pair, naming both
/// instance ids and the directory; a config that cannot be read becomes one
/// [`HomeIssueKind::Malformed`] finding carrying the parser position only.
/// Test: `super::tests::health_tests::flags_a_duplicate_project_dir`.
pub fn inspect_project_claims(root: &Path) -> Vec<HomeIssue> {
    let claims = match project_claims(root) {
        Ok(claims) => claims,
        Err(err) => {
            let path = match &err {
                AssistantError::InvalidConfig { path, .. } => path.clone(),
                _ => root.to_path_buf(),
            };
            return vec![HomeIssue {
                kind: HomeIssueKind::Malformed,
                entry: CONFIG_FILE,
                path,
                detail: err.to_string(),
                remedy: "fix that config.toml so project directories can be compared".into(),
            }];
        }
    };
    duplicate_claims(&claims)
        .into_iter()
        .map(|(dir, first, second)| HomeIssue {
            kind: HomeIssueKind::Conflict,
            entry: PROJECT_DIR_KEY,
            detail: format!("assistants `{first}` and `{second}` both claim it"),
            remedy: format!("remove `{PROJECT_DIR_KEY}` from one of their config.toml files"),
            path: dir,
        })
        .collect()
}

/// The finding for a `config.toml` that does not parse.
fn malformed_config(path: PathBuf, err: &toml::de::Error) -> HomeIssue {
    HomeIssue {
        kind: HomeIssueKind::Malformed,
        entry: CONFIG_FILE,
        path,
        detail: format!("is not valid TOML ({})", first_line(&err.to_string())),
        remedy: "fix the line the error names, or delete the file to have the standard \
                 one regenerated"
            .to_string(),
    }
}

/// One of the three layout directories must exist and be a directory.
fn check_dir(entry: &'static str, path: &Path, issues: &mut Vec<HomeIssue>) {
    if let Some(issue) = missing_or_wrong_kind(entry, path, Kind::Dir) {
        issues.push(issue);
    }
}

/// Whether an entry is expected to be a file or a directory.
#[derive(Clone, Copy)]
enum Kind {
    File,
    Dir,
}

/// The shared "absent, or present as the wrong thing" check.
fn missing_or_wrong_kind(entry: &'static str, path: &Path, kind: Kind) -> Option<HomeIssue> {
    let (ok, wrong_kind, noun) = match kind {
        Kind::File => (path.is_file(), HomeIssueKind::NotAFile, "file"),
        Kind::Dir => (path.is_dir(), HomeIssueKind::NotADirectory, "directory"),
    };
    if ok {
        return None;
    }
    if path.exists() {
        return Some(HomeIssue {
            kind: wrong_kind,
            entry,
            path: path.to_path_buf(),
            detail: format!("exists but is not a {noun}"),
            remedy: format!(
                "move whatever is there aside so a `{entry}` {noun} can take its place"
            ),
        });
    }
    Some(HomeIssue {
        kind: HomeIssueKind::Missing,
        entry,
        path: path.to_path_buf(),
        detail: format!("`{entry}` is missing"),
        remedy: format!(
            "restore it if you moved it, or let the app regenerate the standard `{entry}` {noun}"
        ),
    })
}

/// The first line of a multi-line parser message, for a one-line finding.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or(message).trim()
}
