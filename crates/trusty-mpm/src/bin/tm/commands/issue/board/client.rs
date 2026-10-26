//! The one place `tm issue board` reaches GitHub (#10276, epic #10274 D2).
//!
//! Why: every board verb, `tm issue epic create`'s attach and the issue audit's
//! board row share one transport, so a fake `gh` in the tests sees every call
//! and a reviewer can read every mutation in one file. #9400 and #9401 extend
//! this module rather than adding a second Projects client.
//! What: [`BoardClient`] over the shared [`CommandRunner`] seam. Reads run as
//! `gh api graphql -f query=<named query>` with scalar variables; mutations run
//! as `gh api graphql -f query=mutation { <field>(input: <literal>) … }`, with
//! the input rendered by [`Gql`] in the shapes the Phase 2 trial proved. The
//! issue attach is `gh project item-add` (#7952). The token probe reads the
//! `X-OAuth-Scopes` and `X-GitHub-SSO` response headers of one REST read and
//! never the token value.
//! Test: `board/verb_tests.rs` drives every method through the fake `gh` in
//! `board/fake_gh_tests.rs`; `gql_literals_escape_strings_and_keep_enums_bare_10276`.

use serde_json::Value;

use super::error::BoardError;
use super::spec::OptionColor;
use crate::commands::ticket::runner::CommandRunner;

/// A GraphQL input literal.
///
/// Why: option lists and visible-field lists are lists of objects, which `gh`'s
/// `-F` flags cannot express; the trial passed them as inline literals, so the
/// client does the same.
/// What: a string (escaped), a bare enum value, a list or an object, rendered
/// by [`Gql::render`].
/// Test: `gql_literals_escape_strings_and_keep_enums_bare_10276`.
#[derive(Debug, Clone)]
pub(crate) enum Gql {
    /// A quoted string.
    Str(String),
    /// A bare enum value (`GRAY`, `TABLE_LAYOUT`).
    Enum(String),
    /// A list.
    List(Vec<Gql>),
    /// An input object, keys in order.
    Obj(Vec<(&'static str, Gql)>),
}

impl Gql {
    /// A string literal.
    pub(crate) fn s(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// Render the literal as GraphQL source.
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Str(s) => quote(s),
            Self::Enum(e) => e.clone(),
            Self::List(items) => {
                let inner: Vec<String> = items.iter().map(Gql::render).collect();
                format!("[{}]", inner.join(", "))
            }
            Self::Obj(fields) => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.render()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

/// A GraphQL string literal: JSON-style escapes, which GraphQL shares.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The GitHub enum spelling of an option color.
pub(crate) fn color_name(color: OptionColor) -> &'static str {
    match color {
        OptionColor::Gray => "GRAY",
        OptionColor::Blue => "BLUE",
        OptionColor::Green => "GREEN",
        OptionColor::Yellow => "YELLOW",
        OptionColor::Orange => "ORANGE",
        OptionColor::Red => "RED",
        OptionColor::Pink => "PINK",
        OptionColor::Purple => "PURPLE",
    }
}

/// One single-select option as an update or create sends it.
///
/// Why: owner decision 2 — an option that already exists is sent with its
/// `id`, which is what keeps its id and every item value through the edit.
/// What: the existing `id` (`None` for a new option), `name`, `color` and
/// `description`.
/// Test: `apply_sends_every_existing_option_id_on_an_option_edit_10276`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OptionInput {
    /// The board's id for an existing option.
    pub(crate) id: Option<String>,
    /// Option name.
    pub(crate) name: String,
    /// Option color.
    pub(crate) color: OptionColor,
    /// Option description.
    pub(crate) description: String,
}

impl OptionInput {
    fn literal(&self) -> Gql {
        let mut fields = Vec::with_capacity(4);
        if let Some(id) = &self.id {
            fields.push(("id", Gql::s(id.clone())));
        }
        fields.push(("name", Gql::s(self.name.clone())));
        fields.push(("color", Gql::Enum(color_name(self.color).to_string())));
        fields.push(("description", Gql::s(self.description.clone())));
        Gql::Obj(fields)
    }
}

/// What the token probe read from the response headers.
///
/// Why: AC5 — `check` reports the token type and scopes from `X-OAuth-Scopes`
/// and SSO authorisation from a 403 carrying `X-GitHub-SSO`, never the token.
/// What: the HTTP status, the scopes (`None` when the header is absent, which
/// is what a fine-grained or App token sends), and the SSO header value.
/// Test: `check_reports_scopes_and_sso_from_headers_10276`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TokenProbe {
    /// HTTP status of the probe, if a status line was read.
    pub(crate) status: Option<u16>,
    /// `X-OAuth-Scopes`, split on commas.
    pub(crate) scopes: Option<Vec<String>>,
    /// `X-GitHub-SSO`, verbatim.
    pub(crate) sso: Option<String>,
}

/// Read the probe's status line and the two headers from `gh api --include`.
///
/// Test: `check_reports_scopes_and_sso_from_headers_10276`.
pub(crate) fn parse_probe(stdout: &str) -> TokenProbe {
    let mut probe = TokenProbe::default();
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("HTTP/") {
            probe.status = rest.split_whitespace().nth(1).and_then(|s| s.parse().ok());
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("x-oauth-scopes") {
            probe.scopes = Some(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
        } else if name.eq_ignore_ascii_case("x-github-sso") {
            probe.sso = Some(value.to_string());
        }
    }
    probe
}

/// The `gh`-backed Projects client.
///
/// Why: see the module doc.
/// What: borrows a runner; every method is one `gh` call.
/// Test: see the module doc.
pub(crate) struct BoardClient<'a> {
    runner: &'a dyn CommandRunner,
}

impl<'a> BoardClient<'a> {
    /// A client over `runner`.
    pub(crate) fn new(runner: &'a dyn CommandRunner) -> Self {
        Self { runner }
    }

