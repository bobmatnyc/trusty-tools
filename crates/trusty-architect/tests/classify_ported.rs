//! #9171 A2: the prototype's classify tests that pass against the classifier
//! inline in the live `fleet-poll.py`, ported by name against
//! `trusty_architect::classify`.
//!
//! Sources: `tests/test_fleet_poll.py`, plus the parse halves of
//! `scripts/test_fleet_poll_idle.py`, `scripts/test_fleet_poll_rc.py` and
//! `scripts/test_fleet_poll_ctx.py`. The live classifier is `fleet-poll.py`'s
//! inline copy, not `fleet-classify.py`. Seven prototype tests still encode
//! `fleet-classify.py`'s ctx bar (45/50) and fail against `fleet-poll.py`;
//! #9171 A3a ports them by name (each marked `// Python: <name>`) with the
//! same inputs and `fleet-poll.py`'s actual result.
//!
//! Captures are built by `screen()`, a line-for-line port of the Python
//! helper. Tests that exercise `fleet-poll.py`'s I/O (wake, dispatch, cycle,
//! events, host load, band, rc-restore runs) are A3 work and are not here; the
//! wake tests' classifier halves are, against `input_state_for_send`.

use std::path::Path;

use trusty_architect::classify::{
    Alert, BoxReading, CTX_HIGH, CTX_WARN, DedupState, PaneState, alert, dedup, evaluate,
    fleet_plan_alerts, parse_pane,
};
use trusty_architect::{InputState, classify_input, input_state_for_send};

const NBSP: char = '\u{a0}';
const DIM: &str = "\u{1b}[2m";
const RESET: &str = "\u{1b}[0m";

fn rule() -> String {
    "─".repeat(80)
}

#[derive(Default)]
struct Screen<'a> {
    box_text: &'a str,
    dim: bool,
    agents: &'a [&'a str],
    ctx: Option<u32>,
    plan: Option<(u32, u32)>,
    status: Option<&'a str>,
}

/// A pane capture: transcript, optional status line, input box, footer.
fn screen(body: &[&str], s: Screen<'_>) -> String {
    let boxed = if s.dim {
        format!("{DIM}{}{RESET}", s.box_text)
    } else {
        s.box_text.to_owned()
    };
    let (p5, pw) = s.plan.unwrap_or((30, 40));
    let mut lines: Vec<String> = body.iter().map(|l| (*l).to_owned()).collect();
    lines.extend(s.status.map(str::to_owned));
    lines.extend([rule(), format!("❯{NBSP}{boxed}"), rule()]);
    lines.push(format!(
        "  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx {}% | $1.00 | ⏳{p5}% 📅{pw}% | 💸1%/1%",
        s.ctx.unwrap_or(30)
    ));
    lines.push("  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents".to_owned());
    lines.push(format!("{}/rc", " ".repeat(70)));
    lines.extend([String::new(), "  ⏺ main".to_owned()]);
    for a in s.agents {
        lines.push(format!("  ◯ {a}  Doing things…  3m 2s · ↓ 12.0k tokens"));
    }
    lines.join("\n") + "\n\n"
}

const PM_TURN: [&str; 3] = [
    "⏺ Merged #12 and the CI run is green.",
    "",
    "  Next: the release notes.",
];
const PM_ASKS: [&str; 6] = [
    "⏺ Two options for the cache.",
    "",
    "  - A: keep it",
    "  - B: drop it",
    "",
    "  Which do you want, A or B?",
];

fn permission() -> String {
    let r = rule();
    [
        "⏺ Bash(rm -rf build/)",
        "",
        &r,
        " Bash command · from the local-ops agent",
        "",
        "   rm -rf build/",
        "   Remove the build dir",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and don't ask again for rm commands in this project",
        "   3. No, and tell Claude what to do differently (esc)",
        "",
        " Esc to cancel",
    ]
    .join("\n")
        + "\n"
}

fn ask_user() -> String {
    let r = rule();
    [
        "⏺ I need a decision.",
        &r,
        " ☐ Cache  ✔ Submit  →",
        "",
        "Keep the cache?",
        "",
        "❯ 1. Keep (Recommended)",
        "     Faster builds",
        "  2. Drop",
        "  3. Type something.",
        &r,
        "  4. Chat about this",
        "",
        "Enter to select · ↑/↓ to navigate · Esc to cancel",
    ]
    .join("\n")
        + "\n"
}

