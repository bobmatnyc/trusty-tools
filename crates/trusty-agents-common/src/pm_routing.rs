//! The one agent-routing table both PM prompts are rendered from (#8293).
//!
//! Why: trusty-mpm's PM instructions and trusty-code's delegate-mode PM card
//! each used to author their own routing table, so a routing decision corrected
//! in one product silently stayed wrong in the other. Neither product crate may
//! own the table — trusty-mpm and trusty-code both depend on this crate and
//! neither depends on the other, the same seam [`crate::harness_doc`] already
//! uses for shared instruction prose.
//! What: [`ROUTING_ROWS`] holds one [`RoutingRow`] per class of work. Each row
//! carries a per-consumer [`Route`]: the agent that consumer dispatches the
//! class to, the other agents its cell names, and the rendered [`Cell`] when
//! that consumer's table carries a row for it. [`fill`] substitutes
//! [`TABLE_PLACEHOLDER`] and [`PIPELINE_PLACEHOLDER`] in a consumer's authored
//! template with [`render_table`] and [`render_pipeline`] output, so the
//! delivered prompt is GENERATED from these rows rather than transcribed
//! beside them.
//! Test: `pm_routing_tests` — table/pipeline rendering, the shared spine, and
//! the per-consumer cell-order invariants.
//!
//! The public surface is deliberately narrow (#8293): [`fill`], [`Consumer`],
//! [`FillError`], [`agents`], [`render_table`], [`render_pipeline`], the two
//! placeholder constants, and the two pipeline constants [`MPM_PIPELINE`] /
//! [`TCODE_PIPELINE`]. The rows themselves and the types they are built from
//! are `pub(crate)`: a consumer that read a [`Route`] directly could route work
//! without the drift test seeing it, and every one of those items would
//! otherwise be a semver-gated signature on a published crate.
//!
//! The rows are deliberately NOT one shared string both products `include_str!`.
//! trusty-mpm's cells point at `Skill(...)` references, `make`/`mise run`
//! targets and the tmux-hosted Claude Code harness; trusty-code hosts none of
//! those and DOC-75 §4b forbids its card from naming them. One string cannot be
//! correct in both prompts, so the rows are structured data and each consumer
//! renders its own wording around them.

/// A product prompt rendered from [`ROUTING_ROWS`].
///
/// Why: the two consumers disagree on three axes that are facts about the
/// product, not about the routing: the table's column headers, how the pipeline
/// chain is punctuated, and which agent name a class of work resolves to
/// (trusty-mpm's `qa` runs commands; trusty-code's `qa` fork carries no `bash`,
/// so trusty-code routes verification to `qa-agent`). Naming the consumer keeps
/// all three declared in the rows rather than patched in by the caller.
/// What: a two-variant selector; every accessor in this module takes one.
/// Test: `mpm_table_renders_its_four_choice_rows`,
/// `tcode_table_renders_its_seven_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Consumer {
    /// trusty-mpm's composed PM instructions (`sections/agent-delegation.md`).
    Mpm,
    /// trusty-code's delegate-mode PM card (`assets/agents/pm.md`).
    Tcode,
}

impl Consumer {
    /// The routing table's two column headers for this consumer.
    ///
    /// Test: `mpm_table_renders_its_four_choice_rows`.
    fn headers(self) -> (&'static str, &'static str) {
        match self {
            Consumer::Mpm => ("Choice", "Which agent"),
            Consumer::Tcode => ("The task needs", "Delegate to"),
        }
    }

    /// The separator between two agents in the rendered pipeline chain.
    ///
    /// Test: `pipeline_chain_is_punctuated_per_consumer`.
    fn pipeline_join(self) -> &'static str {
        match self {
            Consumer::Mpm => " → ",
            Consumer::Tcode => ", then ",
        }
    }
}

