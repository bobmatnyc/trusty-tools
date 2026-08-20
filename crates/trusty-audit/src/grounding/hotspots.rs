//! Asking trusty-analyze which of a repository's files are worst (#6081).
//!
//! Why: #6078 gave trusty-review's investigation pass an `inspect_priority`
//! interface and no producer, so selection still ranked files by whether their
//! PATH NAME looked interesting. `GET /indexes/{id}/complexity_hotspots` is a
//! measurement instead — cyclomatic complexity per code chunk, computed over the
//! indexed corpus — and turning it into a ranked path list is the whole of the
//! intelligence the owner ruled belongs in this crate.
//!
//! What: [`fetch`], and the two pure halves it is made of — [`hotspots_url`] and
//! [`rank`] — so the shape of the answer can be asserted without a daemon.
//!
//! ## Chunks in, files out
//!
//! The endpoint ranks CHUNKS (one function, one method), and the manifest names
//! FILES. Several chunks of one file collapse to the file's best rank, which is
//! also what keeps one pathological file from filling the whole list.
//!
//! The client is [`trusty_common::http_client::loopback_client_builder`], not a
//! bare `reqwest::Client::new()`: the target is a loopback daemon, and a client
//! that honoured an exported `HTTP_PROXY` would send a recipient's index id to
//! their corporate proxy (#4392).
//!
//! Test: `hotspot_tests`, and `super::grounding_tests` for the live arms.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// How many chunks to ask for.
///
/// Why 60: the endpoint ranks chunks and the manifest wants files, so the list
/// has to be long enough to survive collapsing. 60 chunks yields a comfortable
/// margin over [`MAX_PRIORITY_PATHS`] on a repository whose worst file holds
/// several of the worst functions, without paying for a top-N the cap discards.
const HOTSPOT_CHUNKS: usize = 60;

/// How many files may be declared as inspection priorities.
///
/// Why a cap at all: `inspect_priority` is a DOMINANT sort key in trusty-review's
/// selection, so a list longer than the investigation's file budget would
/// displace every heuristic-ranked file and turn a ranking into a whitelist.
/// 25 stays comfortably under that budget, leaving room for the analyst brief
/// and the DD-dimension heuristics to still decide something.
const MAX_PRIORITY_PATHS: usize = 25;

/// Per-request budget.
///
/// The endpoint answers from a corpus that is already in memory, so this bounds
/// an unreachable-but-accepting daemon rather than a slow computation.
const REQUEST_BUDGET: Duration = Duration::from_secs(30);

/// The `/complexity_hotspots` envelope, narrowed to what a ranking needs.
#[derive(Debug, Deserialize)]
struct HotspotEnvelope {
    #[serde(default)]
    hotspots: Vec<Hotspot>,
}

/// One ranked chunk. Every other field of the flattened `CodeChunk` is ignored.
#[derive(Debug, Deserialize)]
struct Hotspot {
    #[serde(default)]
    file: String,
    #[serde(default)]
    cyclomatic: u32,
}

/// The endpoint for one index, on one base address.
fn hotspots_url(base: &str, index_id: &str) -> String {
    format!(
        "{}/indexes/{index_id}/complexity_hotspots?top_n={HOTSPOT_CHUNKS}",
        base.trim_end_matches('/')
    )
}

/// Collapse a chunk ranking into a ranked, repo-relative, capped file list.
///
/// Why each step: chunks of one file collapse to that file's best rank, so one
/// pathological file cannot fill the list. A zero-cyclomatic chunk carries no
/// signal — `complexity_hotspots` truncates a DESCENDING sort, so a corpus with
/// nothing complex still answers with entries, and passing those on would
/// declare an arbitrary ranking as a measured one. And the paths are made
/// repo-relative because that is what `inspect_priority` is matched against;
/// trusty-review does accept an absolute one, but a manifest a human reads
/// should not name the machine that produced it.
/// What: descending order preserved (the endpoint already sorted), first
/// occurrence wins, zero-complexity dropped, capped at [`MAX_PRIORITY_PATHS`].
/// Test: `hotspot_tests::{ranking_collapses_chunks_to_files,
/// zero_complexity_chunks_carry_no_ranking, the_ranking_is_capped}`.
fn rank(hotspots: &[Hotspot], checkout: &Path) -> Vec<String> {
    let root = checkout.to_string_lossy().replace('\\', "/");
    let root = root.trim_end_matches('/');
    let mut out: Vec<String> = Vec::new();
    for spot in hotspots {
        if spot.cyclomatic == 0 || spot.file.is_empty() {
            continue;
        }
        let path = spot.file.replace('\\', "/");
        let relative = path
            .strip_prefix(root)
            .map_or(path.as_str(), |rest| rest.trim_start_matches('/'))
            .to_owned();
        if relative.is_empty() || out.contains(&relative) {
            continue;
        }
        out.push(relative);
        if out.len() >= MAX_PRIORITY_PATHS {
            break;
        }
    }
    out
}

