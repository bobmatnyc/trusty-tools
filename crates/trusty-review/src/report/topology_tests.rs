//! Tests for the crate-topology consumer (#6147).

use super::*;
use crate::report::model::RepositoryReport;

fn node(name: &str, deps: &[&str], inbound: usize) -> CrateNode {
    CrateNode {
        name: name.to_string(),
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
        inbound,
    }
}

/// `core` is the shared core, `mid` sits between, `app` is the leaf.
fn three_crate_topology() -> CrateTopology {
    CrateTopology {
        members: 3,
        edges: 3,
        cycles: Vec::new(),
        crates: vec![
            node("app", &["core", "mid"], 0),
            node("core", &[], 2),
            node("mid", &["core"], 1),
        ],
    }
}

fn repo(name: &str, topology: Option<CrateTopology>) -> RepositoryReport {
    RepositoryReport {
        name: name.to_string(),
        slug: name.to_lowercase(),
        source: format!("/tmp/{name}"),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics: None,
        analyze_gap: None,
        authorship: None,
        inspect_priority: Vec::new(),
        crate_topology: topology,
    }
}

fn model_with(repos: Vec<RepositoryReport>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
        instructions: None,
        instructions_source: None,
        report_date: "2026-08-21".to_string(),
        generated_date: "2026-08-21".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: repos,
        gaps: vec![],
        findings: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

/// The table leads with what the workspace is built ON, not with what depends
/// on the most things.
#[test]
fn rows_lead_with_the_shared_core() {
    let topology = three_crate_topology();
    let rows: Vec<&str> = topology
        .table_rows()
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    assert_eq!(rows, vec!["core", "mid", "app"]);
}

/// A crate with the same inbound count as another sorts by fewest direct deps
/// first — the shallower of two equally-depended-on crates is nearer the base.
#[test]
fn ties_break_toward_the_shallower_crate() {
    let topology = CrateTopology {
        members: 3,
        edges: 3,
        cycles: Vec::new(),
        crates: vec![
            node("deep", &["base", "other"], 1),
            node("shallow", &[], 1),
            node("base", &[], 0),
        ],
    };

    let rows: Vec<&str> = topology
        .table_rows()
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    assert_eq!(rows, vec!["shallow", "deep", "base"]);
}

/// The table is capped, and the summary above it still states the true count —
/// a truncated table must never read as the complete list.
#[test]
fn the_table_is_capped_and_the_summary_states_the_true_count() {
    let crates: Vec<CrateNode> = (0..40)
        .map(|i| node(&format!("crate-{i:02}"), &[], 0))
        .collect();
    let topology = CrateTopology {
        members: 40,
        edges: 0,
        cycles: Vec::new(),
        crates,
    };

    assert_eq!(topology.table_rows().len(), TABLE_ROW_CAP);
    assert!(
        topology.summary().contains("40 crates"),
        "{}",
        topology.summary()
    );
}

/// The summary states all three whole-workspace facts.
#[test]
fn the_summary_states_members_edges_and_cycles() {
    let summary = three_crate_topology().summary();

    assert!(summary.contains("3 crates"), "{summary}");
    assert!(
        summary.contains("3 internal dependency edge(s)"),
        "{summary}"
    );
    assert!(summary.contains("No dependency cycles"), "{summary}");
    assert!(summary.contains("core (2)"), "{summary}");
}

/// A cycle is named, and stated for what it is: cargo rejects one, so a
/// workspace that has one does not build.
#[test]
fn a_cycle_is_named_in_the_summary() {
    let topology = CrateTopology {
        members: 2,
        edges: 2,
        cycles: vec![vec!["a".to_string(), "b".to_string()]],
        crates: vec![node("a", &["b"], 1), node("b", &["a"], 1)],
    };

    let summary = topology.summary();

    assert!(summary.contains("1 dependency cycle(s)"), "{summary}");
    assert!(summary.contains("a ↔ b"), "{summary}");
    assert!(summary.contains("does not build"), "{summary}");
}

/// A workspace whose members depend on nothing internal says so, rather than
/// leaving the reader to infer a shared core that is not there.
#[test]
fn a_workspace_with_no_shared_core_says_so() {
    let topology = CrateTopology {
        members: 2,
        edges: 0,
        cycles: Vec::new(),
        crates: vec![node("a", &[], 0), node("b", &[], 0)],
    };

    assert!(
        topology
            .summary()
            .contains("independent rather than built on a shared core"),
        "{}",
        topology.summary()
    );
}

/// The rendered section carries the summary and one row per crate.
#[test]
fn a_declared_topology_renders_rows() {
    let model = model_with(vec![repo("Demo", Some(three_crate_topology()))]);
    let scope = crate::report::reporter::build_scope(&model);

    let rendered = crate::report::fill::render(
        "<!-- BEGIN crate_topology -->\n{{ct_summary}}\n<!-- BEGIN ct_row -->\n| {{ct_crate}} | \
         {{ct_deps}} | {{ct_inbound}} |\n<!-- END ct_row -->\n<!-- END crate_topology -->",
        &scope,
    );

    assert!(rendered.contains("3 crates"), "{rendered}");
    assert!(rendered.contains("| core | — | 2 |"), "{rendered}");
    assert!(rendered.contains("| mid | core | 1 |"), "{rendered}");
    assert!(rendered.contains("| app | core, mid | 0 |"), "{rendered}");
}

/// The shipped template renders the same table, so the block markers in it and
/// the block names this module pushes cannot drift apart.
#[test]
fn the_shipped_template_renders_the_table() {
    let model = model_with(vec![repo("Demo", Some(three_crate_topology()))]);
    let scope = crate::report::reporter::build_scope(&model);

    let template = crate::report::TemplateLoader::bundled_only()
        .load(crate::report::DEFAULT_TEMPLATE)
        .expect("the bundled template loads");
    let rendered = crate::report::fill::render(&template, &scope);

    assert!(
        rendered.contains("| Crate | Direct internal deps | Depended on by |"),
        "{rendered}"
    );
    assert!(rendered.contains("| core | — | 2 |"), "{rendered}");
}

/// A workspace with a leaf nothing depends on renders that leaf too.
///
/// #6082: the renderer used the [`TABLE_ROW_CAP`]-truncated list, and the sort
/// puts `inbound == 0` crates last, so the cap dropped exactly the leaves — 15
/// of 30 in the dogfood run — under a summary line still claiming 30 crates.
#[test]
fn every_crate_reaches_the_rendered_table() {
    let mut crates = vec![node("core", &[], TABLE_ROW_CAP + 4)];
    // More dependents than the cap, every one of them an inbound-0 leaf.
    for i in 0..TABLE_ROW_CAP + 4 {
        crates.push(node(&format!("leaf{i:02}"), &["core"], 0));
    }
    let members = crates.len();
    let topology = CrateTopology {
        members,
        edges: members - 1,
        cycles: Vec::new(),
        crates,
    };
    let model = model_with(vec![repo("Demo", Some(topology))]);
    let scope = crate::report::reporter::build_scope(&model);

    let rendered = crate::report::fill::render(
        "<!-- BEGIN crate_topology --><!-- BEGIN ct_row -->[{{ct_crate}}]<!-- END ct_row --><!-- \
         END crate_topology -->",
        &scope,
    );

    assert_eq!(
        rendered.matches('[').count(),
        members,
        "every declared crate must render a row: {rendered}"
    );
    // The last leaf is the one the cap used to drop.
    let last = format!("[leaf{:02}]", TABLE_ROW_CAP + 3);
    assert!(rendered.contains(&last), "{rendered}");
}

/// The shipped template's topology table survives the polish pass as a real
/// markdown table: header, delimiter, and CONTIGUOUS rows.
///
/// #6082: `ct_row`'s BEGIN/END markers each sat on their own line, so every
/// repetition emitted `\n| row |\n` and the rows came out blank-line separated.
/// That left the header + delimiter as a two-line table with no body, which
/// `polish::process_table` collapses and drops — the rendered report showed 15
/// orphan pipe-lines under no header at all. `fill::render` alone never saw it,
/// which is why the existing template test stayed green; this one polishes.
#[test]
fn the_shipped_topology_table_survives_polish() {
    let model = model_with(vec![repo("Demo", Some(three_crate_topology()))]);
    let scope = crate::report::reporter::build_scope(&model);
    let template = crate::report::TemplateLoader::bundled_only()
        .load(crate::report::DEFAULT_TEMPLATE)
        .expect("the bundled template loads");

    let polished = crate::report::polish::polish(&crate::report::fill::render(&template, &scope));

    let head = polished
        .find("| Crate | Direct internal deps | Depended on by |")
        .unwrap_or_else(|| panic!("the header must survive polish:\n{polished}"));
    let table: Vec<&str> = polished[head..]
        .lines()
        .take_while(|l| l.trim_start().starts_with('|'))
        .collect();

    assert_eq!(
        table.len(),
        5,
        "header + delimiter + 3 contiguous rows, no blank lines between: {table:?}"
    );
    assert_eq!(table[1].trim(), "|---|---|---|", "{table:?}");
    assert_eq!(table[2].trim(), "| core | — | 2 |", "{table:?}");
    assert_eq!(table[4].trim(), "| app | core, mid | 0 |", "{table:?}");
}

/// A repository that is not a Cargo workspace renders a report BYTE-IDENTICAL
/// to one produced by a template with no topology block in it at all. No table,
/// no header, and no honesty marker standing in for the missing data — a
/// non-Rust repository's Code Quality & Architecture section is exactly what it
/// was before this existed.
#[test]
fn no_topology_renders_nothing() {
    let model = model_with(vec![repo("Demo", None)]);
    let scope = crate::report::reporter::build_scope(&model);
    let template = crate::report::TemplateLoader::bundled_only()
        .load(crate::report::DEFAULT_TEMPLATE)
        .expect("the bundled template loads");

    let rendered = crate::report::fill::render(&template, &scope);
    let without = crate::report::fill::render(&strip_topology_block(&template), &scope);

    assert!(
        !rendered.contains("Direct internal deps"),
        "the topology table must not render at all"
    );
    assert_eq!(rendered, without);
}

/// The shipped template with the topology block excised — the "before this
/// existed" template the test above compares against.
fn strip_topology_block(template: &str) -> String {
    const BEGIN: &str = "<!-- BEGIN crate_topology -->";
    const END: &str = "<!-- END crate_topology -->";
    let start = template.find(BEGIN).expect("the block is in the template");
    let end = template.find(END).expect("the block is closed") + END.len();
    format!("{}{}", &template[..start], &template[end..])
}

/// With several workspaces in one report each row names its application, so a
/// crate name is never ambiguous across two repositories.
#[test]
fn several_repositories_are_named_in_the_rows() {
    let model = model_with(vec![
        repo("Alpha", Some(three_crate_topology())),
        repo("Beta", Some(three_crate_topology())),
    ]);
    let scope = crate::report::reporter::build_scope(&model);

    let rendered = crate::report::fill::render(
        "<!-- BEGIN crate_topology --><!-- BEGIN ct_row -->[{{ct_crate}}]<!-- END ct_row --><!-- \
         END crate_topology -->",
        &scope,
    );

    assert!(rendered.contains("[Alpha / core]"), "{rendered}");
    assert!(rendered.contains("[Beta / core]"), "{rendered}");
}

/// The synthesis prompt carries the same facts the table renders.
#[test]
fn prompt_facts_carry_the_summary_and_rows() {
    let model = model_with(vec![repo("Demo", Some(three_crate_topology()))]);

    let facts = prompt_facts(&model);

    assert!(facts.contains("Crate topology"), "{facts}");
    assert!(facts.contains("### Demo"), "{facts}");
    assert!(facts.contains("3 crates"), "{facts}");
    assert!(
        facts.contains("`core`: depends on 0 internal crate(s); depended on by 2"),
        "{facts}"
    );
}

/// No topology, no block: the digest gains no headed-but-empty section.
#[test]
fn prompt_facts_are_empty_without_a_topology() {
    let model = model_with(vec![repo("Demo", None)]);

    assert_eq!(prompt_facts(&model), "");
}

/// #6143's numeric guardrail must ADMIT these figures. They are rendered into
/// the fill scope, and `figures::printed_figures` walks that scope, so the
/// member count and the inbound counts are in-model by construction — without
/// this the model would be asked to cite a number the guardrail then rejects,
/// taking the whole architecture paragraph with it.
#[test]
fn topology_numbers_are_admitted_by_the_numeric_guardrail() {
    let model = model_with(vec![repo("Demo", Some(three_crate_topology()))]);
    let serialized = serde_json::to_value(&model).expect("model serialises");
    let printed = crate::report::figures::printed_figures(&model);

    let allowed = crate::report::synthesize_guard::allowed_numbers_with(&serialized, &printed);

    assert!(
        allowed.contains("3"),
        "the member count must be citable: {allowed:?}"
    );
    assert!(
        allowed.contains("2"),
        "an inbound count must be citable: {allowed:?}"
    );
}

/// The manifest shape trusty-audit writes parses back into the type this crate
/// renders — the two crates' agreement about that TOML is the whole interface.
#[test]
fn a_manifest_crate_topology_is_parsed() {
    let manifest = r#"
[report]
title = "Audit"

[[repositories]]
name = "Demo"
path = "/tmp/demo"

[repositories.crate_topology]
members = 3
edges = 3
cycles = []
crates = [
    { name = "core", deps = [], inbound = 2 },
    { name = "mid", deps = ["core"], inbound = 1 },
    { name = "app", deps = ["core", "mid"], inbound = 0 },
]
"#;

    let parsed: crate::report::manifest::Manifest =
        crate::report::manifest::parse_manifest(manifest, std::path::Path::new("."))
            .expect("parses");
    let topology = parsed.repositories[0]
        .crate_topology
        .as_ref()
        .expect("a crate topology");

    assert_eq!(topology.members, 3);
    assert_eq!(topology.edges, 3);
    assert!(topology.cycles.is_empty());
    assert_eq!(topology.table_rows()[0].name, "core");
}

/// A manifest with no `crate_topology` key parses to `None` — the key is
/// additive, so every manifest written before this existed still loads.
#[test]
fn a_manifest_without_the_key_parses_to_none() {
    let manifest = r#"
[report]
title = "Audit"

[[repositories]]
name = "Demo"
path = "/tmp/demo"
"#;

    let parsed: crate::report::manifest::Manifest =
        crate::report::manifest::parse_manifest(manifest, std::path::Path::new("."))
            .expect("parses");

    assert!(parsed.repositories[0].crate_topology.is_none());
}