fn kinds(alerts: &[Alert]) -> Vec<&str> {
    let mut k: Vec<&str> = alerts.iter().map(|a| a.kind.as_str()).collect();
    k.sort_unstable();
    k
}

fn run(
    capture: &str,
    ps: &mut PaneState,
    now: f64,
) -> (trusty_architect::classify::PaneInfo, Vec<Alert>) {
    let info = parse_pane(capture);
    let alerts = evaluate(&info, ps, now, "tm-x", "%1");
    (info, alerts)
}

fn agents_x() -> Screen<'static> {
    Screen {
        agents: &["x"],
        ..Screen::default()
    }
}

// --- input-box classifier shared with input-state.py ------------------------

#[test]
fn test_input_box_classifier_is_input_states() {
    let cases = [
        ("", false, InputState::Empty),
        ("merge PR #7 once CI is green", true, InputState::Suggestion),
        ("half-typed reply", false, InputState::Typed),
    ];
    for (box_text, dim, expected) in cases {
        let cap = screen(
            &PM_TURN,
            Screen {
                box_text,
                dim,
                ..Screen::default()
            },
        );
        assert_eq!(classify_input(&cap).0, expected, "{box_text:?}");
        assert_eq!(parse_pane(&cap).box_reading, BoxReading::Input(expected));
    }
}

// --- dialog ------------------------------------------------------------------

#[test]
fn test_permission_dialog_detected_and_fingerprint_ignores_cursor() {
    let (info, alerts) = run(&permission(), &mut PaneState::default(), 10_000.0);
    assert!(info.dialog.is_some());
    assert_eq!(kinds(&alerts), ["dialog"]); // a dialog pane is never idle or a question
    assert!(alerts[0].detail.contains("Do you want to proceed?"));
    let moved = permission()
        .replace(" ❯ 1. Yes", "   1. Yes")
        .replace("   2. Yes,", " ❯ 2. Yes,");
    let again = evaluate(
        &parse_pane(&moved),
        &mut PaneState::default(),
        0.0,
        "tm-x",
        "%1",
    );
    assert_eq!(again[0].fingerprint, alerts[0].fingerprint);
}

#[test]
fn test_ask_user_question_box_is_a_dialog() {
    let (_, alerts) = run(&ask_user(), &mut PaneState::default(), 10_000.0);
    assert_eq!(kinds(&alerts), ["dialog"]);
    assert!(alerts[0].detail.contains("Keep the cache?"));
}

#[test]
fn test_dialog_words_in_transcript_above_the_box_are_not_a_dialog() {
    let body = [
        "⏺ The prompt said:",
        "  Do you want to proceed?",
        "  ❯ 1. Yes",
    ];
    let cap = screen(
        &body,
        Screen {
            agents: &["engineer"],
            ..Screen::default()
        },
    );
    assert_eq!(parse_pane(&cap).dialog, None);
}

// --- box detection against a real capture with a labeled top rule -----------

#[test]
fn test_labeled_top_rule_is_still_recognized_as_the_box() -> std::io::Result<()> {
    // The scrubbed copy of the prototype's tests/fixtures capture (see README).
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/idle-pane-labeled-rule.txt");
    let info = parse_pane(&std::fs::read_to_string(path)?);
    assert!(info.has_box);
    assert_eq!(info.box_reading, BoxReading::Input(InputState::Suggestion));
    assert_eq!(info.dialog, None);
    Ok(())
}

#[test]
fn test_a_dialogs_option_lines_are_never_mistaken_for_the_box() {
    let (info, alerts) = run(&permission(), &mut PaneState::default(), 10_000.0);
    assert!(info.dialog.is_some() && !info.has_box);
    assert_eq!(kinds(&alerts), ["dialog"]);
}

// --- idle / busy / question --------------------------------------------------
// #9171: screen() writes `ctx 30%`, over fleet-poll.py's CTX_WARN of 22, so
// every idle pane below also raises a `ctx` alert. The prototype tests these
// replace expect none (fleet-classify.py's 45/50 bar) and fail against
// fleet-poll.py; the ports below keep their names and inputs and assert what
// fleet-poll.py returns (#9171 A3a reconcile list).

