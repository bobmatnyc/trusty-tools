//! A fake `gh` (and `git`) that simulates GitHub Projects for the board
//! verbs' tests (#10276).
//!
//! Why: AC3–AC5 and AC8 are claims about which calls reach GitHub — reads
//! only on a dry run, no mutation on a second `apply`, a cursor that resumes —
//! so the fake must both record argv and hold a board whose state the
//! mutations change, the way GitHub does in the Phase 2 trial.
//! What: [`FakeGh`] implements [`CommandRunner`]. It parses the named read
//! queries' variables and the mutations' inline input literals, answers from
//! a [`Sim`], records every call, and fails a call whose argv contains a
//! scripted substring. [`is_mutation`] is the one "is this a write" test.
//! Test: this IS test support (classified test file: 3000-SLOC cap).

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::commands::ticket::runner::{CommandOutput, CommandRunner};

/// A live option.
#[derive(Debug, Clone)]
pub(super) struct SimOption {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) color: String,
    pub(super) description: String,
}

/// A live field.
#[derive(Debug, Clone)]
pub(super) struct SimField {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) data_type: String,
    pub(super) options: Vec<SimOption>,
}

/// A live view.
#[derive(Debug, Clone)]
pub(super) struct SimView {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) layout: String,
    pub(super) filter: String,
    pub(super) visible: Vec<String>,
}

/// A live item: the issue it carries and its single-select values.
#[derive(Debug, Clone)]
pub(super) struct SimItem {
    pub(super) id: String,
    pub(super) issue: (String, u64),
    pub(super) values: BTreeMap<String, String>,
    /// #10276: archived; a read sees it only when it names both states.
    pub(super) archived: bool,
}

/// A live project.
#[derive(Debug, Clone)]
pub(super) struct SimProject {
    pub(super) id: String,
    pub(super) owner: String,
    pub(super) number: u64,
    pub(super) title: String,
    pub(super) readme: String,
    pub(super) fields: Vec<SimField>,
    pub(super) views: Vec<SimView>,
    pub(super) repos: Vec<String>,
    pub(super) items: Vec<SimItem>,
    pub(super) workflows: Vec<(String, bool)>,
}

