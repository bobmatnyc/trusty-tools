//! Tests for the report manifest loader and validation.
//!
//! Why: the manifest is the user-facing entry point; its parse + validation
//! rules (exactly one source per entry, non-empty list) must be pinned so a
//! malformed manifest always fails with the precise error.
//! What: exercises both source kinds, the two mutual-exclusion errors, the
//! empty-manifest error, orphaned-username tolerance, and slug derivation.
//! Test: included as `#[cfg(test)] mod tests` from `manifest.rs`.

use std::path::Path;

use super::{FunctionHotspot, ManifestError, RepositorySource, parse_manifest, slugify};

fn parse(text: &str) -> std::result::Result<super::Manifest, ManifestError> {
    parse_manifest(text, Path::new("manifest.toml"))
}

/// Why: a local-path entry is the common case and must parse into `LocalPath`.
/// What: parses a single local entry and asserts fields + source kind.
/// Test: this test itself.
#[test]
fn parse_local_path_entry() {
    let toml = r#"
        [report]
        title = "Acme DD"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        ref = "main"
        metrics = "acme.json"
    "#;
    let m = parse(toml).expect("parse ok");
    assert_eq!(m.report.title, "Acme DD");
    assert_eq!(m.report.analyst.as_deref(), Some("bobmatnyc"));
    assert_eq!(m.repositories.len(), 1);
    let r = &m.repositories[0];
    assert_eq!(r.name, "Website");
    assert_eq!(r.slug, "website");
    assert_eq!(r.git_ref.as_deref(), Some("main"));
    assert!(matches!(r.source, RepositorySource::LocalPath { .. }));
}

/// Why: a remote entry must parse into `Remote` and retain the username.
/// What: parses a single remote entry and asserts source kind + username.
/// Test: this test itself.
#[test]
fn parse_remote_entry() {
    let toml = r#"
        [report]
        title = "Remote DD"

        [[repositories]]
        name = "API"
        remote = "bobmatnyc/trusty-tools"
        username = "bobmatnyc"
        ref = "v1.2.3"
    "#;
    let m = parse(toml).expect("parse ok");
    let r = &m.repositories[0];
    assert_eq!(r.username.as_deref(), Some("bobmatnyc"));
    match &r.source {
        RepositorySource::Remote { remote } => assert_eq!(remote, "bobmatnyc/trusty-tools"),
        other => panic!("expected remote, got {other:?}"),
    }
}

/// Why: a manifest with no repositories cannot produce a report.
/// What: asserts `NoRepositories` for a report-only manifest.
/// Test: this test itself.
#[test]
fn no_repositories_errors() {
    let toml = r#"
        [report]
        title = "Empty"
    "#;
    let err = parse(toml).expect_err("must error");
    assert!(matches!(err, ManifestError::NoRepositories));
}

/// Why: declaring both `path` and `remote` is ambiguous and must be rejected.
/// What: asserts `ConflictingSources` naming the offending entry.
/// Test: this test itself.
#[test]
fn conflicting_sources_error() {
    let toml = r#"
        [report]
        title = "Conflict"

        [[repositories]]
        name = "Both"
        path = "/tmp/x"
        remote = "owner/repo"
    "#;
    let err = parse(toml).expect_err("must error");
    match err {
        ManifestError::ConflictingSources { name } => assert_eq!(name, "Both"),
        other => panic!("expected ConflictingSources, got {other:?}"),
    }
}

/// Why: an entry with neither source cannot be analyzed and must be rejected.
/// What: asserts `MissingSource` naming the offending entry.
/// Test: this test itself.
#[test]
fn missing_source_error() {
    let toml = r#"
        [report]
        title = "Missing"

        [[repositories]]
        name = "Neither"
    "#;
    let err = parse(toml).expect_err("must error");
    match err {
        ManifestError::MissingSource { name } => assert_eq!(name, "Neither"),
        other => panic!("expected MissingSource, got {other:?}"),
    }
}

