//! Tests for `crate::agents::describe` (#2074, epic #2892).
//!
//! Why: split out of `describe.rs` so the production file stays well under the
//! 500-SLOC cap `scripts/check_line_cap.sh` enforces, mirroring the
//! `protocol.rs` + `protocol_tests.rs` split already established beside it.
//! What: the provenance ladder end to end — a pristine deployed agent, a
//! hand-edited one, an untracked file, an absent ledger in each root, a corrupt
//! ledger — plus the tier labelling and the payload's own field contract.
//! Test: this file — self-describing.

use super::*;

use crate::agents::deploy::{ensure_roster_deployed, roster_target};

/// A project whose whole roster has been materialized, and its agents dir.
///
/// Why: three tests need a real deployed roster with a real ledger, and
/// re-deriving the target path at each call site would be a second copy of the
/// layout rule.
/// What: a `TempDir` kept alive by the caller plus
/// `<project>/.trusty-code/agents`.
fn deployed_project() -> (tempfile::TempDir, PathBuf) {
    let project = tempfile::tempdir().expect("project tempdir");
    ensure_roster_deployed(project.path()).expect("deploy the roster");
    let target = roster_target(project.path());
    (project, target)
}

/// The `warnings` array of a describe payload, as owned strings.
fn warnings_of(payload: &Value) -> Vec<String> {
    payload["warnings"]
        .as_array()
        .expect("warnings must always be an array")
        .iter()
        .map(|w| w.as_str().expect("warning must be a string").to_string())
        .collect()
}

#[test]
fn deployed_pristine_agent_reports_framework_origin_and_no_warnings() {
    let (_project, target) = deployed_project();

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");

    assert_eq!(payload["tier"], "project");
    assert_eq!(
        payload["path"],
        target.join("engineer.md").display().to_string()
    );
    assert_eq!(payload["provenance"]["manifest"], "present");
    assert_eq!(payload["provenance"]["origin"], "bundled");
    assert_eq!(payload["provenance"]["framework_owned"], true);
    assert_eq!(payload["provenance"]["checksum"], "match");
    assert!(
        payload["provenance"]["deployed_at"].is_string(),
        "a tracked file records when it was deployed: {payload:?}"
    );
    assert!(
        !payload["provenance"]["source_chain"]
            .as_array()
            .expect("source_chain")
            .is_empty(),
        "the deploy records the compose chain it resolved: {payload:?}"
    );
    assert_eq!(
        warnings_of(&payload),
        Vec::<String>::new(),
        "a pristine deployed agent has nothing wrong with it"
    );
}

#[test]
fn hand_edited_agent_reports_a_checksum_mismatch_and_warns() {
    let (_project, target) = deployed_project();
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\nmodel: marker/hand-edited\n---\n\nMine now.\n",
    )
    .expect("hand-edit the deployed agent");

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");

    assert_eq!(payload["provenance"]["checksum"], "mismatch");
    assert_eq!(
        payload["provenance"]["origin"], "bundled",
        "the ledger still records who wrote it originally"
    );
    assert_eq!(
        payload["model"], "marker/hand-edited",
        "the edited file is what resolves, so it is what is reported"
    );
    let warnings = warnings_of(&payload);
    assert_eq!(
        warnings.len(),
        1,
        "exactly one divergence warning: {warnings:?}"
    );
    assert!(
        warnings[0].contains("diverges from the bundled roster"),
        "the warning must name the divergence: {warnings:?}"
    );

    // Its untouched neighbour stays clean — the warning is per-file, not
    // per-directory.
    let neighbour = describe_agent(&target, "project", "rust-engineer", false).expect("describe");
    assert_eq!(neighbour["provenance"]["checksum"], "match");
    assert_eq!(warnings_of(&neighbour), Vec::<String>::new());
}

#[test]
fn unknown_name_is_a_typed_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let err = describe_agent(tmp.path(), "project", "no-such-agent", false)
        .expect_err("an unresolvable name must be an error, never a panic");

    assert_eq!(
        err.code,
        RpcError::not_found("x").code,
        "an unknown agent is a not_found, not an internal error"
    );
    assert!(
        err.message.contains("no-such-agent"),
        "the error must name what was asked for: {err:?}"
    );
}

