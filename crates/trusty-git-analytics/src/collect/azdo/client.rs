//! Azure DevOps client — Phases 2–6.
//!
//! Implements an authenticated HTTP session against the Azure DevOps REST API
//! (`api-version=7.1`) with the following endpoints:
//!
//! * [`AzureDevOpsClient::test_connection`] — `GET _apis/connectionData`
//! * [`AzureDevOpsClient::get_projects`]    — `GET _apis/projects`
//! * [`AzureDevOpsClient::get_work_item_types`] — `GET {proj}/_apis/wit/workitemtypes` (Phase 3)
//! * [`AzureDevOpsClient::get_fields`]      — `GET {proj}/_apis/wit/fields` (Phase 3)
//! * [`AzureDevOpsClient::run_wiql`]        — `POST {proj}/_apis/wit/wiql` (Phase 4)
//! * [`AzureDevOpsClient::get_recent_work_item_ids`] — convenience WIQL (Phase 4)
//! * [`AzureDevOpsClient::get_work_items`]  — `POST _apis/wit/workitemsbatch` (Phase 5)
//!
//! Authentication uses HTTP Basic with an empty username and the PAT as the
//! password — the standard ADO convention.

use serde::{Deserialize, Serialize};

use crate::core::config::AzureDevOpsConfig;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the Azure DevOps client.
#[derive(Debug, thiserror::Error)]
pub enum AzdoError {
    /// The method is not yet implemented. `phase` indicates the planned
    /// phase number (e.g. 6 for work items).
    #[error("not implemented: {method} is planned for Phase {phase}")]
    NotImplemented {
        /// Name of the method that would have performed work.
        method: String,
        /// Phase number in which this method will be implemented.
        phase: u32,
    },

    /// Credentials were rejected at the format-validation stage.
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    /// The configured URL is malformed or not an Azure DevOps URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// HTTP request returned an unhandled status code.
    #[error("HTTP error {status}: {message}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body or reason phrase.
        message: String,
    },

    /// HTTP 401 — PAT is missing, malformed, or rejected by ADO.
    #[error("authentication failed (401): check PAT and organisation URL")]
    Unauthorized,

    /// HTTP 403 — PAT is valid but lacks scope for the requested resource.
    #[error("access denied (403): PAT lacks required scope")]
    Forbidden,

    /// HTTP 404 — the requested resource was not found.
    ///
    /// This can mean the organisation URL is wrong, the project does not
    /// exist, a referenced work item ID is missing, or credentials don't
    /// grant visibility. The endpoint context is not threaded through this
    /// variant — callers should consult the failing endpoint to disambiguate.
    #[error("resource not found (404): check organisation, project, and credentials")]
    NotFound,

    /// Transport-level failure (DNS, TLS, timeout, connection reset, ...).
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),

    /// Response body could not be parsed as the expected JSON shape.
    #[error("response parse error: {0}")]
    Parse(String),

    /// Azure DevOps configuration failed validation at fetcher
    /// construction time: both `project` and `projects` are empty/blank.
    ///
    /// Returning this from `AdoPrFetcher::new` is the load-bearing check
    /// that prevents a misconfigured fetcher from silently returning
    /// `Ok(None)` for every PR (issue #91 regression guard).
    #[error("invalid Azure DevOps configuration: {0}")]
    Config(String),
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of an Azure DevOps connection probe.
///
/// `status` is `"connected"` on a successful `GET _apis/connectionData`,
/// `"failed"` if the probe completed but ADO returned a non-success status
/// (this variant is not currently returned — failures bubble as `AzdoError`
/// instead), or `"stub"` if produced by [`AzureDevOpsClient::test_connection_stub`].
#[derive(Debug, Clone, Serialize)]
pub struct AzdoConnectionInfo {
    /// Probe status: `"connected"`, `"failed"`, or `"stub"`.
    pub status: String,
    /// Phase that produced this result.
    pub phase: u32,
    /// Organisation URL echoed back from config.
    pub organization_url: String,
    /// Human-readable note about the probe outcome.
    pub message: String,
    /// Authenticated user GUID (Phase 2+, present on success).
    pub user_id: Option<String>,
    /// Authenticated user display name (Phase 2+, present on success).
    pub user_name: Option<String>,
    /// ADO instance GUID (Phase 2+, present on success).
    pub instance_id: Option<String>,
}

/// ADO work item (Phase 5 — batch fetch shape).
///
/// Populated from `POST _apis/wit/workitemsbatch` using the field projection
/// listed in [`AzureDevOpsClient::get_work_items`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoWorkItem {
    /// ADO work-item integer ID (the `N` in `AB#N`).
    pub id: u32,
    /// `System.Title`.
    pub title: String,
    /// `System.State` (e.g. `"Active"`, `"Closed"`).
    pub state: String,
    /// `System.WorkItemType` (e.g. `"Bug"`, `"User Story"`, `"Task"`).
    pub work_item_type: String,
    /// `System.Tags` — split on `; ` and trimmed. Empty if no tags.
    pub tags: Vec<String>,
    /// `System.TeamProject`.
    pub team_project: String,
    /// Self URL from ADO (if present in response).
    pub url: Option<String>,
    /// `System.IterationPath` — the sprint/iteration the item lives in.
    /// `None` if ADO did not return the field (some older work items predate
    /// the iteration model).
    #[serde(default)]
    pub iteration_path: Option<String>,
    /// `System.AreaPath` — the team/area the item is owned by.
    #[serde(default)]
    pub area_path: Option<String>,
}

/// Backwards-compatible alias for the original Phase-1 placeholder type.
///
/// `WorkItem` was the stub struct used before Phase 5 batch fetch existed.
/// New code should prefer [`AzdoWorkItem`].
pub type WorkItem = AzdoWorkItem;

/// ADO work item type descriptor (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoWorkItemType {
    /// Display name (e.g. `"User Story"`).
    pub name: String,
    /// Stable reference name (e.g. `"Microsoft.VSTS.WorkItemTypes.UserStory"`).
    pub reference_name: String,
    /// Human-readable description, if provided by ADO.
    pub description: String,
    /// Hex color (e.g. `"009CCC"`), without leading `#`.
    pub color: String,
    /// Icon identifier, if provided by ADO.
    pub icon: String,
}

/// ADO field descriptor (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoField {
    /// Display name (e.g. `"Title"`).
    pub name: String,
    /// Stable reference name (e.g. `"System.Title"`).
    pub reference_name: String,
    /// Field type as reported by ADO (e.g. `"string"`, `"integer"`,
    /// `"dateTime"`, `"html"`).
    pub field_type: String,
}

/// Reference to a work item returned by a WIQL query (Phase 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemRef {
    /// Work item ID.
    pub id: u32,
    /// Self URL.
    pub url: String,
}

/// Result of a WIQL query (Phase 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiqlResult {
    /// `"flat"`, `"oneHop"`, or `"tree"` as reported by ADO.
    pub query_type: String,
    /// Returned work-item references.
    pub work_items: Vec<WorkItemRef>,
}

/// ADO iteration / sprint descriptor (Phase 4 ext).
///
/// Returned by `GET {org}/{project}/_apis/work/teamsettings/iterations`.
/// Dates are kept as ISO 8601 strings rather than `chrono` types to keep
/// the wire format faithful and to avoid timezone-translation surprises;
/// callers that need typed dates can parse on the way out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AzdoIteration {
    /// Iteration GUID. Globally unique across the org.
    pub id: String,
    /// Display name (e.g. `"Sprint 23"`).
    pub name: String,
    /// Iteration path (e.g. `"MyProject\\Release 1\\Sprint 23"`).
    pub path: String,
    /// ISO 8601 start date, if scheduled.
    pub start_date: Option<String>,
    /// ISO 8601 finish date, if scheduled.
    pub finish_date: Option<String>,
    /// Time frame as reported by ADO: `"current"`, `"past"`, or `"future"`.
    pub time_frame: String,
}

