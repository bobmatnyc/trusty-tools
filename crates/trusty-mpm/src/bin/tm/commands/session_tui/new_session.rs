//! The `tm ls` TUI's new-session flow (#7395).
//!
//! Why: the session TUI (#7224) could select, rename and delete a session but
//! never create one, so an operator who wanted a session in a project that had
//! none had to leave the surface and type `tm session new`. This adds the
//! missing verb without adding a second way to do it.
//!
//! What this REUSES. The registered projects come from
//! [`trusty_mpm::client::DaemonClient::registry_list_projects`] — the same
//! `GET /api/v1/projects` read `tm project list` performs
//! ([`crate::commands::project::list_rows`]). An unregistered path is
//! registered through [`crate::commands::projects::registry::register`], the
//! upsert `tm projects register` and the post-clone account step both call. The
//! session itself is created and attached by
//! [`crate::commands::guided_launch::launch_new_session_and_attach`], the same
//! function the numbered picker's launch-new arm reaches — so the TUI cannot
//! disagree with the CLI about what "a new session" is. Nothing here issues a
//! spawn POST or a register PUT of its own.
//!
//! What is NEW is only the choosing: [`NewSessionFlow`] is a pure state machine
//! over the target list and the typed-path buffer, and [`perform_with`] is the
//! driver that orders register-then-create.
//!
//! #7406 polished that choosing without touching either leg: the overlay opens
//! on the cursor session's project ([`preselect_index`]), each row reads
//! `owner/repo` rather than the stored URL or path ([`row_labels`]), and a
//! registration nobody can work in is dropped by
//! [`is_offerable_project`](crate::commands::projects::offerable::is_offerable_project).
//!
//! #7421 made the choosing usable past eight registrations: the rows come in a
//! deterministic order ([`super::new_session_order::ordered_targets`]), the
//! overlay says where in the list the window sits
//! ([`NewSessionFlow::position`]), and typing narrows it
//! ([`NewSessionFlow::filter`]).
//!
//! #7488 made the choosing structural and open-ended: the rows are grouped by
//! owner-or-domain ([`super::new_session_order::group_of`]), and the filter box
//! doubles as free-text entry — a clone URL, `owner/repo` or
//! `domain/owner/repo` the registry does not hold is cloned and registered
//! through the SAME [`perform`] driver a typed checkout path uses
//! ([`super::new_session_entry`]).
//!
//! Test: `new_session_*` in `super::tests`.

use std::future::Future;
use std::path::Path;

use anyhow::Context as _;
use trusty_common::github_path::parse_remote_url;
use trusty_mpm::client::{DaemonClient, ManagedSessionSummary};
use trusty_mpm::project::Project;
use trusty_mpm::project::record::repo_url_matches;

use crate::commands::picker_launch_new::LaunchIsolation;
use crate::commands::projects::registry::RegisterInput;
use crate::commands::tmux_attach::AttachOutcome;

use super::state::Input;

/// How many target rows the overlay shows at once.
const PICK_ROWS: usize = 8;

/// One thing the operator can start a session in.
///
/// Why: the registry answers "which projects does tm already know", and the
/// operator's own checkout answers the rest. Both are rows in one list rather
/// than two separate prompts, so a single Enter confirms either.
/// What: [`Self::Registered`] carries the registry's `repo_url` verbatim — the
/// same string `tm session new <repo>` takes — and [`Self::Other`] is the
/// escape hatch that opens the free-text path entry.
/// Test: `new_session_targets_from_puts_the_path_escape_last`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A project already in the daemon's registry.
    Registered {
        /// The registry key, shown in the row and used as the status label.
        name: String,
        /// The registry's `repo_url`, passed through to the create call.
        repo: String,
    },
    /// Type a path to a checkout the registry does not have yet.
    Other,
}

