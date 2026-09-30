//! End-to-end proof of the Architect's env-file exemption (#8939).
//!
//! Why: owner ruling item 44 with the Architect's rulings — only the
//! process-bound Architect's main thread may run `tm env keys|set` on a
//! dotenv file, on the guarded path and under a bypass; its subagent and a
//! PM may not, and every value-printing command stays denied.
//! What: the #8878 harness — `tm hook --pm-guard` as the child of a fake
//! `claude` recorded as the Architect's launch, with a scratch `$HOME`.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_architect_envfile_8939::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{
    Fixture, bash_payload, run, run_script_under_claude, run_under_claude, run_under_claude_with,
};

/// The Architect's project with a `.env.local`.
fn fixture() -> (Fixture, std::path::PathBuf) {
    let fx = Fixture::new();
    let path = fx.project.join(".env.local");
    std::fs::write(&path, "API_KEY=x\n").expect("write env file");
    (fx, path)
}

/// The one-shot `tm env` grants the guard has minted under the scratch home.
fn grants(fx: &Fixture) -> usize {
    let dir = fx.home.join(".trusty-mpm/architect-launch/envfile-grants");
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "grant"))
            .count()
    })
}

#[test]
fn the_architect_main_thread_lists_keys_and_its_subagent_does_not() {
    let (fx, path) = fixture();
    let p = path.display();
    for command in [
        format!("tm env keys {p}"),
        format!("tm env set {p} API_KEY --from-keychain iris --account bob"),
    ] {
        let main = bash_payload(&fx, &command);
        let before = grants(&fx);
        assert_eq!(run_under_claude(&fx, &main, true).trim(), "", "{command}");
        // #8939 fix round: the exemption mints the verb's one-shot grant.
        assert_eq!(grants(&fx), before + 1, "no grant: {command}");
        let bypass = [("TRUSTY_MPM_PM_UNRESTRICTED", "1")];
        let out = run_under_claude_with(&fx, &main, true, &bypass);
        assert_eq!(out.trim(), "", "under a bypass: {command}");
        assert_eq!(grants(&fx), before + 2, "no grant under a bypass");
        let mut sub: serde_json::Value = serde_json::from_str(&main).expect("json");
        sub["agent_id"] = serde_json::json!("agent-7");
        let out = run_under_claude(&fx, &sub.to_string(), true);
        assert!(out.contains("\"deny\""), "the subagent: {out}");
        assert!(out.contains("the call comes from a subagent"), "{out}");
        // The spoof: the environment with no launch record.
        let out = run_under_claude(&fx, &main, false);
        assert!(out.contains("\"deny\""), "unrecorded: {out}");
        // A PM in the same directory.
        let out = run(&fx, &main, &[], Some("pm"));
        assert!(out.contains("\"deny\""), "a PM: {out}");
        assert_eq!(grants(&fx), before + 2, "a denied caller got a grant");
    }
}

/// #8939 delta critic HIGH 1: every input the verb trusts — the grant, the
/// launch record, the config roots — sits in the scratch `$HOME` here, as a
/// caller setting `HOME=/tmp/f` would build it. The verb takes its home from
/// the password database, so it refuses and spends no grant.
#[test]
fn a_forged_home_with_a_valid_grant_is_refused() {
    let (fx, path) = fixture();
    let p = path.display();
    let main = bash_payload(&fx, &format!("tm env keys {p}"));
    assert_eq!(run_under_claude(&fx, &main, true).trim(), "");
    assert_eq!(grants(&fx), 1, "the guard minted a grant");
    let script = format!("\"$TM\" env keys {p}");
    let out = run_script_under_claude(&fx, &script, "", true, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "the verb ran: {out:?}");
    assert!(!stdout.contains("API_KEY"), "the verb ran: {stdout}");
    assert_eq!(grants(&fx), 1, "the forged grant was spent");
}

#[test]
fn the_architect_still_cannot_cat_an_env_file_end_to_end() {
    let (fx, path) = fixture();
    let p = path.display();
    for command in [format!("cat {p}"), format!("tm env keys {p} | cat")] {
        let out = run_under_claude(&fx, &bash_payload(&fx, &command), true);
        assert!(out.contains("\"deny\""), "{command}: {out}");
    }
}