/// One rendered table row: the left column and the right column.
///
/// Why: the two products describe the same class of work to different readers —
/// trusty-mpm's PM chooses between two agents it already knows, trusty-code's
/// states what the task needs. Holding both cells on one row is what makes them
/// one row rather than two tables that happen to rhyme.
/// What: `order` is this row's 1-based position in that consumer's table;
/// `label` and `text` are the cell bodies, rendered verbatim between pipes.
/// Test: `cell_order_is_unique_and_contiguous_per_consumer`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cell {
    /// 1-based position in the consumer's rendered table.
    pub(crate) order: u8,
    /// Left column — the class of work, as that consumer names it.
    pub(crate) label: &'static str,
    /// Right column — the agent(s), as that consumer names them.
    pub(crate) text: &'static str,
}

/// How one consumer routes one class of work.
///
/// Why: a consumer can route a class without carrying a table row for it —
/// trusty-mpm's table is the four choices that get made wrong, while its
/// pipeline bullet still names `research` and `engineer`. Separating the routing
/// fact (`agent`) from the rendered row (`cell`) is what lets the pipeline and
/// the table come from the same rows.
/// What: `agent` is the dispatch target; `also` lists the other agents `cell`'s
/// text names, each of which must resolve in that consumer's roster; `cell` is
/// `None` when the consumer routes the class but renders no row for it.
/// Test: `every_route_agent_is_named_by_its_own_cell`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Route {
    /// The agent this consumer dispatches this class of work to.
    pub(crate) agent: &'static str,
    /// Other agents the cell names, beyond `agent`. Routing targets only.
    pub(crate) also: &'static [&'static str],
    /// The cell's remaining backtick-quoted spans — the ones that are NOT
    /// routing targets and must never be resolved against a roster: a retired
    /// agent the cell names in order to forbid it (`ops`), or a build verb
    /// (`make`, `mise run`). Declaring them is what lets the drift test treat
    /// every span in the cell as accounted for. Read only by
    /// `every_route_agent_is_named_by_its_own_cell` — a declaration the tests
    /// hold the cell prose to, never a runtime input.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) non_agents: &'static [&'static str],
    /// The rendered table row, when this consumer's table carries one.
    pub(crate) cell: Option<Cell>,
}

/// One class of work, routed by every consumer that handles it.
///
/// Why/What: `id` is the stable key [`SIMPLE_TASK_SPINE`] and the drift tests
/// refer to; `mpm`/`tcode` are `None` when that product routes the class
/// nowhere.
/// Test: `row_ids_are_unique`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoutingRow {
    /// Stable identifier for the class of work.
    pub(crate) id: &'static str,
    /// trusty-mpm's route, or `None` when it routes this class nowhere.
    pub(crate) mpm: Option<Route>,
    /// trusty-code's route, or `None` when it routes this class nowhere.
    pub(crate) tcode: Option<Route>,
}

impl RoutingRow {
    /// This row's route for one consumer.
    ///
    /// Test: `routes_yields_only_the_consumers_rows`.
    pub(crate) fn route(&self, consumer: Consumer) -> Option<&Route> {
        match consumer {
            Consumer::Mpm => self.mpm.as_ref(),
            Consumer::Tcode => self.tcode.as_ref(),
        }
    }
}

/// The classes of work every coding task passes through, in order.
///
/// Why: DOC-75 §1's delegate run is "research, then engineer, then qa" — the
/// ORDER is the behaviour, not just the membership. Declared over row ids rather
/// than agent names because the verification agent's NAME differs per consumer
/// while the class does not.
/// What: three [`ROUTING_ROWS`] ids. Each consumer's pipeline must name these
/// rows' agents in this relative order.
/// Test: `every_pipeline_follows_the_shared_spine`.
///
/// Read only by that test — the spine is an invariant each consumer's pipeline
/// is held to, not a value either product dispatches from.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SIMPLE_TASK_SPINE: &[&str] = &["context", "source-change", "verification"];

/// trusty-mpm's full-pipeline chain, in dispatch order.
///
/// Test: `every_pipeline_follows_the_shared_spine`,
/// `pipeline_chain_is_punctuated_per_consumer`.
pub const MPM_PIPELINE: &[&str] = &["research", "engineer", "local-ops", "qa", "documentation"];