    /// Run `program` and return stdout, or [`BoardError::Command`].
    pub(crate) fn run(
        &self,
        program: &str,
        args: &[&str],
        what: &str,
    ) -> Result<String, BoardError> {
        let out = self
            .runner
            .run(program, args)
            .map_err(|e| BoardError::Command {
                what: what.to_string(),
                detail: format!("{e:#}"),
            })?;
        if out.success {
            return Ok(out.stdout);
        }
        let detail = match (out.stderr.trim(), out.stdout.trim()) {
            ("", "") => "no output".to_string(),
            ("", stdout) => stdout.to_string(),
            (stderr, _) => stderr.to_string(),
        };
        Err(BoardError::Command {
            what: what.to_string(),
            detail,
        })
    }

    /// Run a named read query with scalar variables; return `data`.
    ///
    /// What: string variables go as `-f name=value`, integers as
    /// `-F name=value` (gh types them). A `None` value is omitted, which
    /// GraphQL reads as null — how the first page of a paged read is asked for.
    pub(crate) fn query(
        &self,
        name: &str,
        doc: &str,
        strings: &[(&str, Option<&str>)],
        ints: &[(&str, u64)],
    ) -> Result<Value, BoardError> {
        let what = format!("gh api graphql ({name})");
        let mut owned: Vec<String> = vec![
            "api".into(),
            "graphql".into(),
            "-f".into(),
            format!("query={doc}"),
        ];
        for (k, v) in strings {
            if let Some(v) = v {
                owned.push("-f".into());
                owned.push(format!("{k}={v}"));
            }
        }
        for (k, v) in ints {
            owned.push("-F".into());
            owned.push(format!("{k}={v}"));
        }
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let stdout = self.run("gh", &args, &what)?;
        data_of(&stdout, &what)
    }

    /// Run one mutation `field(input: <input>) { <selection> }`; return the
    /// field's payload.
    ///
    /// Why: every write the board verbs make goes through here, so "no
    /// mutation call" is one predicate in the tests: a `query=` argument
    /// that opens with `mutation`.
    pub(crate) fn mutate(
        &self,
        field: &str,
        input: Gql,
        selection: &str,
    ) -> Result<Value, BoardError> {
        let what = format!("gh api graphql ({field})");
        let doc = format!(
            "mutation {{ {field}(input: {}) {{ {selection} }} }}",
            input.render()
        );
        let query = format!("query={doc}");
        let stdout = self.run("gh", &["api", "graphql", "-f", &query], &what)?;
        let data = data_of(&stdout, &what)?;
        Ok(data.get(field).cloned().unwrap_or(Value::Null))
    }

