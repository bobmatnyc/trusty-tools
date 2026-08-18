//! The one place an operator sees what the audit will cover (#5978).
//!
//! Why: the counts are the headline. "17 repositories, 2 boards" catches a
//! truncated `repos.txt` at a glance where a scrolling list does not — the
//! operator knows what they expected and reads one number against it. So the
//! review states counts first and lists targets only when asked to.
//!
//! It is ONE surface reached two ways: the operator who supplied a targets file
//! and the operator who typed targets at the prompt loop both end here, and both
//! can add, delete, and proceed from the same menu. A menu for one path and a
//! bare yes/no for the other would be two things to keep in step.
//!
//! What: [`review`] is the menu. Add and delete go through
//! [`Command::AddTarget`] and [`Command::RemoveTarget`], so a target added here
//! is declared in `engagement.toml` exactly as `taudit add` declares one, and a
//! deleted one is removed from that file rather than from a list that lives for
//! this run only (#5979). [`summary`] and [`listing`] are the text, shared with
//! the no-terminal path that prints them and proceeds.
//!
//! The menu choice is read through [`super::registration::read_deliberately`],
//! which discards typeahead first. A queued keystroke answering a yes/no starts
//! a sweep the operator did not read; a queued keystroke answering a MENU can
//! select `delete`, which is worse.
//!
//! Test: `super::review_tests`.

use crate::cli::credential::Tty;
use crate::cli::registration::{read_deliberately, register_one, say};
use crate::cli::render::count_of;
use crate::error::AuditError;
use crate::registry::{Target, TargetKind};
use crate::session::{Command, Outcome, Session};

/// The menu itself. Every option is one keystroke and Enter.
const MENU: &str = "[a] add a target   [d] delete a target   [p] proceed: ";

/// Shown once, above the first summary.
const OPENING: &str = "\nThis audit will cover:";

/// What an unrecognised choice is told.
const UNRECOGNISED: &str = "Type a to add, d to delete, or p to proceed.";

/// Asked when the operator chose to add.
const ADD_PROMPT: &str = "Add which target? (owner/repo, jira:KEY or linear:TEAM): ";

/// Asked when the operator chose to delete.
const DELETE_PROMPT: &str = "Delete which target? (its name exactly as listed above): ";

/// Show the counts, then take additions and deletions until the operator
/// proceeds.
///
/// Why: nothing is cloned or collected before the operator leaves this menu, so
/// this is where a wrong target set is still cheap to fix. It returns the set
/// rather than acting on it, because the caller — the guided launch — owns what
/// happens next.
/// What: the summary before every prompt, so a count the operator just changed
/// is the count they read. End of input proceeds: a terminal that closed cannot
/// keep answering, and the sweep's own confirmation is still ahead of it
/// (`super::registration::confirmed`).
/// Test: `super::review_tests::an_added_target_is_declared_in_the_config`,
/// `super::review_tests::a_deleted_target_leaves_the_config`.
///
/// # Errors
///
/// [`AuditError::RegistrationPromptFailed`] when the terminal fails. A refused
/// add is reported and the menu returns, exactly as the prompt loop does.
pub async fn review(session: &Session, tty: &mut dyn Tty) -> Result<Vec<Target>, AuditError> {
    say(tty, OPENING)?;
    loop {
        let targets = registered(session).await?;
        say(tty, &summary(&targets))?;

        let Some(choice) = read_deliberately(tty, MENU)? else {
            return Ok(targets);
        };
        match choice.trim().to_ascii_lowercase().chars().next() {
            Some('p') => return Ok(targets),
            Some('a') => add(session, tty).await?,
            Some('d') => delete(session, tty, &targets).await?,
            _ => say(tty, UNRECOGNISED)?,
        }
    }
}

/// The headline: how many repositories to clone, how many boards to collect
/// from.
///
/// Both counts are always stated, zero included — "0 boards to collect from" is
/// the line that tells an operator their `boards.txt` was never read.
pub fn summary(targets: &[Target]) -> String {
    let repos = targets
        .iter()
        .filter(|t| t.kind() == TargetKind::Repo)
        .count();
    format!(
        "{} to clone, {} to collect from.",
        count_of(repos, "repository", "repositories"),
        count_of(targets.len() - repos, "board", "boards")
    )
}

