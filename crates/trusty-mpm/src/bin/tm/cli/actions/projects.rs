//! `tm projects` registry-B project + config command group (DOC-35 §3.1/§10.8, #2115/#2381/#2120).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap. Deliverable/Milestone sub-actions live
//! in the sibling `actions::deliverables` module.
//! What: [`ProjectsAction`] (`list`/`register`/`show`/`status`/`config`/
//! `deliverables`/`milestones`), [`ConfigAction`] (`set`/`unset`/`tags`),
//! and its [`SettableConfigField`] / [`ClearableConfigField`] value enums.
//! Test: `cli_parses_projects_*` in `tests_projects.rs`;
//! `cli_parses_projects_config_*` in `tests_projects_config_tests.rs`.

use clap::Subcommand;

use super::deliverables::{DeliverablesAction, MilestonesAction};

/// Actions for the `tm projects` subcommand (DOC-35 §3.1/§10.8, #2115/#2381).
///
/// Why: the registry-B project surface plus the Deliverable/Milestone ledger,
/// exposed as a deterministic verb tree of thin HTTP clients.
/// What: the four registry verbs (`list`/`register`/`show`/`status`) and the two
/// nested subtrees (`deliverables`/`milestones`).
/// Test: `cli_parses_projects_*` in `tests_projects.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectsAction {
    /// List registered projects (optionally filtered by tag).
    List {
        /// Emit the raw project JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Only show projects carrying this tag.
        #[arg(long)]
        tag: Option<String>,
    },
    /// Register (idempotent upsert) a project in registry B.
    Register {
        /// Registry key / short project name.
        name: String,
        /// Full repository URL.
        #[arg(long)]
        repo_url: String,
        /// Default branch (daemon defaults to `main` when omitted).
        #[arg(long)]
        default_branch: Option<String>,
        /// Free-form description.
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated classification tags.
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Technology-stack hint (e.g. `rust`).
        #[arg(long)]
        stack_hint: Option<String>,
        /// Preferred `gh` login for this project (#2081).
        #[arg(long)]
        gh_user: Option<String>,
        /// GitHub account login pinned for this project's spawned sessions
        /// (#3025); resolved into `GH_TOKEN`/`GH_USER` at spawn/relaunch.
        #[arg(long)]
        gh_account: Option<String>,
    },
    /// Show a project's config PLUS a read-only nested sessions listing.
    Show {
        /// Project name.
        name: String,
        /// Emit raw JSON (config + sessions) instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Show a project's deterministic status rollup (session histogram + flags).
    Status {
        /// Project name.
        name: String,
        /// Emit the raw status JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// View or edit a project's deterministic config (§3.1/§6, #2120).
    ///
    /// Bare (no `set`/`unset`/`tags` subcommand) is a read-only view (GET);
    /// each subcommand is a single deterministic PATCH — never free text.
    Config {
        /// Project name.
        name: String,
        /// Emit raw JSON instead of the human view (bare view form only).
        #[arg(long)]
        json: bool,
        /// `set <field> <value>` / `unset <field>` / `tags --add/--remove`;
        /// omitted = view.
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Manage a project's Deliverables (§10.8).
    Deliverables {
        /// Deliverable action to perform.
        #[command(subcommand)]
        action: DeliverablesAction,
    },
    /// Manage a project's Milestones (§10.8).
    Milestones {
        /// Milestone action to perform.
        #[command(subcommand)]
        action: MilestonesAction,
    },
}

/// Actions for `tm projects config <name>` (DOC-35 §3.1/§6, #2120).
///
/// Why: the deterministic sub-verbs of the configurator — `set`/`unset` mirror
/// the field-level PATCH exactly (never free text); `tags` is a DEDICATED verb
/// rather than folded into `set`/`unset` — disclosed deviation from a literal
/// reading of the spec's CLI sketch (which groups tags under the same
/// set/unset comment line): `set <field> <value>` structurally cannot express
/// two independent lists (add AND remove) in one positional value, and §6's
/// own field table sanctions "`--add`/`--remove`" as the mechanism; the issue
/// text explicitly allows either "set/unset or dedicated --add/--remove
/// flags" and this is the dedicated-verb form.
/// What: three subcommands routed by `commands::projects::registry::config`.
/// Test: `cli_parses_projects_config_*` in `tests_projects.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum ConfigAction {
    /// Set a field to a new value.
    Set {
        /// Which field to set.
        #[arg(value_enum)]
        field: SettableConfigField,
        /// The new value.
        value: String,
    },
    /// Clear (unset) a field back to absent.
    ///
    /// `default_branch` is deliberately NOT a valid target here — see
    /// [`ClearableConfigField`]'s doc for why.
    Unset {
        /// Which field to clear.
        #[arg(value_enum)]
        field: ClearableConfigField,
    },
    /// Add and/or remove tags in one call (§6: "no free-text
    /// replace-whole-list footgun" — there is no plain tags-replace form).
    Tags {
        /// Comma-separated tags to add.
        #[arg(long, value_delimiter = ',')]
        add: Vec<String>,
        /// Comma-separated tags to remove (applied after `--add`, server-side).
        #[arg(long, value_delimiter = ',')]
        remove: Vec<String>,
    },
}

/// CLI value for a settable config field (§6); maps 1:1 to
/// `trusty_mpm::project_config::ConfigField` via `convert.rs`. Kept local so
/// `cli.rs` carries no domain dependency. Default kebab-case value names
/// (`default-branch`, `stack-hint`, …) match this codebase's other
/// `ValueEnum`s (e.g. `DeliverableStatusArg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum SettableConfigField {
    /// `default_branch` — required, non-empty.
    DefaultBranch,
    /// `description` — free-form, clearable.
    Description,
    /// `stack_hint` — advisory, clearable.
    StackHint,
    /// `gh_user` — preferred `gh` login (#2081), clearable.
    GhUser,
}

/// CLI value for a clearable config field (§6) — DELIBERATELY NARROWER than
/// [`SettableConfigField`]: `default_branch` is excluded because it has no
/// wire representation for "clear" at all (`PatchProjectBody::default_branch`
/// is a plain `Option<String>` — absent=unchanged, present+blank=400,
/// present+non-blank=set; there is no double-Option `null`=clear story like
/// `description`/`stack_hint`/`gh_user` have). Rejecting `unset
/// default-branch` at clap parse time (an "invalid value" error) is strictly
/// better than proxying a request the server cannot honor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ClearableConfigField {
    /// `description`.
    Description,
    /// `stack_hint`.
    StackHint,
    /// `gh_user`.
    GhUser,
}