/// ADO user descriptor (Phase 4 ext).
///
/// Returned by the Graph API:
/// `GET https://vssps.dev.azure.com/{org}/_apis/graph/users`. Requires
/// the `vso.graph` PAT scope. `mail_address` and `principal_name` are
/// optional because external (Live ID) users may not expose them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AzdoUser {
    /// Stable subject descriptor (the Graph API's primary identifier).
    pub descriptor: String,
    /// Display name as shown in the ADO UI.
    pub display_name: String,
    /// Primary email, if visible.
    pub mail_address: Option<String>,
    /// UPN / login name (e.g. `"alice@contoso.com"`), if visible.
    pub principal_name: Option<String>,
}

/// ADO work item comment (Phase 5 ext).
///
/// Returned by
/// `GET {org}/{project}/_apis/wit/workItems/{id}/comments?api-version=7.1-preview.3`.
/// The `text` field may contain HTML formatting as authored in the ADO UI;
/// callers that need plain text should strip tags downstream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AzdoComment {
    /// Comment ID — unique within the work item.
    pub id: u32,
    /// Comment body (may contain HTML markup).
    pub text: String,
    /// Display name of the user who created the comment.
    pub created_by: String,
    /// ISO 8601 creation timestamp.
    pub created_date: String,
}

/// Extended ADO work item with iteration/area paths and arbitrary custom
/// fields (Phase 5 ext).
///
/// Fetched via
/// `GET {org}/{project}/_apis/wit/workitems/{id}?$expand=all&api-version=7.1`.
/// Unlike [`AzdoWorkItem`] (which projects a known field set via the batch
/// endpoint), this shape preserves any process-template / org-specific
/// fields in [`Self::custom_fields`] so callers can read them dynamically.
///
/// `custom_fields` contains every `fields.*` entry from the ADO response
/// that is **not** one of the standard fields surfaced as named struct
/// fields (`System.Id`, `System.Title`, `System.State`,
/// `System.WorkItemType`, `System.Tags`, `System.IterationPath`,
/// `System.AreaPath`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoWorkItemExtended {
    /// ADO work-item integer ID.
    pub id: u32,
    /// `System.Title`.
    pub title: String,
    /// `System.State`.
    pub state: String,
    /// `System.WorkItemType`.
    pub work_item_type: String,
    /// `System.IterationPath` — the sprint/iteration this item lives in.
    pub iteration_path: Option<String>,
    /// `System.AreaPath` — the team/area this item is owned by.
    pub area_path: Option<String>,
    /// `System.Tags` — split on `; ` and trimmed.
    pub tags: Vec<String>,
    /// All non-standard `fields.*` entries from the ADO response, keyed by
    /// reference name (e.g. `"Microsoft.VSTS.Common.Priority"`).
    pub custom_fields: std::collections::HashMap<String, serde_json::Value>,
}

/// ADO project descriptor (Phase 2 — list-projects shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoProject {
    /// ADO project GUID.
    pub id: String,
    /// Project display name.
    pub name: String,
    /// Lifecycle state — `"wellFormed"`, `"createPending"`, `"deleting"`, ...
    pub state: String,
    /// Visibility — `"private"` or `"public"`.
    pub visibility: String,
}

// ---------------------------------------------------------------------------
// Internal response shapes (ADO REST API, partial)
// ---------------------------------------------------------------------------

/// ADO `_apis/connectionData` response (partial — only fields tga uses).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDataResponse {
    authenticated_user: AuthenticatedUser,
    instance_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    deployment_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticatedUser {
    id: String,
    provider_display_name: String,
}

/// ADO `_apis/projects` response envelope.
#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    #[allow(dead_code)]
    count: u32,
    value: Vec<AzdoProjectRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzdoProjectRaw {
    id: String,
    name: String,
    state: String,
    visibility: String,
}

