//! Tests for the #11058 wiring list and the `fleet_seed_wiring` doctor row.

use trusty_mpm::core::doctor::CheckStatus;

use super::super::seed::{embedded_contents, wiring};
use super::Fixture;

/// Every listed symbol is in the shipped core script and, bare, in the shipped
/// sibling, so the list cannot drift from the seed assets.
#[test]
fn every_required_wiring_is_in_its_shipped_seed_11058() {
    assert!(!wiring::REQUIRED.is_empty());
    for w in wiring::REQUIRED {
        let script = embedded_contents(w.dest).unwrap_or_else(|| panic!("{} not seeded", w.dest));
        assert!(script.contains(w.symbol), "{} lacks {}", w.dest, w.symbol);
        let sibling =
            embedded_contents(w.sibling).unwrap_or_else(|| panic!("{} not seeded", w.sibling));
        let bare = w.symbol.trim_matches('"');
        assert!(sibling.contains(bare), "{} lacks {bare}", w.sibling);
    }
}

/// A fresh Architect is `Ok`; a kept poller without the symbol is `Warn`,
/// naming the file and the symbol.
#[test]
fn doctor_names_a_kept_poller_without_owner_answer_wiring_11058() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let ok = wiring::doctor_row(fx.home());
    assert_eq!(ok.name, wiring::CHECK);
    assert_eq!(ok.status, CheckStatus::Ok, "{}", ok.message);
    assert!(
        ok.message.starts_with("2 core fleet script(s) read in 1 "),
        "{}",
        ok.message
    );
    let dest = "scripts/fleet-poll.py";
    let path = fx.dir().join(dest);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("owner_answer_alerts", "gone")).unwrap();
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        // The recorded path may be the canonical form of `fx.dir()`.
        row.message.contains(&format!(": {dest} lacks "))
            && row.message.contains("`owner_answer_alerts`"),
        "{}",
        row.message
    );
}

/// #11058 Fail-Open: a core script that cannot be read warns, never passes.
#[test]
fn doctor_warns_when_a_kept_script_cannot_be_read_11058() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let path = fx.dir().join("scripts/fleet-classify.py");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap(); // a read of a directory fails
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("cannot read") && row.message.contains("unchecked"),
        "{}",
        row.message
    );
}

/// #11058 review: a recorded Architect directory that is gone warns, naming
/// it, instead of reading as "no Architect" or as checked.
#[test]
fn doctor_warns_when_the_recorded_architect_dir_is_gone_11058() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    std::fs::remove_dir_all(&dir).unwrap();
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message
            .contains("does not exist; nothing checked there"),
        "{}",
        row.message
    );
}

/// #11058 review: an Architect with every core script absent warns that
/// nothing was checked, never `Ok`.
#[test]
fn doctor_warns_when_no_core_script_was_checked_11058() {
    let fx = Fixture::new();
    fx.init().unwrap();
    for w in wiring::REQUIRED {
        let _ = std::fs::remove_file(fx.dir().join(w.dest));
    }
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("no core fleet script") && row.message.contains("nothing checked"),
        "{}",
        row.message
    );
}

/// A user config that does not parse is `Unknown`: neither a pass nor a finding.
#[test]
fn doctor_reports_a_malformed_config_as_unknown_11058() {
    let fx = Fixture::new();
    fx.write_config("[supervisor\nprojects = ");
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
}

/// No user config, so no Architect: nothing to check.
#[test]
fn doctor_without_an_architect_is_ok_11058() {
    let fx = Fixture::new();
    let row = wiring::doctor_row(fx.home());
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}
