//! Unit coverage for `tga tui` (#5197).
//!
//! The draw loop is not asserted against; everything it decides lives in
//! `state.rs` and `event_loop.rs` as pure functions, and those are what these
//! tests exercise.

use crossterm::event::{KeyCode, KeyModifiers};

use tga::core::config::{Config, GithubConfig, RepositoryConfig};
use tga::core::db::correlation::{CorrelationCounts, CorrelationFilter};
use tga::core::progress::{ProgressEvent, Stage};

use super::event_loop::{apply, key_to_action, Action, Effect};
use super::render;
use super::state::{RepoSource, TuiState, View, WorkStatus};
use super::work::{run_work, WorkMode};

fn repo(name: &str, path: &str) -> RepositoryConfig {
    RepositoryConfig {
        path: path.into(),
        name: Some(name.to_string()),
        ..Default::default()
    }
}

/// A config with two repos and one org, and NOTHING else — no LLM block, no
/// API key, no board credentials. Every test below runs against this.
fn config() -> Config {
    Config {
        repositories: vec![repo("api", "/tmp/api"), repo("web", "/tmp/web")],
        github: Some(GithubConfig {
            orgs: vec!["acme".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn state() -> TuiState {
    TuiState::new(config())
}

// ------------------------------------------------------------ repo picker

#[test]
fn picker_lists_configured_repos_and_orgs() {
    let s = state();
    assert_eq!(s.repos.len(), 3);
    assert_eq!(s.repos[0].name, "api");
    assert_eq!(s.repos[0].source, RepoSource::Configured);
    assert_eq!(s.repos[0].detail, "/tmp/api");
    assert_eq!(s.repos[1].name, "web");
    assert_eq!(s.repos[2].name, "acme");
    assert_eq!(s.repos[2].source, RepoSource::Org);
    assert_eq!(RepoSource::Configured.label(), "config");
    assert_eq!(RepoSource::Org.label(), "org");
}

#[test]
fn picker_falls_back_to_the_directory_basename() {
    let cfg = Config {
        repositories: vec![RepositoryConfig {
            path: "/srv/repos/monolith".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let s = TuiState::new(cfg);
    assert_eq!(s.repos[0].name, "monolith");
}

#[test]
fn picker_defaults_to_all_configured_repos_selected() {
    let s = state();
    assert_eq!(s.selected_repositories().len(), 2, "all-vs-subset := all");
    assert!(!s.repos[2].selected, "an org is not a repo to walk");
}

#[test]
fn toggle_flips_selection() {
    let mut s = state();
    s.repo_cursor = 0;
    s.toggle_selected();
    assert!(!s.repos[0].selected);
    assert_eq!(s.selected_repositories().len(), 1);
    s.toggle_selected();
    assert!(s.repos[0].selected);
}

#[test]
fn org_rows_are_not_selectable() {
    let mut s = state();
    s.repo_cursor = 2;
    s.toggle_selected();
    assert!(!s.repos[2].selected);
    assert!(!s.repos[2].is_selectable());
    let msg = s.message.clone().unwrap_or_default();
    assert!(msg.contains("acme"), "the refusal names the row: {msg}");
}

#[test]
fn select_all_toggles_between_all_and_none() {
    let mut s = state();
    s.toggle_select_all();
    assert_eq!(s.selected_repositories().len(), 0, "all → none");
    s.toggle_select_all();
    assert_eq!(s.selected_repositories().len(), 2, "none → all");
    // A partial selection goes to all, not to none.
    s.repo_cursor = 0;
    s.toggle_selected();
    s.toggle_select_all();
    assert_eq!(s.selected_repositories().len(), 2);
}

#[test]
fn selected_repositories_maps_back_to_config() {
    let mut s = state();
    s.repo_cursor = 0;
    s.toggle_selected();
    let picked = s.selected_repositories();
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].name.as_deref(), Some("web"));
}

#[test]
fn scoped_config_narrows_repositories_only() {
    let mut s = state();
    s.repo_cursor = 1;
    s.toggle_selected();
    let scoped = s.scoped_config();
    assert_eq!(scoped.repositories.len(), 1);
    assert_eq!(scoped.repositories[0].name.as_deref(), Some("api"));
    assert!(scoped.github.is_some(), "every other setting is preserved");
    assert_eq!(
        s.config.repositories.len(),
        2,
        "the original config is untouched"
    );
}

#[test]
fn cursor_clamps_at_both_ends() {
    let mut s = state();
    s.move_cursor(-1);
    assert_eq!(s.repo_cursor, 0);
    s.move_cursor(100);
    assert_eq!(s.repo_cursor, 2, "clamped to the last row");
    s.move_cursor(1);
    assert_eq!(s.repo_cursor, 2);

    // The results view has its own cursor and an empty list.
    s.view = View::Results;
    s.move_cursor(5);
    assert_eq!(s.row_cursor, 0);
    // The progress view has no cursor at all.
    s.view = View::Progress;
    s.move_cursor(3);
    assert_eq!(s.row_cursor, 0);
    assert_eq!(s.repo_cursor, 2);
}

#[test]
fn empty_config_produces_an_empty_picker_not_a_panic() {
    let mut s = TuiState::new(Config::default());
    assert!(s.repos.is_empty());
    s.toggle_selected();
    s.toggle_select_all();
    s.move_cursor(1);
    assert_eq!(s.repo_cursor, 0);
    assert!(s.selected_repositories().is_empty());
}

// -------------------------------------------------------------- key map

#[test]
fn view_cycles() {
    assert_eq!(View::default(), View::Repos);
    assert_eq!(View::Repos.next(), View::Progress);
    assert_eq!(View::Progress.next(), View::Results);
    assert_eq!(View::Results.next(), View::Repos);
    assert_eq!(View::Repos.label(), "Repos");
}

#[test]
fn key_to_action_maps_every_binding() {
    let n = KeyModifiers::NONE;
    assert_eq!(key_to_action(KeyCode::Char('q'), n), Action::Quit);
    assert_eq!(key_to_action(KeyCode::Esc, n), Action::Quit);
    assert_eq!(
        key_to_action(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Action::Quit,
        "Ctrl-C quits rather than starting a correlate run"
    );
    assert_eq!(key_to_action(KeyCode::Tab, n), Action::NextView);
    assert_eq!(
        key_to_action(KeyCode::Char('2'), n),
        Action::GoTo(View::Progress)
    );
    assert_eq!(key_to_action(KeyCode::Up, n), Action::Move(-1));
    assert_eq!(key_to_action(KeyCode::Char('j'), n), Action::Move(1));
    assert_eq!(key_to_action(KeyCode::PageDown, n), Action::Move(10));
    assert_eq!(key_to_action(KeyCode::Char(' '), n), Action::Toggle);
    assert_eq!(key_to_action(KeyCode::Char('a'), n), Action::ToggleAll);
    assert_eq!(key_to_action(KeyCode::Char('r'), n), Action::RunFull);
    assert_eq!(
        key_to_action(KeyCode::Char('c'), n),
        Action::RunCorrelateOnly
    );
    assert_eq!(key_to_action(KeyCode::Char('f'), n), Action::CycleFilter);
    assert_eq!(key_to_action(KeyCode::Char('g'), n), Action::Reload);
    assert_eq!(key_to_action(KeyCode::Char('?'), n), Action::ToggleHelp);
    assert_eq!(key_to_action(KeyCode::Char('z'), n), Action::None);
}

// ------------------------------------------------------ state transitions

#[test]
fn action_quit_closes_help_first() {
    let mut s = state();
    s.show_help = true;
    apply(&mut s, Action::Quit);
    assert!(!s.show_help);
    assert!(!s.quit, "the first Esc closes help, it does not exit");
    apply(&mut s, Action::Quit);
    assert!(s.quit);
}

#[test]
fn action_run_requests_a_worker() {
    let mut s = state();
    assert_eq!(
        apply(&mut s, Action::RunFull),
        Effect::Start(WorkMode::PullAndCorrelate)
    );
    assert_eq!(s.status, WorkStatus::Running);
    assert_eq!(s.view, View::Progress, "a run switches to the live view");
}

#[test]
fn run_is_refused_while_running() {
    let mut s = state();
    apply(&mut s, Action::RunFull);
    assert_eq!(
        apply(&mut s, Action::RunFull),
        Effect::Nothing,
        "a second run is refused, not queued"
    );
    assert!(s.message.unwrap_or_default().contains("already in flight"));
}

/// #5197 finding 3: `q` / `Esc` / `Ctrl-C` used to set `quit` unconditionally,
/// and the worker is a detached thread nobody joins — so quitting mid-run cut
/// an in-flight fetch or per-week write off at process exit with no warning.
#[test]
fn quit_during_a_run_takes_a_confirming_second_press() {
    let mut s = state();
    apply(&mut s, Action::RunFull);
    assert_eq!(s.status, WorkStatus::Running);

    assert_eq!(apply(&mut s, Action::Quit), Effect::Nothing);
    assert!(!s.quit, "the first press does not abandon a run in flight");
    let msg = s.message.clone().unwrap_or_default();
    assert!(msg.contains("in flight"), "the refusal says why: {msg}");

    apply(&mut s, Action::Quit);
    assert!(s.quit, "the confirming press abandons it");
}

/// The confirmation is armed for the next keypress only — an intervening key
/// must not leave a loaded quit behind.
#[test]
fn any_other_key_disarms_the_quit_confirmation() {
    let mut s = state();
    apply(&mut s, Action::RunFull);
    apply(&mut s, Action::Quit);
    assert!(s.quit_armed);

    apply(&mut s, Action::NextView);
    assert!(!s.quit_armed);
    apply(&mut s, Action::Quit);
    assert!(!s.quit, "the confirmation had to be re-armed");
}

/// The guard is scoped to a run in flight: idle and help-overlay behaviour are
/// unchanged, and help still closes before anything else.
#[test]
fn quit_is_immediate_when_no_run_is_in_flight() {
    let mut s = state();
    apply(&mut s, Action::Quit);
    assert!(s.quit, "an idle TUI exits on the first press");

    let mut s = state();
    s.show_help = true;
    apply(&mut s, Action::RunFull);
    apply(&mut s, Action::Quit);
    assert!(!s.show_help, "help closes first, even mid-run");
    assert!(!s.quit);
}

#[test]
fn pull_is_refused_with_no_repositories_selected() {
    let mut s = state();
    s.toggle_select_all(); // all → none
    assert_eq!(apply(&mut s, Action::RunFull), Effect::Nothing);
    assert_eq!(s.status, WorkStatus::Idle);
    assert!(s.message.unwrap_or_default().contains("no repositories"));
}

/// Correlate-only needs no repository selection, no credentials, and no
/// network — it is the mode that makes the TUI useful with nothing configured.
#[test]
fn correlate_only_runs_with_no_selection_and_no_credentials() {
    let mut s = TuiState::new(Config::default());
    assert!(s.repos.is_empty(), "nothing configured at all");
    assert_eq!(
        apply(&mut s, Action::RunCorrelateOnly),
        Effect::Start(WorkMode::CorrelateOnly)
    );
    assert_eq!(s.status, WorkStatus::Running);
}

#[test]
fn offline_state_refuses_the_pull_action() {
    let mut s = state().offline(true);
    assert_eq!(apply(&mut s, Action::RunFull), Effect::Nothing);
    assert_eq!(s.status, WorkStatus::Idle);
    assert!(s.message.unwrap_or_default().contains("--correlate-only"));
    // The deterministic pass is still available.
    let mut s = state().offline(true);
    assert_eq!(
        apply(&mut s, Action::RunCorrelateOnly),
        Effect::Start(WorkMode::CorrelateOnly)
    );
}

#[test]
fn filter_change_asks_for_a_reload_and_resets_the_cursor() {
    let mut s = state();
    s.row_cursor = 4;
    assert_eq!(apply(&mut s, Action::CycleFilter), Effect::Reload);
    assert_eq!(s.filter, CorrelationFilter::Linked);
    assert_eq!(s.row_cursor, 0);
    assert_eq!(apply(&mut s, Action::Reload), Effect::Reload);
}

#[test]
fn toggle_only_applies_in_the_repos_view() {
    let mut s = state();
    s.view = View::Results;
    apply(&mut s, Action::Toggle);
    apply(&mut s, Action::ToggleAll);
    assert_eq!(
        s.selected_repositories().len(),
        2,
        "keys bound to the picker do nothing elsewhere"
    );
}

#[test]
fn any_bound_action_clears_the_transient_message() {
    let mut s = state();
    s.set_message("stale");
    apply(&mut s, Action::NextView);
    assert!(s.message.is_none());
    // An unbound key leaves it alone.
    s.set_message("kept");
    apply(&mut s, Action::None);
    assert_eq!(s.message.as_deref(), Some("kept"));
}

// -------------------------------------------------------------- progress

#[test]
fn pump_folds_bus_events() {
    let mut s = state();
    s.bus
        .emit(ProgressEvent::started(Stage::Collect, "api", Some(2)));
    s.bus
        .emit(ProgressEvent::completed(Stage::Collect, "api", 2));
    s.pump_progress();
    assert_eq!(s.progress.summary(Stage::Collect).completed, 1);
    assert!(s.progress.is_settled());
    // Draining twice is idempotent.
    s.pump_progress();
    assert_eq!(s.progress.rows(Stage::Collect).len(), 1);
}

#[test]
fn pump_records_the_bus_drop_count() {
    let mut s = state();
    // The default bus is large; force an overflow with a small one.
    s.bus = tga::core::progress::ProgressBus::bounded(2);
    for i in 0..5 {
        s.bus
            .emit(ProgressEvent::advanced(Stage::Collect, "api", i, None));
    }
    s.pump_progress();
    assert_eq!(s.progress.dropped(), 3);
    assert!(render::status_line(&s).contains("dropped 3"));
}

// ----------------------------------------------------------------- work

/// #5197 finding 2: `run_work` read only `stats.commits_collected`, so a
/// collection failure had NO surface in the TUI — the Progress pane said the
/// repo completed, and the `Finished(summary)` status line said "collected 0
/// commit(s)" with no hint anything went wrong. `tga collect` prints it.
#[test]
fn the_run_summary_carries_the_collection_error_count() {
    // An initialised repo with no commits opens fine and then fails its walk —
    // the same shape as a permissions failure on a real repo, without needing
    // one.
    let root = std::env::temp_dir().join(format!("tga-5197-summary-{}", std::process::id()));
    let repo_path = root.join("broken");
    std::fs::create_dir_all(&repo_path).expect("mkdir");
    git2::Repository::init(&repo_path).expect("git init");

    let cfg = Config {
        repositories: vec![RepositoryConfig {
            path: repo_path,
            name: Some("broken".into()),
            since_date: Some("2026-01-05".into()),
            until_date: Some("2026-01-11".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let summary = run_work(
        cfg,
        root.join("tga.db"),
        WorkMode::PullAndCorrelate,
        &tga::core::progress::ProgressBus::disabled(),
    )
    .expect("run_work");
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        summary.contains("collected 0 commit(s)"),
        "the commit count is still reported: {summary}"
    );
    assert!(
        summary.contains("error(s)") && !summary.contains("0 error(s)"),
        "the summary states how many errors the run recorded: {summary}"
    );
}

// ---------------------------------------------------------------- render

#[test]
fn title_line_marks_the_active_view() {
    let mut s = state();
    let line = render::title_line(&s);
    let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(text.contains("Repos") && text.contains("Progress") && text.contains("Results"));
    s.view = View::Results;
    assert_eq!(render::title_line(&s).spans.len(), line.spans.len());
}

#[test]
fn status_line_reports_worker_state() {
    let mut s = state();
    assert!(render::status_line(&s).contains("idle"));
    s.status = WorkStatus::Running;
    assert!(render::status_line(&s).contains("running"));
    s.status = WorkStatus::Finished("collected 4 commit(s); 2 linked".into());
    let line = render::status_line(&s);
    assert!(line.contains("2 linked"));
    assert!(line.contains(render::KEY_HINT));
    s.set_message("something happened");
    assert!(render::status_line(&s).contains("something happened"));
}

#[test]
fn repo_lines_mark_selection_and_source() {
    let mut s = state();
    assert!(render::repo_line(&s, 0).starts_with("[x]"));
    s.repo_cursor = 0;
    s.toggle_selected();
    assert!(render::repo_line(&s, 0).starts_with("[ ]"));
    let org = render::repo_line(&s, 2);
    assert!(!org.contains("[x]") && !org.contains("[ ]"), "{org}");
    assert!(org.contains("org"));
    assert_eq!(render::repo_line(&s, 99), "", "out of range renders empty");
}

#[test]
fn progress_lines_show_counts_and_outcome() {
    let mut s = state();
    assert!(
        render::progress_lines(&s).is_empty(),
        "no stage has started"
    );
    s.bus
        .emit(ProgressEvent::advanced(Stage::Collect, "api", 3, Some(9)));
    s.bus
        .emit(ProgressEvent::failed(Stage::Collect, "web", "no remote"));
    s.pump_progress();
    let lines = render::progress_lines(&s);
    assert!(lines[0].contains("Collect"));
    assert!(lines.iter().any(|l| l.contains("3/9")));
    assert!(lines.iter().any(|l| l.contains("failed")));
    assert!(lines.iter().any(|l| l.contains("no remote")));
}

#[test]
fn results_header_states_coverage() {
    let mut s = state();
    s.set_results(CorrelationCounts::new(10, 4, 3, 3, 7, 2), Vec::new());
    let header = render::results_header(&s);
    assert!(header.contains("10 commits"));
    assert!(header.contains("4 linked (40%)"));
    assert!(header.contains("6 unlinked"));
    assert!(header.contains("3 with a ticket"));
    assert!(header.contains("filter: all"));
}

#[test]
fn help_text_states_the_zero_inference_default() {
    let text = render::help_text();
    assert!(text.contains("no model and no API key are required"));
    assert!(text.contains("correlate only"));
}

#[test]
fn results_reload_clamps_cursor() {
    let mut s = state();
    s.row_cursor = 7;
    s.set_results(CorrelationCounts::default(), Vec::new());
    assert_eq!(s.row_cursor, 0);
}

#[test]
fn result_lines_name_the_linked_items() {
    use tga::core::db::correlation::{correlation_counts, correlation_rows};
    use tga::core::db::work_items::{link_commit_work_item, upsert_work_item, WorkItemRow};
    use tga::core::db::Database;

    // A real database, built with no config, no model, and no API key — the
    // #5197 zero-inference contract, end to end through the results view.
    let db = Database::open_in_memory().expect("open");
    for (sha, msg, ticket) in [
        ("aaaaaaaa11", "PROJ-1 ship the thing", Some("PROJ-1")),
        ("bbbbbbbb22", "PROJ-9 no board item", Some("PROJ-9")),
        ("cccccccc33", "chore: tidy up", None),
    ] {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                      repository, ticket_id) \
                 VALUES (?1, 'A', 'a@x', '2026-01-01T00:00:00Z', ?2, 'api', ?3)",
                rusqlite::params![sha, msg, ticket],
            )
            .expect("insert");
    }
    upsert_work_item(
        db.connection(),
        &WorkItemRow {
            id: "PROJ-1".into(),
            source: "jira".into(),
            title: "Ship the thing".into(),
            status: "Done".into(),
            item_type: "Task".into(),
            tags: None,
            project: None,
            url: None,
            raw_json: None,
        },
    )
    .expect("upsert");
    link_commit_work_item(db.connection(), "aaaaaaaa11", "PROJ-1", "jira").expect("link");

    let mut s = state();
    s.set_results(
        correlation_counts(db.connection()).expect("counts"),
        correlation_rows(db.connection(), CorrelationFilter::All, 10).expect("rows"),
    );

    let lines: Vec<String> = (0..s.rows.len())
        .map(|i| render::result_line(&s, i))
        .collect();
    let joined = lines.join("\n");
    assert!(joined.contains("jira:PROJ-1"), "{joined}");
    assert!(joined.contains("no board item for PROJ-9"), "{joined}");
    assert!(joined.contains("no ticket reference"), "{joined}");
    assert!(render::results_header(&s).contains("3 commits"));
    assert_eq!(render::result_line(&s, 99), "");
}
