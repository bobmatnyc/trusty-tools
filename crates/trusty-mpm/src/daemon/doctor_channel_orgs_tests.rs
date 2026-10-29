//! Tests for `doctor_channel_orgs.rs` (#10423).

use super::doctor_channel_orgs::{NAME, channel_org_rows};
use crate::channels::orgs_tests::INITIAL_TOML;
use crate::channels::secret_ref::RefHealth;
use crate::channels::send_store::orgs_path;
use crate::core::doctor::CheckStatus;

fn write_orgs(home: &std::path::Path, text: &str) {
    let path = orgs_path(home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn doctor_reports_each_org_binding_and_ref_health_by_name() {
    let home = tempfile::tempdir().expect("tempdir");
    write_orgs(home.path(), INITIAL_TOML);
    let rows = channel_org_rows(home.path(), &|scope, r| {
        assert!(scope.is_some(), "the org scope reaches the health check");
        if r.contains("SLACK") {
            RefHealth::Present
        } else {
            RefHealth::Missing
        }
    });
    assert_eq!(rows.len(), 2);
    let bta = rows
        .iter()
        .find(|r| r.name == "channel_org:bta")
        .expect("bta");
    assert_eq!(bta.status, CheckStatus::Fail);
    assert_eq!(bta.message, "gchat space AAAATEST001; sa_key=missing");
    let duetto = rows
        .iter()
        .find(|r| r.name == "channel_org:duetto")
        .expect("duetto");
    assert_eq!(duetto.status, CheckStatus::Ok);
    assert_eq!(
        duetto.message,
        "slack workspace T0TEST001; bot_token=present"
    );
    for row in &rows {
        assert!(
            !row.message.contains("secret://"),
            "names only: {}",
            row.message
        );
    }
    let unknown = channel_org_rows(home.path(), &|_, _| RefHealth::StoreError("x".into()));
    assert!(unknown.iter().all(|r| r.status == CheckStatus::Unknown));
}

#[test]
fn doctor_without_orgs_toml_is_ok_and_a_bad_file_fails() {
    let home = tempfile::tempdir().expect("tempdir");
    let rows = channel_org_rows(home.path(), &|_, _| RefHealth::Present);
    assert_eq!(
        (rows[0].name.as_str(), rows[0].status),
        (NAME, CheckStatus::Ok)
    );
    write_orgs(home.path(), "[orgs.x]\nkind = \"pigeon\"\n");
    let rows = channel_org_rows(home.path(), &|_, _| RefHealth::Present);
    assert_eq!(
        (rows[0].name.as_str(), rows[0].status),
        (NAME, CheckStatus::Fail)
    );
}