/// Every target, one per line, for the paths that print rather than prompt.
pub fn listing(targets: &[Target]) -> String {
    targets
        .iter()
        .map(|target| format!("  {target}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the engagement declares right now, read through the one dispatch door.
///
/// The non-`Targets` arm is unreachable — `Command::ListTargets` produces
/// `Outcome::Targets` and nothing else — but it is an error rather than a
/// `panic!` or an empty list. An empty list here would report "0 repositories to
/// clone" over a set that is actually populated, which is the fail-open this
/// whole feature exists to rule out.
pub(super) async fn registered(session: &Session) -> Result<Vec<Target>, AuditError> {
    match session.execute(Command::ListTargets).await? {
        Outcome::Targets(list) => Ok(list.targets),
        other => Err(AuditError::Render {
            what: "the registered targets",
            source: Box::new(std::io::Error::other(format!(
                "ListTargets produced {other:?}"
            ))),
        }),
    }
}

/// Register one more target, through the path `taudit add` takes.
async fn add(session: &Session, tty: &mut dyn Tty) -> Result<(), AuditError> {
    let Some(spec) = tty
        .read_line(ADD_PROMPT)
        .map_err(super::registration::prompt_failed)?
    else {
        return Ok(());
    };
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(());
    }
    match register_one(session, spec).await {
        Ok(target) => say(tty, &format!("added: {target}")),
        Err(refusal) => say(tty, &format!("not added: {refusal}")),
    }
}

/// Drop one target from `engagement.toml`, through the path `taudit remove`
/// takes.
///
/// The list is shown first: an operator deleting from seventeen repositories
/// needs to see the spelling they are about to type, and the menu deliberately
/// does not print it every time.
async fn delete(
    session: &Session,
    tty: &mut dyn Tty,
    targets: &[Target],
) -> Result<(), AuditError> {
    if targets.is_empty() {
        return say(tty, "Nothing to delete.");
    }
    say(tty, &listing(targets))?;
    let Some(spec) = tty
        .read_line(DELETE_PROMPT)
        .map_err(super::registration::prompt_failed)?
    else {
        return Ok(());
    };
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(());
    }
    match session
        .execute(Command::RemoveTarget {
            spec: spec.to_owned(),
        })
        .await
    {
        Ok(Outcome::Removed(removal)) if removal.was_registered => {
            say(tty, &format!("deleted: {}", removal.target))
        }
        Ok(_) => say(tty, &format!("{spec} is not registered — nothing changed.")),
        Err(refusal) => say(tty, &format!("not deleted: {refusal}")),
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::cli::registration::registration_tests::{
        FakeTty, accepting_session, registered_ids,
    };

    /// The counts are the headline, and both are always stated — a missing
    /// "0 boards" line is how a `boards.txt` that was never read goes unnoticed.
    #[test]
    fn the_summary_states_both_counts_including_zero() {
        let repo = crate::registry::parse(None, "acme/api").expect("parses");
        let board = crate::registry::parse(None, "linear:ENG").expect("parses");

        assert_eq!(
            summary(std::slice::from_ref(&repo)),
            "1 repository to clone, 0 boards to collect from."
        );
        assert_eq!(
            summary(&[repo.clone(), repo.clone(), board]),
            "2 repositories to clone, 1 board to collect from."
        );
        assert_eq!(
            summary(&[]),
            "0 repositories to clone, 0 boards to collect from."
        );
    }

    /// Proceeding is what leaves the menu, and it changes nothing.
    #[tokio::test]
    async fn proceeding_returns_what_is_registered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        register_one(&session, "acme/api").await.expect("registers");

        let mut tty = FakeTty::new(["p"]);
        let targets = review(&session, &mut tty).await.expect("the menu runs");

        assert_eq!(targets.len(), 1);
        assert_eq!(registered_ids(&session), vec!["acme/api"]);
        assert!(
            tty.shown
                .iter()
                .any(|l| l.contains("1 repository to clone")),
            "the counts must be the headline: {:?}",
            tty.shown
        );
    }

    /// 🔴 Add writes THROUGH to `engagement.toml`, which #5979 made the
    /// authoritative target store — not to a list that lives for this run.
    #[tokio::test]
    async fn an_added_target_is_declared_in_the_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new(["a", "acme/web", "p"]);
        let targets = review(&session, &mut tty).await.expect("the menu runs");

        assert_eq!(
            targets.iter().map(Target::id).collect::<Vec<_>>(),
            ["acme/web"]
        );
        let config = crate::config::EngagementConfig::load(session.config_path()).expect("loads");
        assert_eq!(
            config
                .declared_targets()
                .expect("the add must declare a target set")
                .iter()
                .map(Target::id)
                .collect::<Vec<_>>(),
            ["acme/web"]
        );
    }

    /// 🔴 Delete removes the target from `engagement.toml` too, so the next run
    /// does not clone what the operator just took off the list.
    #[tokio::test]
    async fn a_deleted_target_leaves_the_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        register_one(&session, "acme/api").await.expect("registers");
        register_one(&session, "acme/web").await.expect("registers");

        let mut tty = FakeTty::new(["d", "acme/api", "p"]);
        let targets = review(&session, &mut tty).await.expect("the menu runs");

        assert_eq!(
            targets.iter().map(Target::id).collect::<Vec<_>>(),
            ["acme/web"]
        );
        let config = crate::config::EngagementConfig::load(session.config_path()).expect("loads");
        assert_eq!(
            config
                .declared_targets()
                .expect("declared")
                .iter()
                .map(Target::id)
                .collect::<Vec<_>>(),
            ["acme/web"],
            "the deletion must reach the config, not just this run's list"
        );
        assert_eq!(registered_ids(&session), vec!["acme/web"]);
    }

    /// The counts are re-read after every change, so the number the operator
    /// checks is the number they just produced.
    #[tokio::test]
    async fn the_counts_are_restated_after_every_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new(["a", "acme/api", "a", "acme/web", "p"]);
        review(&session, &mut tty).await.expect("the menu runs");

        let counts: Vec<&String> = tty
            .shown
            .iter()
            .filter(|l| l.contains("to clone"))
            .collect();
        assert_eq!(
            counts.len(),
            3,
            "one summary per menu turn: {:?}",
            tty.shown
        );
        assert!(counts[0].starts_with("0 repositories"), "{}", counts[0]);
        assert!(counts[1].starts_with("1 repository"), "{}", counts[1]);
        assert!(counts[2].starts_with("2 repositories"), "{}", counts[2]);
    }

    /// 🔴 The typeahead hazard, one step worse than the sweep prompt's. A
    /// keystroke queued while the file was being parsed must not select an
    /// option — and the option it could select here is `delete`.
    #[tokio::test]
    async fn the_menu_discards_typeahead_before_every_choice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new(["x", "p"]);
        review(&session, &mut tty).await.expect("the menu runs");

        assert_eq!(
            tty.discarded_after,
            vec![0, 1],
            "each menu choice must be read after a discard, never before one: {:?}",
            tty.prompts
        );
    }

    /// An unrecognised choice re-asks rather than guessing at what was meant.
    #[tokio::test]
    async fn an_unrecognised_choice_re_asks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new(["zzz", "p"]);
        review(&session, &mut tty).await.expect("the menu runs");

        assert!(
            tty.shown.iter().any(|l| l == UNRECOGNISED),
            "{:?}",
            tty.shown
        );
        assert_eq!(tty.prompts.iter().filter(|p| *p == MENU).count(), 2);
    }

    /// A refused add is reported and the menu returns — the same rule the
    /// prompt loop holds, for the same reason.
    #[tokio::test]
    async fn a_refused_add_keeps_the_menu() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new(["a", "not a target", "p"]);
        review(&session, &mut tty).await.expect("the menu runs");

        assert!(
            tty.shown.iter().any(|l| l.starts_with("not added:")),
            "{:?}",
            tty.shown
        );
        assert!(registered_ids(&session).is_empty());
    }

    /// Deleting something that is not registered says so rather than reporting
    /// a deletion that did not happen.
    #[tokio::test]
    async fn deleting_an_unregistered_target_changes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());
        register_one(&session, "acme/api").await.expect("registers");

        let mut tty = FakeTty::new(["d", "acme/nope", "p"]);
        review(&session, &mut tty).await.expect("the menu runs");

        assert_eq!(registered_ids(&session), vec!["acme/api"]);
        assert!(
            tty.shown.iter().any(|l| l.contains("not registered")),
            "{:?}",
            tty.shown
        );
    }

    /// End of input leaves the menu rather than spinning on empty reads. The
    /// sweep's own confirmation is still ahead, and it declines on end of input.
    #[tokio::test]
    async fn end_of_input_leaves_the_menu() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = accepting_session(tmp.path());

        let mut tty = FakeTty::new([]);
        let targets = review(&session, &mut tty)
            .await
            .expect("end of input is not a failure");
        assert!(targets.is_empty());
    }
}