#[test]
fn unparseable_disk_agent_is_broken_with_the_parse_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // `extends:` naming a nonexistent parent is the same compose failure
    // `resolve_agent` refuses to dispatch through.
    std::fs::write(
        tmp.path().join("engineer.md"),
        "---\nname: engineer\nextends: nonexistent-parent\n---\n\nBody.\n",
    )
    .expect("write");

    let payload = describe_agent(tmp.path(), "project", "engineer", false)
        .expect("a broken file is reported, not raised");

    assert_eq!(payload["tier"], "broken");
    assert_eq!(payload["name"], "engineer");
    assert!(payload["instructions"].is_null());
    assert!(payload["tools"].is_null());
    let warnings = warnings_of(&payload);
    assert!(
        warnings.iter().any(|w| w.starts_with("parse error:")),
        "the parse failure must be a warning an operator can read: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("nonexistent-parent") || w.contains("compose")),
        "the warning must carry the underlying reason: {warnings:?}"
    );
}

#[test]
fn embedded_agent_has_no_disk_path_and_no_warnings() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let payload = describe_agent(tmp.path(), "project", "engineer", false).expect("describe");

    assert_eq!(payload["tier"], "embedded");
    assert!(payload["path"].is_null());
    assert_eq!(payload["provenance"]["manifest"], "not-applicable");
    assert!(payload["provenance"]["origin"].is_null());
    assert_eq!(
        warnings_of(&payload),
        Vec::<String>::new(),
        "an agent with no file cannot have a stale one"
    );
}

#[test]
fn instructions_text_is_omitted_by_default_and_returned_on_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("solo.md"),
        "---\nname: solo\n---\n\nYou are a solo agent.\n",
    )
    .expect("write");

    let terse = describe_agent(tmp.path(), "project", "solo", false).expect("describe");
    assert_eq!(terse["instructions"]["length_bytes"], 21);
    assert!(
        terse["instructions"]["text"].is_null(),
        "the text is opt-in: {terse:?}"
    );

    let full = describe_agent(tmp.path(), "project", "solo", true).expect("describe");
    assert_eq!(full["instructions"]["text"], "You are a solo agent.");
    assert_eq!(
        full["instructions"]["length_bytes"], 21,
        "the length must describe the same text either way"
    );
}

#[test]
fn describe_reports_the_tools_allowlist_and_declared_skills() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("locked-down.md"),
        "---\nname: locked-down\ntools: [read_file, grep]\nskills: [systematic-debugging]\n---\n\nBody.\n",
    )
    .expect("write");

    let payload = describe_agent(tmp.path(), "project", "locked-down", false).expect("describe");

    assert_eq!(payload["tools"]["allowed"], json!(["read_file", "grep"]));
    assert_eq!(payload["skills"], json!(["systematic-debugging"]));
    assert!(
        payload.get("grants").is_none(),
        "capability grants are not modelled yet — the key must be absent, never invented: {payload:?}"
    );
}

#[test]
fn unrestricted_agent_reports_a_null_allowlist() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("open.md"),
        "---\nname: open\n---\n\nBody.\n",
    )
    .expect("write");

    let payload = describe_agent(tmp.path(), "project", "open", false).expect("describe");

    assert!(
        payload["tools"]["allowed"].is_null(),
        "null means every registered tool, matching ToolsConfig's own semantics: {payload:?}"
    );
}

#[test]
fn untracked_disk_file_warns_that_trusty_code_did_not_write_it() {
    let (_project, target) = deployed_project();
    std::fs::write(
        target.join("my-custom.md"),
        "---\nname: my-custom\n---\n\nProject-owned.\n",
    )
    .expect("write");

    let payload = describe_agent(&target, "project", "my-custom", false).expect("describe");

    assert_eq!(payload["provenance"]["manifest"], "present");
    assert!(payload["provenance"]["origin"].is_null());
    let warnings = warnings_of(&payload);
    assert!(
        warnings.iter().any(|w| w.contains("not tracked")),
        "an untracked file must say so rather than claim an origin: {warnings:?}"
    );
}

#[test]
fn absent_ledger_in_the_native_roster_dir_warns() {
    let project = tempfile::tempdir().expect("project tempdir");
    let target = roster_target(project.path());
    std::fs::create_dir_all(&target).expect("mkdir the native agents dir");
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\n---\n\nBody.\n",
    )
    .expect("write");

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");

    assert_eq!(payload["provenance"]["manifest"], "absent");
    let warnings = warnings_of(&payload);
    assert!(
        warnings.iter().any(|w| w.contains("manifest missing")),
        "a missing ledger in Trusty Code's own root is an anomaly: {warnings:?}"
    );
}

#[test]
fn absent_ledger_in_a_compat_root_does_not_warn() {
    let project = tempfile::tempdir().expect("project tempdir");
    let target = project.path().join(".claude").join("agents");
    std::fs::create_dir_all(&target).expect("mkdir the compat agents dir");
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\n---\n\nBody.\n",
    )
    .expect("write");

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");

    assert_eq!(payload["provenance"]["manifest"], "absent");
    assert_eq!(
        warnings_of(&payload),
        Vec::<String>::new(),
        "a `.claude/agents` catalog never had a ledger and is not expected to"
    );
}