// Python: test_idle_prompt_alerts_after_ten_minutes_only
#[test]
fn test_idle_prompt_alerts_after_ten_minutes_only() {
    let cap = screen(
        &PM_TURN,
        Screen {
            status: Some("✻ Worked for 3m 2s"),
            ..Screen::default()
        },
    );
    let mut ps = PaneState::default();
    let (info, alerts) = run(&cap, &mut ps, 1000.0);
    assert!(!info.busy && !info.turn_active);
    assert_eq!(kinds(&alerts), ["ctx"]);
    assert_eq!(ps.idle_since, Some(1000.0));
    // Literal ten minutes (the prototype reads fp.IDLE_AFTER), so a mutated
    // IDLE_AFTER turns this test red.
    assert_eq!(kinds(&run(&cap, &mut ps, 1000.0 + 599.0).1), ["ctx"]);
    assert_eq!(
        kinds(&run(&cap, &mut ps, 1000.0 + 600.0).1),
        ["ctx", "idle"]
    );
}

// Python: test_suggestion_box_counts_as_idle
#[test]
fn test_suggestion_box_counts_as_idle() {
    let cap = screen(
        &PM_TURN,
        Screen {
            box_text: "do the next thing",
            dim: true,
            ..Screen::default()
        },
    );
    let mut ps = PaneState::default();
    run(&cap, &mut ps, 0.0);
    assert_eq!(kinds(&run(&cap, &mut ps, 600.0).1), ["ctx", "idle"]);
}

// Python: test_busy_pane_is_neither_idle_nor_question
#[test]
fn test_busy_pane_is_neither_idle_nor_question() {
    let cases: [(&[&str], &str); 3] = [
        (
            &["python-engineer", "research"],
            "✻ Waiting for 2 background agents to finish",
        ),
        (&[], "✳ Undulating… (5m 23s · ↓ 19.1k tokens)"),
        (&[], "✻ Waiting for 1 background agent to finish"),
    ];
    for (agents, status) in cases {
        let cap = screen(
            &PM_ASKS,
            Screen {
                agents,
                status: Some(status),
                ..Screen::default()
            },
        );
        let mut ps = PaneState::default();
        let (info, alerts) = run(&cap, &mut ps, 0.0);
        assert!(info.busy && info.turn_active, "{status}");
        assert_eq!(kinds(&alerts), ["ctx"], "{status}");
        assert_eq!(ps.idle_since, None, "{status}");
    }
}

#[test]
fn test_waiting_line_anywhere_in_the_capture_is_busy() {
    // 87ace19: scan the whole capture, so a capture without a bottom rule is not read idle.
    let body = [
        &["✻ Waiting for 1 background agent to finish"][..],
        &PM_TURN,
    ]
    .concat();
    assert!(parse_pane(&screen(&body, Screen::default())).busy);
}

// Python: test_question_is_last_pm_line_ending_in_question_mark
#[test]
fn test_question_is_last_pm_line_ending_in_question_mark() {
    let status = Some("✻ Cooked for 23s · done 7:36 AM");
    let (_, alerts) = run(
        &screen(
            &PM_ASKS,
            Screen {
                status,
                ..Screen::default()
            },
        ),
        &mut PaneState::default(),
        0.0,
    );
    assert_eq!(kinds(&alerts), ["ctx", "question"]);
    let question = alerts.iter().find(|a| a.kind == "question");
    assert_eq!(
        question.map(|a| a.detail.as_str()),
        Some("Which do you want, A or B?")
    );
    let user_turn = [&PM_TURN[..], &["", "❯ can you check the logs?"]].concat();
    assert_eq!(
        parse_pane(&screen(&user_turn, Screen::default())).question,
        None
    );
}

// --- ctx / plan-limit / error ------------------------------------------------

/// The `ctx` alert fingerprints a busy pane at `ctx` percent raises.
fn ctx_fingerprints(ctx: u32) -> Vec<String> {
    let cap = screen(
        &PM_TURN,
        Screen {
            ctx: Some(ctx),
            ..agents_x()
        },
    );
    let alerts = run(&cap, &mut PaneState::default(), 0.0).1;
    assert!(alerts.iter().all(|a| a.kind == "ctx"), "{alerts:?}");
    alerts.into_iter().map(|a| a.fingerprint).collect()
}

