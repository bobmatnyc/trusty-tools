//! The board reads: one snapshot of a live board, and the paged lists the
//! verbs walk (#10276).
//!
//! Why: `init`, `apply` and `check` all decide from the same picture of a
//! board — its fields and options, views, linked repos, workflows and README —
//! so it is read by one query and parsed into one type.
//! What: [`Board`] and its parts, [`read_board`], and the paged reads
//! [`board_items`], [`open_issues_page`], [`owner_projects`],
//! [`repo_projects`], [`owner_node`], [`repo_node`] and [`issue_boards`].
//! Every read is a named GraphQL query through [`BoardClient::query`].
//! Test: `board/verb_tests.rs`, through the fake `gh`.

use serde_json::Value;

use super::client::BoardClient;
use super::error::BoardError;

/// One option of a live single-select field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardOption {
    /// The option id, kept through an edit that sends it.
    pub(crate) id: String,
    /// Option name.
    pub(crate) name: String,
    /// GitHub color name (`GRAY`, …).
    pub(crate) color: String,
    /// Option description.
    pub(crate) description: String,
}

/// One field of a live board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardField {
    /// Field node id.
    pub(crate) id: String,
    /// Field name.
    pub(crate) name: String,
    /// GitHub `ProjectV2FieldType` (`SINGLE_SELECT`, `DATE`, `TITLE`, …).
    pub(crate) data_type: String,
    /// Options, for a single-select.
    pub(crate) options: Vec<BoardOption>,
}

/// The data types a user creates; every other type is GitHub-provided.
pub(crate) const CUSTOM_TYPES: [&str; 6] = [
    "TEXT",
    "SINGLE_SELECT",
    "MULTI_SELECT",
    "NUMBER",
    "DATE",
    "ITERATION",
];

/// One view of a live board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardView {
    /// View node id.
    pub(crate) id: String,
    /// View name.
    pub(crate) name: String,
    /// `TABLE_LAYOUT`, `BOARD_LAYOUT` or `ROADMAP_LAYOUT`.
    pub(crate) layout: String,
    /// The filter, empty when none.
    pub(crate) filter: String,
    /// Visible fields in order (`configuration.visibleFields`; a Roadmap,
    /// which carries none there, reads `fields` instead).
    pub(crate) visible: Vec<String>,
    /// Vertical group-by field names.
    pub(crate) vertical_group_by: Vec<String>,
}

/// A live board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Board {
    /// Project node id.
    pub(crate) id: String,
    /// Project number.
    pub(crate) number: u64,
    /// Project title.
    pub(crate) title: String,
    /// Project URL.
    pub(crate) url: String,
    /// Whether the project is closed.
    pub(crate) closed: bool,
    /// The README.
    pub(crate) readme: String,
    /// Items on the board, archived ones included (#10276).
    pub(crate) item_count: u64,
    /// Every field, built-in and custom.
    pub(crate) fields: Vec<BoardField>,
    /// Every view.
    pub(crate) views: Vec<BoardView>,
    /// Linked repositories, as `owner/name`.
    pub(crate) repositories: Vec<String>,
    /// Built-in workflows: name and whether enabled.
    pub(crate) workflows: Vec<(String, bool)>,
}

impl Board {
    /// The field named `name`.
    pub(crate) fn field(&self, name: &str) -> Option<&BoardField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The view named `name`.
    pub(crate) fn view(&self, name: &str) -> Option<&BoardView> {
        self.views.iter().find(|v| v.name == name)
    }
}

const NAME: &str = "... on ProjectV2FieldCommon { name }";

/// `ProjectV2.items`' filter naming archived and live items alike.
///
/// Why: #10276 — an archived item keeps its field values, so removing an
/// option it uses clears it too; the schema's `archivedStates` argument lets
/// the in-use count ask for both instead of relying on an undocumented default.
pub(crate) const ALL_ITEM_STATES: &str = "archivedStates: [ARCHIVED, NOT_ARCHIVED]";