#[test]
fn corrupt_ledger_is_reported_as_a_warning() {
    let (_project, target) = deployed_project();
    std::fs::write(target.join(MANIFEST_FILE), "{ not json").expect("corrupt the ledger");

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");

    assert_eq!(payload["provenance"]["manifest"], "corrupt");
    assert!(
        payload["provenance"]["checksum"].is_null(),
        "nothing can be verified against an unreadable ledger: {payload:?}"
    );
    let warnings = warnings_of(&payload);
    assert!(
        warnings.iter().any(|w| w.contains("could not be read")),
        "the corruption must be surfaced, never treated as an empty ledger: {warnings:?}"
    );
}

#[test]
fn disk_entry_has_warnings_agrees_with_the_describe_payload() {
    let (_project, target) = deployed_project();
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\n---\n\nEdited.\n",
    )
    .expect("hand-edit");

    let ledger = load_ledger(&target);
    assert!(
        disk_entry_has_warnings(&target, &ledger, "engineer"),
        "the edited row must be flagged"
    );
    assert!(
        !disk_entry_has_warnings(&target, &ledger, "rust-engineer"),
        "an untouched row must not be"
    );
    assert!(
        !warnings_of(&describe_agent(&target, "project", "engineer", false).expect("describe"))
            .is_empty()
    );
}

#[test]
fn native_roster_dir_is_recognised_only_by_both_path_components() {
    let root = Path::new("/tmp/project");
    assert!(is_native_roster_dir(
        &root.join(TRUSTY_CODE_DIRNAME).join(AGENTS_DIRNAME)
    ));
    assert!(!is_native_roster_dir(
        &root.join(".claude").join(AGENTS_DIRNAME)
    ));
    assert!(!is_native_roster_dir(
        &root.join(TRUSTY_CODE_DIRNAME).join("skills")
    ));
}

// ---------------------------------------------------------------------------
// #4698: the file's own `provenance:` claim, reported beside the ledger origin.
// ---------------------------------------------------------------------------

/// EVERY bundled agent lands on disk carrying `provenance: framework-owned`
/// after a real roster deploy — the guarantee #4698 asks for, asserted against
/// the actual bundled assets rather than a fixture.
#[test]
fn every_deployed_bundled_agent_carries_the_framework_owned_stamp() {
    let (_project, target) = deployed_project();

    let mut checked = 0usize;
    for entry in std::fs::read_dir(&target).expect("read the deployed roster") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("read a deployed agent");
        assert!(
            content.contains("provenance: framework-owned"),
            "{} is missing the stamp:\n{content}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "the roster deployed at least one agent");
}

/// The payload reports the declaration beside the ledger's origin, and the two
/// agree on a pristine deployed file.
#[test]
fn deployed_agent_reports_its_declared_provenance() {
    let (_project, target) = deployed_project();

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");
    assert_eq!(payload["provenance"]["origin"], "bundled");
    assert_eq!(payload["provenance"]["framework_owned"], true);
    assert_eq!(
        payload["provenance"]["declared_provenance"], "framework-owned",
        "the file's own claim agrees with the ledger: {payload:?}"
    );
}

/// A hand-edit that strips the field is visible as a null declaration against a
/// still-framework-owned ledger row — the disagreement the checksum alone does
/// not explain. The ledger stays authoritative.
#[test]
fn hand_edited_agent_reports_a_null_declared_provenance() {
    let (_project, target) = deployed_project();
    std::fs::write(
        target.join("engineer.md"),
        "---\nname: engineer\nmodel: marker/hand-edited\n---\n\nMine now.\n",
    )
    .expect("hand-edit the deployed agent");

    let payload = describe_agent(&target, "project", "engineer", false).expect("describe");
    assert_eq!(payload["provenance"]["checksum"], "mismatch");
    assert_eq!(payload["provenance"]["framework_owned"], true);
    assert!(
        payload["provenance"]["declared_provenance"].is_null(),
        "the edit dropped the field: {payload:?}"
    );
}

/// The key is always present, so a client can tell "not declared" from "this
/// server predates the field" — the same contract the other provenance keys hold.
#[test]
fn declared_provenance_key_is_present_even_with_no_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("solo.md"),
        "---\nname: solo\n---\n\nHand-written.\n",
    )
    .expect("write an agent");

    let payload = describe_agent(dir.path(), "project", "solo", false).expect("describe");
    assert!(
        payload["provenance"]
            .as_object()
            .expect("provenance object")
            .contains_key("declared_provenance"),
        "the key is always emitted: {payload:?}"
    );
    assert!(payload["provenance"]["declared_provenance"].is_null());
}
