//! GitHub Projects v2 node ids (`PVT_…`, `PVTI_…`), admitted by exact shape.
//!
//! Why (issue #9748): a project id (`PVT_` + 16 characters) and a project item
//! id (`PVTI_` + 23 characters) are public GraphQL handles that agents record
//! as state, but the tail is base64url with mixed case and digits, so the
//! mixed-case branch of [`super::looks_like_secret`] refused every memory that
//! named one. It is the #7482 `dpl_` family: a vendor prefix, then a tail no
//! shape test can tell from a key body, so the exemption is pinned as narrowly
//! as the format allows.
//! What: [`is_github_project_node_id`] is the whole-token shape test, and
//! [`GITHUB_PROJECT_NODE_IDS`] holds the one admitted shape per id kind.
//! Test: `github_project_node_ids_are_accepted_after_9748`,
//! `github_project_node_id_look_alikes_keep_their_verdict_after_9748`.

/// `(prefix, tail length, tail openings)` for each admitted Projects v2 node
/// id kind. Prefix and openings are case-sensitive.
///
/// Why these values, measured (2026-10-13) over 902 ids the GraphQL API
/// returned for user-owned and organization-owned projects: every `PVT_` tail
/// was 16 characters and every `PVTI_` tail 23, drawn from the base64url
/// alphabet. The tail is base64url-encoded MessagePack, so its first four
/// characters encode the array header and the owner-kind flag: `kwDO`/`lADO`
/// for an organization owner, `kwHO`/`lAHO` for a user owner. Pinning the
/// opening as well as the length cuts the blind spot by a factor of about
/// 64^4 / 2 against an exact-length window alone.
///
/// THE COST, as for `PUBLIC_ID_PREFIXES`: a credential body of exactly the
/// pinned length, opening with a pinned header, written flush behind the
/// prefix, is admitted. No predicate can separate it from a real id. Any other
/// length or opening (an owner id below 2^16, a future 64-bit item id) is not
/// admitted and keeps its pre-#9748 verdict; widen only on evidence.
/// What: consulted only by [`is_github_project_node_id`].
/// Test: `github_project_node_id_look_alikes_keep_their_verdict_after_9748`.
pub(crate) const GITHUB_PROJECT_NODE_IDS: &[(&str, usize, &[&str])] = &[
    ("PVT_", 16, &["kwDO", "kwHO"]),
    ("PVTI_", 23, &["lADO", "lAHO"]),
];

/// True when `token` is, in its entirety, a [`GITHUB_PROJECT_NODE_IDS`]
/// shape: the prefix, then a tail of exactly the pinned length that opens
/// with a pinned header and is otherwise base64url (`A-Za-z0-9`, `-`, `_`).
///
/// Why a whole-token predicate (#7482 precedent): this narrows detection, so
/// anything before or after the id must disqualify the token. A declined token
/// is judged by the heuristics exactly as before #9748.
/// What: case-sensitive prefix strip, exact length, opening match, charset.
/// Returns `false` for every other input; it never defaults to accept.
/// Test: `github_project_node_ids_are_accepted_after_9748`,
/// `github_project_node_id_look_alikes_keep_their_verdict_after_9748`.
pub(crate) fn is_github_project_node_id(token: &str) -> bool {
    GITHUB_PROJECT_NODE_IDS
        .iter()
        .any(|(prefix, tail_len, openings)| {
            token.strip_prefix(prefix).is_some_and(|tail| {
                tail.len() == *tail_len
                    && openings.iter().any(|o| tail.starts_with(o))
                    && tail
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        })
}
