//! Pure state for `tga tui` — everything the renderer draws, and every
//! transition that changes it.
//!
//! Why: a ratatui draw loop is impractical to assert against, so #5197 keeps
//! the decisions (which repos are selected, which view is showing, how events
//! fold into rows, what the results filter is) out of the draw calls and in
//! plain functions here. `render.rs` reads this and does nothing else.
//! What: [`TuiState`] plus the repo-picker model it owns.
//! Test: `super::tests`.

use tga::core::config::Config;
use tga::core::db::correlation::{CorrelationCounts, CorrelationFilter, CorrelationRow};
use tga::core::progress::{ProgressAggregate, ProgressBus};

/// How many correlation rows the results view loads at a time.
///
/// Why: a corpus can hold hundreds of thousands of commits; the view is a
/// scrollable window, not a dump.
/// What: `500` rows.
/// Test: passed to `correlation_rows`, whose limit is covered by
/// `tga::core::db::correlation::tests::rows_respect_limit`.
pub const RESULTS_LIMIT: usize = 500;

/// Where a repository in the picker came from.
///
/// Why: an explicitly configured repo can be walked right now; an org is a
/// discovery source that needs a GitHub token before it yields anything. The
/// picker must not present those as the same thing.
/// What: `Configured` for a `repositories[]` entry, `Org` for a `github.org` /
/// `github.orgs` entry.
/// Test: `super::tests::picker_lists_configured_repos_and_orgs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoSource {
    /// An explicit `repositories[]` entry — walkable with no credentials.
    Configured,
    /// A `github.orgs` / `github.org` entry — needs discovery before it walks.
    Org,
}

impl RepoSource {
    /// Short label for the picker's source column.
    ///
    /// Why/What/Test: see `super::tests::picker_lists_configured_repos_and_orgs`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Configured => "config",
            Self::Org => "org",
        }
    }
}

/// One selectable row in the repo picker.
///
/// Why: the picker unions two config surfaces that have nothing in common
/// structurally; this is the shape they both flatten to.
/// What: display name, provenance, an optional detail line, and whether the
/// row can actually be collected in this run.
/// Test: `super::tests::picker_lists_configured_repos_and_orgs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// Display name — the config `name`, else the directory basename.
    pub name: String,
    /// Where the entry came from.
    pub source: RepoSource,
    /// Local path (configured repos) or a discovery note (orgs).
    pub detail: String,
    /// Index into `Config::repositories`, for `Configured` rows only.
    pub config_index: Option<usize>,
    /// Whether this row is currently ticked for the next run.
    pub selected: bool,
}

impl RepoEntry {
    /// Whether selecting this row would actually collect anything.
    ///
    /// Why: org rows are informational until discovery runs, and ticking one
    /// must not imply a repo will be walked.
    /// What: `true` only for `Configured` rows.
    /// Test: `super::tests::org_rows_are_not_selectable`.
    pub fn is_selectable(&self) -> bool {
        self.config_index.is_some()
    }
}

/// Which pane has the screen.
///
/// Why: three views over one state beat three screens over three states.
/// What: `Repos`, `Progress`, `Results`.
/// Test: `super::tests::view_cycles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The repo picker.
    #[default]
    Repos,
    /// Live pipeline progress.
    Progress,
    /// Commit ↔ board-item correlation results.
    Results,
}

impl View {
    /// Advance to the next view.
    ///
    /// Why/What/Test: `Repos → Progress → Results → Repos`; see
    /// `super::tests::view_cycles`.
    pub fn next(self) -> Self {
        match self {
            Self::Repos => Self::Progress,
            Self::Progress => Self::Results,
            Self::Results => Self::Repos,
        }
    }

    /// Tab-bar label.
    ///
    /// Why/What/Test: see `super::tests::view_cycles`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Repos => "Repos",
            Self::Progress => "Progress",
            Self::Results => "Results",
        }
    }
}

/// What the worker is doing, from the UI's point of view.
///
/// Why: the key handler must refuse to start a second run, and the status bar
/// must say why a key did nothing.
/// What: `Idle`, `Running`, or `Finished` with the worker's own summary line.
/// Test: `super::tests::run_is_refused_while_running`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkStatus {
    /// Nothing has run yet, or the last run's summary was cleared.
    #[default]
    Idle,
    /// A run is in flight.
    Running,
    /// The last run ended; the string is its one-line summary.
    Finished(String),
}