    /// `createProjectV2` — returns the new project's node id and number.
    pub(crate) fn create_project(
        &self,
        owner_id: &str,
        title: &str,
    ) -> Result<(String, u64), BoardError> {
        let input = Gql::Obj(vec![
            ("ownerId", Gql::s(owner_id)),
            ("title", Gql::s(title)),
        ]);
        let out = self.mutate("createProjectV2", input, "projectV2 { id number url }")?;
        let project = &out["projectV2"];
        match (project["id"].as_str(), project["number"].as_u64()) {
            (Some(id), Some(number)) => Ok((id.to_string(), number)),
            _ => Err(BoardError::Parse {
                what: "createProjectV2".into(),
                detail: format!("no project id and number in {out}"),
            }),
        }
    }

    /// `updateProjectV2Field` with the full option list.
    ///
    /// Owner decision 2: `options` carries every kept option's id; the caller
    /// has already refused to drop an option an item uses.
    pub(crate) fn update_options(
        &self,
        field_id: &str,
        options: &[OptionInput],
    ) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![
            ("fieldId", Gql::s(field_id)),
            (
                "singleSelectOptions",
                Gql::List(options.iter().map(OptionInput::literal).collect()),
            ),
        ]);
        self.mutate(
            "updateProjectV2Field",
            input,
            "projectV2Field { ... on ProjectV2SingleSelectField { id } }",
        )?;
        Ok(())
    }

    /// `createProjectV2Field` — a single-select (with options) or a date.
    pub(crate) fn create_field(
        &self,
        project_id: &str,
        name: &str,
        data_type: &str,
        options: &[OptionInput],
    ) -> Result<(), BoardError> {
        let mut input = vec![
            ("projectId", Gql::s(project_id)),
            ("dataType", Gql::Enum(data_type.to_string())),
            ("name", Gql::s(name)),
        ];
        if !options.is_empty() {
            input.push((
                "singleSelectOptions",
                Gql::List(options.iter().map(OptionInput::literal).collect()),
            ));
        }
        self.mutate(
            "createProjectV2Field",
            Gql::Obj(input),
            "projectV2Field { ... on ProjectV2FieldCommon { id } }",
        )?;
        Ok(())
    }

    /// `createProjectV2View`; `visible` is `None` for a ROADMAP view, which
    /// refuses visible fields. Returns the view's node id.
    pub(crate) fn create_view(
        &self,
        project_id: &str,
        name: &str,
        layout: &str,
        visible: Option<&[String]>,
    ) -> Result<String, BoardError> {
        let mut input = vec![
            ("projectId", Gql::s(project_id)),
            ("name", Gql::s(name)),
            ("layout", Gql::Enum(layout.to_string())),
        ];
        if let Some(ids) = visible {
            let ids = ids.iter().map(|id| Gql::s(id.clone())).collect();
            input.push((
                "configuration",
                Gql::Obj(vec![("visibleFieldIds", Gql::List(ids))]),
            ));
        }
        let out = self.mutate(
            "createProjectV2View",
            Gql::Obj(input),
            "projectV2View { id }",
        )?;
        out["projectV2View"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BoardError::Parse {
                what: "createProjectV2View".into(),
                detail: format!("no view id in {out}"),
            })
    }

    /// `updateProjectV2View` setting the filter (create takes none).
    pub(crate) fn set_view_filter(&self, view_id: &str, filter: &str) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![
            ("viewId", Gql::s(view_id)),
            ("filter", Gql::s(filter)),
        ]);
        self.mutate("updateProjectV2View", input, "projectV2View { id }")?;
        Ok(())
    }

    /// `deleteProjectV2View` — `init` only, for GitHub's default view.
    pub(crate) fn delete_view(&self, view_id: &str) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![("viewId", Gql::s(view_id))]);
        self.mutate("deleteProjectV2View", input, "projectV2View { id }")?;
        Ok(())
    }

    /// `linkProjectV2ToRepository`.
    pub(crate) fn link_repo(&self, project_id: &str, repo_id: &str) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![
            ("projectId", Gql::s(project_id)),
            ("repositoryId", Gql::s(repo_id)),
        ]);
        self.mutate("linkProjectV2ToRepository", input, "repository { id }")?;
        Ok(())
    }

    /// `updateProjectV2` setting the README.
    pub(crate) fn set_readme(&self, project_id: &str, readme: &str) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![
            ("projectId", Gql::s(project_id)),
            ("readme", Gql::s(readme)),
        ]);
        self.mutate("updateProjectV2", input, "projectV2 { id }")?;
        Ok(())
    }

    /// `addProjectV2ItemById` — returns the item id (an existing item's id
    /// when the issue is already on the board).
    pub(crate) fn add_item(
        &self,
        project_id: &str,
        content_id: &str,
    ) -> Result<String, BoardError> {
        let input = Gql::Obj(vec![
            ("projectId", Gql::s(project_id)),
            ("contentId", Gql::s(content_id)),
        ]);
        let out = self.mutate("addProjectV2ItemById", input, "item { id }")?;
        out["item"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BoardError::Parse {
                what: "addProjectV2ItemById".into(),
                detail: format!("no item id in {out}"),
            })
    }

    /// `updateProjectV2ItemFieldValue` with a single-select option (D3: only
    /// under the explicit flag).
    pub(crate) fn set_option(
        &self,
        project_id: &str,
        item_id: &str,
        field_id: &str,
        option_id: &str,
    ) -> Result<(), BoardError> {
        let input = Gql::Obj(vec![
            ("projectId", Gql::s(project_id)),
            ("itemId", Gql::s(item_id)),
            ("fieldId", Gql::s(field_id)),
            (
                "value",
                Gql::Obj(vec![("singleSelectOptionId", Gql::s(option_id))]),
            ),
        ]);
        self.mutate(
            "updateProjectV2ItemFieldValue",
            input,
            "projectV2Item { id }",
        )?;
        Ok(())
    }

    /// Add `repo`'s issue `issue` to `owner`'s project `number`.
    ///
    /// Why: #7952 — the owner-and-number form names exactly one project, where
    /// `gh issue edit --add-project "<title>"` can exit 0 and attach nothing.
    /// What: `gh project item-add <number> --owner <owner> --url <issue url>`.
    /// Test: `add_uses_the_project_block_and_ignores_default_project_10276`.
    pub(crate) fn item_add(
        &self,
        repo: &str,
        issue: u64,
        owner: &str,
        number: u64,
    ) -> Result<(), BoardError> {
        let project = number.to_string();
        let url = format!("https://github.com/{repo}/issues/{issue}");
        let args = [
            "project", "item-add", &project, "--owner", owner, "--url", &url,
        ];
        self.run("gh", &args, "gh project item-add")?;
        Ok(())
    }

    /// Read the token's scopes and SSO state from one REST read's headers.
    ///
    /// What: `gh api --include repos/<repo>`. A 403 exits non-zero but still
    /// prints its headers, so the headers are read whatever the exit status;
    /// a probe that printed no status line is an error.
    pub(crate) fn probe_token(&self, repo: &str) -> Result<TokenProbe, BoardError> {
        let endpoint = format!("repos/{repo}");
        let what = "gh api --include repos/<repo>";
        let out = self
            .runner
            .run("gh", &["api", "--include", &endpoint])
            .map_err(|e| BoardError::Command {
                what: what.into(),
                detail: format!("{e:#}"),
            })?;
        let probe = parse_probe(&out.stdout);
        if probe.status.is_none() {
            return Err(BoardError::Command {
                what: what.into(),
                detail: out.stderr.trim().to_string(),
            });
        }
        Ok(probe)
    }
}

/// Parse `gh api graphql` stdout and return `data`, refusing an `errors` list.
fn data_of(stdout: &str, what: &str) -> Result<Value, BoardError> {
    let parsed: Value = serde_json::from_str(stdout).map_err(|e| BoardError::Parse {
        what: what.to_string(),
        detail: e.to_string(),
    })?;
    if let Some(errors) = parsed.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let messages: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect();
        return Err(BoardError::Command {
            what: what.to_string(),
            detail: messages.join("; "),
        });
    }
    Ok(parsed.get("data").cloned().unwrap_or(Value::Null))
}
