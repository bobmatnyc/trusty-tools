//! parity window 5: one regression test per divergence class (B, C,
//! D), each red on the pre-fix commit. #11826: the C grace and the D settle
//! rule were withdrawn for window 8 (the target is fleet-poll.py); their
//! inverse tests live in `tests/parity_window8.rs`. Class E lives in the token watcher's
//! own tests (`src/poller/token/tests.rs`), because its state is crate-private.
//! The epochs keep window 5's values where the brief gives them in full and
//! its suffixes where it gives only those (`…695`, `…704`).

use trusty_architect::classify::{Alert, PaneState, evaluate, parse_pane};

fn rule() -> String {
    "─".repeat(40)
}

/// An idle pane whose transcript ends with `last`.
fn idle_with(last: &str) -> String {
    let r = rule();
    format!("⏺ Done.\n{last}\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n")
}

/// The same pane mid-turn: a background agent row makes it busy.
fn busy() -> String {
    let r = rule();
    format!(
        "⏺ Working.\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n  ◯ x  Doing things…  3m 2s · ↓ 12.0k tokens\n"
    )
}

fn idle_fps(alerts: &[Alert]) -> Vec<String> {
    alerts
        .iter()
        .filter(|a| a.kind == "idle")
        .map(|a| a.fingerprint.clone())
        .collect()
}

fn step(capture: &str, ps: &mut PaneState, now: f64) -> Vec<String> {
    idle_fps(&evaluate(&parse_pane(capture), ps, now, "tm-x", "%1"))
}

// --- B: plan-limit banner ----------------------------------------------------

/// Window 5, class B: chat prose naming a limit or a reset time raised
/// `plan-limit` (tm-trusty-issues 05:58:17, tm-trusty-cloud 06:06:39,
/// tm-dogfood 06:24:17). Only the harness's own banner may.
#[test]
fn window5_plan_limit_ignores_prose_but_keeps_the_banner() {
    let prose = [
        "⏺ GitHub rate limit resets at 06:38Z; I will retry the merge then.",
        "  GitHub rate limit resets at 06:38Z",
        "  the API limit reached its ceiling twice, so the job backs off",
        "⏺ The vendor says your limit will reset at 5pm.",
    ];
    for line in prose {
        assert_eq!(parse_pane(&idle_with(line)).limit, None, "{line}");
    }
    let banners = [
        "  ⎿  Claude usage limit reached. Your limit will reset at 5pm.",
        "Claude AI usage limit reached|1795411460",
        "  5-hour limit reached ∙ resets 3am",
        "  ⎿  You've hit your limit · resets 2pm (America/New_York)",
        "  Weekly limit reached ∙ resets Mon 9am",
    ];
    for line in banners {
        assert!(parse_pane(&idle_with(line)).limit.is_some(), "{line}");
    }
}

// --- C: one idle episode, one alert -------------------------------------------

/// The other side of C: a real busy stretch ends the episode, and the next
/// idle stretch gets its own epoch and its own alert.
#[test]
fn window5_real_busy_stretch_ends_the_idle_episode() {
    let mut ps = PaneState::default();
    let t = 1_795_408_695.0;
    let screen = idle_with("");
    step(&screen, &mut ps, t);
    assert_eq!(step(&screen, &mut ps, t + 600.0).len(), 1);
    for k in 1..=5 {
        step(&busy(), &mut ps, t + 600.0 + 60.0 * f64::from(k));
    }
    step(&screen, &mut ps, t + 960.0);
    let got = step(&screen, &mut ps, t + 1560.0);
    assert_eq!(got.len(), 1);
    assert!(got[0].starts_with("since-1795409655-"), "{got:?}");
}

/// Window 5, class B, critic round: the generalised banner forms still alert.
#[test]
fn window5_plan_limit_banner_variants() {
    let banners = [
        "  ⎿  You've hit your session limit · resets 2pm",
        "  You’ve hit your weekly limit · resets Mon 9am",
        "  You've hit your opus limit · resets Mon 9am",
        "  You've hit your usage limit",
        "  Usage limit reached. Resets at 5pm.",
        "  Limit reached ∙ resets 3am",
        "  │ Claude usage limit reached. Your limit will reset at 5pm.",
        "● 5-hour limit reached ∙ resets 3am",
    ];
    for line in banners {
        assert!(parse_pane(&idle_with(line)).limit.is_some(), "{line}");
    }
}