impl SimProject {
    pub(super) fn field(&self, name: &str) -> Option<&SimField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// An open issue.
#[derive(Debug, Clone)]
pub(super) struct SimIssue {
    pub(super) number: u64,
    pub(super) labels: Vec<String>,
}

/// GitHub, as far as the board verbs see it.
#[derive(Debug, Clone, Default)]
pub(super) struct Sim {
    pub(super) projects: Vec<SimProject>,
    /// `owner/repo` → its open issues.
    pub(super) issues: BTreeMap<String, Vec<SimIssue>>,
    pub(super) dirty: bool,
    /// #10276: `gh repo view`'s answer; empty means `o/r`.
    pub(super) checkout: String,
    /// stdout of `gh api --include repos/<repo>`, and whether it exits 0.
    pub(super) probe: (String, bool),
    pub(super) comments: BTreeMap<u64, Vec<String>>,
    pub(super) next: u64,
}

/// The fields GitHub gives every new project, Status with its defaults.
pub(super) const BUILTINS: [(&str, &str); 12] = [
    ("Title", "TITLE"),
    ("Assignees", "ASSIGNEES"),
    ("Labels", "LABELS"),
    ("Linked pull requests", "LINKED_PULL_REQUESTS"),
    ("Milestone", "MILESTONE"),
    ("Repository", "REPOSITORY"),
    ("Reviewers", "REVIEWERS"),
    ("Parent issue", "PARENT_ISSUE"),
    ("Sub-issues progress", "SUB_ISSUES_PROGRESS"),
    ("Created", "CREATED"),
    ("Updated", "UPDATED"),
    ("Closed", "CLOSED"),
];

/// What a new view or a Roadmap shows when given no visible fields.
const DEFAULT_VISIBLE: [&str; 5] = [
    "Title",
    "Assignees",
    "Status",
    "Linked pull requests",
    "Sub-issues progress",
];

impl Sim {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}_{}", self.next)
    }

    /// A fresh project as `createProjectV2` leaves it.
    pub(super) fn new_project(&mut self, owner: &str, number: u64, title: &str) -> usize {
        let mut fields: Vec<SimField> = BUILTINS
            .iter()
            .map(|(n, t)| SimField {
                id: format!("F_{}", n.replace(' ', "_")),
                name: (*n).to_string(),
                data_type: (*t).to_string(),
                options: Vec::new(),
            })
            .collect();
        let defaults = [
            ("Todo", "GREEN"),
            ("In Progress", "YELLOW"),
            ("Done", "PURPLE"),
        ];
        let options = defaults
            .iter()
            .map(|(n, c)| SimOption {
                id: self.id("O"),
                name: (*n).to_string(),
                color: (*c).to_string(),
                description: String::new(),
            })
            .collect();
        fields.insert(
            2,
            SimField {
                id: "F_Status".into(),
                name: "Status".into(),
                data_type: "SINGLE_SELECT".into(),
                options,
            },
        );
        let view = SimView {
            id: self.id("V"),
            name: "View 1".into(),
            layout: "TABLE_LAYOUT".into(),
            filter: String::new(),
            visible: DEFAULT_VISIBLE.iter().map(|s| s.to_string()).collect(),
        };
        let id = self.id("P");
        self.projects.push(SimProject {
            id,
            owner: owner.into(),
            number,
            title: title.into(),
            readme: String::new(),
            fields,
            views: vec![view],
            repos: Vec::new(),
            items: Vec::new(),
            workflows: vec![
                ("Auto-add to project".into(), false),
                ("Item closed".into(), true),
            ],
        });
        self.projects.len() - 1
    }

    /// Add `n` open issues `1..=n` to `repo`.
    pub(super) fn with_issues(&mut self, repo: &str, n: u64) {
        let list = self.issues.entry(repo.to_string()).or_default();
        for number in 1..=n {
            list.push(SimIssue {
                number,
                labels: Vec::new(),
            });
        }
    }

    /// Put issue `number` of `repo` on project `p`, with Status `option`.
    pub(super) fn put_item(&mut self, p: usize, repo: &str, number: u64, status: Option<&str>) {
        let id = self.id("I");
        let mut values = BTreeMap::new();
        if let Some(name) = status
            && let Some(o) = self.projects[p]
                .field("Status")
                .and_then(|f| f.options.iter().find(|o| o.name == name))
        {
            values.insert("F_Status".to_string(), o.id.clone());
        }
        self.projects[p].items.push(SimItem {
            id,
            issue: (repo.to_string(), number),
            values,
            archived: false,
        });
    }

    fn project_by_id(&mut self, id: &str) -> Option<&mut SimProject> {
        self.projects.iter_mut().find(|p| p.id == id)
    }
}

/// The fake runner.
pub(super) struct FakeGh {
    pub(super) sim: RefCell<Sim>,
    calls: RefCell<Vec<Vec<String>>>,
    fail: RefCell<Vec<(String, String)>>,
}

impl FakeGh {
    pub(super) fn new(sim: Sim) -> Self {
        Self {
            sim: RefCell::new(sim),
            calls: RefCell::new(Vec::new()),
            fail: RefCell::new(Vec::new()),
        }
    }

    /// Fail the next call whose joined argv contains `needle`, once.
    pub(super) fn fail_once(&self, needle: &str, stderr: &str) {
        self.fail
            .borrow_mut()
            .push((needle.to_string(), stderr.to_string()));
    }

    /// Every call so far, `program` first.
    pub(super) fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }

    /// The mutations so far.
    pub(super) fn mutations(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| is_mutation(c))
            .collect()
    }

    /// The parsed `input` of every mutation of `field`.
    pub(super) fn inputs(&self, field: &str) -> Vec<Value> {
        self.mutations()
            .iter()
            .filter_map(|c| query_arg(c))
            .filter(|q| mutation_field(q) == field)
            .map(|q| mutation_input(&q))
            .collect()
    }
}

/// Is this call a GitHub write?
pub(super) fn is_mutation(call: &[String]) -> bool {
    let gh = call.first().is_some_and(|p| p == "gh");
    gh && (query_arg(call).is_some_and(|q| q.trim_start().starts_with("mutation"))
        || call.iter().any(|a| a == "item-add" || a == "comment"))
}

fn query_arg(call: &[String]) -> Option<String> {
    call.iter()
        .find_map(|a| a.strip_prefix("query=").map(str::to_string))
}