/// At fleet-poll.py's 22/25 bar. Literal percentages, so a mutated
/// `CTX_WARN` or `CTX_HIGH` turns it red.
// Python: test_ctx_47_alerts_and_crossing_50_changes_fingerprint
#[test]
fn test_ctx_47_alerts_and_crossing_50_changes_fingerprint() {
    assert!(ctx_fingerprints(21).is_empty());
    assert_eq!(ctx_fingerprints(22), ["22"]);
    assert_eq!(ctx_fingerprints(24), ["22"]);
    assert_eq!(ctx_fingerprints(25), ["25"]);
    // The prototype test's own values: 47 and 52 both read as the high bar.
    assert_eq!(ctx_fingerprints(47), ["25"]);
    assert_eq!(ctx_fingerprints(52), ["25"]);
    assert_eq!(ctx_fingerprints(44), ["25"]);
}

/// `scripts/test_fleet_poll_ctx.py`, both halves since #9171 A3a.
#[test]
fn test_ctx_thresholds_follow_ruling_b7() {
    assert_eq!((CTX_WARN, CTX_HIGH), (22, 25));
    assert_eq!(trusty_architect::poller::SELF_CTX_THRESHOLD_DEFAULT, 25);
}

// Python: test_plan_limit_text_and_status_line
#[test]
fn test_plan_limit_text_and_status_line() {
    let body = [
        &PM_TURN[..],
        &["  ⎿  Claude usage limit reached. Your limit will reset at 5pm."],
    ]
    .concat();
    let (_, alerts) = run(&screen(&body, agents_x()), &mut PaneState::default(), 0.0);
    assert_eq!(kinds(&alerts), ["ctx", "plan-limit"]);
    let prose = ["⏺ Both agents died at the account's usage limit."];
    let (_, alerts) = run(&screen(&prose, agents_x()), &mut PaneState::default(), 0.0);
    assert_eq!(kinds(&alerts), ["ctx"]);
    let infos = [
        (
            "tm-a".to_owned(),
            parse_pane(&screen(
                &PM_TURN,
                Screen {
                    plan: Some((91, 50)),
                    ..Screen::default()
                },
            )),
        ),
        (
            "tm-b".to_owned(),
            parse_pane(&screen(
                &PM_TURN,
                Screen {
                    plan: Some((88, 50)),
                    ..Screen::default()
                },
            )),
        ),
    ];
    let plan = fleet_plan_alerts(&infos, 0.0);
    assert!(plan.len() == 1 && plan[0].session.is_empty() && plan[0].detail.contains("91%"));
    assert!(!plan[0].display.starts_with('['));
}

// Python: test_error_alerts_only_for_lines_new_since_last_cycle
#[test]
fn test_error_alerts_only_for_lines_new_since_last_cycle() {
    let old = [&PM_TURN[..], &["  ⎿  Error: Exit code 1"]].concat();
    let mut ps = PaneState::default();
    // First sight seeds the error set: only the ctx alert.
    assert_eq!(
        kinds(&run(&screen(&old, agents_x()), &mut ps, 0.0).1),
        ["ctx"]
    );
    let new = [&old[..], &["⏺ API Error: 529 overloaded"]].concat();
    let alerts = run(&screen(&new, agents_x()), &mut ps, 0.0).1;
    assert_eq!(kinds(&alerts), ["ctx", "error"]);
    let error = alerts.iter().find(|a| a.kind == "error");
    assert!(error.is_some_and(|a| a.detail.contains("API Error")));
    assert_eq!(
        kinds(&run(&screen(&new, agents_x()), &mut ps, 0.0).1),
        ["ctx"]
    );
}

// --- scripts/test_fleet_poll_idle.py: agents anywhere, current_waiting -------

fn r40() -> String {
    "─".repeat(40)
}

const ROWS: &str = "  ◯ version-control  Polling smoke-prod gate        6m 34s · ↓ 52.9k tokens\n  ◯ code-critic      Reading English bundle         45s · ↓ 124.7k tokens\n";

fn idle_alerts(capture: &str, ps: &mut PaneState) -> Vec<Alert> {
    let info = parse_pane(capture);
    let mut alerts = evaluate(&info, ps, 10_000.0, "s", "p");
    alerts.retain(|a| a.kind == "idle");
    alerts
}