/// A project identity resolved from a typed checkout path.
///
/// Why: registering a project needs a name and a repo URL, and creating a
/// session in it needs the local working-tree root. All three come from one
/// look at the checkout, so they travel together.
/// What: `name` is the repo leaf (the registry's naming convention), `repo_url`
/// the canonical GitHub URL, `root` the git working-tree root.
/// Test: `new_session_unregistered_path_registers_before_creating`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectIdentity {
    /// Registry key for the project.
    pub(crate) name: String,
    /// Canonical repository URL.
    pub(crate) repo_url: String,
    /// Local git working-tree root.
    pub(crate) root: String,
}

/// How a typed path becomes a [`ProjectIdentity`].
///
/// Why: the resolution reads a real git checkout, which no unit test should
/// need. A function pointer keeps the whole unregistered-path branch decidable
/// with a stub — production passes [`derive_identity`], tests pass their own.
/// Test: `new_session_unregistered_path_registers_before_creating`,
/// `new_session_rejects_a_path_that_is_not_a_checkout`.
pub(crate) type Resolver = fn(&str) -> Option<ProjectIdentity>;

/// A project the flow must register before a session can be created in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewProject {
    /// Registry key.
    pub(crate) name: String,
    /// Repository URL.
    pub(crate) repo_url: String,
}

/// One confirmed new-session flow, as the driver must perform it.
///
/// Why: the state machine decides, the driver acts — the same split every other
/// action in this surface makes, which is what lets "register runs before
/// create" be asserted without a daemon.
/// What: `register` is `Some` only for a path the registry does not already
/// hold; `repo` is what the create call receives; `label` names the project in
/// the status line.
/// Test: `new_session_confirming_a_registered_project_creates_without_registering`,
/// `new_session_unregistered_path_registers_before_creating`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewSessionRequest {
    /// Register this first, when the target was not already registered.
    pub(crate) register: Option<NewProject>,
    /// The `repo_url` argument for the create call.
    pub(crate) repo: String,
    /// The project's display name.
    pub(crate) label: String,
}

/// What one keystroke did to the flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// The overlay changed; draw it again.
    Redraw,
    /// Nothing changed.
    Ignore,
    /// Leave the flow and return to the list unchanged.
    Cancel,
    /// The typed text is unusable; keep the entry open and say why.
    Reject(String),
    /// Confirmed — perform this.
    Create(NewSessionRequest),
}

/// The new-session overlay's own state.
///
/// Why: a buffer and a selection that only this flow owns, so opening and
/// closing it cannot disturb the session list, the pinned selection, or the
/// scroll offset behind it (#7395 requirement 4).
/// What: `typed` is `Some` exactly while the free-text path entry is open;
/// until then the operator is moving over `targets`.
/// Test: `new_session_*` in `super::tests`.
#[derive(Debug, Clone)]
pub(crate) struct NewSessionFlow {
    targets: Vec<Target>,
    selected: usize,
    typed: Option<String>,
    /// #7421: the substring the operator has typed to narrow the rows.
    filter: String,
    resolve: Resolver,
}

/// Equality is over what the operator can SEE, never the resolver.
///
/// Why: `Mode` is compared in tests, so this type owes a `PartialEq` — but a
/// derived one compares the function pointer too, and Rust does not guarantee
/// two references to the same function share an address. Comparing the targets,
/// the highlight and the buffer is both meaningful and stable.
impl PartialEq for NewSessionFlow {
    fn eq(&self, other: &Self) -> bool {
        self.targets == other.targets
            && self.selected == other.selected
            && self.typed == other.typed
            && self.filter == other.filter
    }
}

impl Eq for NewSessionFlow {}

impl NewSessionFlow {
    /// Open the flow over `targets`, resolving typed paths against real git.
    pub(crate) fn new(targets: Vec<Target>) -> Self {
        Self::with_resolver(targets, derive_identity)
    }

