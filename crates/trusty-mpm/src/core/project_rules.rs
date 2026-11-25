//! The project's build and test rules, carried into every dispatch (#7684).
//!
//! Why: deployed agents carry `omitClaudeMd: true`, so a subagent no longer
//! loads the project CLAUDE.md, and with it the project's build, test and
//! commit rules. Deployed agent files are machine-global and checksum-tracked,
//! so project text cannot live in them; it reaches the agent at dispatch time
//! through the same `updatedInput` rewrite that carries the account block
//! (#11390).
//! What: [`rules_for`] collects every `CLAUDE.md` from the hook's cwd up to
//! the git root and returns their agent rules, root first: the regions between [`RULES_OPEN`] and
//! [`RULES_CLOSE`] when the file marks any, else the whole file. [`rules_block`]
//! renders them under [`PROJECT_RULES_HEADING`], cut at
//! [`PROJECT_RULES_CAP_BYTES`] on a line boundary with an in-band notice.
//! An absent or blank file yields no block; an unreadable one yields a
//! one-line [`RULES_UNAVAILABLE`] notice in the block, never silence.
//! Test: `project_rules_tests.rs`; the dispatch rewrite in
//! `pm_guard_accounts_tests.rs`.

use std::path::{Path, PathBuf};

/// The project instructions file the rules come from.
pub const CLAUDE_MD: &str = "CLAUDE.md";

/// Opens a region of `CLAUDE.md` that dispatched agents receive.
pub const RULES_OPEN: &str = "<!-- tm:agent-rules -->";

/// Closes a region opened by [`RULES_OPEN`].
pub const RULES_CLOSE: &str = "<!-- /tm:agent-rules -->";

/// Heading that opens the block; its presence in a prompt makes the append a no-op.
pub const PROJECT_RULES_HEADING: &str = "## Project rules (from CLAUDE.md)";

/// Largest rules text one block carries, in bytes.
pub const PROJECT_RULES_CAP_BYTES: usize = 16 * 1024;

/// Every `CLAUDE.md` from `cwd` up to its git root, root first.
///
/// What: walks `cwd`'s ancestors inclusive and stops at the first one holding
/// a `.git` entry (a checkout or a linked worktree). Outside any git repo it
/// keeps only the nearest file, so a stray `CLAUDE.md` above the project is
/// never carried. Any entry by that name counts, not only a regular file, so
/// one that cannot be read reaches [`rules_for`]'s notice (#7684).
/// Test: `nested_claude_md_rules_follow_the_root_rules`,
/// `rules_are_found_from_a_subdirectory`.
pub fn claude_mds_for(cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in cwd.ancestors() {
        let path = dir.join(CLAUDE_MD);
        if path.exists() {
            found.push(path);
        }
        if dir.join(".git").exists() {
            found.reverse();
            return found;
        }
    }
    // #7684: no git root; today's nearest-file behaviour.
    found.truncate(1);
    found
}

/// The agent rules a `CLAUDE.md` body carries.
///
/// What: every [`RULES_OPEN`]..[`RULES_CLOSE`] region, trimmed and joined by a
/// blank line, when the body marks at least one; an unclosed region runs to
/// the end. With no marker, the whole body. `None` when the result is blank.
/// Test: `marked_regions_are_extracted_in_order`,
/// `an_unmarked_file_is_carried_whole`, `a_blank_file_yields_nothing`.
pub fn extract(body: &str) -> Option<String> {
    let mut regions = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(RULES_OPEN) {
        let after = &rest[start + RULES_OPEN.len()..];
        let (region, next) = match after.find(RULES_CLOSE) {
            Some(end) => (&after[..end], &after[end + RULES_CLOSE.len()..]),
            None => (after, ""),
        };
        regions.push(region.trim());
        rest = next;
    }
    let text = if regions.is_empty() {
        body.trim().to_string()
    } else {
        regions.retain(|r| !r.is_empty());
        regions.join("\n\n")
    };
    (!text.is_empty()).then_some(text)
}

/// Opens the one-line notice carried in place of rules that could not be read.
pub const RULES_UNAVAILABLE: &str = "project rules unavailable";

/// The agent rules of the project containing `cwd`, or `None`.
///
/// What: [`extract`] over each file of [`claude_mds_for`], root first, joined
/// by a blank line; files yielding no rules are skipped. An unreadable file is
/// logged at `warn` and contributes a one-line [`RULES_UNAVAILABLE`] notice
/// naming the path and the error, so the agent knows those rules are missing.
/// The size cap applies to the joined text in [`rules_block`].
/// Test: `rules_are_found_from_a_subdirectory`,
/// `nested_claude_md_rules_follow_the_root_rules`,
/// `an_unreadable_claude_md_yields_a_notice`.
pub fn rules_for(cwd: &Path) -> Option<String> {
    let parts: Vec<String> = claude_mds_for(cwd)
        .iter()
        .filter_map(|path| match std::fs::read_to_string(path) {
            Ok(body) => extract(&body),
            // #7684: never drop the rules silently; the notice rides the dispatch.
            Err(err) => {
                tracing::warn!(path = %path.display(), "project rules not read: {err}");
                Some(format!("{RULES_UNAVAILABLE}: {}: {err}", path.display()))
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Render the `## Project rules` block appended to a dispatch prompt.
///
/// What: a separator, [`PROJECT_RULES_HEADING`], then `rules`; over
/// [`PROJECT_RULES_CAP_BYTES`] the whole lines that fit plus a notice naming
/// `CLAUDE.md`, so a cut is never silent.
/// Test: `block_carries_the_heading_and_rules`, `an_over_cap_block_is_cut_with_a_notice`.
pub fn rules_block(rules: &str) -> String {
    let mut text = String::new();
    if rules.len() <= PROJECT_RULES_CAP_BYTES {
        text.push_str(rules);
    } else {
        for line in rules.lines() {
            if text.len() + line.len() + 1 > PROJECT_RULES_CAP_BYTES {
                break;
            }
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("\n[cut at the size cap; read CLAUDE.md for the rest]");
    }
    format!("\n\n---\n{PROJECT_RULES_HEADING}\n\n{text}")
}
