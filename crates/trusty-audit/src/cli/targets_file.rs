//! `repos.txt` and `boards.txt` — the engagement's targets as a list (#5978).
//!
//! Why: registering thirty repositories at the prompt is thirty prompts, and a
//! typo is a repository silently absent from the assessment. An operator who
//! already has the list wants to hand it over, not retype it.
//!
//! What: [`detect`] reads the two files beside `engagement.toml` and hands back
//! the short-form specs they name. Every line is normalized — a browser URL, a
//! `.git` suffix, a Linear issue link — and then validated through
//! [`registry::parse`], the crate's one decision about what a target may be.
//! Nothing here registers anything; [`super::registration::adopt`] does that
//! through [`Command::AddTarget`](crate::session::Command::AddTarget), so a
//! file-detected target is validated and persisted exactly like a typed one.
//!
//! Two properties define it:
//!
//! - **All-or-nothing per read.** One unparseable line registers NOTHING, from
//!   either file. The alternative is an audit that covers fewer repositories
//!   than the file lists and reports success over the absent ones. Every bad
//!   line is named with its number and its own reason, so one run fixes them
//!   all.
//! - **The files live beside the config, not in the working directory.** The
//!   working directory is the client's managed storage for clones, tool
//!   binaries and logs; `engagement.toml`'s own directory is where the operator
//!   puts things (`config.rs`, `EngagementConfig::resolve_path`).
//!
//! Test: `super::targets_file_tests`.

use std::path::{Path, PathBuf};

use crate::error::{AuditError, BadLine, BadLines, UnreadableFile, UnreadableFiles};
use crate::registry::{self, TargetKind};

/// The repository list, read from `engagement.toml`'s own directory.
pub const REPOS_FILE: &str = "repos.txt";

/// The board list, read from the same directory.
pub const BOARDS_FILE: &str = "boards.txt";

/// The shape a repository line may take, quoted back on a refusal.
const REPO_SHAPE: &str = "expected owner/repo, an absolute path to a checkout on this machine, \
                          or a https://github.com/owner/repo URL";

/// What a repository URL that is not GitHub's is told.
const NOT_GITHUB: &str = "only github.com repository URLs are read — expected \
                          https://github.com/owner/repo";

/// What a github.com URL naming one of GitHub's own pages is told.
///
/// It names the fix rather than the shape: someone who pasted an organization
/// URL meant "every repository in this org", and what they need to hear is that
/// the file takes one repository per line.
const NOT_A_REPOSITORY_PAGE: &str = "that is one of github.com's own pages, not a repository — \
                                     /orgs/, /users/ and /topics/ are GitHub's paths, not an \
                                     owner's. List each repository on its own line as owner/repo";

/// First path segments github.com reserves for itself.
///
/// #5990: `github_path` accepted any two-segment path, so
/// `https://github.com/orgs/acme/repositories` normalized to the target
/// `orgs/acme` — and the operator got a refusal naming a repository they never
/// listed. None of these can be an account name on github.com, so refusing them
/// cannot cost a real repository.
const RESERVED_SEGMENTS: &[&str] = &[
    "about",
    "account",
    "apps",
    "codespaces",
    "collections",
    "contact",
    "dashboard",
    "enterprise",
    "events",
    "explore",
    "features",
    "issues",
    "join",
    "login",
    "logout",
    "marketplace",
    "new",
    "notifications",
    "organizations",
    "orgs",
    "pricing",
    "pulls",
    "search",
    "security",
    "settings",
    "sponsors",
    "stars",
    "topics",
    "trending",
    "users",
    "watching",
];

/// The shape a board line may take.
const BOARD_SHAPE: &str = "expected jira:KEY, linear:TEAM, or a Jira or Linear board URL";

/// What a Linear URL that names no team is told.
///
/// It says the workspace slug is not the key because that is the mistake the
/// shape invites: `https://linear.app/acme/team/ENG/active` reads left to right
/// as though `acme` were the identifier.
const LINEAR_SHAPE: &str = "a Linear URL must name a team \
                            (https://linear.app/<workspace>/team/<TEAM-KEY>/…) or an issue \
                            (…/issue/<TEAM-KEY>-123/…) — the workspace slug is not the team key";

/// What a Jira URL that names no project is told.
const JIRA_SHAPE: &str = "a Jira URL must name a project \
                          (https://<site>.atlassian.net/browse/<PROJECT>-123, or \
                          …/projects/<PROJECT>/…)";

/// The targets one or both files name, in the order they were read.
///
/// Why: a value rather than a registration, so the parse and the registration
/// are separately assertable — and so a parse failure cannot have written
/// anything by the time it is reported.
/// What: `specs` are canonical short forms (`owner/repo`, `provider:KEY`), each
/// already accepted by [`registry::parse`]. `sources` is what was read, for the
/// message that tells an operator where the list came from.
/// Test: `super::targets_file_tests::a_url_and_a_short_form_reach_the_same_spec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// Canonical specs, repositories first, then boards.
    pub specs: Vec<String>,
    /// The files these came from.
    pub sources: Vec<PathBuf>,
}