    /// Open the flow with an explicit [`Resolver`] (the test seam).
    pub(crate) fn with_resolver(targets: Vec<Target>, resolve: Resolver) -> Self {
        Self {
            targets,
            selected: 0,
            typed: None,
            filter: String::new(),
            resolve,
        }
    }

    /// Highlight the project the list cursor was already on (#7406).
    ///
    /// Why: the operator opened this overlay from a row, and the project of
    /// that row is overwhelmingly the project they want another session in.
    /// Starting on row 0 instead made them scroll past it every time.
    /// What: moves the highlight to [`preselect_index`]'s answer for `session`,
    /// which falls back to the first row when nothing matches.
    /// Test: `new_session_preselects_the_cursor_sessions_project`.
    pub(crate) fn preselected_for(mut self, session: Option<&ManagedSessionSummary>) -> Self {
        self.selected = preselect_index(&self.targets, session);
        self
    }

    /// The free-text buffer, while the path entry is open.
    pub(crate) fn typed(&self) -> Option<&str> {
        self.typed.as_deref()
    }

    /// The substring the operator has typed to narrow the rows (#7421).
    pub(crate) fn filter(&self) -> &str {
        &self.filter
    }

    /// Which targets the current filter shows, as indices into `targets`.
    ///
    /// Why (#7421): the filter changes what the operator can move over, so the
    /// window, the highlight and the position indicator all have to read the
    /// same answer rather than each deriving one.
    /// What: every row whose [`row_labels`] label contains the filter, compared
    /// case-insensitively. [`Target::Other`] always survives — filtering the
    /// typed-path escape away would leave a non-matching filter with no way
    /// forward and no way to reach a checkout the registry lacks.
    /// Test: `new_session_typing_filters_the_rows`.
    fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        row_labels(&self.targets)
            .into_iter()
            .enumerate()
            .filter(|(i, label)| {
                needle.is_empty()
                    || matches!(self.targets.get(*i), Some(Target::Other))
                    || label.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Where the highlight sits within [`Self::visible`].
    fn cursor(&self, visible: &[usize]) -> usize {
        visible
            .iter()
            .position(|i| *i == self.selected)
            .unwrap_or(0)
    }

    /// First visible row the window draws, given where the highlight is.
    fn window_start(&self, visible: &[usize]) -> usize {
        self.cursor(visible)
            .saturating_sub(PICK_ROWS.saturating_sub(1))
    }

    /// The overlay's visible target rows, at most [`PICK_ROWS`] of them.
    ///
    /// Why: a host with forty registered projects would otherwise draw an
    /// overlay taller than the terminal. The window scrolls only once the
    /// selection passes the bottom, so a short registry never moves.
    /// What: the marked rows, in [`super::new_session_order::ordered_targets`]'
    /// order and narrowed by the filter (#7421), starting at whichever offset
    /// keeps the selection on screen. Each row is [`row_labels`]' short
    /// `owner/repo` form (#7406), never the stored URL or path.
    /// Test: `new_session_rows_window_keeps_the_selection_visible`,
    /// `new_session_rows_show_owner_repo_only`, `new_session_typing_filters_the_rows`.
    pub(crate) fn rows(&self) -> Vec<String> {
        let labels = row_labels(&self.targets);
        let visible = self.visible();
        visible
            .iter()
            .skip(self.window_start(&visible))
            .take(PICK_ROWS)
            .map(|i| {
                let marker = if *i == self.selected { "▸" } else { " " };
                let label = labels.get(*i).map_or("", String::as_str);
                format!("{marker} {label}")
            })
            .collect()
    }

    /// Where the window sits in the list, when the list is longer than it
    /// (#7421).
    ///
    /// Why: the owner could not tell that rows existed past the eighth, so a
    /// project that scrolled out of the window simply looked absent.
    /// What: `Some("3–10 of 27")` — the one-based span the window draws and the
    /// filtered row count. `None` when everything fits, because a count that
    /// never changes is noise.
    /// Test: `new_session_position_indicator_reports_the_window`.
    pub(crate) fn position(&self) -> Option<String> {
        let visible = self.visible();
        if visible.len() <= PICK_ROWS {
            return None;
        }
        let start = self.window_start(&visible);
        let end = (start + PICK_ROWS).min(visible.len());
        Some(format!("{}–{} of {}", start + 1, end, visible.len()))
    }

    /// Route one keystroke through whichever step is open.
    ///
    /// Why: Esc must cancel from EVERY step (#7395 requirement 4), so it is
    /// matched once, ahead of the per-step handlers, rather than repeated in
    /// each of them where one could be forgotten.
    /// What: the path entry takes over as soon as it is open; before that the
    /// keys move over the target list. #7421 gives Esc one job first — a
    /// non-empty filter is cleared, and only an Esc with nothing left to clear
    /// cancels the flow.
    /// Test: `new_session_escape_cancels_from_both_steps`,
    /// `new_session_typing_filters_the_rows`.
    pub(crate) fn apply(&mut self, input: Input) -> Step {
        match input {
            // #7421: clear the filter before cancelling, so a mistyped filter
            // costs one key rather than the whole flow.
            Input::Escape if self.typed.is_none() && !self.filter.is_empty() => {
                self.filter.clear();
                Step::Redraw
            }
            Input::Escape => Step::Cancel,
            _ if self.typed.is_some() => self.apply_path(input),
            _ => self.apply_pick(input),
        }
    }

    /// Target-list keys: move or filter, then Enter to confirm or type a path.
    fn apply_pick(&mut self, input: Input) -> Step {
        let visible = self.visible();
        let cursor = self.cursor(&visible);
        let last = visible.len().saturating_sub(1);
        match input {
            Input::Up => self.move_within(&visible, cursor.saturating_sub(1)),
            Input::Down => self.move_within(&visible, (cursor + 1).min(last)),
            Input::PageUp => self.move_within(&visible, cursor.saturating_sub(PICK_ROWS)),
            Input::PageDown => self.move_within(&visible, (cursor + PICK_ROWS).min(last)),
            Input::Home => self.move_within(&visible, 0),
            Input::End => self.move_within(&visible, last),
            // #7421: a printable key narrows the list rather than moving over
            // it, so `j`/`k` are no longer movement here — the arrows are.
            Input::Char(c) => {
                self.filter.push(c);
                self.keep_selection_visible();
                Step::Redraw
            }
            Input::Backspace => {
                if self.filter.pop().is_none() {
                    return Step::Ignore;
                }
                self.keep_selection_visible();
                Step::Redraw
            }
            Input::Enter => match self.targets.get(self.selected) {
                Some(Target::Registered { name, repo }) => Step::Create(NewSessionRequest {
                    register: None,
                    repo: repo.clone(),
                    label: name.clone(),
                }),
                Some(Target::Other) => self.confirm_escape_row(),
                None => Step::Ignore,
            },
            _ => Step::Ignore,
        }
    }

    /// Enter on the escape row: free-text entry, or the typed-path step (#7488).
    ///
    /// Why: the filter box is already where the operator types a project's
    /// name, so a name the registry does NOT hold is the one keystroke sequence
    /// that used to lead nowhere — it emptied the list and left Enter meaning
    /// "open an empty path prompt and type it all again".
    /// What: a filter that still matches a registered row (or no filter at all)
    /// keeps the pre-#7488 behaviour and opens the typed-path entry. A filter
    /// matching nothing is handed to
    /// [`request_for_entry`](super::new_session_entry::request_for_entry): a
    /// clone URL, `owner/repo` or `domain/owner/repo` becomes a create request,
    /// and anything else is a [`Step::Reject`] the overlay shows inline, with
    /// the filter left intact so it can be corrected.
    /// Test: `new_session_entry_from_the_filter_creates_a_clone_request`,
    /// `new_session_entry_from_the_filter_rejects_malformed_text`.
    fn confirm_escape_row(&mut self) -> Step {
        let typed = self.filter.trim().to_string();
        if typed.is_empty() || self.filter_matches_a_registered_row() {
            self.typed = Some(String::new());
            return Step::Redraw;
        }
        match super::new_session_entry::request_for_entry(&typed, &self.targets) {
            Ok(request) => Step::Create(request),
            Err(message) => Step::Reject(message),
        }
    }

    /// True while the filter still leaves a registered project on screen.
    fn filter_matches_a_registered_row(&self) -> bool {
        self.visible()
            .iter()
            .any(|i| matches!(self.targets.get(*i), Some(Target::Registered { .. })))
    }

    /// Move the highlight to a position within the filtered rows.
    fn move_within(&mut self, visible: &[usize], position: usize) -> Step {
        match visible.get(position) {
            Some(index) => self.move_to(*index),
            None => Step::Ignore,
        }
    }

    /// Move the highlight, reporting whether anything changed.
    fn move_to(&mut self, index: usize) -> Step {
        if self.targets.is_empty() || index == self.selected {
            return Step::Ignore;
        }
        self.selected = index;
        Step::Redraw
    }

    /// Re-seat the highlight when the filter just hid the row it was on (#7421).
    fn keep_selection_visible(&mut self) {
        let visible = self.visible();
        if visible.contains(&self.selected) {
            return;
        }
        self.selected = visible.first().copied().unwrap_or(0);
    }

    /// Path-entry keys: edit the buffer, then Enter to resolve and confirm.
    fn apply_path(&mut self, input: Input) -> Step {
        let Some(typed) = self.typed.as_mut() else {
            return Step::Ignore;
        };
        match input {
            Input::Char(c) => {
                typed.push(c);
                Step::Redraw
            }
            Input::Backspace => {
                typed.pop();
                Step::Redraw
            }
            Input::Enter => match request_for_path(typed, &self.targets, self.resolve) {
                Ok(request) => Step::Create(request),
                Err(msg) => Step::Reject(msg),
            },
            _ => Step::Ignore,
        }
    }
}

/// Turn the operator's typed path into a create request.
///
/// Why (#7395 requirement 2): a path the registry already holds must NOT be
/// registered a second time — `register` is an unqualified upsert, so a
/// redundant call would replace the stored record's optional fields with the
/// two this flow can supply. Comparing against the targets already in hand
/// answers that without another round trip.
/// What: an empty buffer or a path that is not a GitHub-backed git checkout is
/// rejected with the reason. Otherwise the request carries the checkout's git
/// ROOT as `repo` (what the daemon needs to resolve `source_id` for a local
/// tree) and a `register` step only when neither the name nor the URL is
/// already a registered target.
/// Test: `new_session_unregistered_path_registers_before_creating`,
/// `new_session_known_path_skips_registration`,
/// `new_session_rejects_a_path_that_is_not_a_checkout`,
/// `new_session_rejects_an_empty_path`.
pub(crate) fn request_for_path(
    typed: &str,
    targets: &[Target],
    resolve: Resolver,
) -> Result<NewSessionRequest, String> {
    let trimmed = typed.trim();
    if trimmed.is_empty() {
        return Err("type the path, clone URL or owner/repo, then Enter".to_string());
    }
    let Some(id) = resolve(trimmed) else {
        // #7488: text that is not a checkout on this host may still name a
        // project to clone, so the same recogniser the filter entry uses gets
        // the second look — one implementation, two ways in.
        return super::new_session_entry::request_for_entry(trimmed, targets).map_err(|_| {
            format!(
                "{trimmed} is not a git checkout, a clone URL, or an \
                 owner/repo project"
            )
        });
    };
    let known = targets.iter().any(|t| match t {
        Target::Registered { name, repo } => *name == id.name || *repo == id.repo_url,
        Target::Other => false,
    });
    Ok(NewSessionRequest {
        register: (!known).then(|| NewProject {
            name: id.name.clone(),
            repo_url: id.repo_url,
        }),
        repo: id.root,
        label: id.name,
    })
}

/// Resolve a typed path against the real checkout on disk.
///
/// Why: [`crate::commands::guided::derive_project`] is the SAME detector bare
/// `tm` and `tm session start` use to decide what counts as a GitHub-backed
/// project, so this flow cannot recognise a different set of checkouts than
/// they do.
/// What: `None` for a path that is not inside a git working tree, or whose
/// origin remote is not a parseable GitHub one. Otherwise the repo leaf as the
/// registry name, the canonical `https://github.com/<owner>/<repo>` URL, and
/// the working-tree root.
/// Test: exercised through the CLI; the branch it feeds is covered by
/// `new_session_unregistered_path_registers_before_creating` with a stub.
pub(crate) fn derive_identity(typed: &str) -> Option<ProjectIdentity> {
    let (source_id, _workspace, git_root) =
        crate::commands::guided::derive_project(Path::new(typed))?;
    let name = source_id.rsplit('/').next()?.to_string();
    Some(ProjectIdentity {
        name,
        repo_url: format!("https://github.com/{source_id}"),
        root: git_root.to_string_lossy().to_string(),
    })
}

/// Read the registered projects and build the flow's target list.
///
/// Why: `GET /api/v1/projects` is the authoritative registry — the same read
/// `tm project list` makes (#5994) and the same store the `project_register`
/// MCP tool writes. Asking it here is what keeps the TUI's project list from
/// being a third opinion.
/// What: every registry row becomes a [`Target::Registered`], with
/// [`Target::Other`] appended so an unregistered checkout is always reachable
/// even when the registry is empty. `sessions` is the list the `tm ls` TUI has
/// already fetched — it decides the order (#7421), so no second read is made
/// for it.
/// Test: the pure half is `new_session_targets_from_puts_the_path_escape_last`;
/// the read itself is the one `list_rows` already covers.
pub(crate) async fn fetch_targets(
    client: &reqwest::Client,
    url: &str,
    sessions: &[ManagedSessionSummary],
) -> anyhow::Result<Vec<Target>> {
    let projects = DaemonClient::with_client(client.clone(), url.to_string())
        .registry_list_projects(None)
        .await?;
    Ok(targets_from(&projects, sessions))
}

/// Pure half of [`fetch_targets`]: registry rows → targets, escape hatch last.
///
/// #7406: a row the operator must never be offered — a temp-directory or
/// scratchpad registration, a path that is gone, a directory that is not a
/// checkout — is dropped, through the shared
/// [`is_offerable_project`](crate::commands::projects::offerable::is_offerable_project)
/// predicate.
/// #7421: the surviving rows are ordered by
/// [`super::new_session_order::ordered_targets`] rather than left in the
/// registry's `HashMap` iteration order.
/// Test: `new_session_targets_from_drops_a_scratchpad_registration`,
/// `new_session_order_is_independent_of_registry_iteration_order`.
pub(crate) fn targets_from(
    projects: &[Project],
    sessions: &[ManagedSessionSummary],
) -> Vec<Target> {
    super::new_session_order::ordered_targets(projects, sessions)
}

/// One display label per target — `owner/repo`, never a URL or a path (#7406).
///
/// Why: the owner's screen showed a stored `repo_url` verbatim, which is either
/// a full `https://github.com/owner/repo` or an absolute checkout path. Both are
/// wider than the overlay, so the row wrapped and the list stopped being
/// scannable. `owner/repo` is the identity, and everything else was noise.
/// What: a parseable git remote becomes `owner/repo`, widened to
/// `host/owner/repo` ONLY when another row on a different host spells the same
/// `owner/repo`. An absolute path becomes `<basename> (local)`, as does a row
/// with no `repo_url` at all. Anything else falls back to the registry name.
/// Test: `new_session_rows_show_owner_repo_only`,
/// `new_session_labels_disambiguate_by_host`.
pub(crate) fn row_labels(targets: &[Target]) -> Vec<String> {
    let labels: Vec<Option<Label>> = targets
        .iter()
        .map(|t| match t {
            Target::Registered { name, repo } => Some(label_for(name, repo)),
            Target::Other => None,
        })
        .collect();
    labels
        .iter()
        .map(|label| match label {
            Some(Label::Remote { host, owner, repo }) => {
                if collides_across_hosts(&labels, host, owner, repo) {
                    format!("{host}/{owner}/{repo}")
                } else {
                    format!("{owner}/{repo}")
                }
            }
            Some(Label::Local(base)) => format!("{base} (local)"),
            Some(Label::Plain(name)) => name.clone(),
            None => "other — type a project path…".to_string(),
        })
        .collect()
}

/// What one registry row reduces to before the host is decided.
enum Label {
    /// A git remote: owner and repo as the remote spells them, plus its host.
    Remote {
        /// Remote host, kept only for the disambiguating form.
        host: String,
        /// Repository owner.
        owner: String,
        /// Repository name.
        repo: String,
    },
    /// A checkout that exists only on this host; the directory's basename.
    Local(String),
    /// Neither a remote nor a path — the registry name is all there is.
    Plain(String),
}

/// Reduce one registry row to its [`Label`].
fn label_for(name: &str, repo: &str) -> Label {
    if let Ok(remote) = parse_remote_url(repo) {
        return Label::Remote {
            host: remote.host,
            owner: remote.owner,
            repo: remote.repo,
        };
    }
    let trimmed = repo.trim();
    if trimmed.is_empty() {
        return Label::Local(name.to_string());
    }
    if trimmed.starts_with('/') {
        let base = Path::new(trimmed)
            .file_name()
            .map(|b| b.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());
        return Label::Local(base);
    }
    Label::Plain(name.to_string())
}

/// True when another row spells this `owner/repo` on a DIFFERENT host.
///
/// The host is width the row can rarely afford, so it is added only when
/// leaving it out would make two rows read identically.
fn collides_across_hosts(labels: &[Option<Label>], host: &str, owner: &str, repo: &str) -> bool {
    labels.iter().flatten().any(|l| {
        matches!(
            l,
            Label::Remote {
                host: h,
                owner: o,
                repo: r,
            } if o.eq_ignore_ascii_case(owner)
                && r.eq_ignore_ascii_case(repo)
                && !h.eq_ignore_ascii_case(host)
        )
    })
}

/// Which target row the overlay should open on, given the list cursor (#7406).
///
/// Why: "start where I already am" is the whole of the owner's first
/// requirement, and the session under the cursor is the only thing that says
/// where that is.
/// What: the first target whose repo is the cursor session's — matched through
/// [`repo_url_matches`], so an `https` row and an `ssh` session agree, and
/// failing that against the session's `owner/repo` `source_id`. Falls back to
/// row 0 when there is no cursor session, or its project is not in the list.
/// Test: `new_session_preselects_the_cursor_sessions_project`,
/// `new_session_preselect_falls_back_to_the_first_row`.
pub(crate) fn preselect_index(
    targets: &[Target],
    session: Option<&ManagedSessionSummary>,
) -> usize {
    let Some(session) = session else { return 0 };
    targets
        .iter()
        .position(|t| is_session_project(t, session))
        .unwrap_or(0)
}

/// True when `target` is the project `session` runs in.
fn is_session_project(target: &Target, session: &ManagedSessionSummary) -> bool {
    let Target::Registered { repo, .. } = target else {
        return false;
    };
    repo_is_session_project(repo, session)
}

/// True when `session` runs in the project a registry row's `repo_url` names.
///
/// Why: #7421's ordering asks the same question [`preselect_index`] does — "is
/// this session's project THIS row" — so both route through one predicate
/// rather than each spelling the URL comparison out.
/// What: matches the session's `repo_url` through
/// [`repo_url_matches`] (so an `https` row and an `ssh`
/// session agree), and failing that the session's `owner/repo` `source_id`
/// against the row's parsed remote.
/// Test: `new_session_preselects_the_cursor_sessions_project`,
/// `new_session_order_puts_a_live_session_project_first`.
pub(crate) fn repo_is_session_project(repo: &str, session: &ManagedSessionSummary) -> bool {
    if let Some(url) = session.repo_url.as_deref()
        && !url.trim().is_empty()
        && repo_url_matches(url, repo)
    {
        return true;
    }
    let Some(source) = session.source_id.as_deref() else {
        return false;
    };
    parse_remote_url(repo).is_ok_and(|r| r.owner_repo().eq_ignore_ascii_case(source.trim()))
}

/// Perform a confirmed request against the real daemon.
///
/// Why: this is the only place the flow touches HTTP, and it routes both legs
/// through the entry points the CLI already has — no second registration and no
/// second spawn (#7395 requirement 2 and 3).
/// What: [`perform_with`] supplied with
/// [`crate::commands::projects::registry::register`] and
/// [`crate::commands::guided_launch::launch_new_session_and_attach`]. The
/// create call is the picker's launch-new call, unnamed and with the default
/// isolation, so the session lands exactly where `tm`'s own launch-new puts it.
/// Test: the ordering and the arguments are
/// `new_session_unregistered_path_registers_before_creating` and
/// `new_session_confirming_a_registered_project_creates_without_registering`,
/// both against [`perform_with`].
pub(crate) async fn perform(
    client: &reqwest::Client,
    url: &str,
    request: NewSessionRequest,
) -> anyhow::Result<AttachOutcome> {
    perform_with(
        request,
        |project: NewProject| async move {
            crate::commands::projects::registry::register(
                client,
                url,
                RegisterInput {
                    name: project.name,
                    repo_url: project.repo_url,
                    default_branch: None,
                    description: None,
                    tags: Vec::new(),
                    stack_hint: None,
                    gh_user: None,
                    gh_account: None,
                    gh_config_dir: None,
                },
            )
            .await
        },
        |repo: String| async move {
            crate::commands::guided_launch::launch_new_session_and_attach(
                client,
                url,
                &repo,
                None,
                LaunchIsolation::default(),
            )
            .await
        },
    )
    .await
}

/// Order the two legs of a confirmed request: register first, then create.
///
/// Why (#7395 requirement 2): a session cannot be created in a project the
/// daemon has never heard of, so the registration is not optional cleanup that
/// can follow — it must land first, and a failure there must stop the create
/// rather than leaving a session pointing at an unregistered project. Taking
/// both legs as arguments is what makes that ordering assertable with fakes.
/// What: runs `register` only when the request carries one, then `create` with
/// the request's `repo`. Returns whatever the create call resolved, which the
/// caller reads for the #2678 terminal hand-off.
/// Test: `new_session_unregistered_path_registers_before_creating`,
/// `new_session_confirming_a_registered_project_creates_without_registering`,
/// `new_session_a_failed_registration_never_creates`.
pub(crate) async fn perform_with<R, RFut, C, CFut>(
    request: NewSessionRequest,
    register: R,
    create: C,
) -> anyhow::Result<AttachOutcome>
where
    R: FnOnce(NewProject) -> RFut,
    RFut: Future<Output = anyhow::Result<()>>,
    C: FnOnce(String) -> CFut,
    CFut: Future<Output = anyhow::Result<AttachOutcome>>,
{
    let NewSessionRequest {
        register: to_register,
        repo,
        label,
    } = request;
    if let Some(project) = to_register {
        register(project)
            .await
            .with_context(|| format!("could not register project '{label}'"))?;
    }
    create(repo).await
}
