//! `routes.toml`: the per-project list of who the Chat app may message, and
//! with which kinds.
//!
//! Why: every outbound Chat message must match a reviewed route (#9448
//! rulings 1–3). The file is project config, so the project dir is a
//! parameter, never a global.
//! What: [`load_routes`] reads `<project>/.trusty-channels/routes.toml`,
//! applies the load gate ([`crate::gchat::load_gate`]) and the schema-1 load
//! rules, and returns a [`RouteTable`]. A missing file is an empty table.
//! A route without `space` is a DM route: its DM space is learned when the
//! recipient first messages the app. A route with `space = "spaces/<id>"`
//! posts to, and accepts replies only from, that named space; several
//! routes may share one (#9448).
//! Test: `src/gchat/tests/routes_load.rs`, `src/gchat/tests/space_routes.rs`.
//!
//! ```toml
//! version = 1
//!
//! [gchat.connection]
//! project_id = "my-project"
//! subscription = "chat-in"
//! key_file = "~/.config/trusty/chat-sa.json"
//!
//! [[gchat.routes]]          # DM route
//! name = "janet"
//! recipient = "janet@example.com"
//! kinds = ["question", "review_notice"]
//!
//! [[gchat.routes]]          # space route (optional `space`)
//! name = "bob"
//! recipient = "bob@example.com"
//! kinds = ["question"]
//! space = "spaces/AAAAexample"
//! ```

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::gchat::api::client::is_space_name;
use crate::gchat::error::RouteError;
use crate::gchat::load_gate;

/// The config directory under a project root.
pub const CONFIG_DIR: &str = ".trusty-channels";
/// The routes file name inside [`CONFIG_DIR`].
pub const ROUTES_FILE: &str = "routes.toml";
/// The only schema version this layer reads.
pub const SCHEMA_VERSION: i64 = 1;

/// A message kind a route can allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageKind {
    /// A question; a reply binds to its question id.
    Question,
    /// A notice that a ticket or task waits for review; no reply expected.
    ReviewNotice,
}

impl MessageKind {
    /// Every kind, in schema order.
    pub const ALL: [MessageKind; 2] = [MessageKind::Question, MessageKind::ReviewNotice];

    /// The schema spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::ReviewNotice => "review_notice",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// The `[gchat.connection]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// Google Cloud project id.
    pub project_id: String,
    /// Pub/Sub subscription id (not the full resource name).
    pub subscription: String,
    /// Service-account key file path, `~/` expanded. A path only; S1
    /// enforces mode 0600 when the client opens it.
    pub key_file: PathBuf,
}

impl Connection {
    /// `projects/{project_id}/subscriptions/{subscription}`.
    pub fn subscription_name(&self) -> String {
        format!(
            "projects/{}/subscriptions/{}",
            self.project_id, self.subscription
        )
    }
}

/// One `[[gchat.routes]]` entry, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Unique route name (`[A-Za-z0-9_-]+`).
    pub name: String,
    /// The recipient's Chat email, ASCII-lowercased.
    pub recipient: String,
    /// Allowed kinds; never empty.
    pub kinds: BTreeSet<MessageKind>,
    /// The named Chat space this route posts to and accepts replies from,
    /// `spaces/{space}`; `None` for a DM route.
    ///
    /// Why: the owner ruled one shared Space for several people (#9448), so
    /// a route may name its space instead of learning a DM.
    /// What: validated at load like every outbound space name; several
    /// routes may share one. A route with a space never reads or writes the
    /// learned `gchat-spaces.json` binding.
    /// Test: `space_field_loads_and_a_malformed_space_fails_the_load`,
    /// `space_route_sends_to_its_configured_space_without_a_learned_binding`.
    pub space: Option<String>,
}

impl Route {
    /// True when this route allows `kind`.
    pub fn allows(&self, kind: MessageKind) -> bool {
        self.kinds.contains(&kind)
    }
}

/// The validated routes file. Empty when the file is missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteTable {
    /// `[gchat.connection]`, when the file has a `[gchat]` table.
    pub connection: Option<Connection>,
    /// Routes in file order.
    pub routes: Vec<Route>,
    /// False when no routes file exists.
    pub file_present: bool,
}