/// Generic ADO list envelope: `{ "count": N, "value": [...] }`.
#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    #[allow(dead_code)]
    #[serde(default)]
    count: u32,
    value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemTypeRaw {
    name: String,
    reference_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    icon: IconRaw,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IconRaw {
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FieldRaw {
    name: String,
    reference_name: String,
    #[serde(rename = "type", default)]
    field_type: String,
}

/// WIQL response envelope (Phase 4).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WiqlResponseRaw {
    #[serde(default)]
    query_type: String,
    #[serde(default)]
    work_items: Vec<WorkItemRefRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemRefRaw {
    id: u32,
    #[serde(default)]
    url: String,
}

/// Work-items-batch response envelope (Phase 5).
#[derive(Debug, Deserialize)]
struct WorkItemBatchResponse {
    #[allow(dead_code)]
    #[serde(default)]
    count: u32,
    value: Vec<WorkItemRaw>,
}

#[derive(Debug, Deserialize)]
struct WorkItemRaw {
    id: u32,
    #[serde(default)]
    fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    url: Option<String>,
}

/// Iteration list response (Phase 4 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IterationRaw {
    id: String,
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    attributes: IterationAttributesRaw,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IterationAttributesRaw {
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    finish_date: Option<String>,
    #[serde(default)]
    time_frame: String,
}

/// Work-item comments response (Phase 5 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentsResponse {
    #[serde(default)]
    comments: Vec<CommentRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentRaw {
    id: u32,
    #[serde(default)]
    text: String,
    #[serde(default)]
    created_by: IdentityRefRaw,
    #[serde(default)]
    created_date: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IdentityRefRaw {
    #[serde(default)]
    display_name: String,
}

/// Work-item single-fetch response with `relations` expanded (Phase 5 ext).
#[derive(Debug, Deserialize)]
struct WorkItemRelationsResponse {
    #[serde(default)]
    relations: Vec<WorkItemRelationRaw>,
}

#[derive(Debug, Deserialize)]
struct WorkItemRelationRaw {
    #[serde(default)]
    rel: String,
    #[serde(default)]
    url: String,
    /// Relation attributes (e.g. `name: "Fixed in Commit"`). Currently
    /// unused by [`extract_commit_shas_from_relations`] — we identify
    /// commit links via the `vstfs:///Git/Commit/` URL scheme — but we
    /// deserialize the field to validate the wire format and to keep the
    /// door open for filtering by `attributes.name` in the future.
    #[serde(default)]
    #[allow(dead_code)]
    attributes: serde_json::Map<String, serde_json::Value>,
}

/// Graph user response (Phase 4 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserRaw {
    #[serde(default)]
    descriptor: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    mail_address: Option<String>,
    #[serde(default)]
    principal_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Azure DevOps client. Holds config + a lazily built `reqwest::Client`.
pub struct AzureDevOpsClient {
    config: AzureDevOpsConfig,
}

/// Percent-encode a single path segment (e.g. an ADO project name).
///
/// Encodes any byte outside the unreserved set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) as `%HH`. This is conservative
/// but correct: it never produces an invalid URL, even if the project name
/// contains spaces, slashes, or non-ASCII characters.
fn encode_path_segment(s: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
    }
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Build an authenticated [`reqwest::Client`] for ADO API calls.
///
/// * Uses HTTP Basic auth with an empty username and `pat` as the password
///   via reqwest's per-request [`reqwest::RequestBuilder::basic_auth`] — no
///   `base64` dependency required.
/// * Sets a 30-second total request timeout.
/// * Identifies via `User-Agent: tga/{CARGO_PKG_VERSION}`.
fn build_client() -> Result<reqwest::Client, AzdoError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(concat!("tga/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(AzdoError::Request)
}

impl AzureDevOpsClient {
    /// Construct a new client; expands any `${ENV_VAR}` in `pat` (issue #741).
    pub fn new(mut config: AzureDevOpsConfig) -> Self {
        config.pat = crate::collect::env_expand::expand_env_var(&config.pat);
        Self { config }
    }

    /// Borrow the underlying config.
    pub fn config(&self) -> &AzureDevOpsConfig {
        &self.config
    }

    /// Validate credentials format only — no HTTP probe.
    ///
    /// # Errors
    ///
    /// Returns [`AzdoError::InvalidCredentials`] if the PAT is empty or
    /// whitespace-only.
    pub fn validate_credentials(&self) -> Result<(), AzdoError> {
        if self.config.pat.trim().is_empty() {
            return Err(AzdoError::InvalidCredentials(
                "PAT is empty (a non-empty PAT is required)".into(),
            ));
        }
        Ok(())
    }

    /// Phase 1 stub retained for tests that must not touch the network.
    ///
    /// Phase 2 callers should prefer [`Self::test_connection`].
    pub fn test_connection_stub(&self) -> AzdoConnectionInfo {
        AzdoConnectionInfo {
            status: "stub".to_string(),
            phase: 1,
            organization_url: self.config.organization_url.clone(),
            message: "stub probe — call test_connection() for a real check".to_string(),
            user_id: None,
            user_name: None,
            instance_id: None,
        }
    }

    /// Trim a trailing slash from `organization_url` (if any).
    fn org_url(&self) -> &str {
        self.config.organization_url.trim_end_matches('/')
    }

    /// Test connection by calling `GET _apis/connectionData`.
    ///
    /// Returns [`AzdoConnectionInfo`] with `status = "connected"` on success,
    /// populated with the authenticated user identity and instance GUID.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::InvalidCredentials`] — empty PAT (pre-flight check).
    /// * [`AzdoError::Unauthorized`] — HTTP 401 (invalid PAT).
    /// * [`AzdoError::Forbidden`] — HTTP 403 (PAT lacks scope).
    /// * [`AzdoError::NotFound`] — HTTP 404 (wrong organisation URL).
    /// * [`AzdoError::Http`] — any other non-2xx response.
    /// * [`AzdoError::Request`] — transport failure (network, TLS, timeout).
    /// * [`AzdoError::Parse`] — response body did not match expected shape.
    pub async fn test_connection(&self) -> Result<AzdoConnectionInfo, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/_apis/connectionData?connectOptions=none&api-version=7.1-preview.1",
            self.org_url()
        );

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        let status = resp.status();
        match status.as_u16() {
            200 => {
                let body: ConnectionDataResponse = resp
                    .json()
                    .await
                    .map_err(|e| AzdoError::Parse(e.to_string()))?;
                Ok(AzdoConnectionInfo {
                    status: "connected".to_string(),
                    phase: 2,
                    organization_url: self.config.organization_url.clone(),
                    message: format!(
                        "authenticated as {} (instance {})",
                        body.authenticated_user.provider_display_name, body.instance_id
                    ),
                    user_id: Some(body.authenticated_user.id),
                    user_name: Some(body.authenticated_user.provider_display_name),
                    instance_id: Some(body.instance_id),
                })
            }
            401 => Err(AzdoError::Unauthorized),
            403 => Err(AzdoError::Forbidden),
            404 => Err(AzdoError::NotFound),
            s => {
                let message = resp.text().await.unwrap_or_default();
                Err(AzdoError::Http { status: s, message })
            }
        }
    }

    /// List ADO projects via `GET _apis/projects`.
    ///
    /// Returns up to 100 projects in a single page. Phase 4 will add
    /// continuation-token pagination.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_projects(&self) -> Result<Vec<AzdoProject>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!("{}/_apis/projects?api-version=7.1&$top=100", self.org_url());

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        let status = resp.status();
        match status.as_u16() {
            200 => {
                let body: ProjectsResponse = resp
                    .json()
                    .await
                    .map_err(|e| AzdoError::Parse(e.to_string()))?;
                let projects = body
                    .value
                    .into_iter()
                    .map(|p| AzdoProject {
                        id: p.id,
                        name: p.name,
                        state: p.state,
                        visibility: p.visibility,
                    })
                    .collect();
                Ok(projects)
            }
            401 => Err(AzdoError::Unauthorized),
            403 => Err(AzdoError::Forbidden),
            404 => Err(AzdoError::NotFound),
            s => {
                let message = resp.text().await.unwrap_or_default();
                Err(AzdoError::Http { status: s, message })
            }
        }
    }

    /// Map an HTTP status to the standard [`AzdoError`] variants used by all
    /// ADO endpoints. 401/403/404 get dedicated variants; everything else
    /// falls back to [`AzdoError::Http`].
    async fn map_status(resp: reqwest::Response) -> AzdoError {
        let status = resp.status().as_u16();
        match status {
            401 => AzdoError::Unauthorized,
            403 => AzdoError::Forbidden,
            404 => AzdoError::NotFound,
            s => {
                let message = resp.text().await.unwrap_or_default();
                AzdoError::Http { status: s, message }
            }
        }
    }

    /// List work-item types available in a project (Phase 3).
    ///
    /// Calls `GET {org}/{project}/_apis/wit/workitemtypes?api-version=7.1`.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_work_item_types(
        &self,
        project: &str,
    ) -> Result<Vec<AzdoWorkItemType>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/workitemtypes?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "GET work item types");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<WorkItemTypeRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|t| AzdoWorkItemType {
                name: t.name,
                reference_name: t.reference_name,
                description: t.description,
                color: t.color,
                icon: t.icon.id,
            })
            .collect())
    }

    /// List fields available in a project (Phase 3).
    ///
    /// Calls `GET {org}/{project}/_apis/wit/fields?api-version=7.1`.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_fields(&self, project: &str) -> Result<Vec<AzdoField>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/fields?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "GET fields");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<FieldRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|f| AzdoField {
                name: f.name,
                reference_name: f.reference_name,
                field_type: f.field_type,
            })
            .collect())
    }

    /// Run a WIQL query against a project (Phase 4).
    ///
    /// Calls `POST {org}/{project}/_apis/wit/wiql?api-version=7.1` with the
    /// body `{ "query": <query> }`. Returns the list of matching work-item
    /// references (IDs + self URLs).
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn run_wiql(&self, project: &str, query: &str) -> Result<WiqlResult, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/wiql?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "POST wiql");

        let resp = client
            .post(&url)
            .basic_auth("", Some(&self.config.pat))
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: WiqlResponseRaw = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(WiqlResult {
            query_type: body.query_type,
            work_items: body
                .work_items
                .into_iter()
                .map(|w| WorkItemRef {
                    id: w.id,
                    url: w.url,
                })
                .collect(),
        })
    }

    /// Convenience helper: returns the IDs of work items modified in the last
    /// `since_days` days, ordered by `[System.ChangedDate] DESC` (Phase 4).
    ///
    /// # Errors
    ///
    /// Same set as [`Self::run_wiql`].
    pub async fn get_recent_work_item_ids(
        &self,
        project: &str,
        since_days: u32,
    ) -> Result<Vec<u32>, AzdoError> {
        let query = format!(
            "SELECT [System.Id] FROM WorkItems \
             WHERE [System.TeamProject] = @project \
             AND [System.ChangedDate] >= @today - {since_days} \
             ORDER BY [System.ChangedDate] DESC"
        );
        let result = self.run_wiql(project, &query).await?;
        Ok(result.work_items.into_iter().map(|w| w.id).collect())
    }

    /// Fetch work items by ID via the batch endpoint (Phase 5).
    ///
    /// Calls `POST {org}/_apis/wit/workitemsbatch?api-version=7.1`. IDs are
    /// chunked in groups of 200 (the ADO server-side limit). The projected
    /// field set is:
    ///
    /// * `System.Id`
    /// * `System.Title`
    /// * `System.State`
    /// * `System.WorkItemType`
    /// * `System.Tags`
    /// * `System.TeamProject`
    ///
    /// Returns work items in the order ADO returns them — callers that need
    /// input-order alignment should re-sort by ID.
    ///
    /// An empty `ids` slice short-circuits to `Ok(vec![])` without any HTTP
    /// traffic.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_work_items(&self, ids: &[u32]) -> Result<Vec<AzdoWorkItem>, AzdoError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/_apis/wit/workitemsbatch?api-version=7.1",
            self.org_url()
        );

        let fields = [
            "System.Id",
            "System.Title",
            "System.State",
            "System.WorkItemType",
            "System.Tags",
            "System.TeamProject",
            "System.IterationPath",
            "System.AreaPath",
        ];

        let mut all = Vec::with_capacity(ids.len());

        for chunk in ids.chunks(200) {
            tracing::debug!(url = %url, count = chunk.len(), "POST workitemsbatch");
            let resp = client
                .post(&url)
                .basic_auth("", Some(&self.config.pat))
                .json(&serde_json::json!({
                    "ids": chunk,
                    "fields": fields,
                    "errorPolicy": "omit",
                }))
                .send()
                .await?;

            if !resp.status().is_success() {
                return Err(Self::map_status(resp).await);
            }

            let body: WorkItemBatchResponse = resp
                .json()
                .await
                .map_err(|e| AzdoError::Parse(e.to_string()))?;

            // Detect IDs silently dropped by ADO's errorPolicy=omit behavior.
            // When `workitemsbatch` cannot resolve an ID (e.g., wrong project,
            // deleted, or never existed), it omits it from the response without
            // raising an error. Without this log, a misconfigured `ticket_regex`
            // is indistinguishable from a correct one.
            if body.value.len() < chunk.len() {
                let returned_ids: std::collections::HashSet<u32> =
                    body.value.iter().map(|w| w.id).collect();
                let dropped: Vec<u32> = chunk
                    .iter()
                    .copied()
                    .filter(|id| !returned_ids.contains(id))
                    .collect();
                let first_dropped: Vec<u32> = dropped.iter().take(10).copied().collect();
                tracing::debug!(
                    requested = chunk.len(),
                    returned = body.value.len(),
                    dropped = dropped.len(),
                    first_dropped = ?first_dropped,
                    "ADO workitemsbatch silently omitted some IDs (errorPolicy=omit)"
                );
            }

            for w in body.value {
                all.push(parse_work_item(w));
            }
        }

        Ok(all)
    }

    /// Fetch all iterations (sprints) for a project (Phase 4 ext).
    ///
    /// Calls
    /// `GET {org}/{project}/_apis/work/teamsettings/iterations?api-version=7.1`.
    ///
    /// Returns iterations in the order ADO returns them — typically
    /// chronological by start date but not guaranteed.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_iterations(&self, project: &str) -> Result<Vec<AzdoIteration>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/work/teamsettings/iterations?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "GET iterations");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<IterationRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|it| AzdoIteration {
                id: it.id,
                name: it.name,
                path: it.path,
                start_date: it.attributes.start_date,
                finish_date: it.attributes.finish_date,
                time_frame: it.attributes.time_frame,
            })
            .collect())
    }

    /// Fetch all users from the ADO organisation Graph API (Phase 4 ext).
    ///
    /// Calls
    /// `GET https://vssps.dev.azure.com/{org}/_apis/graph/users?api-version=7.1-preview.1`.
    /// Requires the `vso.graph` PAT scope; missing scope surfaces as
    /// [`AzdoError::Forbidden`].
    ///
    /// The Graph endpoint lives on `vssps.dev.azure.com`, not the
    /// `dev.azure.com/{org}` endpoint used by the rest of the client. The
    /// organisation slug is extracted from the configured
    /// `organization_url` — supports both `https://dev.azure.com/{org}` and
    /// `https://{org}.visualstudio.com` formats.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::InvalidUrl`] if the organisation URL is unrecognised.
    /// * Otherwise the same set as [`Self::test_connection`].
    pub async fn get_users(&self) -> Result<Vec<AzdoUser>, AzdoError> {
        self.validate_credentials()?;

        let graph_url = self.graph_users_url()?;
        let client = build_client()?;
        tracing::debug!(url = %graph_url, "GET graph users");

        let resp = client
            .get(&graph_url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<UserRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|u| AzdoUser {
                descriptor: u.descriptor,
                display_name: u.display_name,
                mail_address: u.mail_address,
                principal_name: u.principal_name,
            })
            .collect())
    }

    /// Fetch all comments for a single work item (Phase 5 ext).
    ///
    /// Calls
    /// `GET {org}/{project}/_apis/wit/workItems/{id}/comments?api-version=7.1-preview.3`.
    /// Returns comments in the order ADO returns them — typically
    /// chronological (oldest first), but this is not guaranteed.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`]. Returns
    /// [`AzdoError::NotFound`] if the work item ID does not exist or is in
    /// a different organisation.
    pub async fn get_work_item_comments(
        &self,
        project: &str,
        work_item_id: u32,
    ) -> Result<Vec<AzdoComment>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/workItems/{}/comments?api-version=7.1-preview.3",
            self.org_url(),
            encode_path_segment(project),
            work_item_id,
        );
        tracing::debug!(url = %url, "GET work item comments");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: CommentsResponse = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .comments
            .into_iter()
            .map(|c| AzdoComment {
                id: c.id,
                text: c.text,
                created_by: c.created_by.display_name,
                created_date: c.created_date,
            })
            .collect())
    }

    /// Fetch a single work item with **all** fields expanded (Phase 5 ext).
    ///
    /// Calls
    /// `GET {org}/_apis/wit/workitems/{id}?$expand=all&api-version=7.1`.
    /// Unlike [`Self::get_work_items`] (which projects a fixed field list
    /// via the batch endpoint), this returns every field on the work item,
    /// including process-template-specific custom fields.
    ///
    /// Returns `Ok(None)` if ADO returns 404 (work item deleted or wrong
    /// organisation). All other non-success statuses surface as errors.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`], except 404 is mapped to
    /// `Ok(None)` rather than [`AzdoError::NotFound`].
    pub async fn get_work_item_extended(
        &self,
        id: u32,
    ) -> Result<Option<AzdoWorkItemExtended>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/_apis/wit/workitems/{}?$expand=all&api-version=7.1",
            self.org_url(),
            id,
        );
        tracing::debug!(url = %url, "GET work item extended");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let raw: WorkItemRaw = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(Some(parse_work_item_extended(raw)))
    }

    /// Fetch the native commit-link list for a work item (Phase 5 ext).
    ///
    /// Calls
    /// `GET {org}/_apis/wit/workItems/{id}?$expand=relations&api-version=7.1`
    /// and scans `relations[]` for entries whose `rel` is
    /// `"ArtifactLink"` with attribute name `"Fixed in Commit"` or any
    /// `System.LinkTypes.Versioned*` relation. The commit SHA is extracted
    /// from the `vstfs:///Git/Commit/<projectId>%2F<repoId>%2F<sha>` URL
    /// scheme used by ADO artifact links.
    ///
    /// Returns the list of commit SHAs linked to this work item, in the
    /// order ADO returns them. Returns an empty `Vec` if the work item has
    /// no commit links, or if 404 (work item deleted).
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`], except 404 maps to
    /// `Ok(vec![])`.
    pub async fn get_work_item_commit_links(
        &self,
        project: &str,
        work_item_id: u32,
    ) -> Result<Vec<String>, AzdoError> {
        self.validate_credentials()?;
        // `project` is part of the URL for symmetry with other methods; the
        // work-item endpoint itself is org-scoped, but routing through the
        // project segment makes the request appear in the project's audit
        // log and is what ADO's own UI emits.
        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/workItems/{}?$expand=relations&api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
            work_item_id,
        );
        tracing::debug!(url = %url, "GET work item relations");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if resp.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let raw: WorkItemRelationsResponse = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(extract_commit_shas_from_relations(&raw.relations))
    }

    /// Build the Graph users URL for this client's organisation.
    ///
    /// Supports two organisation URL forms:
    /// - `https://dev.azure.com/{org}` →
    ///   `https://vssps.dev.azure.com/{org}/_apis/graph/users?...`
    /// - `https://{org}.visualstudio.com` →
    ///   `https://vssps.dev.azure.com/{org}/_apis/graph/users?...`
    ///
    /// Tests use a mock server URL (e.g. `http://127.0.0.1:1234`); in that
    /// case we route the Graph call to the same mock host with an
    /// `/_graph` prefix so wiremock can intercept it.
    fn graph_users_url(&self) -> Result<String, AzdoError> {
        let org = self.org_url();
        let lower = org.to_lowercase();
        // Test/mock fallback — wiremock servers don't have the
        // vssps subdomain, so we synthesize a path-prefixed URL on the
        // same host and let the test mount a matching mock.
        if !lower.contains("dev.azure.com") && !lower.contains(".visualstudio.com") {
            return Ok(format!("{org}/_graph/users?api-version=7.1-preview.1"));
        }
        let org_slug = if let Some(rest) = lower.strip_prefix("https://dev.azure.com/") {
            // Strip trailing slashes / query just in case.
            rest.trim_end_matches('/').split('/').next().unwrap_or("")
        } else if let Some(rest) = lower.strip_prefix("https://") {
            // {org}.visualstudio.com form.
            rest.split('.').next().unwrap_or("")
        } else {
            return Err(AzdoError::InvalidUrl(format!(
                "cannot derive org slug from {org}"
            )));
        };
        if org_slug.is_empty() {
            return Err(AzdoError::InvalidUrl(format!(
                "cannot derive org slug from {org}"
            )));
        }
        Ok(format!(
            "https://vssps.dev.azure.com/{org_slug}/_apis/graph/users?api-version=7.1-preview.1"
        ))
    }
}