/// Read one board by owner login and number.
///
/// What: `repositoryOwner(login) { ... on ProjectV2Owner { projectV2(number) } }`
/// works for a user and an organisation alike. A null project is
/// [`BoardError::ProjectNotFound`].
pub(crate) fn read_board(
    client: &BoardClient<'_>,
    owner: &str,
    number: u64,
) -> Result<Board, BoardError> {
    let doc = format!(
        "query BoardSnapshot($owner: String!, $number: Int!) {{ repositoryOwner(login: $owner) {{ \
         ... on ProjectV2Owner {{ projectV2(number: $number) {{ id number title url closed readme \
         items({ALL_ITEM_STATES}) {{ totalCount }} \
         fields(first: 100) {{ nodes {{ ... on ProjectV2FieldCommon {{ id name dataType }} \
         ... on ProjectV2SingleSelectField {{ options {{ id name color description }} }} }} }} \
         views(first: 50) {{ nodes {{ id name layout filter \
         verticalGroupByFields(first: 5) {{ nodes {{ {NAME} }} }} \
         fields(first: 50) {{ nodes {{ {NAME} }} }} \
         configuration {{ visibleFields(first: 50) {{ nodes {{ {NAME} }} }} }} }} }} \
         repositories(first: 100) {{ nodes {{ nameWithOwner }} }} \
         workflows(first: 50) {{ nodes {{ name enabled }} }} }} }} }} }}"
    );
    let data = client.query(
        "BoardSnapshot",
        &doc,
        &[("owner", Some(owner))],
        &[("number", number)],
    )?;
    let project = &data["repositoryOwner"]["projectV2"];
    if project.is_null() {
        return Err(BoardError::ProjectNotFound {
            owner: owner.to_string(),
            number,
        });
    }
    Ok(parse_board(project))
}