/// trusty-code's delegate-mode pipeline, in dispatch order.
///
/// Why: re-exported as `trusty_code::assets::PM_ROUTING_ORDER`, so it stays a
/// plain const rather than an accessor.
/// Test: `every_pipeline_follows_the_shared_spine`.
pub const TCODE_PIPELINE: &[&str] = &["research", "engineer", "qa-agent"];

/// The marker an authored template carries where the rendered table belongs.
///
/// Why: a Markdown comment is invisible to a renderer, so the marker can sit in
/// the template exactly where the table belongs. [`fill`] consumes this line
/// itself in both products — what it is replaced by is the table alone.
/// Whether OTHER comments in a template survive delivery is a per-consumer
/// fact, not a property of this marker: trusty-mpm's compose-time fold
/// (`core::instruction_fold`) drops every whole-line HTML comment, while
/// trusty-code delivers an agent card verbatim, so `pm.md`'s own
/// `<!-- pm-routing-table:begin/end -->` block markers do reach the model.
/// Test: `fill_replaces_both_placeholders`.
pub const TABLE_PLACEHOLDER: &str = "<!-- pm-routing-table -->";

/// The marker an authored template carries where the pipeline chain belongs.
///
/// Test: `fill_replaces_both_placeholders`.
pub const PIPELINE_PLACEHOLDER: &str = "<!-- pm-routing-pipeline -->";

/// Every class of work either PM prompt routes.
///
/// Why: this table IS the shared source #8293 asks for. A routing correction
/// lands here once and reaches both products' prompts, and a drift check in each
/// consumer fails when its prompt stops matching what this renders.
/// What: eight rows. Cell text is verbatim Markdown, reproducing each product's
/// table byte for byte as authored before the lift.
/// Test: `mpm_table_renders_its_four_choice_rows`,
/// `tcode_table_renders_its_seven_rows`, plus each consumer's own drift test.
pub(crate) const ROUTING_ROWS: &[RoutingRow] = &[
    RoutingRow {
        id: "context",
        mpm: Some(Route {
            agent: "research",
            also: &[],
            non_agents: &[],
            cell: None,
        }),
        tcode: Some(Route {
            agent: "research",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 1,
                label: "Context you do not already hold — which files, which call sites, how the code works today",
                text: "`research`",
            }),
        }),
    },
    RoutingRow {
        id: "source-change",
        mpm: Some(Route {
            agent: "engineer",
            also: &[],
            non_agents: &[],
            cell: None,
        }),
        tcode: Some(Route {
            agent: "engineer",
            also: &[
                "rust-engineer",
                "typescript-engineer",
                "python-engineer",
                "react-engineer",
                "golang-engineer",
            ],
            non_agents: &[],
            cell: Some(Cell {
                order: 2,
                label: "A source change",
                text: "`engineer`, or a language specialist (`rust-engineer`, `typescript-engineer`, `python-engineer`, `react-engineer`, `golang-engineer`)",
            }),
        }),
    },
    RoutingRow {
        id: "verification",
        mpm: Some(Route {
            agent: "qa",
            also: &["api-qa", "web-qa"],
            non_agents: &[],
            cell: Some(Cell {
                order: 4,
                label: "Testing",
                text: "`qa`, or `api-qa` for APIs. Browser, screenshot, click, navigate, DOM, console errors → `web-qa`, never chrome-devtools, claude-in-chrome, or playwright directly",
            }),
        }),
        // #8293: `qa-agent`, not `qa`. tcode's `qa` fork is reviewer-intent and
        // carries no `bash` (trusty-code `assets::mod`'s tools-restriction
        // deviation), so it cannot produce the raw test output DOC-75 §6 wants
        // quoted. The CLASS is shared; the agent name is a per-product fact.
        tcode: Some(Route {
            agent: "qa-agent",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 3,
                label: "Verification — run the project's real tests and report the raw output",
                text: "`qa-agent`",
            }),
        }),
    },
    RoutingRow {
        id: "issue-work",
        mpm: Some(Route {
            agent: "ticketing",
            also: &["version-control"],
            non_agents: &[],
            cell: Some(Cell {
                order: 2,
                label: "Issue work vs. PR/git work",
                text: "Route by artifact (#5202): the Issue is `ticketing`'s, whole (P6); the Pull Request — including its title and body — plus every git operation is `version-control`'s (P7). Never split one PR edit across both",
            }),
        }),
        tcode: Some(Route {
            agent: "ticketing",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 4,
                label: "Issue search, filing, comments, labels, transitions",
                text: "`ticketing`",
            }),
        }),
    },
    RoutingRow {
        id: "pr-work",
        // trusty-mpm renders no row of its own here: the issue-work cell above
        // is authored as the issue-vs-PR contrast and names `version-control`.
        mpm: Some(Route {
            agent: "version-control",
            also: &[],
            non_agents: &[],
            cell: None,
        }),
        tcode: Some(Route {
            agent: "version-control",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 5,
                label: "A branch, a commit, a push, a pull request",
                text: "`version-control`",
            }),
        }),
    },
    RoutingRow {
        id: "ops",
        mpm: Some(Route {
            agent: "local-ops",
            also: &[],
            non_agents: &["make", "mise run", "ops"],
            cell: Some(Cell {
                order: 3,
                label: "Ops, build, release",
                text: "`local-ops` — every `make` and `mise run` target, ports, processes, install, publish, deploy. Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED",
            }),
        }),
        tcode: Some(Route {
            agent: "local-ops",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 6,
                label: "Builds, test gates, lint gates, version bumps, changelog entries",
                text: "`local-ops`",
            }),
        }),
    },
    RoutingRow {
        id: "docs",
        mpm: Some(Route {
            agent: "documentation",
            also: &[],
            non_agents: &[],
            cell: None,
        }),
        tcode: Some(Route {
            agent: "documentation",
            also: &[],
            non_agents: &[],
            cell: Some(Cell {
                order: 7,
                label: "README, guide and reference prose",
                text: "`documentation`",
            }),
        }),
    },
    RoutingRow {
        id: "review",
        mpm: Some(Route {
            agent: "code-analyzer",
            also: &["code-critic"],
            non_agents: &[],
            cell: Some(Cell {
                order: 1,
                label: "Review BEFORE implementation vs. of code that already exists",
                text: "`code-analyzer` before, verdict APPROVED / NEEDS_IMPROVEMENT / BLOCKED; `code-critic` after, adversarially. Separate agents, not interchangeable",
            }),
        }),
        // trusty-code routes no review class from the PM card: DOC-75 §4b's
        // delegate run is research -> engineer -> qa-agent.
        tcode: None,
    },
];