impl WorkStatus {
    /// Whether a run is in flight.
    ///
    /// Why/What/Test: see `super::tests::run_is_refused_while_running`.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Everything `tga tui` draws.
///
/// Why: one owner for the picker, the live progress fold, and the results
/// window, so a key handler never has to reach into three places to keep them
/// consistent.
/// What: the config it was built from, the picker rows, the current view and
/// cursors, the [`ProgressAggregate`] fed from the bus, and the last-loaded
/// correlation window.
/// Test: `super::tests`.
#[derive(Debug)]
pub struct TuiState {
    /// The config the TUI was launched with.
    pub config: Config,
    /// Repo-picker rows, configured entries first.
    pub repos: Vec<RepoEntry>,
    /// Cursor into [`TuiState::repos`].
    pub repo_cursor: usize,
    /// Which pane is showing.
    pub view: View,
    /// Live progress folded from the bus.
    pub progress: ProgressAggregate,
    /// The bus the pipelines publish to. Cloned into each worker.
    pub bus: ProgressBus,
    /// Worker state.
    pub status: WorkStatus,
    /// Correlation roll-up as of the last reload.
    pub counts: CorrelationCounts,
    /// The loaded correlation window.
    pub rows: Vec<CorrelationRow>,
    /// Which slice of commits the results view shows.
    pub filter: CorrelationFilter,
    /// Cursor into [`TuiState::rows`].
    pub row_cursor: usize,
    /// Whether the help overlay is up.
    pub show_help: bool,
    /// Transient status-bar message, cleared on the next keypress.
    pub message: Option<String>,
    /// Set when the operator asks to quit.
    pub quit: bool,
    /// Set when a quit was refused because a run is in flight; the next quit
    /// keypress is taken as the confirmation (#5197).
    pub quit_armed: bool,
    /// `--correlate-only`: refuse the pull action entirely, so the session
    /// provably touches no network. The correlation pass and every view stay
    /// fully available (#5197 zero-inference default).
    pub offline: bool,
}

impl TuiState {
    /// Build the initial state from a config.
    ///
    /// Why: the picker is derived entirely from config — no database, no
    /// network, no credentials — which is what makes the TUI useful on a fresh
    /// checkout with nothing configured beyond `repositories[]`.
    /// What: flattens `repositories[]` then the effective org list into
    /// [`RepoEntry`] rows, ticking every configured repo by default (the
    /// all-vs-subset default is "all"). The bus is active, because the TUI is
    /// the subscriber.
    /// Test: `super::tests::picker_lists_configured_repos_and_orgs`.
    pub fn new(config: Config) -> Self {
        let repos = build_repo_entries(&config);
        Self {
            config,
            repos,
            repo_cursor: 0,
            view: View::default(),
            progress: ProgressAggregate::new(),
            bus: ProgressBus::new(),
            status: WorkStatus::default(),
            counts: CorrelationCounts::default(),
            rows: Vec::new(),
            filter: CorrelationFilter::default(),
            row_cursor: 0,
            show_help: false,
            message: None,
            quit: false,
            quit_armed: false,
            offline: false,
        }
    }

    /// Refuse network pulls for this session.
    ///
    /// Why: `--correlate-only` is the switch that proves the TUI is useful
    /// with no credentials — with it set, no code path in the session can
    /// reach a board client or a git remote.
    /// What: builder setter for [`TuiState::offline`].
    /// Test: `super::tests::offline_state_refuses_the_pull_action`.
    #[must_use]
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Drain the bus into the aggregate.
    ///
    /// Why: this is the once-per-tick call that makes the progress view live,
    /// and the only place the two halves of the bus meet.
    /// What: folds every queued event, then records the bus's drop count so
    /// the pane can label a gap rather than silently show one.
    /// Test: `super::tests::pump_folds_bus_events`.
    pub fn pump_progress(&mut self) {
        for event in self.bus.drain() {
            self.progress.apply(event);
        }
        self.progress.set_dropped(self.bus.dropped());
    }

    /// Move the active view's cursor by `delta`, clamped to its list.
    ///
    /// Why: one handler for both lists keeps the key map small.
    /// What: steps the repo cursor on [`View::Repos`] and the row cursor on
    /// [`View::Results`]; the progress view has no cursor. Never wraps and
    /// never goes out of range.
    /// Test: `super::tests::cursor_clamps_at_both_ends`.
    pub fn move_cursor(&mut self, delta: isize) {
        let (cursor, len) = match self.view {
            View::Repos => (&mut self.repo_cursor, self.repos.len()),
            View::Results => (&mut self.row_cursor, self.rows.len()),
            View::Progress => return,
        };
        if len == 0 {
            *cursor = 0;
            return;
        }
        let next = (*cursor as isize + delta).clamp(0, len as isize - 1);
        *cursor = next as usize;
    }

