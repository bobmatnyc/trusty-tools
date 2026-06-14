//! Internal (private) ADO REST API response shapes.
//!
//! Why: keeps `serde` deserialization structs separate from the public domain
//! types and client logic so each file stays under the SLOC cap and the public
//! API surface remains clean.
//! What: defines all private `*Raw` / response-envelope structs used by
//! `AzureDevOpsClient` to parse ADO REST API JSON.
//! Test: covered indirectly by the HTTP-level tests in `azdo/tests.rs` which
//! exercise the full parse path via wiremock.

use serde::Deserialize;

/// ADO `_apis/connectionData` response (partial — only fields tga uses).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConnectionDataResponse {
    pub(super) authenticated_user: AuthenticatedUser,
    pub(super) instance_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) deployment_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthenticatedUser {
    pub(super) id: String,
    pub(super) provider_display_name: String,
}

/// ADO `_apis/projects` response envelope.
#[derive(Debug, Deserialize)]
pub(super) struct ProjectsResponse {
    #[allow(dead_code)]
    pub(super) count: u32,
    pub(super) value: Vec<AzdoProjectRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AzdoProjectRaw {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) state: String,
    pub(super) visibility: String,
}

/// Generic ADO list envelope: `{ "count": N, "value": [...] }`.
#[derive(Debug, Deserialize)]
pub(super) struct ListEnvelope<T> {
    #[allow(dead_code)]
    #[serde(default)]
    pub(super) count: u32,
    pub(super) value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkItemTypeRaw {
    pub(super) name: String,
    pub(super) reference_name: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) color: String,
    #[serde(default)]
    pub(super) icon: IconRaw,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct IconRaw {
    #[serde(default)]
    pub(super) id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FieldRaw {
    pub(super) name: String,
    pub(super) reference_name: String,
    #[serde(rename = "type", default)]
    pub(super) field_type: String,
}

/// WIQL response envelope (Phase 4).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WiqlResponseRaw {
    #[serde(default)]
    pub(super) query_type: String,
    #[serde(default)]
    pub(super) work_items: Vec<WorkItemRefRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkItemRefRaw {
    pub(super) id: u32,
    #[serde(default)]
    pub(super) url: String,
}

/// Work-items-batch response envelope (Phase 5).
#[derive(Debug, Deserialize)]
pub(super) struct WorkItemBatchResponse {
    #[allow(dead_code)]
    #[serde(default)]
    pub(super) count: u32,
    pub(super) value: Vec<WorkItemRaw>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkItemRaw {
    pub(super) id: u32,
    #[serde(default)]
    pub(super) fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub(super) url: Option<String>,
}

/// Iteration list response (Phase 4 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IterationRaw {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) path: String,
    #[serde(default)]
    pub(super) attributes: IterationAttributesRaw,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct IterationAttributesRaw {
    #[serde(default)]
    pub(super) start_date: Option<String>,
    #[serde(default)]
    pub(super) finish_date: Option<String>,
    #[serde(default)]
    pub(super) time_frame: String,
}

/// Work-item comments response (Phase 5 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommentsResponse {
    #[serde(default)]
    pub(super) comments: Vec<CommentRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommentRaw {
    pub(super) id: u32,
    #[serde(default)]
    pub(super) text: String,
    #[serde(default)]
    pub(super) created_by: IdentityRefRaw,
    #[serde(default)]
    pub(super) created_date: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct IdentityRefRaw {
    #[serde(default)]
    pub(super) display_name: String,
}

/// Work-item single-fetch response with `relations` expanded (Phase 5 ext).
#[derive(Debug, Deserialize)]
pub(super) struct WorkItemRelationsResponse {
    #[serde(default)]
    pub(super) relations: Vec<WorkItemRelationRaw>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkItemRelationRaw {
    #[serde(default)]
    pub(super) rel: String,
    #[serde(default)]
    pub(super) url: String,
    /// Relation attributes (e.g. `name: "Fixed in Commit"`). Currently
    /// unused by `extract_commit_shas_from_relations` — we identify
    /// commit links via the `vstfs:///Git/Commit/` URL scheme — but we
    /// deserialize the field to validate the wire format and to keep the
    /// door open for filtering by `attributes.name` in the future.
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) attributes: serde_json::Map<String, serde_json::Value>,
}

/// Graph user response (Phase 4 ext).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserRaw {
    #[serde(default)]
    pub(super) descriptor: String,
    #[serde(default)]
    pub(super) display_name: String,
    #[serde(default)]
    pub(super) mail_address: Option<String>,
    #[serde(default)]
    pub(super) principal_name: Option<String>,
}