/// Every `(id, route)` pair one consumer holds, in declaration order.
///
/// Test: `routes_yields_only_the_consumers_rows`.
pub(crate) fn routes(consumer: Consumer) -> impl Iterator<Item = (&'static str, &'static Route)> {
    ROUTING_ROWS
        .iter()
        .filter_map(move |row| row.route(consumer).map(|route| (row.id, route)))
}

/// Every agent name one consumer's rows name, deduplicated, in row order.
///
/// Why: this is the list each consumer's drift test resolves against its own
/// roster — a row that names an agent the product cannot dispatch would make the
/// PM emit a delegation that fails resolution (#4594).
/// What: each route's `agent` followed by its `also` names, first occurrence
/// wins. Deliberately excludes names a cell mentions as retired.
/// Test: `agents_are_deduplicated`, plus each consumer's roster test.
pub fn agents(consumer: Consumer) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, route) in routes(consumer) {
        for name in std::iter::once(&route.agent).chain(route.also.iter()) {
            if !out.contains(name) {
                out.push(name);
            }
        }
    }
    out
}

/// The dispatch order one consumer's PM follows for a whole task.
///
/// Test: `every_pipeline_follows_the_shared_spine`.
pub(crate) fn pipeline(consumer: Consumer) -> &'static [&'static str] {
    match consumer {
        Consumer::Mpm => MPM_PIPELINE,
        Consumer::Tcode => TCODE_PIPELINE,
    }
}

