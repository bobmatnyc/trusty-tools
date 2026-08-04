//! Which agent SYSTEM authored this `.md` file? (issue #4448)
//!
//! Why: `.claude/agents/` is a shared convention, not a trusty-mpm namespace.
//! claude-mpm — a separate, still-live project — deploys into the same
//! directory reusing trusty-mpm's exact filenames: `engineer.md`, `ops.md`,
//! `qa.md`, `research.md`, `ticketing.md`, `memory-manager.md`,
//! `code-analyzer.md`, `code-critic.md`, `documentation.md`,
//! `web-ui-engineer.md`, `mpm-agent-manager.md`, `mpm-skills-manager.md`. A
//! sweep keyed on the FILENAME moves another project's files. So does one
//! keyed on the declared `name:` alone, because those collide too. The only
//! signal that separates the two systems is the SHAPE of the frontmatter, and
//! [`quarantine`](crate::agents::quarantine) — the one caller that MOVES
//! files — gates on it.
//!
//! What: [`agent_schema`] returns [`AgentSchema::TrustyMpm`] only when THREE
//! conditions hold together — no foreign key, the identity keys `name:`+`role:`,
//! and the composer's own base preamble ([`COMPOSED_BASE_MARKER`]) in the body.
//! Anything else is foreign or unknown, and the quarantine refuses it.
//!
//! The key set alone is NOT enough, and that was #4448 review's HIGH finding.
//! A whitelist is satisfied by OMISSION: a hand-authored agent declaring only
//! `name`/`role`/`description`/`model` declares nothing exotic, so it passed —
//! a real operator-written file on this machine classified as tm's. A key set
//! can rule a file OUT (a foreign key proves the composer did not write it);
//! only something present in the file can rule it IN. The body marker is that
//! something. See [`COMPOSED_BASE_MARKER`] for why a compose-time provenance
//! key would not work.
//!
//! The negative half is the key SET, deliberately not any single key:
//!
//! | Key | trusty-mpm | claude-mpm |
//! |---|---|---|
//! | `role:` | always | never (uses `agent_type:`) |
//! | `agent_type:` | never | always |
//! | `version:`, `effort:`, `author:`, `schema_version:`, `agent_id:` | never | one or more |
//! | `initialPrompt:` | **often** | often |
//!
//! That last row is the trap. `initialPrompt:` reads like a claude-mpm marker
//! and is not one: `merge_frontmatter` injects a role-derived prompt when the
//! source declares none (`builder.rs`, HR-1 Part B), so trusty-mpm emits it
//! for most agents. Treating it as foreign would exempt nearly the whole
//! bundled roster from the sweep.
//!
//! SIZE IS NOT A SIGNAL. It looks like one — trusty-mpm bodies run 3–4 KB and
//! claude-mpm's 8–80 KB — but the 2026-07-31 inventory found middle-generation
//! claude-mpm files at 23–81 KB overlapping and exceeding the bundled band.
//! Nothing here measures length.
//!
//! Test: `agent_schema_tests.rs`.

use crate::agents::frontmatter::parse_kv_line;

/// The agent system whose frontmatter shape this document matches.
///
/// Why: the quarantine's move gate is a POSITIVE identification — it acts only
/// on files it can prove trusty-mpm's own composer wrote. The two non-matching
/// cases stay distinct rather than collapsing into one "not ours", because the
/// receipt has to tell an operator WHICH foreign thing it declined to touch;
/// "skipped: claude-mpm schema" and "skipped: unrecognized schema" send an
/// operator to different places.
/// What: [`TrustyMpm`](Self::TrustyMpm) — every declared key is one this
/// crate's composer emits, `name:`/`role:` are both present, AND the body
/// carries [`COMPOSED_BASE_MARKER`]; [`ClaudeMpm`](Self::ClaudeMpm) — carries at
/// least one key unique to claude-mpm's schema;
/// [`Unrecognized`](Self::Unrecognized) — everything else, including a
/// hand-authored file (even a minimal one declaring nothing exotic), a
/// trusty-mpm SOURCE file (it still declares `extends:`, which the composer
/// strips, and its body is not yet the composed chain), and any document
/// without a well-formed frontmatter block.
/// Test: `schema_of_a_composed_trusty_mpm_agent`,
/// `schema_of_a_claude_mpm_agent`, `schema_of_a_hand_authored_file`,
/// `schema_of_a_minimal_hand_authored_file_with_no_exotic_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSchema {
    /// Matches the shape [`crate::agents::builder::compose_agent`] emits.
    TrustyMpm,
    /// Carries a key unique to claude-mpm's agent schema.
    ClaudeMpm,
    /// Neither shape — hand-authored, a source file, or unparseable.
    Unrecognized,
}