/// Why: a username on a local entry is meaningless but must not fail the load
/// (documented behaviour: kept + warned, not an error).
/// What: asserts a local entry with a username parses and retains the username.
/// Test: this test itself.
#[test]
fn orphaned_username_tolerated() {
    let toml = r#"
        [report]
        title = "Orphan"

        [[repositories]]
        name = "Local"
        path = "/tmp/local"
        username = "ignored"
    "#;
    let m = parse(toml).expect("parse ok — username tolerated");
    assert_eq!(m.repositories[0].username.as_deref(), Some("ignored"));
    assert!(matches!(
        m.repositories[0].source,
        RepositorySource::LocalPath { .. }
    ));
}

/// Why: bad TOML must surface a parse error, not a silent empty manifest.
/// What: asserts `Parse` for malformed input.
/// Test: this test itself.
#[test]
fn bad_toml_parse_error() {
    let err = parse("this is not = valid = toml").expect_err("must error");
    assert!(matches!(err, ManifestError::Parse { .. }));
}

/// Why: slug derivation must be stable and filesystem-safe.
/// What: asserts lowercasing, non-alnum collapse, trimming, and empty fallback.
/// Test: this test itself.
#[test]
fn slug_derivation() {
    assert_eq!(slugify("Acme Web App"), "acme-web-app");
    assert_eq!(slugify("  Foo//Bar  "), "foo-bar");
    assert_eq!(slugify("!!!"), "report");
    assert_eq!(slugify("Already-slugged"), "already-slugged");
}

/// Why: #5239 — an orchestrator's unassessed areas travel to the report through
/// the manifest, and a manifest that declares none must behave exactly as before.
/// Test: itself.
#[test]
fn parse_declared_gaps() {
    let m = parse_manifest(
        "[report]\ntitle = \"T\"\ngaps = [\"Stage `dora` did not complete.\", \"No CVE scan.\"]\n\n\
         [[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert_eq!(
        m.report.gaps,
        vec![
            "Stage `dora` did not complete.".to_string(),
            "No CVE scan.".to_string()
        ]
    );

    let none = parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert!(none.report.gaps.is_empty(), "absent key defaults to empty");
}

/// #5405: the ticketing artifact rides on `[report]`, and `metrics` rides on a
/// repository entry. The fixture declares both, so each must land in its own
/// field with the other's value untouched — `metrics` is asserted nowhere else
/// in this file (`parse_local_path_entry` declares the key and never reads it).
#[test]
fn parse_ticketing_path() {
    let m = parse_manifest(
        "[report]\ntitle = \"T\"\nticketing = \"ticketing.json\"\n\n\
         [[repositories]]\nname = \"A\"\npath = \"/x\"\nmetrics = \"acme.json\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert_eq!(
        m.report.ticketing,
        Some(std::path::PathBuf::from("ticketing.json"))
    );
    assert_eq!(
        m.repositories[0].metrics,
        Some(std::path::PathBuf::from("acme.json")),
        "a declared metrics file must survive alongside a declared ticketing artifact"
    );

    let none = parse_manifest(
        "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        Path::new("m.toml"),
    )
    .expect("parses");
    assert!(
        none.report.ticketing.is_none(),
        "absent key defaults to None, which renders the section as unassessed"
    );
}

/// Why: `inspect_priority` is the interface trusty-audit writes its selection
/// ranking to, and it must accept both a bare path list (hand-written) and a
/// weighted table (generated) without a second key.
/// What: parses a mixed list and asserts each entry's path and weight.
/// Test: this test itself.
#[test]
fn parse_inspect_priority_shapes() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = [
            "src/auth/login.rs",
            { path = "src/billing.rs", weight = 5000 },
            { path = "src/queue.rs" },
        ]
    "#;
    let m = parse(toml).expect("parse ok");
    let p = &m.repositories[0].inspect_priority;
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].path, "src/auth/login.rs");
    assert_eq!(p[1].path, "src/billing.rs");
    assert_eq!(
        p[1].weight, 5000,
        "an explicit weight wins over the position"
    );
    assert_eq!(p[2].path, "src/queue.rs");
    assert!(
        p.iter()
            .all(|e| e.dimension.is_none() && e.reason.is_none()),
        "attribution is optional and absent here"
    );
}

