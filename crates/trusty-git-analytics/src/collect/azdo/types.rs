//! Public domain types for the Azure DevOps integration.
//!
//! Why: separates the stable public API surface from the HTTP client logic and
//! internal wire shapes so callers can import types without dragging in client
//! machinery.
//! What: defines all public structs/type-aliases returned by `AzureDevOpsClient`
//! and related free functions.
//! Test: types are exercised indirectly by every client-level test that
//! asserts on their fields; see `azdo/tests.rs`.

use serde::{Deserialize, Serialize};

/// Result of an Azure DevOps connection probe.
///
/// `status` is `"connected"` on a successful `GET _apis/connectionData`,
/// `"failed"` if the probe completed but ADO returned a non-success status
/// (this variant is not currently returned — failures bubble as `AzdoError`
/// instead), or `"stub"` if produced by [`crate::collect::azdo::client::AzureDevOpsClient::test_connection_stub`].
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
/// listed in `AzureDevOpsClient::get_work_items`.
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