/// Fetch one index's complexity hotspots and rank them into manifest paths.
///
/// # Errors
///
/// One line, safe to show the recipient, when the daemon cannot be reached,
/// answers non-2xx, or answers something that is not the documented envelope.
/// The caller turns it into a gap; nothing here fails a run.
///
/// An empty result is `Ok(vec![])`, not an error: "measured, nothing complex"
/// and "could not measure" are different facts and the caller states them
/// differently.
///
/// Test: `super::grounding_tests::{an_unreachable_hotspots_endpoint_is_a_named_gap,
/// an_empty_hotspot_list_is_a_named_gap,
/// hotspots_become_ranked_inspect_priority_in_the_manifest}`.
pub async fn fetch(base: &str, index_id: &str, checkout: &Path) -> Result<Vec<String>, String> {
    let url = hotspots_url(base, index_id);
    let client = trusty_common::http_client::loopback_client_builder()
        .timeout(REQUEST_BUDGET)
        .build()
        .map_err(|e| format!("no HTTP client could be built for {base} ({e})"))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("trusty-analyze did not answer {url} ({e})"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("trusty-analyze answered {status} for {url}"));
    }
    let envelope: HotspotEnvelope = response
        .json()
        .await
        .map_err(|e| format!("trusty-analyze answered {url} with an unreadable body ({e})"))?;
    Ok(rank(&envelope.hotspots, checkout))
}

#[cfg(test)]
mod hotspot_tests {
    use super::*;

    fn spot(file: &str, cyclomatic: u32) -> Hotspot {
        Hotspot {
            file: file.to_owned(),
            cyclomatic,
        }
    }

    #[test]
    fn the_url_names_the_index_and_the_chunk_count() {
        assert_eq!(
            hotspots_url("http://127.0.0.1:7879/", "acme-api"),
            format!(
                "http://127.0.0.1:7879/indexes/acme-api/complexity_hotspots?top_n={HOTSPOT_CHUNKS}"
            )
        );
    }

    /// The endpoint's own envelope, as `service::handlers::analysis` writes it:
    /// a flattened `CodeChunk` with two sibling complexity fields. Fields this
    /// reader ignores must not break the parse.
    #[test]
    fn the_daemons_own_envelope_parses() {
        let body = r#"{
            "index_id": "acme-api",
            "top_n": 60,
            "hotspots": [
                {"id":"a:1:9","file":"/w/repos/acme-api/src/pay.rs","start_line":1,
                 "end_line":9,"content":"fn f(){}","function_name":"f","score":0.0,
                 "match_reason":"","cyclomatic":31,"cognitive":44}
            ]
        }"#;
        let env: HotspotEnvelope = serde_json::from_str(body).expect("parses");
        assert_eq!(env.hotspots.len(), 1);
        assert_eq!(env.hotspots[0].cyclomatic, 31);
        assert_eq!(
            rank(&env.hotspots, Path::new("/w/repos/acme-api")),
            vec!["src/pay.rs".to_string()]
        );
    }

    /// An envelope with no `hotspots` key at all is an empty measurement, not a
    /// parse failure — the caller distinguishes it from an unreachable daemon.
    #[test]
    fn a_body_without_hotspots_reads_as_no_measurement() {
        let env: HotspotEnvelope = serde_json::from_str("{\"index_id\":\"x\"}").expect("parses");
        assert!(rank(&env.hotspots, Path::new("/w")).is_empty());
    }

    #[test]
    fn ranking_collapses_chunks_to_files_keeping_the_best_rank() {
        let spots = [
            spot("/w/repos/api/src/pay.rs", 31),
            spot("/w/repos/api/src/pay.rs", 22),
            spot("/w/repos/api/src/auth.rs", 18),
        ];
        assert_eq!(
            rank(&spots, Path::new("/w/repos/api")),
            vec!["src/pay.rs".to_string(), "src/auth.rs".to_string()]
        );
    }

    /// A descending truncated sort still answers on a corpus with nothing
    /// complex in it. Passing those on would declare an arbitrary order as a
    /// measured one.
    #[test]
    fn zero_complexity_chunks_carry_no_ranking() {
        let spots = [spot("/w/repos/api/src/a.rs", 0), spot("", 9)];
        assert!(rank(&spots, Path::new("/w/repos/api")).is_empty());
    }

    #[test]
    fn the_ranking_is_capped_so_it_stays_a_ranking() {
        let spots: Vec<Hotspot> = (0..MAX_PRIORITY_PATHS + 10)
            .map(|i| spot(&format!("/w/repos/api/src/f{i}.rs"), 40))
            .collect();
        assert_eq!(
            rank(&spots, Path::new("/w/repos/api")).len(),
            MAX_PRIORITY_PATHS
        );
    }

    /// A path outside the checkout keeps its absolute form rather than being
    /// mangled — trusty-review's matcher accepts an absolute component, and a
    /// silently truncated path would name a different file.
    #[test]
    fn a_path_outside_the_checkout_is_left_absolute() {
        let spots = [spot("/elsewhere/src/a.rs", 12)];
        assert_eq!(
            rank(&spots, Path::new("/w/repos/api")),
            vec!["/elsewhere/src/a.rs".to_string()]
        );
    }
}