impl RouteTable {
    /// Find a route by name, or by recipient email when `to` holds an `@`.
    ///
    /// Why: callers address a route either way; names cannot hold `@`, so
    /// the two never collide.
    /// What: recipient match is ASCII case-insensitive.
    /// Test: `egress_per_route_allows_exactly_its_kinds`.
    pub fn find(&self, to: &str) -> Option<&Route> {
        let to = to.trim();
        if to.contains('@') {
            let to = to.to_ascii_lowercase();
            self.routes.iter().find(|r| r.recipient == to)
        } else {
            self.routes.iter().find(|r| r.name == to)
        }
    }

    /// Find the route whose recipient is `email` (case-insensitive).
    pub fn by_recipient(&self, email: &str) -> Option<&Route> {
        let email = email.trim().to_ascii_lowercase();
        self.routes.iter().find(|r| r.recipient == email)
    }
}

/// `<project_dir>/.trusty-channels/routes.toml`.
pub fn routes_path(project_dir: &Path) -> PathBuf {
    project_dir.join(CONFIG_DIR).join(ROUTES_FILE)
}

/// Load and validate a project's routes file.
///
/// Why: the one entry point that turns reviewed config into routes; any
/// failure must refuse every send rather than fall back (rulings 2–3).
/// What: a missing file returns an empty table. Otherwise refuses a symlink,
/// reads the bytes once, runs the git load gate on those exact bytes (they
/// must equal the blob committed at `HEAD`), then parses the same bytes with
/// unknown keys denied and applies the load rules.
/// Test: `load_gate_refuses_untracked_modified_and_staged`,
/// `load_gate_checks_the_bytes_read_not_the_file_after`,
/// `missing_file_is_zero_routes`, `load_rules_refuse_each_invalid_file`,
/// `duplicates_name_both_entries`, `valid_file_loads_routes_in_order`.
pub fn load_routes(project_dir: &Path) -> Result<RouteTable, RouteError> {
    let path = routes_path(project_dir);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RouteTable::default()),
        Err(e) => return Err(read_error(&path, &e)),
    };
    if !meta.file_type().is_file() {
        return Err(RouteError::Read {
            path,
            reason: "not a regular file (a symlink or directory is refused)".into(),
        });
    }
    let bytes = std::fs::read(&path).map_err(|e| read_error(&path, &e))?;
    // #9448 review: gate and parse see the same bytes.
    load_gate::check_committed(&path, &bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| RouteError::Read {
        path: path.clone(),
        reason: "not valid UTF-8".into(),
    })?;
    parse_routes(&path, &text, home_dir().as_deref())
}

/// Parse and validate routes-file text (no load gate).
///
/// Why: separated from [`load_routes`] so the load rules are testable
/// without a git repo.
/// What: schema version 1 with `deny_unknown_fields` at every level, then
/// the per-route rules and the duplicate checks.
/// Test: `load_rules_refuse_each_invalid_file`, `duplicates_name_both_entries`.
pub fn parse_routes(
    path: &Path,
    text: &str,
    home: Option<&Path>,
) -> Result<RouteTable, RouteError> {
    let raw: RawFile = toml::from_str(text).map_err(|e| RouteError::Parse {
        path: path.to_path_buf(),
        reason: e.message().to_string(),
    })?;
    if raw.version != SCHEMA_VERSION {
        return Err(RouteError::Version { found: raw.version });
    }
    let Some(gchat) = raw.gchat else {
        return Ok(RouteTable {
            file_present: true,
            ..RouteTable::default()
        });
    };
    let connection = validate_connection(gchat.connection, home)?;
    let mut routes = Vec::with_capacity(gchat.routes.len());
    let mut by_name: HashMap<String, String> = HashMap::new();
    let mut by_recipient: HashMap<String, String> = HashMap::new();
    for (i, raw_route) in gchat.routes.into_iter().enumerate() {
        let entry = format!("gchat.routes[{i}] {:?}", raw_route.name);
        let route = validate_route(&entry, raw_route)?;
        if let Some(first) = by_name.insert(route.name.clone(), entry.clone()) {
            return Err(duplicate(first, entry, "name", &route.name));
        }
        if let Some(first) = by_recipient.insert(route.recipient.clone(), entry.clone()) {
            return Err(duplicate(first, entry, "recipient", &route.recipient));
        }
        routes.push(route);
    }
    Ok(RouteTable {
        connection: Some(connection),
        routes,
        file_present: true,
    })
}