fn idle_since_zero() -> PaneState {
    PaneState {
        idle_since: Some(0.0),
        ..PaneState::default()
    }
}

#[test]
fn test_agent_rows_are_not_idle_and_reset_timer() {
    // Box with no bottom rule: the footer slice is empty, so only a whole-capture scan sees the rows.
    let r = r40();
    let with_rows = format!(
        "⏺ Done.\n\n{r}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 2 agents\n  ⏺ main\n{ROWS}"
    );
    let mut ps = idle_since_zero();
    assert!(idle_alerts(&with_rows, &mut ps).is_empty());
    assert_eq!(ps.idle_since, None);
}

#[test]
fn test_waiting_line_is_not_idle() {
    let r = r40();
    let with_waiting =
        format!("⏺ x\n✻ Waiting for 3 background agents to finish\n\n{r}\n❯ \n  ⏵⏵ bypass\n");
    let mut ps = idle_since_zero();
    assert!(idle_alerts(&with_waiting, &mut ps).is_empty());
    assert_eq!(ps.idle_since, None);
}

#[test]
fn test_no_agents_still_idle() {
    let r = r40();
    let without = format!("⏺ Done.\n\n{r}\n❯ \n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n");
    let alerts = idle_alerts(&without, &mut idle_since_zero());
    assert!(alerts.len() == 1 && alerts[0].detail.contains("no agents running"));
}

fn stale() -> String {
    "✻ Waiting for 1 background agent to finish\n\n".repeat(3)
}

#[test]
fn test_stale_waiting_lines_are_not_busy() {
    let r = r40();
    let stale_then_done = format!(
        "{}✻ Brewed for 42s · done 1:13 AM\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n",
        stale()
    );
    let info = parse_pane(&stale_then_done);
    assert!(!info.busy && !info.turn_active);
}

#[test]
fn test_newest_waiting_line_is_busy() {
    let r = r40();
    let newest_waiting = format!(
        "✻ Brewed for 42s · done 1:13 AM\n\n✻ Waiting for 1 background agent to finish\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n"
    );
    let info = parse_pane(&newest_waiting);
    assert!(info.busy && info.turn_active);
}

// --- scripts/test_fleet_poll_rc.py: the parse_pane halves -------------------
// rc_step / architect_rc_step (rc-restore runs, debounce, fan-out) are A3.

fn rc_base() -> String {
    let r = r40();
    format!("⏺ Done.\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)")
}

#[test]
fn test_rc_marker_detected() {
    let info = |footer: &str| parse_pane(&format!("{} · {footer}\n", rc_base()));
    assert!(info("/rc failed").rc_failed);
    assert!(!info("/rc").rc_failed);
}

#[test]
fn test_account_field_parsed_and_truncation_ignored() {
    let acct = |account: &str| {
        parse_pane(&format!(
            "{} · TM 1.7.12 ● | architect | @x | ✻{account} | Opus 5.5 | ctx 3%\n",
            rc_base()
        ))
        .account
    };
    assert_eq!(acct("a@x.com").as_deref(), Some("a@x.com"));
    let cut = parse_pane(&format!("{} · TM 1 ● | p | ✻a@x.co…\n", rc_base()));
    assert_eq!(cut.account, None);
}

// --- dedup / state -----------------------------------------------------------

fn a(kind: &str, f: &str) -> Alert {
    alert(kind, f, "d", "tm-x", "%1", 0.0)
}

#[test]
fn test_dedup_suppresses_until_cleared_for_one_cycle() {
    let mut st = DedupState::default();
    assert_eq!(dedup(&mut st, &[a("dialog", "f1")], 0.0).0.len(), 1);
    assert_eq!(dedup(&mut st, &[a("dialog", "f1")], 60.0), (vec![], 1));
    dedup(&mut st, &[], 120.0); // cleared for one cycle
    assert_eq!(dedup(&mut st, &[a("dialog", "f1")], 180.0).0.len(), 1);
    assert_eq!(dedup(&mut st, &[a("dialog", "f2")], 240.0).0.len(), 1); // new fingerprint
}

