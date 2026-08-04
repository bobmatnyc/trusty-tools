//! Tests for the delegation-authority scanner, roster resolver, and renderer.
//!
//! Split out of `delegation_authority.rs` (#4589) so the production file stays
//! under the 500-SLOC cap enforced by `scripts/check_line_cap.sh` — the same
//! move `instruction_pipeline.rs` made in #4318. The module is included with
//! `#[path]`, so `use super::*` still reaches every item (including the private
//! `is_foundation_file`) exactly as it did inline; no assertion changed.

use super::*;
use std::fs;
use tempfile::TempDir;

/// Write `<name>.md` into `dir` with the given raw content.
fn write_agent(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(format!("{name}.md")), content).expect("write agent");
}

#[test]
fn scan_finds_agents() {
    // A directory with one deployable agent and one base agent must yield
    // exactly the deployable one, with its frontmatter parsed.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nrole: engineer\nextends: base-engineer\n\
         description: Implements features and fixes bugs.\nmodel: sonnet\n---\n\n# Engineer\n",
    );
    write_agent(
        tmp.path(),
        "BASE-AGENT",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n",
    );

    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1, "only the engineer is deployable");
    let engineer = &agents[0];
    assert_eq!(engineer.name, "engineer");
    assert_eq!(engineer.role, "engineer");
    assert_eq!(
        engineer.description.as_deref(),
        Some("Implements features and fixes bugs.")
    );
    assert_eq!(engineer.model.as_deref(), Some("sonnet"));
    assert_eq!(
        engineer.extends_chain,
        vec!["base-engineer".to_string(), "engineer".to_string()]
    );
}

#[test]
fn scan_excludes_base_agents() {
    // Foundation templates are excluded by the `BASE-*` FILE NAME
    // convention, case-insensitively, and never by frontmatter (#4589 —
    // see `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`
    // for why the `role:` half was removed).
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "BASE-AGENT",
        "---\nname: base-agent\nrole: base\n---\n\nfoundation\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nrole: base-engineer\n---\n\nfoundation\n",
    );
    write_agent(
        tmp.path(),
        "qa",
        "---\nname: qa\nrole: qa\ndescription: Tests things.\n---\n\n# QA\n",
    );

    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "qa");
}

#[test]
fn agent_with_a_base_role_but_no_base_filename_stays_in_the_roster() {
    // #4589 REGRESSION. `memory-manager`, `mpm-agent-manager` and
    // `mpm-skills-manager` shipped with `role: base` and were therefore
    // dropped from every tier of the roster — deployed, dispatchable, and
    // invisible to the PM. The exclusion keys off the `BASE-*` FILE NAME
    // convention (tm's own, applied to the files tm ships) rather than a
    // frontmatter value tm does not control in the project and generic
    // `~/.claude/agents` tiers it also unions.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "memory-manager",
        "---\nname: memory-manager\nrole: base\ndescription: Manages memory.\n---\n\n# MM\n",
    );

    let agents = scan_agents(tmp.path());
    assert_eq!(
        agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        vec!["memory-manager"],
        "a non-`BASE-*` file must never be classified as a foundation \
         template by its `role:` value alone (#4589)"
    );
}

#[test]
fn near_miss_base_names_are_not_treated_as_foundation_files() {
    // #4589 REVIEW (HIGH). The first cut of this fix read
    // `starts_with("base")` with no separator while its docs and its test
    // name both said "`BASE-*` file name". Under that rule
    // `baseline-analyzer`, `base64-decoder`, `basecamp-sync` and
    // `based-reviewer` are all silently deleted from the roster — the same
    // "a rule tm controls eats an agent tm did not author, and no asset
    // edit can fix it" failure the frontmatter `role:` rule was removed
    // for. The failure would have MOVED rather than been removed.
    //
    // This pins both directions: the five bundled foundation files stay
    // out, and every near miss stays in.
    let tmp = TempDir::new().unwrap();
    let kept = [
        "baseline-analyzer",
        "base64-decoder",
        "basecamp-sync",
        "based-reviewer",
        "database-migrator",
        "engineer",
    ];
    let excluded = [
        "BASE-AGENT",
        "BASE-ENGINEER",
        "BASE-OPS",
        "BASE-QA",
        "BASE-RESEARCH",
        // Lower-case and mixed-case spellings are the same convention.
        "base-custom",
        "Base-Custom-Two",
    ];
    for name in kept.iter().chain(excluded.iter()) {
        write_agent(
            tmp.path(),
            name,
            &format!("---\nname: {name}\nrole: {name}\n---\n\n# {name}\n"),
        );
    }

    let mut scanned: Vec<String> = scan_agents(tmp.path())
        .into_iter()
        .map(|a| a.name)
        .collect();
    scanned.sort();
    let mut want: Vec<String> = kept.iter().map(|s| s.to_string()).collect();
    want.sort();

    assert_eq!(
        scanned, want,
        "only `base-`-prefixed files are foundation templates; a bare \
         `base` prefix silently deletes real agents (#4589 review)"
    );
}

