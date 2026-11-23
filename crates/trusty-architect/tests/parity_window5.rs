//! ar-T9u parity window 5: one regression test per divergence class (B, C,
//! D), each red on the pre-fix commit. Class E lives in the token watcher's
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

/// Window 5, class C: tm-agentic-bi alerted `since-…695` at 05:21:57 and
/// `since-…704` at 05:28:12 inside one idle episode. A single not-idle read
/// re-armed the epoch. It must not.
#[test]
fn window5_transient_not_idle_read_keeps_the_idle_epoch() {
    let mut ps = PaneState::default();
    let e695 = 1_795_408_695.0;
    let screen = idle_with("");
    assert!(step(&screen, &mut ps, e695).is_empty());
    let first = step(&screen, &mut ps, e695 + 602.0);
    assert_eq!(
        first,
        [format!("since-1795408695-{}", parse_pane(&screen).screen)]
    );
    // One cycle reads the pane not idle; the next reads it idle again.
    assert!(step(&busy(), &mut ps, e695 + 662.0).is_empty());
    let mut seen = Vec::new();
    for k in 0..12 {
        seen.extend(step(&screen, &mut ps, e695 + 722.0 + 60.0 * f64::from(k)));
    }
    assert!(
        seen.iter().all(|f| f.starts_with("since-1795408695-")),
        "re-armed: {seen:?}"
    );
}

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

// --- D: an alerted epoch never alerts again -------------------------------------

/// Window 5, class D: tm-trusty-computer at 05:24:02 alerted epoch
/// 1795296612, already alerted at 04:46:39, with the new screen hash, 18 s
/// before the new episode began at 1795411460.
#[test]
fn window5_alerted_epoch_does_not_alert_again_on_a_new_screen() {
    let old = 1_795_296_612.0;
    let mut ps = PaneState::default();
    let before = idle_with("⏺ Merged #12.");
    step(&before, &mut ps, old);
    assert_eq!(step(&before, &mut ps, old + 600.0).len(), 1);
    // The screen changed but the held epoch did not move.
    let after = idle_with("⏺ Started the release notes.");
    assert_ne!(parse_pane(&before).screen, parse_pane(&after).screen);
    let got = step(&after, &mut ps, 1_795_411_442.0);
    assert!(got.is_empty(), "stale epoch re-alerted: {got:?}");
    // The same alert stays a candidate on its own screen (dedup keeps it).
    assert_eq!(step(&before, &mut ps, 1_795_411_502.0).len(), 1);
}
