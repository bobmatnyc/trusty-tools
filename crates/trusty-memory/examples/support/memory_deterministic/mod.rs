//! Portable deterministic evaluation; authoritative sources are never rewritten by age.
mod derive;
mod maintain;
mod retrieval;
#[cfg(test)]
mod tests;
mod types;
mod validate;

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub use types::*;

pub fn identity(scope: &str, id: &str) -> String {
    serde_json::to_string(&[scope, id]).expect("string serialization cannot fail")
}
pub fn doc_id(scope: &str, id: &str, child: usize) -> String {
    serde_json::json!([scope, id, child]).to_string()
}
pub fn doc_identity(doc: &str) -> Result<String> {
    let (scope, id, _): (String, String, usize) = serde_json::from_str(doc)
        .map_err(|_| ExperimentError::new("invalid_state", "invalid doc_id encoding"))?;
    if scope.is_empty() || id.is_empty() {
        return Err(ExperimentError::new("invalid_state", "empty doc identity"));
    }
    Ok(identity(&scope, &id))
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn canonical<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: BTreeMap<_, _> = map.into_iter().map(|(k, v)| (k, sort(v))).collect();
                serde_json::to_value(sorted).expect("JSON value serialization cannot fail")
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sort).collect())
            }
            other => other,
        }
    }
    serde_json::to_value(value)
        .and_then(|v| serde_json::to_vec(&sort(v)))
        .map_err(|e| ExperimentError::new("internal", e.to_string()))
}
pub fn instant(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .expect("timestamp was validated")
        .with_timezone(&chrono::Utc)
}
pub fn fingerprint(
    source: &Source,
    treatment: Treatment,
    policy: &Policy,
    sources: &BTreeMap<String, Source>,
) -> Result<String> {
    let dependencies: Vec<_> = source
        .drawer
        .links
        .iter()
        .filter_map(|link| {
            sources.get(&identity(&source.drawer.scope,&link.target_id)).map(|s| {
            serde_json::json!({"id":s.drawer.id,"revision":s.revision,"aliases":s.drawer.aliases})
        })
        })
        .collect();
    Ok(digest(&canonical(
        &serde_json::json!({"source":source,"treatment":treatment,"policy":policy,
        "tokenizer":"trusty-common-bm25-v1","version":VERSION,"dependencies":dependencies}),
    )?))
}
pub fn evaluate(mut request: Request) -> Result<Response> {
    validate::normalize_validate(&mut request)?;
    let mut state = request
        .state
        .take()
        .unwrap_or_else(|| State::new(request.treatment, request.policy.clone()));
    maintain::apply(&mut state, request.mutations)?;
    let maintenance = maintain::maintain(&mut state, &request.maintenance)?;
    let index = retrieval::index(&state)?;
    let results = request
        .queries
        .iter()
        .map(|q| retrieval::search(&state, q, &index))
        .collect::<Result<Vec<_>>>()?;
    Ok(Response {
        schema_version: 1,
        request_id: request.request_id,
        ok: true,
        state,
        maintenance,
        results,
    })
}
pub fn respond(input: &str) -> serde_json::Value {
    let parsed: std::result::Result<serde_json::Value, _> = serde_json::from_str(input);
    let id = parsed
        .as_ref()
        .ok()
        .and_then(|v| v.get("request_id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let result = match parsed {
        Err(error) => Err(ExperimentError::new("invalid_json", error.to_string())),
        Ok(value) => serde_json::from_value::<Request>(value)
            .map_err(|e| ExperimentError::new("invalid_request", e.to_string()))
            .and_then(evaluate),
    };
    match result {
        Ok(response) => serde_json::to_value(response).expect("validated response serialization"),
        Err(error) => serde_json::json!({"schema_version":1,"request_id":id,"ok":false,
            "error":{"code":error.code,"message":error.message,"path":error.path}}),
    }
}
