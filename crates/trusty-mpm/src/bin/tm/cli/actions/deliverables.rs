//! `tm projects deliverables` / `tm projects milestones` command groups
//! (DOC-35 §10.8, #2381).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`DeliverablesAction`] and [`MilestonesAction`] plus their
//! [`DeliverableKindArg`] / [`EstimationTierArg`] / [`DeliverableStatusArg`]
//! value enums.
//! Test: `cli_parses_projects_*` in `tests_projects.rs`.

use clap::Subcommand;

/// Actions for `tm projects deliverables` (DOC-35 §10.8, #2381).
#[derive(Debug, Subcommand)]
pub(crate) enum DeliverablesAction {
    /// List a project's Deliverables (optionally filtered by status).
    List {
        /// Project name.
        project: String,
        /// Emit raw JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Only show Deliverables in this status.
        #[arg(long)]
        status: Option<DeliverableStatusArg>,
    },
    /// Create a Deliverable (starts in `proposed`).
    #[command(alias = "create")]
    Add {
        /// Project name.
        project: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// Category of work.
        #[arg(long)]
        kind: DeliverableKindArg,
        /// Coarse effort tier (S/M/L/XL).
        #[arg(long)]
        estimate: EstimationTierArg,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
        /// Repo-relative spec path (plain string, §10.4).
        #[arg(long)]
        spec_ref: Option<String>,
        /// Opaque gh-first ticket reference (plain string, §13 Q6).
        #[arg(long)]
        ticket_ref: Option<String>,
    },
    /// Show one Deliverable by id.
    Show {
        /// Project name.
        project: String,
        /// Deliverable id (UUID).
        id: String,
        /// Emit raw JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Transition a Deliverable's status (enforces the §10.3 state machine).
    SetStatus {
        /// Project name.
        project: String,
        /// Deliverable id (UUID).
        id: String,
        /// Target status.
        status: DeliverableStatusArg,
    },
}

/// Actions for `tm projects milestones` (DOC-35 §10.8, #2381).
#[derive(Debug, Subcommand)]
pub(crate) enum MilestonesAction {
    /// List a project's Milestones.
    List {
        /// Project name.
        project: String,
        /// Emit raw JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Create a Milestone.
    #[command(alias = "create")]
    Add {
        /// Project name.
        project: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// Target date (RFC 3339, e.g. `2026-09-01T00:00:00Z`).
        #[arg(long)]
        target_date: String,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Show one Milestone by id.
    Show {
        /// Project name.
        project: String,
        /// Milestone id (UUID).
        id: String,
        /// Emit raw JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
}

/// CLI value for a Deliverable kind (§10.2); maps 1:1 to
/// `trusty_mpm::deliverable::DeliverableKind`. Kept local so `cli.rs` carries no
/// domain dependency; the projects command module does the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DeliverableKindArg {
    /// A new capability.
    Feature,
    /// A defect repair.
    Bugfix,
    /// A behavior-preserving restructuring.
    Refactor,
    /// Maintenance / housekeeping.
    Chore,
    /// Test-only work.
    Test,
    /// Documentation-only work.
    Docs,
}

/// CLI value for an estimation tier (§10.2); the value names are the exact
/// uppercase `S`/`M`/`L`/`XL` letters the spec uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum EstimationTierArg {
    /// Small.
    #[value(name = "S")]
    S,
    /// Medium.
    #[value(name = "M")]
    M,
    /// Large.
    #[value(name = "L")]
    L,
    /// Extra-large.
    #[value(name = "XL")]
    Xl,
}

/// CLI value for a Deliverable status (§10.3); default kebab-case value names
/// match the wire encoding (`in-progress`, `proposed`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DeliverableStatusArg {
    /// Planned but not started.
    Proposed,
    /// Actively worked.
    InProgress,
    /// Paused on an external blocker.
    Blocked,
    /// Objective gate passed or user-confirmed.
    Complete,
    /// Terminal: delivered.
    Delivered,
    /// Terminal: shipped.
    Shipped,
}