/// The routing table as Markdown, for one consumer.
///
/// Why: this is the generated text a consumer's prompt carries. Rendering it
/// rather than authoring it is what makes the rows the single source — an edit
/// to a cell reaches the delivered prompt with no second file to update.
/// What: a GitHub-flavoured Markdown table — the consumer's headers, a `|---|---|`
/// rule, then every row that consumer carries a [`Cell`] for, ordered by
/// [`Cell::order`]. No trailing newline, so a template may place it mid-line.
/// Test: `mpm_table_renders_its_four_choice_rows`,
/// `tcode_table_renders_its_seven_rows`.
pub fn render_table(consumer: Consumer) -> String {
    let mut cells: Vec<&Cell> = routes(consumer)
        .filter_map(|(_, route)| route.cell.as_ref())
        .collect();
    cells.sort_by_key(|cell| cell.order);

    let (left, right) = consumer.headers();
    let mut out = format!("| {left} | {right} |\n|---|---|");
    for cell in cells {
        out.push_str(&format!("\n| {} | {} |", cell.label, cell.text));
    }
    out
}

/// The pipeline as a backticked chain, punctuated for one consumer.
///
/// What: `` `research` → `engineer` → … `` for trusty-mpm,
/// `` `research`, then `engineer`, then `qa-agent` `` for trusty-code.
/// Test: `pipeline_chain_is_punctuated_per_consumer`.
pub fn render_pipeline(consumer: Consumer) -> String {
    pipeline(consumer)
        .iter()
        .map(|agent| format!("`{agent}`"))
        .collect::<Vec<_>>()
        .join(consumer.pipeline_join())
}

/// Why a consumer's template cannot be filled.
///
/// Why: a template that authors a placeholder twice is a defect no drift test
/// can see — each consumer's check asserts the delivered text CONTAINS the
/// rendered table, which a prompt carrying it twice satisfies. Returning an
/// error is what stops that prompt from being delivered at all (#8293).
/// What: one variant, carrying the marker and how many times it occurs.
/// Test: `fill_refuses_a_duplicated_placeholder`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FillError {
    /// A routing placeholder occurs more than once in the template.
    #[error(
        "the routing placeholder {placeholder:?} occurs {count} times in this template; \
         at most one occurrence is allowed, or the prompt ships the routing table twice"
    )]
    DuplicatePlaceholder {
        /// The marker that repeats — [`TABLE_PLACEHOLDER`] or
        /// [`PIPELINE_PLACEHOLDER`].
        placeholder: &'static str,
        /// How many times it occurs in the template.
        count: usize,
    },
}

/// Substitute both placeholders in a consumer's authored template.
///
/// Why: the consumers' templates are Markdown assets (a prompt section, an agent
/// card), so the seam between authored prose and generated rows is a marker in
/// the text rather than a call at the consumer's assembly site. One function
/// means neither product can substitute only half of it.
/// What: replaces [`TABLE_PLACEHOLDER`] with [`render_table`] and
/// [`PIPELINE_PLACEHOLDER`] with [`render_pipeline`]. Each marker may occur at
/// most once; a second occurrence is [`FillError::DuplicatePlaceholder`] rather
/// than a second rendered table. A template carrying neither marker is returned
/// unchanged — each consumer's own test asserts its template carries the markers
/// it expects.
/// Test: `fill_replaces_both_placeholders`, `fill_leaves_an_unmarked_template_alone`,
/// `fill_refuses_a_duplicated_placeholder`.
pub fn fill(template: &str, consumer: Consumer) -> Result<String, FillError> {
    let mut filled = template.to_string();
    for (placeholder, rendered) in [
        (TABLE_PLACEHOLDER, render_table(consumer)),
        (PIPELINE_PLACEHOLDER, render_pipeline(consumer)),
    ] {
        let count = template.matches(placeholder).count();
        if count > 1 {
            return Err(FillError::DuplicatePlaceholder { placeholder, count });
        }
        filled = filled.replace(placeholder, &rendered);
    }
    Ok(filled)
}

#[cfg(test)]
#[path = "pm_routing_tests.rs"]
mod pm_routing_tests;
