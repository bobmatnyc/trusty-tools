//! Tests for the Azure DevOps client, helpers, and types.
//!
//! Why: collected here so the source files stay under the 500-SLOC production
//! cap while tests can use the 1500-SLOC test-file budget.
//! What: unit tests for helpers + wiremock HTTP integration tests for every
//! `AzureDevOpsClient` method.
//! Test: run with `cargo test -p tga`.

use crate::core::config::AzureDevOpsConfig;

use super::client::AzureDevOpsClient;
use super::errors::AzdoError;
use super::helpers::{
    encode_path_segment, extract_work_item_refs, feed_azdo_users, fetch_referenced_work_items,
};
use super::types::AzdoUser;
use super::wire::WorkItemRelationRaw;

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    use super::helpers::extract_commit_shas_from_relations;

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