#[test]
fn scan_empty_dir() {
    // An empty directory must yield an empty vec with no error.
    let tmp = TempDir::new().unwrap();
    let agents = scan_agents(tmp.path());
    assert!(agents.is_empty());
}

#[test]
fn scan_missing_dir_is_empty() {
    // A non-existent directory must also yield an empty vec, not panic.
    let agents = scan_agents(Path::new("/no/such/agents/dir/xyz"));
    assert!(agents.is_empty());
}

#[test]
fn scan_ignores_manifest_and_non_md() {
    // The deploy manifest and non-Markdown files must be skipped.
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("manifest.json"), "{}").unwrap();
    fs::write(tmp.path().join("notes.txt"), "hello").unwrap();
    write_agent(
        tmp.path(),
        "writer",
        "---\nname: writer\nrole: writer\n---\n\n# Writer\n",
    );
    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "writer");
}

#[test]
fn scan_handles_no_frontmatter() {
    // #4711 INVERTS this assertion. It previously read "an agent file with no
    // frontmatter falls back to the file stem" — that fallback is what minted
    // `base` as a delegatable agent from a prompt fragment. Claude Code
    // dispatches a subagent by its frontmatter `name:`, so a file without one
    // could never have been delegated to; advertising it was always wrong.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "# Plain agent\n\nNo frontmatter here.\n",
    );
    let agents = scan_agents(tmp.path());
    assert!(
        agents.is_empty(),
        "a file with no frontmatter has no `name:`, so it is not dispatchable \
         and must not enter the roster (#4711)"
    );
}

#[test]
fn bare_base_md_is_excluded_from_the_roster() {
    // #4711: `is_foundation_file` matched `base-` (hyphen) only, so the
    // hyphen-less `~/.claude/agents/base.md` — a "Base QA Instructions…
    // Appended to all QA agents" prompt fragment — became a delegatable agent
    // named `base`. Pinned WITH a `name:` so this test isolates the file-name
    // rule rather than passing incidentally via the `name:`-required rule.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base",
        "---\nname: base\nrole: base\n---\n\n# Base QA Instructions\n\n\
         Appended to all QA agents.\n",
    );
    write_agent(
        tmp.path(),
        "Base",
        "---\nname: Base\n---\n\n# Mixed-case spelling of the same convention\n",
    );
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nrole: engineer\n---\n\n# Engineer\n",
    );

    let names: Vec<String> = scan_agents(tmp.path())
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(
        names,
        vec!["engineer".to_string()],
        "a bare `base.md` is a foundation file in every spelling (#4711)"
    );
    assert!(is_foundation_file("base"));
    assert!(is_foundation_file("BASE"));
}

#[test]
fn base_prefixed_near_misses_survive_the_bare_base_rule() {
    // #4711 must not re-open #4589: matching the WHOLE stem `base` (not a
    // `base` PREFIX) is what keeps these real names in the roster.
    for stem in [
        "baseline-analyzer",
        "base64-decoder",
        "basecamp-sync",
        "based-reviewer",
    ] {
        assert!(
            !is_foundation_file(stem),
            "`{stem}` is an ordinary agent name, not a foundation template (#4589)"
        );
    }
    // …while the original `base-*` convention still excludes.
    for stem in ["BASE-AGENT", "base-custom", "Base-Custom-Two"] {
        assert!(
            is_foundation_file(stem),
            "`{stem}` is a foundation template"
        );
    }
}