/// Every frontmatter key `merge_frontmatter` can emit, lower-cased.
///
/// Why: this is the whitelist the positive identification is built on, so it
/// must track the emitter EXACTLY. It is derived from the `out.push_str`
/// sequence in `crate::agents::builder::merge_frontmatter`; adding an emitted
/// key there without adding it here would make every freshly deployed agent
/// classify [`AgentSchema::Unrecognized`] and silently disable the sweep.
/// `pinned_to_the_composer_emission` fails when the two drift.
const TRUSTY_MPM_KEYS: &[&str] = &[
    "name",
    "role",
    "description",
    "model",
    "max_tokens",
    "resource_tier",
    "skills",
    "tools",
    "initialprompt",
];

/// Keys that appear ONLY in claude-mpm's agent schema.
///
/// Why: naming the foreign system explicitly is what lets the receipt say
/// "this belongs to claude-mpm, left alone" instead of a vague "skipped".
/// `initialPrompt` is deliberately ABSENT — see the module doc's trap note.
const CLAUDE_MPM_KEYS: &[&str] = &[
    "agent_type",
    "author",
    "schema_version",
    "agent_id",
    "effort",
    "version",
];

/// The composition root every tm-composed agent BODY carries.
///
/// Why: the frontmatter key set alone cannot answer "did tm write this?". It is
/// a whitelist, and a whitelist is satisfied by OMISSION — a hand-authored file
/// declaring nothing but `name`/`role`/`description`/`model` passes it, which is
/// how #4448 review found a real operator-written agent classifying as tm's. The
/// key set can only rule a file OUT (a foreign key proves it is not composer
/// output); something in the file has to rule it IN.
///
/// This is that something. `compose_agent` concatenates the whole `extends:`
/// chain base-first, and EVERY bundled agent's chain roots at `base-agent` —
/// all 42 in `crates/trusty-mpm/src/assets/agents/`, and `trusty-code`'s set
/// too. So every composed agent body opens with this line, and no file that tm
/// did not compose carries it unless someone pasted tm's own base preamble
/// verbatim, at which point the file IS a copy of a tm artifact.
///
/// What: byte-identical across EVERY revision of `BASE-AGENT.md` back to
/// 2026-05-19, including the pre-rename `trusty-mpm-core` path — which matters
/// because the files this sweep exists to move were written by OLD binaries.
/// That also rules out the alternative fix of stamping a fresh provenance key
/// at compose time: a new key marks only files written by NEW binaries, i.e.
/// exactly the ones that are not stale, making the sweep a no-op against its
/// own target.
///
/// A future edit to that heading does not cause a wrong move — it causes a
/// REFUSAL to move, which is the safe direction and the one this issue asks
/// for. Two tests guard it: `composed_marker_survives_composition` here, and
/// `the_bundled_base_agent_carries_the_composition_marker` in `trusty-mpm`,
/// which owns the asset and asserts its heading line is EXACTLY this string —
/// public for that reason, so the pin compares the real constant rather than a
/// second copy that could drift on its own.
/// Test: `composed_marker_survives_composition`.
pub const COMPOSED_BASE_MARKER: &str = "# BASE-AGENT — Foundation for all trusty-mpm agents";