    /// Toggle the repo under the cursor.
    ///
    /// Why: the subset half of all-vs-subset selection.
    /// What: flips `selected` on a selectable row; sets a message and changes
    /// nothing for an org row, which cannot be collected until discovery runs.
    /// Test: `super::tests::toggle_flips_selection`,
    /// `org_rows_are_not_selectable`.
    pub fn toggle_selected(&mut self) {
        let Some(entry) = self.repos.get_mut(self.repo_cursor) else {
            return;
        };
        if !entry.is_selectable() {
            self.message = Some(format!(
                "{} is a discovery source, not a repository — add it to repositories[] to collect it",
                entry.name
            ));
            return;
        }
        entry.selected = !entry.selected;
    }

    /// Select every selectable repo, or none if all are already selected.
    ///
    /// Why: the all half of all-vs-subset selection, on one key.
    /// What: ticks every `Configured` row; if they are all already ticked,
    /// unticks them instead. Org rows are never touched.
    /// Test: `super::tests::select_all_toggles_between_all_and_none`.
    pub fn toggle_select_all(&mut self) {
        let all_on = self
            .repos
            .iter()
            .filter(|r| r.is_selectable())
            .all(|r| r.selected);
        for entry in self.repos.iter_mut().filter(|r| r.is_selectable()) {
            entry.selected = !all_on;
        }
    }

    /// The `repositories[]` entries currently ticked.
    ///
    /// Why: the worker collects exactly this subset, so the mapping from
    /// picker rows back to config entries lives with the picker.
    /// What: config entries in config order, for selected `Configured` rows.
    /// Test: `super::tests::selected_repositories_maps_back_to_config`.
    pub fn selected_repositories(&self) -> Vec<tga::core::config::RepositoryConfig> {
        let mut indices: Vec<usize> = self
            .repos
            .iter()
            .filter(|r| r.selected)
            .filter_map(|r| r.config_index)
            .collect();
        indices.sort_unstable();
        indices
            .into_iter()
            .filter_map(|i| self.config.repositories.get(i).cloned())
            .collect()
    }

    /// A copy of the config narrowed to the selected repositories.
    ///
    /// Why: the collection pipeline takes a whole `Config`; narrowing it is
    /// how the picker's subset reaches the walk without a second code path.
    /// What: clones the config and replaces `repositories` with
    /// [`TuiState::selected_repositories`]. Every other setting — board
    /// clients, PR fetching, the classify cascade's tiers — is untouched.
    /// Test: `super::tests::scoped_config_narrows_repositories_only`.
    pub fn scoped_config(&self) -> Config {
        let mut config = self.config.clone();
        config.repositories = self.selected_repositories();
        config
    }

    /// Set the transient status-bar message.
    ///
    /// Why/What/Test: see `super::tests::toggle_flips_selection`.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Replace the loaded results window.
    ///
    /// Why: a reload must never leave the cursor pointing past the new end.
    /// What: stores the counts and rows and clamps `row_cursor`.
    /// Test: `super::tests::results_reload_clamps_cursor`.
    pub fn set_results(&mut self, counts: CorrelationCounts, rows: Vec<CorrelationRow>) {
        self.counts = counts;
        self.rows = rows;
        self.row_cursor = self.row_cursor.min(self.rows.len().saturating_sub(1));
    }
}

/// Flatten the two config surfaces into picker rows.
///
/// Why: org-wide discovery and the explicit `repositories[]` list are the two
/// ways this crate learns about a repository, and the picker has to show both
/// or the operator cannot tell why a repo is missing.
/// What: every `repositories[]` entry as a selected `Configured` row, then
/// every entry of the effective org list (`github.orgs` ++ `github.org`,
/// deduped) as an unselected `Org` row.
/// Test: `super::tests::picker_lists_configured_repos_and_orgs`.
fn build_repo_entries(config: &Config) -> Vec<RepoEntry> {
    let mut entries: Vec<RepoEntry> = config
        .repositories
        .iter()
        .enumerate()
        .map(|(i, repo)| RepoEntry {
            name: repo.name.clone().unwrap_or_else(|| {
                repo.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| repo.path.display().to_string())
            }),
            source: RepoSource::Configured,
            detail: repo.path.display().to_string(),
            config_index: Some(i),
            selected: true,
        })
        .collect();

    if let Some(gh) = config.github.as_ref() {
        for org in tga::collect::github::org_discovery::effective_orgs(gh.org.as_deref(), &gh.orgs)
        {
            entries.push(RepoEntry {
                name: org,
                source: RepoSource::Org,
                detail: "org-wide discovery — needs a GitHub token".to_string(),
                config_index: None,
                selected: false,
            });
        }
    }
    entries
}