#[test]
fn file_without_name_frontmatter_is_excluded_from_the_roster() {
    // #4711, the durable half: a content-less prompt fragment can carry any
    // file name at all. Requiring `name:` — the field Claude Code dispatches
    // by — is what stops the NEXT `base.md`-shaped file from becoming an
    // agent, whatever it is called.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "shared-qa-preamble",
        "---\nrole: qa\ndescription: Appended to all QA agents.\n---\n\n# Preamble\n",
    );
    write_agent(
        tmp.path(),
        "real-agent",
        "---\nname: real-agent\ndescription: Does real work.\n---\n\n# Real\n",
    );

    let agents = scan_agents(tmp.path());
    assert_eq!(
        agents.len(),
        1,
        "only the file declaring `name:` is a dispatchable agent (#4711)"
    );
    assert_eq!(agents[0].name, "real-agent");
    assert_eq!(
        agents[0].description.as_deref(),
        Some("Does real work."),
        "a real agent with `name:` frontmatter is still admitted unchanged"
    );
}

#[test]
fn generate_authority_nonempty() {
    // A single agent renders under the heading with its name and details.
    let agents = vec![AgentSummary {
        name: "engineer".to_string(),
        role: "engineer".to_string(),
        description: Some("Implements features.".to_string()),
        model: Some("sonnet".to_string()),
        extends_chain: vec![
            "base-agent".to_string(),
            "base-engineer".to_string(),
            "engineer".to_string(),
        ],
    }];
    let md = generate_authority(&agents);
    assert!(md.contains("## Delegation Authority"));
    assert!(md.contains("### engineer"));
    assert!(md.contains("Implements features."));
    assert!(md.contains("base-agent → base-engineer → engineer"));
    assert!(md.contains("**Model:** sonnet"));
}

#[test]
fn generate_authority_empty() {
    // With no agents the heading still renders, plus a "no agents" note.
    let md = generate_authority(&[]);
    assert!(md.contains("## Delegation Authority"));
    assert!(md.to_lowercase().contains("no delegatable agents"));
}

// ── colon-in-value regression tests (issue #389) ─────────────────────────

#[test]
fn scan_preserves_url_in_description() {
    // A `description:` value that is or contains a URL must not be
    // silently truncated at the first colon.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "docs-agent",
        "---\nname: docs-agent\nrole: docs\ndescription: See https://docs.example.com/guide\n---\n\n# Docs\n",
    );
    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(
        agents[0].description.as_deref(),
        Some("See https://docs.example.com/guide"),
        "URL in description must not be truncated"
    );
}

#[test]
fn scan_preserves_bedrock_model_id() {
    // A `model:` value containing a bedrock model id (with `/` and `.`)
    // must survive the scan without truncation.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "ml-agent",
        "---\nname: ml-agent\nrole: ml\nmodel: bedrock/us.anthropic.claude-sonnet-4-6\n---\n\n# ML\n",
    );
    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(
        agents[0].model.as_deref(),
        Some("bedrock/us.anthropic.claude-sonnet-4-6"),
        "bedrock model id must be preserved verbatim"
    );
}

#[test]
fn scan_preserves_timestamp_in_description() {
    // A description that embeds an ISO-8601 timestamp must keep the full
    // timestamp including the time component (colons after the first).
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "timed-agent",
        "---\nname: timed-agent\nrole: timer\ndescription: Deployed at 2026-06-05T14:31:34\n---\n\n# Timed\n",
    );
    let agents = scan_agents(tmp.path());
    assert_eq!(agents.len(), 1);
    assert_eq!(
        agents[0].description.as_deref(),
        Some("Deployed at 2026-06-05T14:31:34"),
        "timestamp in description must not be truncated"
    );
}

#[test]
fn every_bundled_non_base_agent_reaches_the_roster() {
    // Issue #4069 / #4589 REGRESSION GATE. The scanner's only exclusion is
    // the `BASE-*` file-name convention, so this asserts the property that
    // matters end to end: every bundled `.md` that is not a `BASE-*`
    // foundation file survives `scan_agents` and can therefore be routed
    // to. It is the asset-level half of the same invariant
    // `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`
    // asserts at the scanner level — one guards the shipped files, the
    // other guards the rule, and #4589 needed both.
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/agents");

    let mut expected: Vec<String> = fs::read_dir(&assets)
        .expect("bundled agent assets dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                // Shares the production predicate deliberately: a private
                // copy of the rule here would let the gate and the scanner
                // drift, which is the defect shape this PR removes.
                .filter(|stem| !is_foundation_file(stem))
                .map(str::to_string)
        })
        .collect();
    expected.sort();

    let mut scanned: Vec<String> = scan_agents(&assets).into_iter().map(|a| a.name).collect();
    scanned.sort();

    let dropped: Vec<&String> = expected.iter().filter(|n| !scanned.contains(n)).collect();
    assert!(
        dropped.is_empty(),
        "bundled agents silently dropped from every roster (check for a stray \
         `role: base` in their frontmatter): {dropped:?}"
    );
    assert_eq!(
        scanned.len(),
        expected.len(),
        "every non-BASE bundled agent must be delegatable"
    );
}