/// Classify a document by its frontmatter key set.
///
/// Why: the movability predicate's fourth gate (#4448). A quarantine that
/// cannot tell trusty-mpm's artifacts from claude-mpm's moves a live project's
/// agents out from under it, and three of six independent inventory passes on
/// 2026-08-03 made exactly that error by keying on the filename.
/// What: reads the leading `---`-fenced frontmatter block's keys via
/// [`frontmatter_keys`] and returns [`AgentSchema::TrustyMpm`] only when the
/// key set is a SUBSET of [`TRUSTY_MPM_KEYS`] and contains both `name` and
/// `role`; otherwise [`AgentSchema::ClaudeMpm`] when any [`CLAUDE_MPM_KEYS`]
/// entry is present; otherwise [`AgentSchema::Unrecognized`]. Fail-closed at
/// every step: a missing block, an unterminated block, an unknown key, or a
/// missing `role:` all land outside `TrustyMpm`, and only `TrustyMpm` is
/// movable.
///
/// The body check is what makes this a POSITIVE identification rather than a
/// whitelist. Three conditions must ALL hold for [`AgentSchema::TrustyMpm`]:
/// no foreign key, the identity keys `name:`+`role:`, and
/// [`COMPOSED_BASE_MARKER`] in the body. Dropping the last one is the #4448
/// review's HIGH finding — a minimal hand-authored agent satisfies a key
/// whitelist by declaring nothing exotic.
///
/// Test: `schema_of_a_composed_trusty_mpm_agent`, `schema_of_a_claude_mpm_agent`,
/// `schema_of_a_hand_authored_file`,
/// `schema_of_a_minimal_hand_authored_file_with_no_exotic_key`,
/// `schema_of_a_source_file_with_extends`,
/// `schema_without_frontmatter_is_unrecognized`,
/// `schema_of_an_unterminated_block_is_unrecognized`,
/// `schema_requires_role`, `initial_prompt_alone_is_not_foreign`,
/// `composed_marker_survives_composition`,
/// `the_body_marker_is_what_separates_composed_from_hand_authored`.
pub fn agent_schema(content: &str) -> AgentSchema {
    let Some((keys, body)) = split_keys_and_body(content) else {
        return AgentSchema::Unrecognized;
    };

    // A foreign key is PROOF the composer did not write this, so it settles the
    // question before anything else is considered.
    if keys.iter().any(|k| CLAUDE_MPM_KEYS.contains(&k.as_str())) {
        return AgentSchema::ClaudeMpm;
    }

    let all_known = keys.iter().all(|k| TRUSTY_MPM_KEYS.contains(&k.as_str()));
    let has_identity = keys.iter().any(|k| k == "name") && keys.iter().any(|k| k == "role");
    // #4448: the positive half. Without it a hand-authored file passes by
    // declaring nothing unusual.
    let composed = body.contains(COMPOSED_BASE_MARKER);
    if all_known && has_identity && composed {
        return AgentSchema::TrustyMpm;
    }

    AgentSchema::Unrecognized
}

/// The keys declared in a document's leading frontmatter block, lower-cased.
///
/// Why: [`crate::agents::metadata::agent_metadata_from_str`] projects only the
/// keys trusty-mpm models and DROPS the rest, so it cannot answer "does this
/// file declare anything foreign?" — the exact question the schema gate asks.
/// This reads the raw key list instead.
/// What: `None` unless the document's FIRST line is a `---` fence and a
/// closing `---` fence follows — an unterminated block is not a frontmatter
/// block. Otherwise the keys [`parse_kv_line`] finds, in declaration order,
/// lower-cased by that parser. Non-`key: value` lines (a `- item` block-list
/// element, a folded-scalar continuation, a comment) yield no key. A
/// continuation line that happens to contain a colon contributes a spurious
/// key; that only ever ADDS keys, which can push a document out of
/// [`AgentSchema::TrustyMpm`] but never into it — the fail-closed direction.
/// Test: `keys_are_declaration_ordered`, `keys_skip_block_list_items`,
/// `keys_require_a_leading_fence`, `keys_require_a_closing_fence`.
pub fn frontmatter_keys(content: &str) -> Option<Vec<String>> {
    split_keys_and_body(content).map(|(keys, _)| keys)
}

/// [`frontmatter_keys`] plus everything after the closing fence.
///
/// Why: [`agent_schema`]'s positive half reads the BODY, and re-scanning for the
/// fence a second time would let the two halves disagree about where it is.
/// What: `None` unless the document's first line is a `---` fence and a closing
/// `---` fence follows. Otherwise the declared keys and the remaining text.
/// Test: `keys_are_declaration_ordered`, `keys_require_a_closing_fence`,
/// `schema_of_a_composed_trusty_mpm_agent`.
fn split_keys_and_body(content: &str) -> Option<(Vec<String>, &str)> {
    let mut rest = content.strip_prefix("---")?;
    // Tolerate `\r\n` and any trailing spaces on the opening fence.
    rest = rest
        .trim_start_matches([' ', '\t', '\r'])
        .strip_prefix('\n')?;

    let mut keys = Vec::new();
    let mut cursor = rest;
    while let Some(newline) = cursor.find('\n') {
        let (line, tail) = cursor.split_at(newline);
        let tail = &tail[1..];
        if line.trim_end() == "---" {
            return Some((keys, tail));
        }
        if let Some((key, _)) = parse_kv_line(line) {
            keys.push(key);
        }
        cursor = tail;
    }
    // A closing fence on the final line with no trailing newline still counts;
    // the body is then empty.
    if cursor.trim_end() == "---" {
        return Some((keys, ""));
    }
    // Unterminated: not a frontmatter block at all.
    None
}

#[cfg(test)]
#[path = "agent_schema_tests.rs"]
mod tests;