impl Detected {
    /// The files, as a comma-separated list for an operator-facing line.
    pub fn named(&self) -> String {
        self.sources
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Read whichever targets files sit beside `config_path`.
///
/// Why: the presence of either file is what decides that the per-target prompt
/// loop is skipped, so the answer is `Option` rather than an empty list — a
/// `repos.txt` holding only comments is still a declaration that the operator
/// supplied the list, and re-prompting them for one would be the behaviour this
/// issue removes.
/// What: `None` when neither file exists. Both files are read before any
/// verdict, so one run names every bad line in both.
///
/// #5993: a file that cannot be READ is collected the same way a bad line is,
/// rather than returning immediately. An unreadable `repos.txt` used to end the
/// read before `boards.txt` was opened, so the operator fixed the permissions,
/// re-ran, and only then learned about the bad lines in the other file. One
/// problem still reports the shape it always did — a single unreadable file and
/// no bad lines is [`AuditError::Read`], with the OS error kept as its `source`.
/// Anything else is one [`AuditError::TargetsFileRefused`] carrying both lists.
/// Test: `super::targets_file_tests::neither_file_present_is_no_detection`,
/// `super::targets_file_tests::one_bad_line_refuses_the_whole_read`,
/// `super::targets_file_tests::an_unreadable_file_does_not_hide_the_other_files_bad_lines`.
///
/// # Errors
///
/// [`AuditError::Read`] when one file exists, cannot be read, and is the only
/// problem; [`AuditError::TargetsFileRefused`] when any line in either file is
/// not a target, or when more than one file could not be read — in which case
/// nothing from either file is returned.
pub fn detect(config_path: &Path) -> Result<Option<Detected>, AuditError> {
    let dir = config_path.parent().unwrap_or(Path::new("."));
    let mut specs = Vec::new();
    let mut sources = Vec::new();
    let mut bad = Vec::new();
    let mut unreadable: Vec<Unreadable> = Vec::new();

    for (file, kind) in [
        (REPOS_FILE, TargetKind::Repo),
        (BOARDS_FILE, TargetKind::Board),
    ] {
        let path = dir.join(file);
        // #5993: collected, never returned from here — the other file's
        // diagnostics are the thing an early return was throwing away.
        let text = match read_if_present(file, &path) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(failure) => {
                unreadable.push(failure);
                continue;
            }
        };
        sources.push(path);
        for (offset, line) in text.lines().enumerate() {
            let Some(entry) = meaningful(line) else {
                continue;
            };
            match spec_of(kind, entry) {
                Ok(spec) => specs.push(spec),
                Err(reason) => bad.push(BadLine {
                    file: file.to_owned(),
                    line: offset + 1,
                    entry: entry.to_owned(),
                    reason,
                }),
            }
        }
    }

    match (bad.is_empty(), unreadable.len()) {
        // Nothing wrong.
        (true, 0) => {}
        // One unreadable file and nothing else: the shape this has always had,
        // keeping the OS error as the refusal's `source`.
        (true, 1) => return Err(unreadable.swap_remove(0).into_read_error()),
        // Bad lines, or two unreadable files — one refusal naming everything.
        _ => {
            return Err(AuditError::TargetsFileRefused {
                bad: BadLines(bad),
                unreadable: UnreadableFiles(unreadable.iter().map(Unreadable::described).collect()),
            });
        }
    }
    if sources.is_empty() {
        return Ok(None);
    }
    Ok(Some(Detected { specs, sources }))
}

/// Read one NAMED file as this engagement's target list (#5483).
///
/// Why: `taudit distribute --repos <file>` pre-populates the package's
/// `engagement.toml` so the recipient audits a list the auditor already has
/// rather than registering twenty repositories at a prompt. That list must be
/// read by the same parser [`detect`] uses, or the handoff would have a second
/// repository-list format with its own normalization and its own bugs. This is
/// that parser, pointed at a path the operator named instead of at a fixed
/// filename beside the config.
///
/// What: two shapes, both already this crate's own. A file that parses as TOML
/// and carries a `targets` array is an `engagement.toml` — its declared targets
/// come back verbatim, so an auditor can hand over the previous engagement's
/// config. Anything else is read as [`REPOS_FILE`]: one `owner/name`, absolute
/// checkout path, or GitHub URL per line, `#` starting a comment, every line
/// normalized and validated through [`registry::parse`].
///
/// All-or-nothing, for the reason [`detect`] is: a list that registers
/// nineteen of twenty repositories produces an audit that reports success over
/// the one it never saw. Every bad line is named with its number and its own
/// reason, so one run fixes them all.
/// Test: `super::targets_file_tests::a_named_repo_list_reads_the_line_format`,
/// `super::targets_file_tests::a_named_repo_list_reads_an_engagement_config`,
/// `super::targets_file_tests::one_bad_line_refuses_a_named_repo_list`.
///
/// # Errors
///
/// [`AuditError::Read`] when the file cannot be opened;
/// [`AuditError::TargetsFileRefused`] when any line is not a repository.
pub fn read_repo_list(path: &Path) -> Result<Vec<registry::Target>, AuditError> {
    let text = std::fs::read_to_string(path)
        .map(without_bom)
        .map_err(|source| AuditError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    // An `engagement.toml` handed over from a previous engagement. Checked
    // first because a repos.txt is not valid TOML, so there is no file both
    // arms could claim.
    if let Ok(table) = toml::from_str::<toml::Table>(&text)
        && let Some(declared) = table.get(crate::config::TARGETS_FIELD)
    {
        return declared
            .clone()
            .try_into::<Vec<registry::Target>>()
            .map_err(|source| AuditError::Parse {
                path: path.to_path_buf(),
                what: "engagement targets",
                source: Box::new(source),
            });
    }

    let name = path.file_name().map_or_else(
        || REPOS_FILE.to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut targets = Vec::new();
    let mut bad = Vec::new();
    for (offset, line) in text.lines().enumerate() {
        let Some(entry) = meaningful(line) else {
            continue;
        };
        match spec_of(TargetKind::Repo, entry) {
            // `spec_of` already proved this parses; re-parsing is how the spec
            // becomes the `Target` the config declares, with no second charset.
            Ok(spec) => match registry::parse(Some(TargetKind::Repo), &spec) {
                Ok(target) => targets.push(target),
                Err(refusal) => bad.push(BadLine {
                    file: name.clone(),
                    line: offset + 1,
                    entry: entry.to_owned(),
                    reason: refusal.to_string(),
                }),
            },
            Err(reason) => bad.push(BadLine {
                file: name.clone(),
                line: offset + 1,
                entry: entry.to_owned(),
                reason,
            }),
        }
    }
    if !bad.is_empty() {
        return Err(AuditError::TargetsFileRefused {
            bad: BadLines(bad),
            unreadable: UnreadableFiles(Vec::new()),
        });
    }
    Ok(targets)
}

/// A targets file that is there and could not be opened (#5993).
///
/// Held rather than raised so [`detect`] can decide, once both files have been
/// tried, whether this is the only problem — and therefore still the plain
/// [`AuditError::Read`] callers have always seen — or one of several.
struct Unreadable {
    /// The file's own name, e.g. `repos.txt`.
    file: &'static str,
    /// Where it was looked for.
    path: PathBuf,
    /// What `read_to_string` failed with.
    source: std::io::Error,
}

impl Unreadable {
    /// This failure as the crate's structured read error, `source` intact.
    fn into_read_error(self) -> AuditError {
        AuditError::Read {
            path: self.path,
            source: self.source,
        }
    }

    /// This failure as a line in a combined refusal.
    fn described(&self) -> UnreadableFile {
        UnreadableFile {
            file: self.file.to_owned(),
            path: self.path.clone(),
            reason: self.source.to_string(),
        }
    }
}

/// A file's text, or `None` when it is not there.
fn read_if_present(file: &'static str, path: &Path) -> Result<Option<String>, Unreadable> {
    match std::fs::read_to_string(path) {
        // #5990: the byte-order mark comes off HERE, once, before any line is
        // looked at — see [`without_bom`].
        Ok(text) => Ok(Some(without_bom(text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Unreadable {
            file,
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The text with a leading U+FEFF removed, and nothing else touched.
///
/// Why: `str::trim` uses `char::is_whitespace`, and U+FEFF is not whitespace, so
/// a byte-order mark survives into the charset check and refuses line 1. Because
/// the parse is all-or-nothing, one invisible byte then blocks every repository
/// in the engagement — and the refusal echoes the entry, which renders identical
/// to a valid one. Notepad, Excel's "Save as .txt" and PowerShell's `Out-File`
/// all emit a BOM by default, so the operator who was told to hand over a list
/// they already have is the one who hits this (#5990).
/// What: the prefix at offset 0 only. A U+FEFF anywhere else is a real character
/// in a malformed entry and is still refused, so this cannot silently repair a
/// line that says something other than what it looks like.
/// Test: `super::targets_file_tests::a_byte_order_mark_does_not_refuse_the_file`.
fn without_bom(text: String) -> String {
    match text.strip_prefix('\u{FEFF}') {
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// The entry on this line, or `None` for a blank line or a comment.
fn meaningful(line: &str) -> Option<&str> {
    let entry = line.trim();
    (!entry.is_empty() && !entry.starts_with('#')).then_some(entry)
}

/// Normalize one line, then prove it is a target the registry accepts.
///
/// The second half is what keeps the all-or-nothing rule honest: a line that
/// normalizes to a plausible-looking spec the registry would later refuse must
/// fail HERE, while nothing has been written, rather than at registration where
/// the rest of the file is already on disk. It is [`registry::parse`] doing the
/// deciding, so there is no second charset in this crate.
fn spec_of(kind: TargetKind, entry: &str) -> Result<String, String> {
    let spec = match kind {
        TargetKind::Repo => repo_spec(entry),
        TargetKind::Board => board_spec(entry),
    }
    .map_err(str::to_owned)?;
    registry::parse(Some(kind), &spec).map_err(|refusal| refusal.to_string())?;
    Ok(spec)
}

/// `owner/repo`, from either the short form or a GitHub URL.
///
/// #5978: the `.git` suffix is stripped on BOTH forms. `clone::split_name`
/// allows `.` in its charset, so `acme/api.git` already parses today — as the
/// literal repository name `api.git`, a target that looks valid and clones
/// nothing.
fn repo_spec(entry: &str) -> Result<String, &'static str> {
    // #6001: an absolute path is a checkout on disk and reaches `registry::parse`
    // verbatim. It must skip every normalization below — `/srv/apex.git` is a
    // bare mirror whose directory really is named `apex.git`, and the owner/name
    // split would make `srv` an owner. The parse stays all-or-nothing either
    // way: a path that is not a repository is refused by `crate::validate` at
    // registration, naming which condition failed.
    if crate::local_repo::is_local_spec(entry) {
        return Ok(entry.to_owned());
    }
    let (path, from_url) = match scheme_stripped(entry) {
        Some(rest) => (github_path(rest)?, true),
        None => (entry, false),
    };
    let (owner, rest) = path
        .trim_end_matches('/')
        .split_once('/')
        .ok_or(REPO_SHAPE)?;
    // A URL carries whatever the operator copied out of the browser after the
    // repository — `/tree/main`, `/pull/12`. The short form carries nothing, so
    // `acme/api/tree/main` typed bare is a mistake rather than a target.
    let name = match rest.split_once('/') {
        Some((name, _)) if from_url => name,
        Some(_) => return Err(REPO_SHAPE),
        None => rest,
    };
    let name = name.strip_suffix(".git").unwrap_or(name);
    if owner.is_empty() || name.is_empty() {
        return Err(REPO_SHAPE);
    }
    Ok(format!("{owner}/{name}"))
}

/// `provider:KEY`, from either the short form or a board URL.
fn board_spec(entry: &str) -> Result<String, &'static str> {
    let Some(rest) = scheme_stripped(entry) else {
        // The short form is already the canonical spelling; `spec_of` proves it.
        return Ok(entry.to_owned());
    };
    let (host, path) = rest
        .trim_end_matches('/')
        .split_once('/')
        .ok_or(BOARD_SHAPE)?;
    let host = host.to_ascii_lowercase();
    if host == "linear.app" {
        return linear_key(path).map(|key| format!("linear:{key}"));
    }
    if host.ends_with(".atlassian.net") {
        return jira_key(path).map(|key| format!("jira:{key}"));
    }
    Err(BOARD_SHAPE)
}

/// Everything after `https://` or `http://`, when the entry is a URL at all.
fn scheme_stripped(entry: &str) -> Option<&str> {
    entry
        .strip_prefix("https://")
        .or_else(|| entry.strip_prefix("http://"))
}

/// The repository path of a GitHub URL, host stripped.
///
/// #5990: a two-segment path is not enough to make it a repository. GitHub's own
/// pages are two-segment paths too, so the owner position is checked against
/// [`RESERVED_SEGMENTS`] before the path is handed back. What must keep working
/// is everything an operator legitimately copies out of the browser bar while
/// looking AT a repository — `/issues/12`, `/blob/main/…`, `/tree/release/2.x` —
/// and those all carry a real owner in the first segment.
fn github_path(rest: &str) -> Result<&str, &'static str> {
    let (host, path) = rest.split_once('/').ok_or(NOT_GITHUB)?;
    match host.to_ascii_lowercase().as_str() {
        "github.com" | "www.github.com" => owned_path(path),
        _ => Err(NOT_GITHUB),
    }
}

/// The path back, unless its first segment is one github.com keeps.
fn owned_path(path: &str) -> Result<&str, &'static str> {
    let owner = path.split('/').next().unwrap_or(path).to_ascii_lowercase();
    if RESERVED_SEGMENTS.contains(&owner.as_str()) {
        return Err(NOT_A_REPOSITORY_PAGE);
    }
    Ok(path)
}

/// The team key in a Linear URL path — never the workspace slug.
fn linear_key(path: &str) -> Result<String, &'static str> {
    let mut parts = path.split('/');
    // The first segment is the workspace slug, which identifies the account
    // rather than the team. It is deliberately read and discarded.
    let (_workspace, marker, segment) = (
        parts.next().ok_or(LINEAR_SHAPE)?,
        parts.next().ok_or(LINEAR_SHAPE)?,
        parts.next().ok_or(LINEAR_SHAPE)?,
    );
    match marker {
        "team" => Ok(segment.to_owned()),
        "issue" => Ok(issue_prefix(segment).to_owned()),
        _ => Err(LINEAR_SHAPE),
    }
}

/// The project key in a Jira URL path.
fn jira_key(path: &str) -> Result<String, &'static str> {
    let parts: Vec<&str> = path.split('/').collect();
    for (index, segment) in parts.iter().enumerate() {
        let next = parts.get(index + 1).copied().filter(|s| !s.is_empty());
        match *segment {
            "browse" => return next.map(|s| issue_prefix(s).to_owned()).ok_or(JIRA_SHAPE),
            "projects" => return next.map(str::to_owned).ok_or(JIRA_SHAPE),
            _ => {}
        }
    }
    Err(JIRA_SHAPE)
}

/// `ENG` from `ENG-123`, and the whole segment when there is no issue number.
fn issue_prefix(segment: &str) -> &str {
    match segment.rsplit_once('-') {
        Some((key, number)) if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) => {
            key
        }
        _ => segment,
    }
}

#[cfg(test)]
mod targets_file_tests {
    use super::*;

    /// #5483: `--repos <file>` reads the same line format `repos.txt` uses,
    /// through the same normalizer — a URL, a `.git` suffix and a short form
    /// all reach the same target, and a comment is skipped.
    #[test]
    fn a_named_repo_list_reads_the_line_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client-repos.txt");
        std::fs::write(
            &path,
            "# the auditor's list\nacme/api\nhttps://github.com/acme/web.git\n\n",
        )
        .expect("write the list");

        let targets = read_repo_list(&path).expect("reads");
        assert_eq!(
            targets.iter().map(registry::Target::id).collect::<Vec<_>>(),
            vec!["acme/api", "acme/web"]
        );
    }

    /// A previous engagement's config is the other shape an auditor has to
    /// hand, and reusing its `[[targets]]` is what keeps this from being a
    /// second repository-list format.
    #[test]
    fn a_named_repo_list_reads_an_engagement_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("previous-engagement.toml");
        std::fs::write(
            &path,
            "openrouter_key = \"\"\ninstructions = \"x\"\n\n\
             [tools]\ntga = \"5.0.3\"\ntrusty-search = \"0.52.0\"\n\
             trusty-analyze = \"0.12.5\"\ntrusty-review = \"0.31.2\"\n\n\
             [[targets]]\nkind = \"repo\"\nname_with_owner = \"acme/api\"\n",
        )
        .expect("write the config");

        let targets = read_repo_list(&path).expect("reads");
        assert_eq!(
            targets.iter().map(registry::Target::id).collect::<Vec<_>>(),
            vec!["acme/api"]
        );
    }

    /// All-or-nothing, for the reason [`detect`] is: a list that registers
    /// nineteen of twenty produces an audit reporting success over the one it
    /// never saw.
    #[test]
    fn one_bad_line_refuses_a_named_repo_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client-repos.txt");
        std::fs::write(&path, "acme/api\nnot a repository\n").expect("write the list");

        let error = read_repo_list(&path).expect_err("one bad line refuses the read");
        let AuditError::TargetsFileRefused { bad, .. } = &error else {
            panic!("{error}");
        };
        assert_eq!(bad.0.len(), 1);
        assert_eq!(bad.0[0].line, 2);
    }

    /// Write `repos.txt` / `boards.txt` beside a config path and read them.
    fn detected(dir: &Path, repos: Option<&str>, boards: Option<&str>) -> Option<Detected> {
        seed(dir, repos, boards);
        detect(&dir.join("engagement.toml")).expect("the files parse")
    }

    fn seed(dir: &Path, repos: Option<&str>, boards: Option<&str>) {
        for (file, body) in [(REPOS_FILE, repos), (BOARDS_FILE, boards)] {
            if let Some(body) = body {
                std::fs::write(dir.join(file), body).expect("write the list");
            }
        }
    }

    fn specs(detected: Option<Detected>) -> Vec<String> {
        detected.expect("a detection").specs
    }

    /// Every accepted repository spelling, short form and URL, reaching the one
    /// canonical `owner/repo`.
    ///
    /// The `.git` cases are the trap #5978 names: `clone::split_name` allows
    /// `.`, so `acme/api.git` parses today as the literal name `api.git` — a
    /// target that looks valid and clones nothing.
    #[test]
    fn every_repository_form_reaches_the_short_spec() {
        for entry in [
            "acme/api",
            "  acme/api  ",
            "acme/api.git",
            "https://github.com/acme/api",
            "http://github.com/acme/api",
            "https://www.github.com/acme/api",
            "https://github.com/acme/api/",
            "https://github.com/acme/api.git",
            "https://github.com/acme/api/tree/main",
            "https://github.com/acme/api/tree/release/2.x",
            "https://github.com/acme/api/pull/12",
        ] {
            assert_eq!(
                spec_of(TargetKind::Repo, entry.trim()),
                Ok("acme/api".to_owned()),
                "{entry} must reach the canonical spec"
            );
        }
    }

    /// Every accepted board spelling. The Linear cases are the ones that matter:
    /// the workspace slug sits BEFORE the team key in the path, so a parser that
    /// takes the first segment registers the account and audits nothing.
    #[test]
    fn every_board_form_reaches_the_short_spec() {
        for (entry, expected) in [
            ("linear:ENG", "linear:ENG"),
            ("https://linear.app/acme/team/ENG/active", "linear:ENG"),
            ("https://linear.app/acme/team/ENG", "linear:ENG"),
            (
                "https://linear.app/acme/issue/ENG-123/fix-the-thing",
                "linear:ENG",
            ),
            ("jira:OPS", "jira:OPS"),
            ("https://acme.atlassian.net/browse/OPS-123", "jira:OPS"),
            (
                "https://acme.atlassian.net/jira/software/projects/OPS/boards/1",
                "jira:OPS",
            ),
            ("https://acme.atlassian.net/projects/OPS/issues", "jira:OPS"),
        ] {
            assert_eq!(
                spec_of(TargetKind::Board, entry),
                Ok(expected.to_owned()),
                "{entry} must reach {expected}"
            );
        }
    }

    /// 🔴 The workspace slug is not the team key, stated as its own case.
    ///
    /// A parser that reads `https://linear.app/acme/team/ENG/active` as
    /// `linear:acme` registers the account rather than the team, and every
    /// collection against it returns nothing.
    #[test]
    fn a_linear_url_yields_the_team_key_not_the_workspace() {
        let spec = spec_of(
            TargetKind::Board,
            "https://linear.app/wonka-labs/team/ENG/active",
        )
        .expect("a team URL parses");
        assert_eq!(spec, "linear:ENG");
        assert!(
            !spec.contains("wonka-labs"),
            "the workspace slug must never become the key: {spec}"
        );
    }

    /// 🔴 #6001: a `repos.txt` line may be an absolute path, and it must reach
    /// the registry VERBATIM — no `.git` stripping, no owner/name split.
    ///
    /// The parse is all-or-nothing, so against `7eef4bb9b` one path line
    /// refuses every repository in the file: `/srv/apex` splits to an empty
    /// owner and `registry::parse` rejects it. `/srv/apex.git` is the sharper
    /// case — the normalization would rewrite it to a directory that does not
    /// exist, and the operator would get a refusal naming a path they never
    /// listed.
    #[test]
    fn an_absolute_path_reaches_the_registry_unchanged() {
        for entry in ["/srv/apex", "/srv/apex.git", "/srv/deep/nested/apex"] {
            assert_eq!(
                spec_of(TargetKind::Repo, entry),
                Ok(entry.to_owned()),
                "{entry} must reach the registry verbatim"
            );
        }
    }

    /// And a file mixing the two forms parses as one list, because a path line
    /// halting the read is the same all-or-nothing failure a bad line is.
    #[test]
    fn a_path_and_a_remote_are_read_from_one_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            specs(detected(
                tmp.path(),
                Some("acme/api\n/srv/apex\nhttps://github.com/acme/web.git\n"),
                None
            )),
            ["acme/api", "/srv/apex", "acme/web"]
        );
    }

    /// A URL and its short form are the same target, so a file mixing the two
    /// registers one entry per repository rather than two.
    #[test]
    fn a_url_and_a_short_form_reach_the_same_spec() {
        assert_eq!(
            spec_of(TargetKind::Repo, "https://github.com/acme/api/tree/main"),
            spec_of(TargetKind::Repo, "acme/api")
        );
    }

    /// Every refusal, each naming what it wanted instead.
    ///
    /// #6001 moved `/acme/api` out of this list: an absolute path is now a
    /// checkout on disk, and whether THAT path is a usable repository is
    /// `crate::validate`'s question — asked at registration, where the refusal
    /// can name the condition that failed. A relative `acme/../etc` is still a
    /// malformed `owner/repo` and still refused here.
    #[test]
    fn entries_that_are_not_targets_are_refused() {
        for entry in [
            "acme",
            "acme/api/tree/main",
            "https://gitlab.com/acme/api",
            "https://github.com/acme",
            "acme/../etc",
        ] {
            assert!(
                spec_of(TargetKind::Repo, entry).is_err(),
                "{entry} must be refused"
            );
        }
        for entry in [
            "ENG",
            "gitlab:ENG",
            "https://linear.app/acme",
            "https://linear.app/acme/inbox/ENG",
            "https://acme.atlassian.net/dashboard",
            "https://example.com/browse/OPS-1",
        ] {
            assert!(
                spec_of(TargetKind::Board, entry).is_err(),
                "{entry} must be refused"
            );
        }
    }

    /// A trailing `.git` is stripped rather than becoming part of the name.
    ///
    /// Against a parser that only strips the scheme, this returns
    /// `acme/api.git` — which `registry::parse` ACCEPTS, because
    /// `clone::split_name` allows `.`.
    #[test]
    fn the_git_suffix_never_becomes_the_repository_name() {
        for entry in ["acme/api.git", "https://github.com/acme/api.git"] {
            let spec = spec_of(TargetKind::Repo, entry).expect("parses");
            assert_eq!(spec, "acme/api");
            assert!(!spec.ends_with(".git"), "{entry} kept its suffix: {spec}");
        }
    }

    /// 🔴 A byte-order mark on line 1 must not refuse the engagement (#5990).
    ///
    /// `str::trim` uses `char::is_whitespace` and U+FEFF is not whitespace, so
    /// against `2c606d10d` the mark reaches `clone::split_name`'s charset check
    /// and line 1 is refused — which, under the all-or-nothing rule, blocks
    /// EVERY repository in the file. Notepad, Excel's "Save as .txt" and
    /// PowerShell's `Out-File` all write one by default.
    ///
    /// The second half is the boundary: a U+FEFF anywhere but offset 0 is a real
    /// character in a malformed entry and is still refused, so stripping the
    /// prefix cannot quietly repair a line that is not what it looks like.
    #[test]
    fn a_byte_order_mark_does_not_refuse_the_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            specs(detected(
                tmp.path(),
                Some("\u{FEFF}acme/api\nacme/web\n"),
                None
            )),
            ["acme/api", "acme/web"],
            "a leading byte-order mark must not refuse the whole engagement"
        );

        let mid = tempfile::tempdir().expect("tempdir");
        seed(mid.path(), Some("acme/api\n\u{FEFF}acme/web\n"), None);
        let err = detect(&mid.path().join("engagement.toml"))
            .expect_err("a mark mid-file is a malformed entry, not a file artifact");
        assert!(
            matches!(err, AuditError::TargetsFileRefused { .. }),
            "{err:?}"
        );
    }

    /// 🔴 GitHub's own pages are two-segment paths, and were accepted as targets
    /// (#5990).
    ///
    /// Against `2c606d10d` `https://github.com/orgs/acme/repositories` — the
    /// natural paste for someone who means "every repository in this org" —
    /// normalizes to `orgs/acme` and the operator gets a refusal at registration
    /// naming a repository they never listed.
    ///
    /// The second loop is what must not break: the browser-bar spellings of a
    /// URL taken while looking AT a repository all carry a real owner.
    #[test]
    fn a_github_page_that_is_not_a_repository_is_refused() {
        for entry in [
            "https://github.com/orgs/acme/repositories",
            "https://github.com/users/acme/projects",
            "https://github.com/topics/rust",
            "https://github.com/settings/profile",
            "https://www.github.com/ORGS/acme/repositories",
        ] {
            assert!(
                spec_of(TargetKind::Repo, entry).is_err(),
                "{entry} is a github.com page, not a repository"
            );
        }
        for entry in [
            "https://github.com/acme/api",
            "https://github.com/acme/api/issues/12",
            "https://github.com/acme/api/blob/main/README.md",
        ] {
            assert_eq!(
                spec_of(TargetKind::Repo, entry),
                Ok("acme/api".to_owned()),
                "{entry} names a real owner and must still parse"
            );
        }
    }

    /// Neither file present is the pre-#5978 world: no detection, so the caller
    /// falls back to the prompt loop.
    #[test]
    fn neither_file_present_is_no_detection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(detected(tmp.path(), None, None), None);
    }

    /// A file holding only comments and blanks is still a detection: the
    /// operator supplied the list, and re-prompting them for one is the
    /// behaviour this issue removes.
    #[test]
    fn a_file_of_comments_is_a_detection_with_nothing_in_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let detected = detected(tmp.path(), Some("# nothing yet\n\n   \n"), None)
            .expect("the file is present");
        assert!(detected.specs.is_empty());
        assert_eq!(detected.sources.len(), 1);
    }

    /// Both files are read, repositories first, in file order.
    #[test]
    fn both_files_are_read_into_one_ordered_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let list = specs(detected(
            tmp.path(),
            Some("acme/api\nhttps://github.com/acme/web.git\n"),
            Some("linear:ENG\nhttps://acme.atlassian.net/browse/OPS-7\n"),
        ));
        assert_eq!(list, ["acme/api", "acme/web", "linear:ENG", "jira:OPS"]);
    }

    /// 🔴 The all-or-nothing rule. One bad line in twenty registers nothing and
    /// names the line, because an audit covering fewer repositories than the
    /// file lists reports success over the absent ones.
    ///
    /// Against a per-line-skip parser this returns nineteen specs and no error.
    #[test]
    fn one_bad_line_refuses_the_whole_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(
            tmp.path(),
            Some("acme/api\n# a comment\nnot a repository\nacme/web\n"),
            Some("linear:ENG\n"),
        );
        let err =
            detect(&tmp.path().join("engagement.toml")).expect_err("a bad line refuses the read");
        let AuditError::TargetsFileRefused { bad, .. } = &err else {
            panic!("expected a targets-file refusal: {err:?}");
        };
        assert_eq!(bad.0.len(), 1);
        assert_eq!(bad.0[0].line, 3, "the line number must be the file's");
        assert_eq!(bad.0[0].file, REPOS_FILE);

        let message = err.to_string();
        assert!(message.contains("line 3"), "{message}");
        assert!(message.contains("not a repository"), "{message}");
        assert!(
            message.contains("key is saved"),
            "the operator must be told they will not be re-prompted: {message}"
        );
    }

    /// Bad lines from BOTH files are named in one report, so one run fixes them
    /// all rather than one run per file.
    #[test]
    fn every_bad_line_in_both_files_is_named_at_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed(
            tmp.path(),
            Some("acme/api\nacme\n"),
            Some("linear:ENG\nhttps://linear.app/acme\n"),
        );
        let err = detect(&tmp.path().join("engagement.toml")).expect_err("both files are bad");
        let AuditError::TargetsFileRefused { bad, .. } = &err else {
            panic!("expected a targets-file refusal: {err:?}");
        };
        assert_eq!(bad.0.len(), 2);
        assert_eq!(bad.0[0].file, REPOS_FILE);
        assert_eq!(bad.0[1].file, BOARDS_FILE);
        assert_eq!(bad.0[1].line, 2);
    }

    /// A file that exists but cannot be read is an error rather than an absence:
    /// treating it as absent would fall back to the prompt loop and quietly
    /// audit whatever the operator typed instead of what they listed.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_not_an_absent_one() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(REPOS_FILE);
        std::fs::write(&path, "acme/api\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // Running as root defeats the mode, and CI containers do. Skip rather
        // than assert something the environment cannot produce.
        if std::fs::read_to_string(&path).is_ok() {
            return;
        }

        let err = detect(&tmp.path().join("engagement.toml")).expect_err("unreadable");
        assert!(matches!(err, AuditError::Read { .. }), "{err:?}");
    }

    /// Make `file` present and unopenable. `None` when this account can read it
    /// anyway — running as root defeats the mode, and CI containers do.
    #[cfg(unix)]
    fn unreadable(dir: &Path, file: &str, body: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join(file);
        std::fs::write(&path, body).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        std::fs::read_to_string(&path).is_err().then_some(path)
    }

    /// 🔴 #5993: an unreadable `repos.txt` must not hide `boards.txt`'s bad
    /// lines.
    ///
    /// Against `6d937ba66` the read failure returned through `?` from the first
    /// file, so `boards.txt` was never opened: the operator fixed the
    /// permissions, re-ran, and only then met the bad line. Both problems are
    /// now one refusal, so one run fixes them together.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_does_not_hide_the_other_files_bad_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let Some(repos) = unreadable(tmp.path(), REPOS_FILE, "acme/api\n") else {
            return;
        };
        std::fs::write(tmp.path().join(BOARDS_FILE), "linear:ENG\nnot a board\n").expect("write");

        let err = detect(&tmp.path().join("engagement.toml")).expect_err("two problems, one read");
        let AuditError::TargetsFileRefused { bad, unreadable } = &err else {
            panic!("expected one refusal carrying both problems: {err:?}");
        };
        assert_eq!(bad.0.len(), 1, "{bad:?}");
        assert_eq!(bad.0[0].file, BOARDS_FILE);
        assert_eq!(bad.0[0].line, 2);
        assert_eq!(unreadable.0.len(), 1, "{unreadable:?}");
        assert_eq!(unreadable.0[0].file, REPOS_FILE);
        assert_eq!(unreadable.0[0].path, repos);

        let message = err.to_string();
        assert!(message.contains(REPOS_FILE), "{message}");
        assert!(message.contains("not a board"), "{message}");
        assert!(message.contains("key is saved"), "{message}");
    }

    /// Two unreadable files are both named, for the same reason one unreadable
    /// file plus a bad line is: fixing the first and re-running to discover the
    /// second is the round trip #5993 removes.
    #[cfg(unix)]
    #[test]
    fn both_unreadable_files_are_named_at_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let Some(_) = unreadable(tmp.path(), REPOS_FILE, "acme/api\n") else {
            return;
        };
        let Some(_) = unreadable(tmp.path(), BOARDS_FILE, "linear:ENG\n") else {
            return;
        };

        let err = detect(&tmp.path().join("engagement.toml")).expect_err("neither file opened");
        let AuditError::TargetsFileRefused { bad, unreadable } = &err else {
            panic!("expected a refusal naming both files: {err:?}");
        };
        assert!(bad.0.is_empty(), "{bad:?}");
        assert_eq!(
            unreadable
                .0
                .iter()
                .map(|u| u.file.as_str())
                .collect::<Vec<_>>(),
            [REPOS_FILE, BOARDS_FILE]
        );

        // A refusal with no bad lines must not announce a section it has no
        // entries for.
        let message = err.to_string();
        assert!(
            !message.contains("these lines are not audit targets"),
            "{message}"
        );
        assert!(
            message.contains("these targets files could not be read"),
            "{message}"
        );
    }
}