fn mutation_field(q: &str) -> String {
    let rest = q.trim_start().trim_start_matches("mutation").trim_start();
    let rest = rest.trim_start_matches('{').trim_start();
    rest.split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn mutation_input(q: &str) -> Value {
    let Some(at) = q.find("input:") else {
        return Value::Null;
    };
    let bytes = q.as_bytes();
    let mut i = at + "input:".len();
    parse_value(bytes, &mut i)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
        *i += 1;
    }
}

/// Parse one GraphQL input literal into JSON (enums become strings).
pub(super) fn parse_value(b: &[u8], i: &mut usize) -> Value {
    skip_ws(b, i);
    match b.get(*i) {
        Some(b'"') => {
            let start = *i;
            *i += 1;
            while *i < b.len() && b[*i] != b'"' {
                if b[*i] == b'\\' {
                    *i += 1;
                }
                *i += 1;
            }
            *i += 1;
            let raw = std::str::from_utf8(&b[start..*i]).expect("utf8");
            serde_json::from_str(raw).expect("a JSON-compatible string literal")
        }
        Some(b'[') => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(b, i);
                if b.get(*i) == Some(&b']') {
                    *i += 1;
                    return Value::Array(items);
                }
                items.push(parse_value(b, i));
            }
        }
        Some(b'{') => {
            *i += 1;
            let mut map = serde_json::Map::new();
            loop {
                skip_ws(b, i);
                if b.get(*i) == Some(&b'}') {
                    *i += 1;
                    return Value::Object(map);
                }
                let start = *i;
                while b[*i] != b':' {
                    *i += 1;
                }
                let key = std::str::from_utf8(&b[start..*i])
                    .expect("utf8")
                    .trim()
                    .to_string();
                *i += 1;
                map.insert(key, parse_value(b, i));
            }
        }
        _ => {
            let start = *i;
            while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'_') {
                *i += 1;
            }
            Value::String(
                std::str::from_utf8(&b[start..*i])
                    .expect("utf8")
                    .to_string(),
            )
        }
    }
}