#[test]
fn generate_authority_omits_self_referential_lines() {
    // Review MEDIUM-3: `Role` that merely repeats the heading and a
    // single-element `Foundation` chain are pure noise, re-emitted into
    // every PM session. A real (multi-element) chain still renders.
    let agents = vec![AgentSummary {
        name: "ticketing".to_string(),
        role: "ticketing".to_string(),
        description: Some("Files tickets.".to_string()),
        model: None,
        extends_chain: vec!["ticketing".to_string()],
    }];
    let md = generate_authority(&agents);

    assert!(md.contains("### ticketing"));
    assert!(md.contains("Files tickets."));
    assert!(
        !md.contains("**Role:**"),
        "role duplicating the heading must be omitted"
    );
    assert!(
        !md.contains("**Foundation:**"),
        "a self-referential single-element chain must be omitted"
    );
}

#[test]
fn roster_from_dirs_dedup_is_case_insensitive() {
    // Review LOW-1: agent names are case-insensitive to the dispatcher, so
    // `QA` and `qa` across two tiers are one agent, not two entries.
    let high = TempDir::new().unwrap();
    let low = TempDir::new().unwrap();
    write_agent(
        high.path(),
        "QA",
        "---\nname: QA\nrole: qa\ndescription: PROJECT TIER\n---\n\n# QA\n",
    );
    write_agent(
        low.path(),
        "qa",
        "---\nname: qa\nrole: qa\ndescription: USER TIER\n---\n\n# QA\n",
    );

    let roster = roster_from_dirs(&[high.path().to_path_buf(), low.path().to_path_buf()]);

    assert_eq!(roster.len(), 1, "QA and qa are the same agent");
    assert_eq!(roster[0].description.as_deref(), Some("PROJECT TIER"));
}

#[test]
fn deployed_agent_dirs_puts_project_tier_first() {
    // Issue #4069: the project tier is the destination the daemon
    // managed-spawn path deploys into AND the only tier readable under
    // `--setting-sources project,local`, so it must rank first.
    let dirs = deployed_agent_dirs(Path::new("/proj"));
    assert_eq!(dirs[0], PathBuf::from("/proj/.claude/agents"));
    assert!(
        dirs.len() >= 2,
        "the user-level tier(s) must also be consulted"
    );
}

#[test]
fn roster_from_dirs_dedupes_with_first_dir_winning() {
    // The same agent is normally deployed into more than one tier; the
    // higher-precedence copy must be the one described, exactly once.
    let high = TempDir::new().unwrap();
    let low = TempDir::new().unwrap();
    write_agent(
        high.path(),
        "qa",
        "---\nname: qa\nrole: qa\ndescription: PROJECT TIER\n---\n\n# QA\n",
    );
    write_agent(
        low.path(),
        "qa",
        "---\nname: qa\nrole: qa\ndescription: USER TIER\n---\n\n# QA\n",
    );
    write_agent(
        low.path(),
        "ops",
        "---\nname: ops\nrole: ops\ndescription: USER TIER\n---\n\n# Ops\n",
    );

    let roster = roster_from_dirs(&[high.path().to_path_buf(), low.path().to_path_buf()]);

    assert_eq!(roster.len(), 2, "qa must appear once, plus ops");
    assert_eq!(roster[0].name, "ops", "roster is name-sorted");
    assert_eq!(roster[1].name, "qa");
    assert_eq!(roster[1].description.as_deref(), Some("PROJECT TIER"));
}

#[test]
fn roster_from_dirs_ignores_missing_dirs() {
    // A tier that does not exist is an empty tier, never a failure — this is
    // the input that makes `deployed_roster_section` return `None`, leaving
    // an unprovisioned environment with the bundled asset alone.
    let roster = roster_from_dirs(&[PathBuf::from("/nonexistent/agents")]);
    assert!(roster.is_empty());
    assert!(roster_from_dirs(&[]).is_empty());
}