#[test]
fn test_dedup_reemits_idle_every_30_minutes_but_not_dialog() {
    let mut st = DedupState::default();
    let both = [a("idle", "f1"), a("dialog", "f1")];
    dedup(&mut st, &both, 0.0);
    assert_eq!(dedup(&mut st, &both, 1799.0), (vec![], 2));
    let (new, sup) = dedup(&mut st, &both, 1800.0);
    assert_eq!((kinds(&new), sup), (vec!["idle"], 1));
    let seen = st.get("tm-x|%1|idle|f1").map(|s| (s.first, s.last));
    assert_eq!(seen, Some((0.0, 1800.0)));
}

#[test]
fn test_alert_display_and_redaction() {
    let x = alert(
        "dialog",
        "f",
        "export API_KEY=abc123 then run",
        "tm-api",
        "%3",
        0.0,
    );
    assert!(x.display.starts_with("[tm-api]: dialog: "));
    assert!(!x.detail.contains("abc123"));
    let load = alert("load", "load", "load 20.0 > 16 cores", "", "", 0.0);
    assert_eq!(load.display, "load: load 20.0 > 16 cores");
}

// --- wake(): the classifier half, against the send_guarded producer ----------
// The prototype's wake() skips on a dialog, on no box, and on any box state but
// empty/suggestion. input_state_for_send folds those skips into one reading
// that fails closed; the send/confirm/cleanup half of wake() is A3.

/// A box whose `text` is split across two rows with no rule between them —
/// the shape a narrow pane produces, which a single-line read never sees whole.
fn wrapped_box_screen(body: &[&str], text: &str) -> String {
    let half = text.chars().count() / 2;
    let (head, tail): (String, String) = (
        text.chars().take(half).collect(),
        text.chars().skip(half).collect(),
    );
    let mut lines: Vec<String> = body.iter().map(|l| (*l).to_owned()).collect();
    lines.extend([rule(), format!("❯{NBSP}{head}"), tail, rule()]);
    lines.join("\n") + "\n"
}

#[test]
fn test_wake_reports_no_input_box_found_not_a_misleading_box_state() {
    let cap = "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n";
    assert!(!parse_pane(cap).has_box); // the actual cause
    assert_eq!(
        classify_input(cap).0,
        InputState::Empty,
        "the misleading single-line guess"
    );
    assert_eq!(input_state_for_send(cap), InputState::NoPrompt);
}

#[test]
fn test_wake_reports_input_typed_when_a_real_box_holds_a_draft() {
    let cap = screen(
        &PM_TURN,
        Screen {
            box_text: "half-typed reply",
            ..Screen::default()
        },
    );
    assert!(parse_pane(&cap).has_box);
    assert_eq!(input_state_for_send(&cap), InputState::Typed);
}

#[test]
fn test_wake_confirms_a_message_that_wraps_across_screen_rows() {
    // Classifier half: the pre-send box is empty and accepts input; the same
    // box holding the just-typed text wrapped across rows reads typed.
    let pre = screen(&PM_TURN, Screen::default());
    assert_eq!(input_state_for_send(&pre), InputState::Empty);
    let msg = "[poll 11:02Z] 3 new alerts -> /Users/x/inbox/alerts.jsonl";
    assert_eq!(
        input_state_for_send(&wrapped_box_screen(&PM_TURN, msg)),
        InputState::Typed
    );
}

#[test]
fn test_wake_detects_its_own_stale_pointer_and_does_not_send() {
    let stale = "[poll 11:15Z] 4 new alerts -> /Users/x/inbox/alerts.jsonl";
    let cap = wrapped_box_screen(&PM_TURN, stale);
    assert_eq!(
        parse_pane(&cap).box_reading,
        BoxReading::Input(InputState::Typed)
    );
    assert!(!input_state_for_send(&cap).accepts_input());
}

#[test]
fn test_wake_does_not_mistake_a_human_draft_starting_with_poll_for_its_own() {
    let cap = screen(
        &PM_TURN,
        Screen {
            box_text: "poll the team about the release date",
            ..Screen::default()
        },
    );
    assert_eq!(input_state_for_send(&cap), InputState::Typed);
}

