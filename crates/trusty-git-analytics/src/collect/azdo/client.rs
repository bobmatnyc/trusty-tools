//! Azure DevOps HTTP client — Phases 2–6.
//!
//! Why: provides an authenticated, async HTTP client over the ADO REST API
//! (`api-version=7.1`) for all work-item, project, user, and iteration
//! endpoints used by tga.
//! What: implements [`AzureDevOpsClient`] with methods for connection probing,
//! project listing, WIQL queries, work-item batch fetch, iteration fetch, user
//! fetch, and commit-link extraction.
//! Test: see `azdo/tests.rs` for full HTTP-level tests via wiremock.

use crate::collect::azdo::{
    errors::AzdoError,
    helpers::{
        build_client, encode_path_segment, extract_commit_shas_from_relations, map_response_error,
        parse_work_item, parse_work_item_extended,
    },
    types::{
        AzdoComment, AzdoConnectionInfo, AzdoField, AzdoIteration, AzdoProject, AzdoUser,
        AzdoWorkItem, AzdoWorkItemExtended, AzdoWorkItemType, WiqlResult, WorkItemRef,
    },
    wire::{
        CommentsResponse, ConnectionDataResponse, FieldRaw, IterationRaw, ListEnvelope,
        ProjectsResponse, UserRaw, WiqlResponseRaw, WorkItemBatchResponse,
        WorkItemRelationsResponse, WorkItemTypeRaw,
    },
};
use crate::core::config::AzureDevOpsConfig;

/// Azure DevOps client. Holds config + a lazily built `reqwest::Client`.
///
/// Why: encapsulates the ADO organisation URL and PAT so callers need not
/// thread authentication through every call site.
/// What: wraps `AzureDevOpsConfig` and exposes async methods for each ADO
/// endpoint tga uses.
/// Test: see `azdo/tests.rs` — every public method has at least one test.
pub struct AzureDevOpsClient {
    config: AzureDevOpsConfig,
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
            return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
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
                return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
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
            return Err(map_response_error(resp).await);
        }

        let raw = resp
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
            return Err(map_response_error(resp).await);
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
    pub(super) fn graph_users_url(&self) -> Result<String, AzdoError> {
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