/// Project a raw ADO work item (with arbitrary fields map) into our
/// flat [`AzdoWorkItem`] shape. Missing fields default to empty strings.
fn parse_work_item(raw: WorkItemRaw) -> AzdoWorkItem {
    let get_str = |key: &str| -> String {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let tags_raw = get_str("System.Tags");
    let tags = if tags_raw.is_empty() {
        Vec::new()
    } else {
        tags_raw
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let get_opt = |key: &str| -> Option<String> {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    AzdoWorkItem {
        id: raw.id,
        title: get_str("System.Title"),
        state: get_str("System.State"),
        work_item_type: get_str("System.WorkItemType"),
        tags,
        team_project: get_str("System.TeamProject"),
        url: raw.url,
        iteration_path: get_opt("System.IterationPath"),
        area_path: get_opt("System.AreaPath"),
    }
}

/// Build an [`AzdoWorkItemExtended`] from a raw single-fetch work item
/// (the `$expand=all` shape). Splits `System.Tags` on `; ` and routes
/// non-standard fields into [`AzdoWorkItemExtended::custom_fields`].
fn parse_work_item_extended(raw: WorkItemRaw) -> AzdoWorkItemExtended {
    use std::collections::HashMap;

    // The "standard" fields we surface as named struct fields; everything
    // else lands in `custom_fields`.
    const STANDARD_FIELDS: &[&str] = &[
        "System.Id",
        "System.Title",
        "System.State",
        "System.WorkItemType",
        "System.Tags",
        "System.IterationPath",
        "System.AreaPath",
    ];

    let get_str = |key: &str| -> String {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let get_opt = |key: &str| -> Option<String> {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };

    let tags_raw = get_str("System.Tags");
    let tags = if tags_raw.is_empty() {
        Vec::new()
    } else {
        tags_raw
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut custom_fields: HashMap<String, serde_json::Value> = HashMap::new();
    for (k, v) in &raw.fields {
        if !STANDARD_FIELDS.contains(&k.as_str()) {
            custom_fields.insert(k.clone(), v.clone());
        }
    }

    AzdoWorkItemExtended {
        id: raw.id,
        title: get_str("System.Title"),
        state: get_str("System.State"),
        work_item_type: get_str("System.WorkItemType"),
        iteration_path: get_opt("System.IterationPath"),
        area_path: get_opt("System.AreaPath"),
        tags,
        custom_fields,
    }
}

/// Extract commit SHAs from a list of ADO work-item relations.
///
/// ADO encodes a commit link as a relation with `rel == "ArtifactLink"`
/// and a `url` of the form
/// `vstfs:///Git/Commit/<projectId>%2F<repoId>%2F<sha>`. The SHA is the
/// segment after the second `%2F` (or `/` after URL-decoding).
fn extract_commit_shas_from_relations(relations: &[WorkItemRelationRaw]) -> Vec<String> {
    let mut out = Vec::new();
    for r in relations {
        // ADO uses `ArtifactLink` with attribute `name == "Fixed in Commit"`
        // (or "Branch", "Pull Request", ...). We accept any artifact link
        // whose URL points to `vstfs:///Git/Commit/...`. We also keep the
        // legacy `System.LinkTypes.Versioned*` `rel` values in case ADO
        // surfaces them on older work items.
        let is_artifact = r.rel.eq_ignore_ascii_case("ArtifactLink");
        let is_versioned = r.rel.starts_with("System.LinkTypes.Versioned");
        if !(is_artifact || is_versioned) {
            continue;
        }
        // Match the commit URL scheme. We accept both `%2F` (URL-encoded)
        // and `/` separators between the path segments.
        let lower = r.url.to_lowercase();
        if !lower.starts_with("vstfs:///git/commit/") {
            continue;
        }
        let suffix = &r.url["vstfs:///Git/Commit/".len()..];
        // Take the last segment after either `%2F` or `/`. ADO emits
        // `%2F` in practice; we tolerate both.
        let last = suffix
            .rsplit_once("%2F")
            .or_else(|| suffix.rsplit_once("%2f"))
            .or_else(|| suffix.rsplit_once('/'))
            .map(|(_, sha)| sha)
            .unwrap_or(suffix);
        // Strip any trailing query string just in case.
        let sha = last.split('?').next().unwrap_or(last).trim();
        if !sha.is_empty() {
            out.push(sha.to_string());
        }
    }
    out
}

/// Extract Azure DevOps work-item IDs from arbitrary text using a
/// caller-provided regex.
///
/// The first capture group of `re` is treated as the numeric work-item ID.
/// The default `AB#(\d+)` pattern lives on
/// [`AzureDevOpsConfig::ticket_regex`](crate::core::config::AzureDevOpsConfig);
/// callers are expected to compile it once and reuse the result. IDs are
/// deduplicated in first-seen order. Captures whose first group does not
/// parse as `u32` are silently skipped.
pub fn extract_work_item_refs(re: &regex::Regex, text: &str) -> Vec<u32> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            if let Ok(id) = m.as_str().parse::<u32>() {
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Feed ADO Graph users into an [`crate::collect::identity::IdentityResolver`].
///
/// For each user with a non-empty `mail_address`, registers the email
/// address as an alias for the user's display name via the resolver's
/// alias map. Users without an email are skipped — there is no reliable
/// canonical join key to register them under.
///
/// This is a one-shot ingestion helper; it does not mutate the resolver
/// after construction. Callers that need a long-lived ingestion loop should
/// roll their own using the resolver's public alias-update APIs.
pub fn feed_azdo_users(
    resolver: &mut crate::collect::identity::IdentityResolver,
    users: &[AzdoUser],
) {
    for u in users {
        let Some(email) = u.mail_address.as_deref() else {
            continue;
        };
        let email = email.trim();
        if email.is_empty() || u.display_name.trim().is_empty() {
            continue;
        }
        resolver.add_alias(email, &u.display_name);
    }
}

/// Scan a list of commit messages (or other text) for work-item references and
/// fetch the referenced work items in a single batch call (Phase 6).
///
/// `re` is the caller-compiled work-item-reference pattern (typically
/// [`AzureDevOpsConfig::ticket_regex`](crate::core::config::AzureDevOpsConfig)).
/// IDs are deduplicated across all messages. `project` is currently unused —
/// the batch endpoint is organisation-scoped — but is retained in the API for
/// future per-project filtering and for symmetry with the other methods.
///
/// Returns an empty vector if no references are found.
///
/// # Errors
///
/// Same set as [`AzureDevOpsClient::get_work_items`].
pub async fn fetch_referenced_work_items(
    client: &AzureDevOpsClient,
    re: &regex::Regex,
    messages: &[&str],
    _project: &str,
) -> Result<Vec<AzdoWorkItem>, AzdoError> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for msg in messages {
        for id in extract_work_item_refs(re, msg) {
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    client.get_work_items(&ids).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_config_for(server_url: &str) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: server_url.to_string(),
            pat: "secret-pat".into(),
            project: Some("MyProject".into()),
            projects: vec![],
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        }
    }

    fn sample_config() -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".into(),
            pat: "secret-pat".into(),
            project: Some("MyProject".into()),
            projects: vec![],
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        }
    }

    // ----- Phase 1 carry-over tests -----
    #[test]
    fn stub_connection_info_has_phase_1() {
        let client = AzureDevOpsClient::new(sample_config());
        let info = client.test_connection_stub();
        assert_eq!(info.phase, 1);
        assert_eq!(info.status, "stub");
        assert_eq!(info.organization_url, "https://dev.azure.com/myorg");
    }

    #[test]
    fn validate_credentials_accepts_non_empty_pat() {
        let client = AzureDevOpsClient::new(sample_config());
        client
            .validate_credentials()
            .expect("non-empty PAT should validate");
    }

    #[test]
    fn validate_credentials_rejects_empty_pat() {
        let mut cfg = sample_config();
        cfg.pat = "   ".into();
        let client = AzureDevOpsClient::new(cfg);
        let err = client
            .validate_credentials()
            .expect_err("whitespace PAT should be rejected");
        assert!(matches!(err, AzdoError::InvalidCredentials(_)));
    }

    #[tokio::test]
    async fn get_work_items_empty_ids_short_circuits() {
        // No HTTP server — verifies we don't hit the network for empty input.
        let client = AzureDevOpsClient::new(sample_config());
        let out = client
            .get_work_items(&[])
            .await
            .expect("empty ids should short-circuit to Ok(vec![])");
        assert!(out.is_empty());
    }

    // ----- Phase 2: HTTP tests via wiremock -----

    /// Expected Basic-auth header for `":secret-pat"` (empty user + PAT).
    /// `:secret-pat` → base64 → `OnNlY3JldC1wYXQ=`
    const EXPECTED_AUTH: &str = "Basic OnNlY3JldC1wYXQ=";

    #[tokio::test]
    async fn test_connection_succeeds_on_200() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "authenticatedUser": {
                "id": "11111111-1111-1111-1111-111111111111",
                "providerDisplayName": "John Doe",
                "subjectDescriptor": "aad.xxx"
            },
            "instanceId": "22222222-2222-2222-2222-222222222222",
            "deploymentType": "hosted"
        });

        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .and(query_param("api-version", "7.1-preview.1"))
            .and(query_param("connectOptions", "none"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let info = client
            .test_connection()
            .await
            .expect("200 should yield connected info");
        assert_eq!(info.status, "connected");
        assert_eq!(info.phase, 2);
        assert_eq!(info.user_name.as_deref(), Some("John Doe"));
        assert_eq!(
            info.user_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            info.instance_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
    }

    #[tokio::test]
    async fn test_connection_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_forbidden_on_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("403 should err");
        assert!(matches!(err, AzdoError::Forbidden), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_not_found_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("404 should err");
        assert!(matches!(err, AzdoError::NotFound), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_http_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("500 should err");
        match err {
            AzdoError::Http { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("upstream boom"), "msg: {message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_rejects_empty_pat_pre_flight() {
        let mut cfg = sample_config();
        cfg.pat = "   ".into();
        let client = AzureDevOpsClient::new(cfg);
        let err = client
            .test_connection()
            .await
            .expect_err("empty PAT short-circuits before HTTP");
        assert!(matches!(err, AzdoError::InvalidCredentials(_)));
    }

    #[tokio::test]
    async fn get_projects_returns_list_on_200() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "name": "MyProject",
                    "state": "wellFormed",
                    "visibility": "private",
                    "lastUpdateTime": "2025-01-01T00:00:00Z"
                },
                {
                    "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "name": "OtherProject",
                    "state": "wellFormed",
                    "visibility": "public",
                    "lastUpdateTime": "2025-01-02T00:00:00Z"
                }
            ]
        });

        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .and(query_param("api-version", "7.1"))
            .and(query_param("$top", "100"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let projects = client.get_projects().await.expect("200 should yield list");
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "MyProject");
        assert_eq!(projects[0].state, "wellFormed");
        assert_eq!(projects[0].visibility, "private");
        assert_eq!(projects[1].name, "OtherProject");
        assert_eq!(projects[1].visibility, "public");
    }

    #[tokio::test]
    async fn get_projects_returns_empty_on_zero_count() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "count": 0,
            "value": []
        });

        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let projects = client.get_projects().await.expect("200 empty list ok");
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn get_projects_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.get_projects().await.expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized), "got {err:?}");
    }

    // ----- Phase 3: work item types & fields -----

    #[tokio::test]
    async fn get_work_item_types_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "name": "Bug",
                    "referenceName": "Microsoft.VSTS.WorkItemTypes.Bug",
                    "description": "Tracks a defect",
                    "color": "CC293D",
                    "icon": { "id": "icon_insect", "url": "https://x" }
                },
                {
                    "name": "User Story",
                    "referenceName": "Microsoft.VSTS.WorkItemTypes.UserStory",
                    "description": "",
                    "color": "009CCC",
                    "icon": { "id": "icon_book" }
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workitemtypes"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let types = client
            .get_work_item_types("MyProject")
            .await
            .expect("200 ok");
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].name, "Bug");
        assert_eq!(types[0].reference_name, "Microsoft.VSTS.WorkItemTypes.Bug");
        assert_eq!(types[0].color, "CC293D");
        assert_eq!(types[0].icon, "icon_insect");
        assert_eq!(types[1].name, "User Story");
        assert_eq!(types[1].icon, "icon_book");
    }

    #[tokio::test]
    async fn get_work_item_types_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workitemtypes"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .get_work_item_types("MyProject")
            .await
            .expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized));
    }

    #[tokio::test]
    async fn get_fields_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                { "name": "Title", "referenceName": "System.Title", "type": "string" },
                { "name": "State", "referenceName": "System.State", "type": "string" }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/fields"))
            .and(query_param("api-version", "7.1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let fields = client.get_fields("MyProject").await.expect("200 ok");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].reference_name, "System.Title");
        assert_eq!(fields[0].field_type, "string");
    }

    #[tokio::test]
    async fn get_fields_encodes_project_with_spaces() {
        let server = MockServer::start().await;
        // "My Project" → "My%20Project"
        Mock::given(method("GET"))
            .and(path("/My%20Project/_apis/wit/fields"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let fields = client
            .get_fields("My Project")
            .await
            .expect("space in project name should encode");
        assert!(fields.is_empty());
    }

    // ----- Phase 4: WIQL -----

    #[tokio::test]
    async fn run_wiql_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "queryType": "flat",
            "queryResultType": "workItem",
            "workItems": [
                { "id": 42, "url": "https://x/42" },
                { "id": 43, "url": "https://x/43" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let result = client
            .run_wiql("MyProject", "SELECT [System.Id] FROM WorkItems")
            .await
            .expect("200 ok");
        assert_eq!(result.query_type, "flat");
        assert_eq!(result.work_items.len(), 2);
        assert_eq!(result.work_items[0].id, 42);
        assert_eq!(result.work_items[0].url, "https://x/42");
    }

    #[tokio::test]
    async fn get_recent_work_item_ids_returns_ids() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "queryType": "flat",
            "workItems": [
                { "id": 1, "url": "" },
                { "id": 2, "url": "" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let ids = client
            .get_recent_work_item_ids("MyProject", 30)
            .await
            .expect("200 ok");
        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn run_wiql_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .run_wiql("MyProject", "SELECT [System.Id] FROM WorkItems")
            .await
            .expect_err("401");
        assert!(matches!(err, AzdoError::Unauthorized));
    }

    // ----- Phase 5: work items batch -----

    #[tokio::test]
    async fn get_work_items_batch_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": 42,
                    "url": "https://x/42",
                    "fields": {
                        "System.Id": 42,
                        "System.Title": "Fix login bug",
                        "System.State": "Active",
                        "System.WorkItemType": "Bug",
                        "System.Tags": "frontend; urgent ; ",
                        "System.TeamProject": "MyProject"
                    }
                },
                {
                    "id": 43,
                    "fields": {
                        "System.Id": 43,
                        "System.Title": "New feature",
                        "System.State": "New",
                        "System.WorkItemType": "User Story",
                        "System.TeamProject": "MyProject"
                    }
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let items = client.get_work_items(&[42, 43]).await.expect("200 ok");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 42);
        assert_eq!(items[0].title, "Fix login bug");
        assert_eq!(items[0].state, "Active");
        assert_eq!(items[0].work_item_type, "Bug");
        assert_eq!(items[0].tags, vec!["frontend", "urgent"]);
        assert_eq!(items[0].team_project, "MyProject");
        assert_eq!(items[0].url.as_deref(), Some("https://x/42"));
        // Missing tags → empty vec.
        assert!(items[1].tags.is_empty());
        assert!(items[1].url.is_none());
    }

    #[tokio::test]
    async fn get_work_items_chunks_in_batches_of_200() {
        let server = MockServer::start().await;
        // Mount a permissive responder; we'll assert call count via wiremock.
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .expect(2) // 250 ids = 2 chunks (200 + 50)
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let ids: Vec<u32> = (1..=250).collect();
        let items = client.get_work_items(&ids).await.expect("200 ok");
        assert!(items.is_empty());
        // Drop() on server verifies `.expect(2)`.
        drop(server);
    }

    #[tokio::test]
    async fn get_work_items_maps_403() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.get_work_items(&[1]).await.expect_err("403");
        assert!(matches!(err, AzdoError::Forbidden));
    }

    // ----- Phase 6: work-item reference detection -----

    fn default_ab_regex() -> regex::Regex {
        regex::Regex::new(r"(?i)\bAB#(\d+)\b").expect("default AB# regex compiles")
    }

    #[test]
    fn extract_work_item_refs_finds_ids() {
        let re = default_ab_regex();
        let out = extract_work_item_refs(&re, "Fixes AB#42 and AB#100 and AB#42 again");
        assert_eq!(out, vec![42, 100]);
    }

    #[test]
    fn extract_work_item_refs_is_case_insensitive() {
        let re = default_ab_regex();
        let out = extract_work_item_refs(&re, "see ab#7 and Ab#8 and AB#9");
        assert_eq!(out, vec![7, 8, 9]);
    }

    #[test]
    fn extract_work_item_refs_returns_empty_when_no_match() {
        let re = default_ab_regex();
        let out = extract_work_item_refs(&re, "nothing to see here #42 PROJ-1");
        assert!(out.is_empty());
    }

    #[test]
    fn extract_work_item_refs_honours_custom_regex() {
        // Regression test for #90: orgs that don't use the AB# convention
        // configure a different ticket_regex (e.g. bare `#NNNN` IDs) and
        // expect the extractor to honour it instead of the AB# default.
        let re = regex::Regex::new(r"\B#(\d{4,8})\b").expect("custom regex compiles");
        let msg = "Merged PR 12: fix login (#12345) and #67890 follow-up";
        let out = extract_work_item_refs(&re, msg);
        assert_eq!(out, vec![12_345, 67_890]);
    }

    #[tokio::test]
    async fn fetch_referenced_work_items_aggregates_from_messages() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": 7,
                    "fields": {
                        "System.Id": 7,
                        "System.Title": "Seven",
                        "System.State": "Active",
                        "System.WorkItemType": "Task",
                        "System.TeamProject": "MyProject"
                    }
                },
                {
                    "id": 9,
                    "fields": {
                        "System.Id": 9,
                        "System.Title": "Nine",
                        "System.State": "Closed",
                        "System.WorkItemType": "Bug",
                        "System.TeamProject": "MyProject"
                    }
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let re = default_ab_regex();
        let msgs = ["fix AB#7", "AB#7 again and AB#9"];
        let items = fetch_referenced_work_items(&client, &re, &msgs, "MyProject")
            .await
            .expect("batch ok");
        assert_eq!(items.len(), 2);
        let ids: Vec<u32> = items.iter().map(|w| w.id).collect();
        assert!(ids.contains(&7));
        assert!(ids.contains(&9));
    }

    #[tokio::test]
    async fn fetch_referenced_work_items_uses_custom_regex_end_to_end() {
        // Regression test for #90: an ADO org configured with a bare-`#NNNN`
        // ticket_regex must end up POSTing those IDs to workitemsbatch.
        // Pre-fix, the hardcoded `AB#(\d+)` would extract zero IDs from
        // messages like `Merged PR 12: fix (#12345)` and the batch call
        // would never happen. The mock matches on the request body so this
        // test fails (mock never fires) if the wrong IDs are extracted.
        use wiremock::matchers::body_partial_json;

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": 12345,
                    "fields": {
                        "System.Id": 12345,
                        "System.Title": "Issue twelve-thousand",
                        "System.State": "Active",
                        "System.WorkItemType": "Task",
                        "System.TeamProject": "MyProject"
                    }
                },
                {
                    "id": 67890,
                    "fields": {
                        "System.Id": 67890,
                        "System.Title": "Follow-up",
                        "System.State": "Closed",
                        "System.WorkItemType": "Bug",
                        "System.TeamProject": "MyProject"
                    }
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .and(body_partial_json(
                serde_json::json!({"ids": [12345, 67890]}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        // The actual ticket_regex from the #90 repro config.
        let re = regex::Regex::new(r"\B#(\d{4,8})\b").expect("repro regex compiles");
        let msgs = [
            "Merged PR 12: fix login (#12345)",
            "follow-up to #67890 thanks",
        ];
        let items = fetch_referenced_work_items(&client, &re, &msgs, "MyProject")
            .await
            .expect("batch ok with custom regex");
        let ids: Vec<u32> = items.iter().map(|w| w.id).collect();
        assert!(ids.contains(&12_345), "expected 12345 to be fetched");
        assert!(ids.contains(&67_890), "expected 67890 to be fetched");
        // `PR 12` is only 2 digits; the {4,8} length bound must keep it out.
        assert!(!ids.contains(&12), "PR 12 must not be extracted");
    }

    #[tokio::test]
    async fn fetch_referenced_work_items_empty_when_no_refs() {
        let client = AzureDevOpsClient::new(sample_config());
        let re = default_ab_regex();
        let msgs = ["nothing here", "PROJ-1 only"];
        let items = fetch_referenced_work_items(&client, &re, &msgs, "MyProject")
            .await
            .expect("no HTTP should be triggered");
        assert!(items.is_empty());
    }

    // ----- Path encoding helper -----

    #[test]
    fn encode_path_segment_passes_through_unreserved() {
        assert_eq!(
            encode_path_segment("MyProject_1.2-3~x"),
            "MyProject_1.2-3~x"
        );
    }

    #[test]
    fn encode_path_segment_encodes_space() {
        assert_eq!(encode_path_segment("My Project"), "My%20Project");
    }

    #[test]
    fn encode_path_segment_encodes_slash() {
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
    }

    // ----- Phase 4 ext: iterations & users -----

    #[tokio::test]
    async fn get_iterations_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": "11111111-1111-1111-1111-111111111111",
                    "name": "Sprint 1",
                    "path": "MyProject\\Release 1\\Sprint 1",
                    "attributes": {
                        "startDate": "2025-01-01T00:00:00Z",
                        "finishDate": "2025-01-14T00:00:00Z",
                        "timeFrame": "past"
                    }
                },
                {
                    "id": "22222222-2222-2222-2222-222222222222",
                    "name": "Sprint 2",
                    "path": "MyProject\\Release 1\\Sprint 2",
                    "attributes": {
                        "startDate": null,
                        "finishDate": null,
                        "timeFrame": "future"
                    }
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/work/teamsettings/iterations"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let iters = client.get_iterations("MyProject").await.expect("200 ok");
        assert_eq!(iters.len(), 2);
        assert_eq!(iters[0].name, "Sprint 1");
        assert_eq!(iters[0].time_frame, "past");
        assert_eq!(iters[0].start_date.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(iters[1].time_frame, "future");
        assert!(iters[1].start_date.is_none());
    }

    #[tokio::test]
    async fn get_iterations_maps_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/work/teamsettings/iterations"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .get_iterations("MyProject")
            .await
            .expect_err("403 should err");
        assert!(matches!(err, AzdoError::Forbidden));
    }

    #[tokio::test]
    async fn get_users_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "descriptor": "aad.xxx",
                    "displayName": "Alice Smith",
                    "mailAddress": "alice@contoso.com",
                    "principalName": "alice@contoso.com"
                },
                {
                    "descriptor": "msa.yyy",
                    "displayName": "Bob Jones"
                    // mailAddress and principalName missing
                }
            ]
        });
        // The mock-server fallback in `graph_users_url` routes to
        // `{org}/_graph/users` for non-dev.azure.com hosts.
        Mock::given(method("GET"))
            .and(path("/_graph/users"))
            .and(query_param("api-version", "7.1-preview.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let users = client.get_users().await.expect("200 ok");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].display_name, "Alice Smith");
        assert_eq!(users[0].mail_address.as_deref(), Some("alice@contoso.com"));
        assert_eq!(users[1].display_name, "Bob Jones");
        assert!(users[1].mail_address.is_none());
    }

    #[tokio::test]
    async fn get_users_maps_403_for_missing_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_graph/users"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.get_users().await.expect_err("403 expected");
        assert!(matches!(err, AzdoError::Forbidden));
    }

    #[test]
    fn graph_users_url_dev_azure_form() {
        let mut cfg = sample_config();
        cfg.organization_url = "https://dev.azure.com/myorg".into();
        let client = AzureDevOpsClient::new(cfg);
        let url = client.graph_users_url().expect("derive url");
        assert!(
            url.starts_with("https://vssps.dev.azure.com/myorg/_apis/graph/users"),
            "got {url}"
        );
    }

    #[test]
    fn graph_users_url_visualstudio_form() {
        let mut cfg = sample_config();
        cfg.organization_url = "https://myorg.visualstudio.com".into();
        let client = AzureDevOpsClient::new(cfg);
        let url = client.graph_users_url().expect("derive url");
        assert!(
            url.starts_with("https://vssps.dev.azure.com/myorg/_apis/graph/users"),
            "got {url}"
        );
    }

    #[test]
    fn feed_azdo_users_registers_email_aliases() {
        use crate::collect::identity::IdentityResolver;
        let mut resolver = IdentityResolver::new(None);
        let users = vec![
            AzdoUser {
                descriptor: "d1".into(),
                display_name: "Alice Smith".into(),
                mail_address: Some("alice@contoso.com".into()),
                principal_name: Some("alice@contoso.com".into()),
            },
            AzdoUser {
                descriptor: "d2".into(),
                display_name: "Bob Jones".into(),
                mail_address: None,
                principal_name: None,
            },
            AzdoUser {
                descriptor: "d3".into(),
                display_name: "".into(),
                mail_address: Some("ghost@contoso.com".into()),
                principal_name: None,
            },
        ];
        feed_azdo_users(&mut resolver, &users);
        // Alice's email should resolve to her display name.
        let (name, _) = resolver.resolve("anybody", "alice@contoso.com");
        assert_eq!(name, "Alice Smith");
        // Bob and Ghost should not have been registered.
        let (name, email) = resolver.resolve("Bob Jones", "unknown@x.com");
        // No alias for that email; resolver passes the input through.
        assert_eq!(name, "Bob Jones");
        assert_eq!(email, "unknown@x.com");
    }

    #[tokio::test]
    async fn org_url_trailing_slash_is_trimmed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .mount(&server)
            .await;

        // Append a trailing slash to the org URL.
        let mut cfg = sample_config_for(&server.uri());
        cfg.organization_url.push('/');
        let client = AzureDevOpsClient::new(cfg);
        let projects = client
            .get_projects()
            .await
            .expect("trailing slash should be tolerated");
        assert!(projects.is_empty());
    }

    // ----- Phase 5 ext: comments, extended work items, commit links -----

    #[tokio::test]
    async fn get_work_item_comments_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "totalCount": 2,
            "count": 2,
            "comments": [
                {
                    "id": 101,
                    "workItemId": 42,
                    "text": "Looks good to me",
                    "createdBy": { "displayName": "Alice" },
                    "createdDate": "2025-01-01T12:00:00Z"
                },
                {
                    "id": 102,
                    "workItemId": 42,
                    "text": "<div>Done</div>",
                    "createdBy": { "displayName": "Bob" },
                    "createdDate": "2025-01-02T08:00:00Z"
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workItems/42/comments"))
            .and(query_param("api-version", "7.1-preview.3"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let comments = client
            .get_work_item_comments("MyProject", 42)
            .await
            .expect("200 ok");
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, 101);
        assert_eq!(comments[0].text, "Looks good to me");
        assert_eq!(comments[0].created_by, "Alice");
        assert_eq!(comments[0].created_date, "2025-01-01T12:00:00Z");
        assert_eq!(comments[1].text, "<div>Done</div>");
    }

    #[tokio::test]
    async fn get_work_item_comments_maps_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workItems/999/comments"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .get_work_item_comments("MyProject", 999)
            .await
            .expect_err("404");
        assert!(matches!(err, AzdoError::NotFound));
    }

    #[tokio::test]
    async fn get_work_item_extended_returns_full_fields() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "id": 42,
            "url": "https://x/42",
            "fields": {
                "System.Id": 42,
                "System.Title": "Improve cache",
                "System.State": "Active",
                "System.WorkItemType": "User Story",
                "System.Tags": "perf; cache",
                "System.IterationPath": "MyProject\\Sprint 3",
                "System.AreaPath": "MyProject\\Backend",
                "Microsoft.VSTS.Common.Priority": 2,
                "Custom.RiskScore": "medium"
            }
        });
        Mock::given(method("GET"))
            .and(path("/_apis/wit/workitems/42"))
            .and(query_param("api-version", "7.1"))
            .and(query_param("$expand", "all"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let item = client
            .get_work_item_extended(42)
            .await
            .expect("200 ok")
            .expect("found");
        assert_eq!(item.id, 42);
        assert_eq!(item.title, "Improve cache");
        assert_eq!(item.state, "Active");
        assert_eq!(item.work_item_type, "User Story");
        assert_eq!(item.tags, vec!["perf", "cache"]);
        assert_eq!(item.iteration_path.as_deref(), Some("MyProject\\Sprint 3"));
        assert_eq!(item.area_path.as_deref(), Some("MyProject\\Backend"));
        // Standard fields must NOT be in custom_fields.
        assert!(!item.custom_fields.contains_key("System.Title"));
        assert!(!item.custom_fields.contains_key("System.IterationPath"));
        // Custom fields must be present.
        assert_eq!(
            item.custom_fields
                .get("Microsoft.VSTS.Common.Priority")
                .and_then(|v| v.as_i64()),
            Some(2)
        );
        assert_eq!(
            item.custom_fields
                .get("Custom.RiskScore")
                .and_then(|v| v.as_str()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn get_work_item_extended_maps_404_to_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/wit/workitems/999"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let out = client.get_work_item_extended(999).await.expect("ok(None)");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn get_work_item_commit_links_extracts_shas() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "id": 42,
            "relations": [
                {
                    "rel": "ArtifactLink",
                    "url": "vstfs:///Git/Commit/proj-guid%2Frepo-guid%2Fabc123def456",
                    "attributes": { "name": "Fixed in Commit" }
                },
                {
                    "rel": "ArtifactLink",
                    "url": "vstfs:///Git/Commit/proj-guid%2Frepo-guid%2F0123456789abcdef",
                    "attributes": { "name": "Fixed in Commit" }
                },
                {
                    "rel": "System.LinkTypes.Related",
                    "url": "https://dev.azure.com/myorg/_apis/wit/workItems/77",
                    "attributes": {}
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workItems/42"))
            .and(query_param("api-version", "7.1"))
            .and(query_param("$expand", "relations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let shas = client
            .get_work_item_commit_links("MyProject", 42)
            .await
            .expect("200 ok");
        assert_eq!(shas, vec!["abc123def456", "0123456789abcdef"]); // pragma: allowlist secret
    }

    #[tokio::test]
    async fn get_work_item_commit_links_404_returns_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workItems/999"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let shas = client
            .get_work_item_commit_links("MyProject", 999)
            .await
            .expect("404 yields empty");
        assert!(shas.is_empty());
    }

    #[test]
    fn extract_commit_shas_handles_versioned_and_artifact_rels() {
        let rels = vec![
            WorkItemRelationRaw {
                rel: "ArtifactLink".into(),
                url: "vstfs:///Git/Commit/p%2Fr%2Fdeadbeef".into(),
                attributes: serde_json::Map::new(),
            },
            WorkItemRelationRaw {
                rel: "System.LinkTypes.VersionedRelated".into(),
                url: "vstfs:///Git/Commit/p/r/cafebabe".into(),
                attributes: serde_json::Map::new(),
            },
            WorkItemRelationRaw {
                rel: "AttachedFile".into(),
                url: "vstfs:///Git/Commit/p%2Fr%2Fnotacommit".into(),
                attributes: serde_json::Map::new(),
            },
        ];
        let shas = extract_commit_shas_from_relations(&rels);
        assert_eq!(shas, vec!["deadbeef", "cafebabe"]);
    }
}