/// #9171 A2 fail-closed contract: a capture that is not positively an empty or
/// suggestion box never yields a state that accepts input. Red against a
/// reader that defaults to `Empty` (the empty, plain-text and stale-❯ cases),
/// against a guard without its dialog or rposition arm, and against a guard
/// that trusts `classify_input`'s `suggestion` (the typed-beside-dim cases).
#[test]
fn unrecognised_captures_never_accept_input() {
    let r = rule();
    let unrecognised = [
        String::new(),
        "plain shell output\n$ ls\n".to_owned(),
        "❯\n\n  ✻ Doing work… (2m 14s · esc to interrupt)\n".to_owned(),
        permission(),
        ask_user(),
        // A ❯ row with no rule under it: no bounded box.
        format!("{}\n{r}\n❯{NBSP}\n", PM_TURN.join("\n")),
        // An empty first row with a draft on the wrapped row below it.
        format!(
            "{}\n{r}\n❯{NBSP}\nstray draft row\n{r}\n",
            PM_TURN.join("\n")
        ),
        // A dim first row whose wrapped row is not dim.
        format!(
            "{}\n{r}\n❯{NBSP}{DIM}suggest{RESET}\ntyped\n{r}\n",
            PM_TURN.join("\n")
        ),
        // A dialog's option cursor below an otherwise empty box.
        screen(&PM_TURN, Screen::default()) + " ❯ 1. Yes\n Esc to cancel\n",
        // #9171 critic: dialog text below a box with no ❯ of its own. Red
        // when the guard's dialog arm is removed.
        screen(&PM_TURN, Screen::default()) + " Do you want to proceed?\n Esc to cancel\n",
        // #9171 critic: a stray ❯ row below a box holding a draft; the
        // classifier reads the stray row. Red when the rposition arm is removed.
        screen(
            &PM_TURN,
            Screen {
                box_text: "half-typed reply",
                ..Screen::default()
            },
        ) + "❯\u{a0}\n",
        // #9171 critic: typed text beside a dim span, which classify_input
        // reads as a suggestion. A typed slash command and its dim hint:
        screen(
            &PM_TURN,
            Screen {
                box_text: &format!("/tm-fleet-check {DIM}[pr-number]{RESET}"),
                ..Screen::default()
            },
        ),
        // typed text, then a dim tail:
        format!("⏺ done\n{r}\n❯{NBSP}my draft reply{DIM} …{RESET}…\n{r}\n"),
        // a dim row, then a wrapped row with typed and dim text:
        format!(
            "{}\n{r}\n❯{NBSP}{DIM}a dim start{RESET}\ntyped {DIM}tail{RESET}\n{r}\n",
            PM_TURN.join("\n")
        ),
        // an empty dim pair, then typed text:
        format!("⏺ done\n{r}\n❯{NBSP}{DIM}{RESET}typed reply\n{r}\n"),
        // dim cleared by SGR 22 and by an empty SGR, then typed text:
        format!("⏺ done\n{r}\n❯{NBSP}\u{1b}[2mhint\u{1b}[22mtyped\n{r}\n"),
        format!("⏺ done\n{r}\n❯{NBSP}\u{1b}[2mhint\u{1b}[mtyped\n{r}\n"),
        // palette colour 2 (`38;5;2`) is a colour, not dim, beside a dim span:
        format!("⏺ done\n{r}\n❯{NBSP}{DIM}{RESET}\u{1b}[38;5;2mtyped{RESET}\n{r}\n"),
    ];
    for cap in &unrecognised {
        let state = input_state_for_send(cap);
        assert!(!state.accepts_input(), "{state} accepted for {cap:?}");
    }
    // Positive controls: the reading is not simply "never accept".
    let dim = Screen {
        box_text: "next",
        dim: true,
        ..Screen::default()
    };
    assert_eq!(
        input_state_for_send(&screen(&PM_TURN, Screen::default())),
        InputState::Empty
    );
    assert_eq!(
        input_state_for_send(&screen(&PM_TURN, dim)),
        InputState::Suggestion
    );
    let wrapped_dim = format!(
        "{}\n{r}\n❯{NBSP}{DIM}a long{RESET}\n{DIM}suggestion{RESET}\n{r}\n",
        PM_TURN.join("\n")
    );
    assert_eq!(input_state_for_send(&wrapped_dim), InputState::Suggestion);
    // Dim re-set inside a combined parameter list (`0;2`), reset at the end.
    let combined = format!("⏺ done\n{r}\n❯{NBSP}{DIM}all \u{1b}[0;2mdim text{RESET}\n{r}\n");
    assert_eq!(input_state_for_send(&combined), InputState::Suggestion);
}