/// Parse the `projectV2` object of a [`read_board`] answer.
fn parse_board(p: &Value) -> Board {
    let fields = nodes(&p["fields"])
        .iter()
        .filter(|f| f["id"].is_string())
        .map(|f| BoardField {
            id: text(&f["id"]),
            name: text(&f["name"]),
            data_type: text(&f["dataType"]),
            options: f["options"]
                .as_array()
                .map(|opts| {
                    opts.iter()
                        .map(|o| BoardOption {
                            id: text(&o["id"]),
                            name: text(&o["name"]),
                            color: text(&o["color"]),
                            description: text(&o["description"]),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let views = nodes(&p["views"])
        .iter()
        .map(|v| {
            let configured = names(&v["configuration"]["visibleFields"]);
            BoardView {
                id: text(&v["id"]),
                name: text(&v["name"]),
                layout: text(&v["layout"]),
                filter: text(&v["filter"]),
                visible: if configured.is_empty() {
                    names(&v["fields"])
                } else {
                    configured
                },
                vertical_group_by: names(&v["verticalGroupByFields"]),
            }
        })
        .collect();
    Board {
        id: text(&p["id"]),
        number: p["number"].as_u64().unwrap_or(0),
        title: text(&p["title"]),
        url: text(&p["url"]),
        closed: p["closed"].as_bool().unwrap_or(false),
        readme: text(&p["readme"]),
        item_count: p["items"]["totalCount"].as_u64().unwrap_or(0),
        fields,
        views,
        repositories: nodes(&p["repositories"])
            .iter()
            .map(|r| text(&r["nameWithOwner"]))
            .collect(),
        workflows: nodes(&p["workflows"])
            .iter()
            .map(|w| (text(&w["name"]), w["enabled"].as_bool().unwrap_or(false)))
            .collect(),
    }
}

/// One item on a board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardItem {
    /// Item node id.
    pub(crate) id: String,
    /// The issue it carries, as (`owner/repo`, number); `None` for a draft or
    /// a pull request.
    pub(crate) issue: Option<(String, u64)>,
    /// The option id of the single-select field the read named.
    pub(crate) option_id: Option<String>,
}

/// Every item on board `project_id`, with its value of `field`.
///
/// What: pages `items(first: 100, after: $cursor)` to the end.
pub(crate) fn board_items(
    client: &BoardClient<'_>,
    project_id: &str,
    field: &str,
) -> Result<Vec<BoardItem>, BoardError> {
    read_items(client, project_id, field, "")
}

/// [`board_items`], archived items included ([`ALL_ITEM_STATES`]).
///
/// Test: `apply_refuses_to_remove_an_option_an_archived_item_uses_10276`.
pub(crate) fn board_items_with_archived(
    client: &BoardClient<'_>,
    project_id: &str,
    field: &str,
) -> Result<Vec<BoardItem>, BoardError> {
    read_items(client, project_id, field, &format!(", {ALL_ITEM_STATES}"))
}

fn read_items(
    client: &BoardClient<'_>,
    project_id: &str,
    field: &str,
    states: &str,
) -> Result<Vec<BoardItem>, BoardError> {
    let doc = format!(
        "query BoardItems($id: ID!, $field: String!, $cursor: String) {{ node(id: $id) {{ \
         ... on ProjectV2 {{ items(first: 100, after: $cursor{states}) {{ pageInfo {{ hasNextPage endCursor }} \
         nodes {{ id content {{ __typename ... on Issue {{ number repository {{ nameWithOwner }} }} }} \
         fieldValueByName(name: $field) {{ ... on ProjectV2ItemFieldSingleSelectValue {{ optionId }} }} }} }} }} }} }}"
    );
    let doc = doc.as_str();
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let data = client.query(
            "BoardItems",
            doc,
            &[
                ("id", Some(project_id)),
                ("field", Some(field)),
                ("cursor", cursor.as_deref()),
            ],
            &[],
        )?;
        let conn = &data["node"]["items"];
        for n in nodes(conn) {
            let content = &n["content"];
            let issue = (content["__typename"].as_str() == Some("Issue"))
                .then(|| {
                    content["number"]
                        .as_u64()
                        .map(|num| (text(&content["repository"]["nameWithOwner"]), num))
                })
                .flatten();
            items.push(BoardItem {
                id: text(&n["id"]),
                issue,
                option_id: n["fieldValueByName"]["optionId"]
                    .as_str()
                    .map(str::to_string),
            });
        }
        match next_cursor(conn) {
            Some(next) => cursor = Some(next),
            None => return Ok(items),
        }
    }
}

/// One open issue of the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenIssue {
    /// Issue node id.
    pub(crate) id: String,
    /// Issue number.
    pub(crate) number: u64,
    /// Label names.
    pub(crate) labels: Vec<String>,
}

/// One page of the repo's open issues, oldest first, and the next cursor.
///
/// Why: AC4's backfill records the cursor between pages, so a page is the
/// unit of resumption; oldest-first keeps a cursor stable while new issues
/// arrive at the end.
pub(crate) fn open_issues_page(
    client: &BoardClient<'_>,
    repo: &str,
    after: Option<&str>,
) -> Result<(Vec<OpenIssue>, Option<String>), BoardError> {
    let (owner, name) = split_repo(repo);
    let doc = "query OpenIssues($owner: String!, $name: String!, $cursor: String) { \
               repository(owner: $owner, name: $name) { issues(states: OPEN, first: 100, after: $cursor, \
               orderBy: {field: CREATED_AT, direction: ASC}) { pageInfo { hasNextPage endCursor } \
               nodes { id number labels(first: 50) { nodes { name } } } } } }";
    let data = client.query(
        "OpenIssues",
        doc,
        &[
            ("owner", Some(owner)),
            ("name", Some(name)),
            ("cursor", after),
        ],
        &[],
    )?;
    let conn = &data["repository"]["issues"];
    let issues = nodes(conn)
        .iter()
        .map(|n| OpenIssue {
            id: text(&n["id"]),
            number: n["number"].as_u64().unwrap_or(0),
            labels: names(&n["labels"]),
        })
        .collect();
    Ok((issues, next_cursor(conn)))
}

/// A project as a listing shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectSummary {
    /// Project number.
    pub(crate) number: u64,
    /// Project title.
    pub(crate) title: String,
}

/// Every project of `owner`, paged.
pub(crate) fn owner_projects(
    client: &BoardClient<'_>,
    owner: &str,
) -> Result<Vec<ProjectSummary>, BoardError> {
    let doc = "query OwnerProjects($owner: String!, $cursor: String) { repositoryOwner(login: $owner) { \
               ... on ProjectV2Owner { projectsV2(first: 100, after: $cursor) { \
               pageInfo { hasNextPage endCursor } nodes { number title } } } } }";
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let data = client.query(
            "OwnerProjects",
            doc,
            &[("owner", Some(owner)), ("cursor", cursor.as_deref())],
            &[],
        )?;
        let conn = &data["repositoryOwner"]["projectsV2"];
        out.extend(nodes(conn).iter().map(summary));
        match next_cursor(conn) {
            Some(next) => cursor = Some(next),
            None => return Ok(out),
        }
    }
}

/// The projects `repo` is linked to (one page of 100; a repo linked to more
/// is refused by `init` either way).
pub(crate) fn repo_projects(
    client: &BoardClient<'_>,
    repo: &str,
) -> Result<Vec<ProjectSummary>, BoardError> {
    let (owner, name) = split_repo(repo);
    let doc = "query RepoProjects($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { \
               projectsV2(first: 100) { nodes { number title } } } }";
    let data = client.query(
        "RepoProjects",
        doc,
        &[("owner", Some(owner)), ("name", Some(name))],
        &[],
    )?;
    Ok(nodes(&data["repository"]["projectsV2"])
        .iter()
        .map(summary)
        .collect())
}

/// The owner's node id (`createProjectV2` takes it).
pub(crate) fn owner_node(client: &BoardClient<'_>, owner: &str) -> Result<String, BoardError> {
    let doc = "query OwnerNode($owner: String!) { repositoryOwner(login: $owner) { id } }";
    let data = client.query("OwnerNode", doc, &[("owner", Some(owner))], &[])?;
    required_id(&data["repositoryOwner"]["id"], "OwnerNode", owner)
}

/// The repo's node id (`linkProjectV2ToRepository` takes it).
pub(crate) fn repo_node(client: &BoardClient<'_>, repo: &str) -> Result<String, BoardError> {
    let (owner, name) = split_repo(repo);
    let doc = "query RepoNode($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { id } }";
    let data = client.query(
        "RepoNode",
        doc,
        &[("owner", Some(owner)), ("name", Some(name))],
        &[],
    )?;
    required_id(&data["repository"]["id"], "RepoNode", repo)
}

/// The boards issue `number` of `repo` is on, as (owner login, number).
pub(crate) fn issue_boards(
    client: &BoardClient<'_>,
    repo: &str,
    number: u64,
) -> Result<Vec<(String, u64)>, BoardError> {
    let (owner, name) = split_repo(repo);
    let doc = "query IssueBoards($owner: String!, $name: String!, $number: Int!) { \
               repository(owner: $owner, name: $name) { issue(number: $number) { \
               projectItems(first: 50) { nodes { project { number \
               owner { ... on User { login } ... on Organization { login } } } } } } } }";
    let data = client.query(
        "IssueBoards",
        doc,
        &[("owner", Some(owner)), ("name", Some(name))],
        &[("number", number)],
    )?;
    Ok(nodes(&data["repository"]["issue"]["projectItems"])
        .iter()
        .filter_map(|n| {
            let p = &n["project"];
            p["number"]
                .as_u64()
                .map(|num| (text(&p["owner"]["login"]), num))
        })
        .collect())
}

/// `owner/name` → (`owner`, `name`); a slug without `/` is all owner.
pub(crate) fn split_repo(repo: &str) -> (&str, &str) {
    repo.split_once('/').unwrap_or((repo, ""))
}

fn summary(n: &Value) -> ProjectSummary {
    ProjectSummary {
        number: n["number"].as_u64().unwrap_or(0),
        title: text(&n["title"]),
    }
}

fn required_id(v: &Value, what: &str, of: &str) -> Result<String, BoardError> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| BoardError::Parse {
            what: what.to_string(),
            detail: format!("no node id for `{of}`"),
        })
}

/// `conn.nodes`, or empty.
fn nodes(conn: &Value) -> Vec<Value> {
    conn["nodes"]
        .as_array()
        .map(|a| a.iter().filter(|n| !n.is_null()).cloned().collect())
        .unwrap_or_default()
}

/// The `name` of every node of `conn`.
fn names(conn: &Value) -> Vec<String> {
    nodes(conn)
        .iter()
        .filter_map(|n| n["name"].as_str().map(str::to_string))
        .collect()
}

/// `endCursor` when `hasNextPage` is true.
fn next_cursor(conn: &Value) -> Option<String> {
    let page = &conn["pageInfo"];
    if page["hasNextPage"].as_bool() == Some(true) {
        page["endCursor"].as_str().map(str::to_string)
    } else {
        None
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}