/// Why: #6082's discovery leg writes WHY it ranked a file — which dimension it
/// is evidence for, and which query found it — and the coverage section renders
/// both. A reader that dropped them would silently discard the attribution.
/// What: the attributed table shape parses, alongside the bare and weighted
/// ones, and keeps its positional weight when no explicit one is declared.
/// Test: this test itself.
#[test]
fn parse_inspect_priority_attribution() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = [
            { path = "src/session.rs", dimension = "authentication & secrets", reason = "trusty-search hit for \"credential handling\" (score 0.88, line 18)" },
            "src/queue.rs",
        ]
    "#;
    let m = parse(toml).expect("parse ok");
    let p = &m.repositories[0].inspect_priority;
    assert_eq!(p[0].path, "src/session.rs");
    assert_eq!(p[0].weight, 1000, "the position still sets the weight");
    assert_eq!(p[0].dimension.as_deref(), Some("authentication & secrets"));
    assert!(
        p[0].reason
            .as_deref()
            .expect("a reason")
            .contains("credential handling"),
    );
    assert!(p[1].dimension.is_none(), "a bare path carries none");
}

/// #6145/#6146: trusty-audit declares the file's worst measured function as a
/// nested table, and this reader turns it into the prompt's focus line. A
/// manifest written before the key existed — every shape in
/// `parse_inspect_priority_shapes` above — carries `None` and is unaffected.
#[test]
fn parse_hotspot_table() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = [
            { path = "src/pay.rs", reason = "trusty-analyze complexity hotspot (rank 1)", hotspot = { function = "settle_invoice", start_line = 40, end_line = 190, cyclomatic = 31 } },
            { path = "src/queue.rs", hotspot = { start_line = 4, end_line = 44, cyclomatic = 12 } },
            "src/plain.rs",
        ]
    "#;
    let m = parse(toml).expect("parse ok");
    let p = &m.repositories[0].inspect_priority;

    let measured = p[0].hotspot.as_ref().expect("a declared measurement");
    assert_eq!(measured.function.as_deref(), Some("settle_invoice"));
    assert_eq!((measured.start_line, measured.end_line), (40, 190));
    assert_eq!(measured.cyclomatic, 31);
    assert_eq!(
        measured.focus().as_deref(),
        Some(
            "Hotspot: lines 40-190, fn settle_invoice, cyclomatic 31 — prioritize DD analysis of \
             this function."
        )
    );

    // An unnamed measurement still points at its range.
    let unnamed = p[1].hotspot.as_ref().expect("a declared measurement");
    assert_eq!(unnamed.function, None);
    assert_eq!(
        unnamed.focus().as_deref(),
        Some("Hotspot: lines 4-44, cyclomatic 12 — prioritize DD analysis of this function.")
    );

    assert!(p[2].hotspot.is_none(), "a bare path carries none");
    assert_eq!(p[0].weight, 1000, "the position still sets the weight");
}

/// #6146: a table missing the range is inert, not a manifest that fails to
/// load. The interface's rule is that a declaration it cannot act on costs
/// nothing — the same rule a priority naming no tracked file already follows.
#[test]
fn a_hotspot_without_a_range_has_no_focus_line() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = [
            { path = "src/pay.rs", hotspot = { function = "settle_invoice" } },
            { path = "src/queue.rs", hotspot = { function = "drain", start_line = 90, end_line = 12 } },
        ]
    "#;
    let m = parse(toml).expect("a half-written table must not fail the load");
    for entry in &m.repositories[0].inspect_priority {
        assert_eq!(
            entry.hotspot.as_ref().and_then(FunctionHotspot::focus),
            None,
            "{entry:?}"
        );
    }
}

