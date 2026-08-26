//! Asking trusty-analyze which of a repository's files are worst (#6081).
//!
//! Why: #6078 gave trusty-review's investigation pass an `inspect_priority`
//! interface and no producer, so selection still ranked files by whether their
//! PATH NAME looked interesting. `GET /indexes/{id}/complexity_hotspots` is a
//! measurement instead — cyclomatic complexity per code chunk, computed over the
//! indexed corpus — and turning it into a ranked path list is the whole of the
//! intelligence the owner ruled belongs in this crate.
//!
//! What: [`fetch`], and the two pure halves it is made of —
//! [`hotspots_params`] and [`rank`] — so the shape of the answer can be
//! asserted without a daemon.
//!
//! ## Chunks in, files out
//!
//! The endpoint ranks CHUNKS (one function, one method), and the manifest names
//! FILES. Several chunks of one file collapse to the file's best rank, which is
//! also what keeps one pathological file from filling the whole list.
//!
//! #6145: the collapse KEEPS the winning chunk rather than only its path. The
//! chunks arrive already sorted descending, so the first chunk of a file is that
//! file's hottest function, and carrying its name, line range and cyclomatic
//! count is what lets trusty-review point the analysis at the function instead
//! of the file (#6146).
//!
//! #6287 (ADR-0032) moved the daemon onto a Unix socket, which retires the
//! `HTTP_PROXY` hazard #4392 added a loopback-pinned client for: a socket has no
//! proxy to be routed through, so a recipient's index id cannot leave the
//! machine by that path at all.
//!
//! Test: `hotspot_tests`, and `super::grounding_tests` for the live arms.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::priority::FunctionHotspot;

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
///
/// #6145: `function_name`, `start_line` and `end_line` were already on the wire
/// and discarded here. A daemon that omits any of them still parses — the
/// function-level record is then simply not built.
#[derive(Debug, Deserialize)]
struct Hotspot {
    #[serde(default)]
    file: String,
    #[serde(default)]
    cyclomatic: u32,
    #[serde(default)]
    function_name: Option<String>,
    #[serde(default)]
    start_line: u32,
    #[serde(default)]
    end_line: u32,
}

impl Hotspot {
    /// This chunk as a function-level record, when its line range is usable.
    ///
    /// Why the range and not the name decides: the range is what an instruction
    /// to "read this function" needs, and trusty-analyze's chunker names a
    /// function only for the languages whose parser it has. A named chunk with
    /// no range would produce "prioritize fn X" with nothing to point at.
    fn measured_function(&self) -> Option<FunctionHotspot> {
        if self.start_line == 0 || self.end_line < self.start_line {
            return None;
        }
        Some(FunctionHotspot {
            function: self
                .function_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned),
            start_line: self.start_line,
            end_line: self.end_line,
            cyclomatic: self.cyclomatic,
        })
    }
}

/// One file of the ranking, with the worst function measured inside it (#6145).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RankedFile {
    /// Repo-relative path (absolute when the chunk named a file outside it).
    pub path: String,
    /// The file's hottest measured function, when the chunk carried a range.
    pub hotspot: Option<FunctionHotspot>,
}

/// The method `trusty-analyze` ranks chunks on.
///
/// Duplicated as a literal rather than imported: this crate's whole discipline
/// is running PINNED binaries, and a Cargo edge on a workspace-version
/// `trusty-analyze` would defeat that.
/// `trusty_analyze::service::rpc::METHODS` is the definition.
const HOTSPOTS_METHOD: &str = "analyze.complexity_hotspots";