fn ok(stdout: impl Into<String>) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn failed(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

/// `-f`/`-F` variables of a read.
fn vars(call: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for w in call.windows(2) {
        if (w[0] == "-f" || w[0] == "-F")
            && let Some((k, v)) = w[1].split_once('=')
            && k != "query"
        {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

/// `{nodes, pageInfo}` for `all[from..from+100]`, with index cursors.
fn page(all: Vec<Value>, cursor: Option<&String>) -> Value {
    let from: usize = cursor
        .and_then(|c| c.strip_prefix('c'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let to = (from + 100).min(all.len());
    let more = to < all.len();
    json!({
        "nodes": all[from.min(all.len())..to].to_vec(),
        "pageInfo": {"hasNextPage": more, "endCursor": format!("c{to}")}
    })
}

fn names(list: &[String]) -> Value {
    json!({"nodes": list.iter().map(|n| json!({"name": n})).collect::<Vec<_>>()})
}

impl Sim {
    fn snapshot(&self, owner: &str, number: u64, all: bool) -> Value {
        let Some(p) = self
            .projects
            .iter()
            .find(|p| p.owner == owner && p.number == number)
        else {
            return json!({"repositoryOwner": {"projectV2": null}});
        };
        let fields: Vec<Value> = p
            .fields
            .iter()
            .map(|f| {
                let mut v = json!({"id": f.id, "name": f.name, "dataType": f.data_type});
                if f.data_type == "SINGLE_SELECT" {
                    v["options"] = json!(f.options.iter().map(|o| json!({
                        "id": o.id, "name": o.name, "color": o.color, "description": o.description
                    })).collect::<Vec<_>>());
                }
                v
            })
            .collect();
        let views: Vec<Value> = p
            .views
            .iter()
            .map(|v| {
                let roadmap = v.layout == "ROADMAP_LAYOUT";
                let group = if v.layout == "BOARD_LAYOUT" {
                    vec!["Status".to_string()]
                } else {
                    Vec::new()
                };
                json!({
                    "id": v.id, "name": v.name, "layout": v.layout,
                    "filter": if v.filter.is_empty() { Value::Null } else { json!(v.filter) },
                    "verticalGroupByFields": names(&group),
                    "fields": names(&v.visible),
                    "configuration": {"visibleFields": names(if roadmap { &[] } else { &v.visible[..] })},
                })
            })
            .collect();
        json!({"repositoryOwner": {"projectV2": {
            "id": p.id, "number": p.number, "title": p.title,
            "url": format!("https://github.com/orgs/{}/projects/{}", p.owner, p.number),
            "closed": false, "readme": p.readme,
            "items": {"totalCount": p.items.iter().filter(|i| all || !i.archived).count()},
            "fields": {"nodes": fields},
            "views": {"nodes": views},
            "repositories": {"nodes": p.repos.iter().map(|r| json!({"nameWithOwner": r})).collect::<Vec<_>>()},
            "workflows": {"nodes": p.workflows.iter().map(|(n, e)| json!({"name": n, "enabled": e})).collect::<Vec<_>>()},
        }}})
    }

    /// `all`: the query names both archived states (#10276); otherwise an
    /// archived item is hidden, the conservative reading of GitHub's default.
    fn read(&self, name: &str, v: &BTreeMap<String, String>, all: bool) -> Value {
        let get = |k: &str| v.get(k).cloned().unwrap_or_default();
        match name {
            "BoardSnapshot" => {
                self.snapshot(&get("owner"), get("number").parse().unwrap_or(0), all)
            }
            "BoardItems" => {
                let p = self.projects.iter().find(|p| p.id == get("id"));
                let field_id = p
                    .and_then(|p| p.field(&get("field")))
                    .map(|f| f.id.clone())
                    .unwrap_or_default();
                let all = p
                    .map(|p| {
                        p.items
                            .iter()
                            .filter(|it| all || !it.archived)
                            .map(|it| {
                                json!({
                                    "id": it.id,
                                    "content": {"__typename": "Issue", "number": it.issue.1,
                                                "repository": {"nameWithOwner": it.issue.0}},
                                    "fieldValueByName": it.values.get(&field_id)
                                        .map_or(Value::Null, |o| json!({"optionId": o})),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({"node": {"items": page(all, v.get("cursor"))}})
            }
            "OpenIssues" => {
                let repo = format!("{}/{}", get("owner"), get("name"));
                let all = self
                    .issues
                    .get(&repo)
                    .map(|l| {
                        l.iter()
                            .map(|i| {
                                json!({"id": format!("ISSUE_{}", i.number), "number": i.number,
                                            "labels": names(&i.labels)})
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({"repository": {"issues": page(all, v.get("cursor"))}})
            }
            "OwnerProjects" => {
                let all = self
                    .projects
                    .iter()
                    .filter(|p| p.owner == get("owner"))
                    .map(|p| json!({"number": p.number, "title": p.title}))
                    .collect();
                json!({"repositoryOwner": {"projectsV2": page(all, v.get("cursor"))}})
            }
            "RepoProjects" => {
                let repo = format!("{}/{}", get("owner"), get("name"));
                let all: Vec<Value> = self
                    .projects
                    .iter()
                    .filter(|p| p.repos.contains(&repo))
                    .map(|p| json!({"number": p.number, "title": p.title}))
                    .collect();
                json!({"repository": {"projectsV2": {"nodes": all}}})
            }
            "OwnerNode" => json!({"repositoryOwner": {"id": format!("OWNER_{}", get("owner"))}}),
            "RepoNode" => {
                json!({"repository": {"id": format!("REPO_{}/{}", get("owner"), get("name"))}})
            }
            "IssueBoards" => {
                let repo = format!("{}/{}", get("owner"), get("name"));
                let n: u64 = get("number").parse().unwrap_or(0);
                let nodes: Vec<Value> = self
                    .projects
                    .iter()
                    .filter(|p| p.items.iter().any(|i| i.issue == (repo.clone(), n)))
                    .map(|p| json!({"project": {"number": p.number, "owner": {"login": p.owner}}}))
                    .collect();
                json!({"repository": {"issue": {"projectItems": {"nodes": nodes}}}})
            }
            other => panic!("unexpected read {other}"),
        }
    }

    fn options_from(&mut self, list: &Value) -> Vec<SimOption> {
        list.as_array()
            .map(|a| a.to_vec())
            .unwrap_or_default()
            .iter()
            .map(|o| SimOption {
                id: o["id"]
                    .as_str()
                    .map_or_else(|| self.id("O"), str::to_string),
                name: o["name"].as_str().unwrap_or_default().into(),
                color: o["color"].as_str().unwrap_or_default().into(),
                description: o["description"].as_str().unwrap_or_default().into(),
            })
            .collect()
    }

    fn write(&mut self, field: &str, input: &Value) -> Result<Value, String> {
        let s = |k: &str| input[k].as_str().unwrap_or_default().to_string();
        match field {
            "createProjectV2" => {
                let owner = s("ownerId").trim_start_matches("OWNER_").to_string();
                let number = self.projects.iter().map(|p| p.number).max().unwrap_or(0) + 1;
                let p = self.new_project(&owner, number, &s("title"));
                let id = self.projects[p].id.clone();
                let url = format!("https://github.com/orgs/{owner}/projects/{number}");
                Ok(json!({"projectV2": {"id": id, "number": number, "url": url}}))
            }
            "updateProjectV2Field" => {
                let options = self.options_from(&input["singleSelectOptions"]);
                let fid = s("fieldId");
                for p in &mut self.projects {
                    if let Some(f) = p.fields.iter_mut().find(|f| f.id == fid) {
                        // The trial: a removed option clears the items using it.
                        let kept: Vec<String> = options.iter().map(|o| o.id.clone()).collect();
                        for it in &mut p.items {
                            if it.values.get(&fid).is_some_and(|o| !kept.contains(o)) {
                                it.values.remove(&fid);
                            }
                        }
                        f.options = options;
                        return Ok(json!({"projectV2Field": {"id": fid}}));
                    }
                }
                Err("field not found".into())
            }
            "createProjectV2Field" => {
                let options = self.options_from(&input["singleSelectOptions"]);
                let name = s("name");
                let p = self.project_by_id(&s("projectId")).ok_or("no project")?;
                p.fields.push(SimField {
                    id: format!("F_{}", name.replace(' ', "_")),
                    name,
                    data_type: s("dataType"),
                    options,
                });
                Ok(json!({"projectV2Field": {"id": "x"}}))
            }
            "createProjectV2View" => {
                let layout = s("layout");
                let ids: Vec<String> = input["configuration"]["visibleFieldIds"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if layout == "ROADMAP_LAYOUT" && !input["configuration"].is_null() {
                    return Err("Roadmap views do not support visible fields.".into());
                }
                let id = self.id("V");
                let p = self.project_by_id(&s("projectId")).ok_or("no project")?;
                let visible = if ids.is_empty() {
                    DEFAULT_VISIBLE.iter().map(|x| x.to_string()).collect()
                } else {
                    ids.iter()
                        .filter_map(|i| {
                            p.fields.iter().find(|f| &f.id == i).map(|f| f.name.clone())
                        })
                        .collect()
                };
                p.views.push(SimView {
                    id: id.clone(),
                    name: s("name"),
                    layout,
                    filter: String::new(),
                    visible,
                });
                Ok(json!({"projectV2View": {"id": id}}))
            }
            "updateProjectV2View" | "deleteProjectV2View" => {
                let vid = s("viewId");
                for p in &mut self.projects {
                    if field == "deleteProjectV2View" {
                        p.views.retain(|v| v.id != vid);
                    } else if let Some(v) = p.views.iter_mut().find(|v| v.id == vid) {
                        v.filter = s("filter");
                    }
                }
                Ok(json!({"projectV2View": {"id": vid}}))
            }
            "linkProjectV2ToRepository" => {
                let repo = s("repositoryId").trim_start_matches("REPO_").to_string();
                let p = self.project_by_id(&s("projectId")).ok_or("no project")?;
                p.repos.push(repo);
                Ok(json!({"repository": {"id": "r"}}))
            }
            "updateProjectV2" => {
                let readme = s("readme");
                let p = self.project_by_id(&s("projectId")).ok_or("no project")?;
                p.readme = readme;
                Ok(json!({"projectV2": {"id": "p"}}))
            }
            "addProjectV2ItemById" => {
                let number: u64 = s("contentId")
                    .trim_start_matches("ISSUE_")
                    .parse()
                    .map_err(|_| "bad content id")?;
                let repo = self
                    .issues
                    .iter()
                    .find(|(_, l)| l.iter().any(|i| i.number == number))
                    .map(|(r, _)| r.clone())
                    .ok_or("no such issue")?;
                let pid = s("projectId");
                let p = self
                    .projects
                    .iter()
                    .position(|p| p.id == pid)
                    .ok_or("no project")?;
                if let Some(it) = self.projects[p]
                    .items
                    .iter()
                    .find(|i| i.issue == (repo.clone(), number))
                {
                    return Ok(json!({"item": {"id": it.id}}));
                }
                self.put_item(p, &repo, number, None);
                let id = self.projects[p]
                    .items
                    .last()
                    .map(|i| i.id.clone())
                    .unwrap_or_default();
                Ok(json!({"item": {"id": id}}))
            }
            "updateProjectV2ItemFieldValue" => {
                let (item, fid) = (s("itemId"), s("fieldId"));
                let option = input["value"]["singleSelectOptionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let p = self.project_by_id(&s("projectId")).ok_or("no project")?;
                let it = p.items.iter_mut().find(|i| i.id == item).ok_or("no item")?;
                it.values.insert(fid, option);
                Ok(json!({"projectV2Item": {"id": item}}))
            }
            other => Err(format!("unexpected mutation {other}")),
        }
    }
}

impl CommandRunner for FakeGh {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().map(|a| a.to_string()));
        self.calls.borrow_mut().push(call.clone());
        let joined = call.join(" ");
        let scripted = {
            let mut fail = self.fail.borrow_mut();
            fail.iter()
                .position(|(needle, _)| joined.contains(needle.as_str()))
                .map(|i| fail.remove(i).1)
        };
        if let Some(stderr) = scripted {
            return Ok(failed(&stderr));
        }
        let mut sim = self.sim.borrow_mut();
        Ok(match (program, args) {
            ("git", ["rev-parse", ..]) => ok("/repo\n"),
            ("git", [_, _, "status", ..]) => ok(if sim.dirty {
                " M issue-state.yaml\n"
            } else {
                ""
            }),
            ("gh", ["repo", "view", ..]) if sim.checkout.is_empty() => ok("o/r\n"),
            ("gh", ["repo", "view", ..]) => ok(format!("{}\n", sim.checkout)),
            ("gh", ["api", "--include", _]) => {
                let (stdout, success) = sim.probe.clone();
                CommandOutput {
                    success,
                    stdout,
                    stderr: if success {
                        String::new()
                    } else {
                        "HTTP 403".into()
                    },
                }
            }
            ("gh", ["api", "graphql", ..]) => {
                let q = query_arg(&call).unwrap_or_default();
                if q.trim_start().starts_with("mutation") {
                    let field = mutation_field(&q);
                    match sim.write(&field, &mutation_input(&q)) {
                        Ok(v) => ok(json!({"data": {field: v}}).to_string()),
                        Err(e) => failed(&format!("gh: {e}")),
                    }
                } else {
                    let name = q
                        .trim_start()
                        .trim_start_matches("query")
                        .trim_start()
                        .split(['(', ' '])
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    let all = q.contains("archivedStates: [ARCHIVED, NOT_ARCHIVED]");
                    ok(json!({"data": sim.read(&name, &vars(&call), all)}).to_string())
                }
            }
            (
                "gh",
                [
                    "project",
                    "item-add",
                    number,
                    "--owner",
                    owner,
                    "--url",
                    url,
                ],
            ) => {
                let n: u64 = url
                    .rsplit('/')
                    .next()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0);
                let num: u64 = number.parse().unwrap_or(0);
                let Some(p) = sim
                    .projects
                    .iter()
                    .position(|p| p.owner == *owner && p.number == num)
                else {
                    return Ok(failed(&format!(
                        "Could not resolve to a ProjectV2 with the number {num}."
                    )));
                };
                sim.put_item(p, "o/r", n, None);
                ok("{}")
            }
            ("gh", ["issue", "view", n, ..]) => {
                let n: u64 = n.parse().unwrap_or(0);
                let bodies = sim.comments.get(&n).cloned().unwrap_or_default();
                let list: Vec<Value> = bodies.iter().map(|b| json!({"body": b})).collect();
                ok(json!({"comments": list}).to_string())
            }
            ("gh", ["issue", "comment", n, "--repo", _, "--body", body]) => {
                let n: u64 = n.parse().unwrap_or(0);
                sim.comments.entry(n).or_default().push((*body).to_string());
                ok("")
            }
            _ => panic!("unexpected call: {joined}"),
        })
    }
}