/// Why: the declared order IS the ranking, so an unweighted list must come out
/// strictly descending — otherwise the selection sort breaks ties by path and
/// scrambles what the ranker asked for.
/// What: three bare paths get 1000/999/998; an absent key yields an empty list,
/// which is what keeps selection byte-identical to a pre-#6078 manifest.
/// Test: this test itself.
#[test]
fn inspect_priority_weights_follow_declared_rank() {
    let toml = r#"
        [report]
        title = "Acme DD"

        [[repositories]]
        name = "Website"
        path = "/tmp/acme-web"
        inspect_priority = ["c.rs", "b.rs", "a.rs"]
    "#;
    let m = parse(toml).expect("parse ok");
    let weights: Vec<u32> = m.repositories[0]
        .inspect_priority
        .iter()
        .map(|p| p.weight)
        .collect();
    assert_eq!(weights, vec![1000, 999, 998]);

    let none = parse("[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n")
        .expect("parses");
    assert!(
        none.repositories[0].inspect_priority.is_empty(),
        "absent key must leave selection byte-identical to a pre-#6078 manifest"
    );
}

/// Why: #6135 — the manifest is the durable carrier of a run's inference
/// identity, so the section has to parse into the four values the resolver
/// consumes.
/// What: a full `[inference]` table, read back field by field.
/// Test: this test itself.
#[test]
fn parse_inference_section() {
    let toml = r#"
        [report]
        title = "Acme"

        [inference]
        provider = "openrouter"
        reviewer = "anthropic/claude-opus-4.8"
        verifier = "anthropic/claude-haiku-4.5"
        summarizer = "anthropic/claude-haiku-4.5"

        [[repositories]]
        name = "API"
        path = "/tmp/acme-api"
    "#;
    let m = parse(toml).expect("parse ok");
    let inference = m.inference.expect("the section is declared");
    assert_eq!(inference.provider.as_deref(), Some("openrouter"));
    assert_eq!(
        inference.reviewer.as_deref(),
        Some("anthropic/claude-opus-4.8")
    );
    assert_eq!(
        inference.verifier.as_deref(),
        Some("anthropic/claude-haiku-4.5")
    );
    assert_eq!(
        inference.summarizer.as_deref(),
        Some("anthropic/claude-haiku-4.5")
    );
    assert!(!inference.is_empty());
}

/// Why: back-compat is the whole reason the key is optional — every manifest
/// written before #6135 must load unchanged and resolve through the layers it
/// always did.
/// What: a manifest with no section, and one with an empty table, both read as
/// absent.
/// Test: this test itself.
#[test]
fn an_absent_inference_section_is_none() {
    let without =
        parse("[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n")
            .expect("parses");
    assert!(without.inference.is_none());

    let empty = parse(
        "[report]\ntitle = \"T\"\n\n[inference]\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
    )
    .expect("parses");
    assert!(
        empty.inference.is_none(),
        "a section that declares nothing must not become four unset overrides"
    );
}

/// Why: the section reaches the resolver as a `RoleManifest`, and a field
/// mis-mapped there would silently select the wrong model for a role.
/// What: converts a partially-declared section and checks every field lands.
/// Test: this test itself.
#[test]
fn the_section_becomes_a_resolution_layer() {
    let toml = "[report]\ntitle = \"T\"\n\n[inference]\nprovider = \"openrouter\"\n\
                reviewer = \"anthropic/claude-opus-4.8\"\n\n\
                [[repositories]]\nname = \"A\"\npath = \"/x\"\n";
    let m = parse(toml).expect("parses");
    let layer = m.inference.expect("declared").as_role_layer();
    assert_eq!(layer.provider.as_deref(), Some("openrouter"));
    assert_eq!(
        layer.reviewer_model.as_deref(),
        Some("anthropic/claude-opus-4.8")
    );
    assert_eq!(layer.verifier_model, None);
    assert_eq!(layer.summarizer_model, None);
}