/// The params for one index's ranking.
///
/// #6287: this was a URL builder's job — a path segment plus a query string. A
/// JSON-RPC frame carries one `params` object, which is what removes the
/// trailing-slash tolerance that builder needed.
fn hotspots_params(index_id: &str) -> serde_json::Value {
    serde_json::json!({ "index_id": index_id, "top_n": HOTSPOT_CHUNKS })
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
/// occurrence wins — and since the sort is descending, that first occurrence IS
/// the file's hottest function, which the entry keeps (#6145) — zero-complexity
/// dropped, capped at [`MAX_PRIORITY_PATHS`].
/// Test: `hotspot_tests::{ranking_collapses_chunks_to_files_keeping_the_best_rank,
/// the_hottest_function_of_each_file_is_kept,
/// zero_complexity_chunks_carry_no_ranking, the_ranking_is_capped}`.
fn rank(hotspots: &[Hotspot], checkout: &Path) -> Vec<RankedFile> {
    let root = checkout.to_string_lossy().replace('\\', "/");
    let root = root.trim_end_matches('/');
    let mut out: Vec<RankedFile> = Vec::new();
    for spot in hotspots {
        if spot.cyclomatic == 0 || spot.file.is_empty() {
            continue;
        }
        let path = spot.file.replace('\\', "/");
        let relative = path
            .strip_prefix(root)
            .map_or(path.as_str(), |rest| rest.trim_start_matches('/'))
            .to_owned();
        if relative.is_empty() || out.iter().any(|ranked| ranked.path == relative) {
            continue;
        }
        out.push(RankedFile {
            path: relative,
            hotspot: spot.measured_function(),
        });
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
pub async fn fetch(
    socket: &Path,
    index_id: &str,
    checkout: &Path,
) -> Result<Vec<RankedFile>, String> {
    let at = socket.display();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": HOTSPOTS_METHOD,
        "params": hotspots_params(index_id),
    });
    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request(socket, &request, REQUEST_BUDGET)
            .await
            .map_err(|e| {
                format!("trusty-analyze did not answer {HOTSPOTS_METHOD} on {at} ({e})")
            })?;
    if let Some(error) = response.error {
        return Err(format!(
            "trusty-analyze refused {HOTSPOTS_METHOD} on {at} ({}: {})",
            error.code, error.message
        ));
    }
    let result = response.result.ok_or_else(|| {
        format!("trusty-analyze answered {at} with neither a result nor an error")
    })?;
    let envelope: HotspotEnvelope = serde_json::from_value(result)
        .map_err(|e| format!("trusty-analyze answered {at} with an unreadable body ({e})"))?;
    Ok(rank(&envelope.hotspots, checkout))
}

#[cfg(test)]
mod hotspot_tests {
    use super::*;

    fn spot(file: &str, cyclomatic: u32) -> Hotspot {
        Hotspot {
            file: file.to_owned(),
            cyclomatic,
            function_name: None,
            start_line: 0,
            end_line: 0,
        }
    }

    /// A chunk the daemon named and located, which is the ordinary case.
    fn located(file: &str, cyclomatic: u32, function: &str, start: u32, end: u32) -> Hotspot {
        Hotspot {
            file: file.to_owned(),
            cyclomatic,
            function_name: Some(function.to_owned()),
            start_line: start,
            end_line: end,
        }
    }

    /// The ranked paths alone, for the assertions that only care about order.
    fn paths(ranked: &[RankedFile]) -> Vec<String> {
        ranked.iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn the_params_name_the_index_and_the_chunk_count() {
        assert_eq!(
            hotspots_params("acme-api"),
            serde_json::json!({ "index_id": "acme-api", "top_n": HOTSPOT_CHUNKS })
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
        let ranked = rank(&env.hotspots, Path::new("/w/repos/acme-api"));
        assert_eq!(paths(&ranked), vec!["src/pay.rs".to_string()]);
        // #6145: the per-function fields the daemon already sent are kept.
        let measured = ranked[0].hotspot.as_ref().expect("the chunk was located");
        assert_eq!(measured.function.as_deref(), Some("f"));
        assert_eq!((measured.start_line, measured.end_line), (1, 9));
        assert_eq!(measured.cyclomatic, 31);
    }

    /// #6145: chunks arrive sorted descending, so the chunk that wins a file's
    /// place in the ranking is that file's hottest function — and the entry
    /// keeps it rather than only the path. Pre-fix this data reached the
    /// manifest as two bare strings.
    #[test]
    fn the_hottest_function_of_each_file_is_kept() {
        let spots = [
            located("/w/repos/api/src/pay.rs", 31, "settle_invoice", 40, 190),
            located("/w/repos/api/src/pay.rs", 22, "refund", 210, 260),
            located("/w/repos/api/src/auth.rs", 18, "verify_token", 12, 60),
        ];
        let ranked = rank(&spots, Path::new("/w/repos/api"));
        assert_eq!(
            ranked
                .iter()
                .map(|r| {
                    let h = r.hotspot.as_ref().expect("a measured function");
                    (
                        r.path.as_str(),
                        h.function.as_deref(),
                        h.start_line,
                        h.end_line,
                        h.cyclomatic,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("src/pay.rs", Some("settle_invoice"), 40, 190, 31),
                ("src/auth.rs", Some("verify_token"), 12, 60, 18),
            ],
            "the losing chunk of pay.rs must not displace the winner: {ranked:?}"
        );
    }

    /// A daemon that ranks a chunk without locating it still ranks its file —
    /// the file-level collapse is what the manifest needs, and the function
    /// record is an addition to it, never a precondition.
    #[test]
    fn a_chunk_with_no_usable_range_still_ranks_its_file() {
        let spots = [
            spot("/w/repos/api/src/a.rs", 30),
            Hotspot {
                start_line: 90,
                end_line: 12,
                ..located("/w/repos/api/src/b.rs", 20, "reversed", 0, 0)
            },
        ];
        let ranked = rank(&spots, Path::new("/w/repos/api"));
        assert_eq!(
            paths(&ranked),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
        assert!(
            ranked.iter().all(|r| r.hotspot.is_none()),
            "an absent or inverted range carries no function: {ranked:?}"
        );
    }

    /// An empty `function_name` is the daemon saying it could not name the
    /// chunk, not a function called "". The range still carries the analysis.
    #[test]
    fn an_unnamed_chunk_keeps_its_range() {
        let spots = [located("/w/repos/api/src/a.rs", 30, "  ", 4, 44)];
        let measured = rank(&spots, Path::new("/w/repos/api"))[0]
            .hotspot
            .clone()
            .expect("a located chunk");
        assert_eq!(measured.function, None);
        assert_eq!((measured.start_line, measured.end_line), (4, 44));
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
            paths(&rank(&spots, Path::new("/w/repos/api"))),
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
            paths(&rank(&spots, Path::new("/w/repos/api"))),
            vec!["/elsewhere/src/a.rs".to_string()]
        );
    }
}