fn duplicate(first: String, second: String, field: &'static str, value: &str) -> RouteError {
    RouteError::Duplicate {
        first,
        second,
        field,
        value: value.to_string(),
    }
}

fn invalid(entry: &str, reason: impl Into<String>) -> RouteError {
    RouteError::Invalid {
        entry: entry.to_string(),
        reason: reason.into(),
    }
}

fn validate_route(entry: &str, raw: RawRoute) -> Result<Route, RouteError> {
    let name_ok = !raw.name.is_empty()
        && raw
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !name_ok {
        return Err(invalid(entry, "name must match [A-Za-z0-9_-]+"));
    }
    if !is_email(&raw.recipient) {
        return Err(invalid(
            entry,
            format!("recipient {:?} is not an email address", raw.recipient),
        ));
    }
    if raw.kinds.is_empty() {
        return Err(invalid(entry, "kinds is empty"));
    }
    let mut kinds = BTreeSet::new();
    for k in &raw.kinds {
        let kind = MessageKind::parse(k).ok_or_else(|| {
            invalid(
                entry,
                format!("kind {k:?} is not one of question, review_notice"),
            )
        })?;
        kinds.insert(kind);
    }
    // #9448: a configured space passes the same check as a send target.
    if let Some(space) = &raw.space {
        if !is_space_name(space) {
            return Err(invalid(
                entry,
                format!("space {space:?} must be spaces/{{space}}"),
            ));
        }
    }
    Ok(Route {
        name: raw.name,
        recipient: raw.recipient.to_ascii_lowercase(),
        kinds,
        space: raw.space,
    })
}

fn validate_connection(raw: RawConnection, home: Option<&Path>) -> Result<Connection, RouteError> {
    const ENTRY: &str = "gchat.connection";
    let id_ok = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '+' | '~')
            })
    };
    if !id_ok(&raw.project_id) {
        return Err(invalid(ENTRY, "project_id is empty or malformed"));
    }
    if !id_ok(&raw.subscription) {
        return Err(invalid(
            ENTRY,
            "subscription must be a subscription id, not a resource path",
        ));
    }
    // #9448 ruling 1: a path, never a credential.
    let k = raw.key_file.trim();
    if k.is_empty() || k.contains('\n') || k.contains('{') || k.contains("PRIVATE KEY") {
        return Err(invalid(ENTRY, "key_file must be a file path"));
    }
    let key_file = expand_home(k, home).ok_or_else(|| {
        invalid(
            ENTRY,
            "key_file starts with ~/ but no home directory is known",
        )
    })?;
    Ok(Connection {
        project_id: raw.project_id,
        subscription: raw.subscription,
        key_file,
    })
}

/// Expand a leading `~/` against `home`. `None` when `~/` has no home.
pub(crate) fn expand_home(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => home.map(|h| h.join(rest)),
        None if path == "~" => home.map(Path::to_path_buf),
        None => Some(PathBuf::from(path)),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// A plain address check: one `@`, a non-empty local part, a dotted domain,
/// no whitespace or control characters.
pub(crate) fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let clean = |p: &str| {
        !p.is_empty()
            && !p.contains('@')
            && p.chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '<' | '>' | ',' | ';'))
    };
    clean(local)
        && clean(domain)
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    version: i64,
    #[serde(default)]
    gchat: Option<RawGchat>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGchat {
    connection: RawConnection,
    #[serde(default)]
    routes: Vec<RawRoute>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    project_id: String,
    subscription: String,
    key_file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    name: String,
    recipient: String,
    kinds: Vec<String>,
    #[serde(default)]
    space: Option<String>,
}

fn read_error(path: &Path, e: &std::io::Error) -> RouteError {
    RouteError::Read {
        path: path.to_path_buf(),
        reason: e.kind().to_string(),
    }
}
