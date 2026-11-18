//! Grants: operator-only mint, sensitive apps, persistence, expiry (issue #11460, Phase 3).

mod common;

use std::os::unix::fs::PermissionsExt;

use common::rig::{Rig, T0, mint};
use common::short_tmp;
use trusty_computer::grants::{GrantError, Grants, OperatorMint};
use trusty_computer::manifest::LeaseScope;

const SCOPE: LeaseScope = LeaseScope {
    pid: 4242,
    window_id: 7,
};

#[test]
fn grants_survive_a_broker_restart() {
    let dir = short_tmp();
    let path = dir.path().join("grants.json");
    let mut grants = Grants::load(&path, T0).expect("load");
    let g = grants
        .mint(OperatorMint::stub("dev.x", SCOPE), T0)
        .expect("mint");
    assert!(g.id.starts_with("g-") && g.id.len() == 14, "{}", g.id);
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let again = Grants::load(&path, T0 + 1.0).expect("reload");
    let back = again.get(&g.id).expect("kept");
    assert_eq!(
        (back.scope, &back.bundle_id, &back.tools),
        (SCOPE, &g.bundle_id, &g.tools)
    );
}

#[test]
fn malformed_grants_file_is_refused() {
    let dir = short_tmp();
    let path = dir.path().join("grants.json");
    std::fs::write(&path, "{not json").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(matches!(Grants::load(&path, T0), Err(GrantError::Store(_))));
    std::fs::write(&path, "[]").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(Grants::load(&path, T0).is_err(), "a shared file is refused");
}

#[test]
fn mint_refuses_sensitive_apps_and_denied_tools() {
    let rig = Rig::new();
    let mut term = OperatorMint::stub("com.apple.Terminal", SCOPE);
    let refused = rig
        .broker
        .operator_mint(term.clone())
        .expect_err("sensitive");
    assert_eq!(refused["code"], "sensitive_app");
    term.allow_sensitive = true;
    let g = rig.broker.operator_mint(term).expect("allowed");
    assert!(g.sensitive);
    let line = rig.audit().last().cloned().expect("line");
    assert_eq!(
        (&line["op"], &line["sensitive"]),
        (&"mint".into(), &true.into())
    );
    let mut bad = mint(&["kill_app"], 1);
    assert_eq!(
        rig.broker.operator_mint(bad.clone()).expect_err("deny")["code"],
        "bad_tool"
    );
    bad.extra_tools.clear();
    bad.ttl = 0.0;
    assert_eq!(
        rig.broker.operator_mint(bad).expect_err("ttl")["code"],
        "bad_ttl"
    );
}

#[test]
fn mint_write_failure_refuses() {
    let dir = short_tmp();
    let path = dir.path().join("grants.json");
    let mut grants = Grants::load(&path, T0).expect("load");
    std::fs::create_dir(dir.path().join("grants.json.tmp")).expect("block the temp file");
    std::fs::create_dir_all(dir.path().join("grants.json.tmp/x")).expect("non-empty");
    let err = grants
        .mint(OperatorMint::stub("dev.x", SCOPE), T0)
        .expect_err("store");
    assert_eq!(err.code(), "grant_store");
    assert!(Grants::load(&path, T0).expect("reload").get("g-").is_none());
}

#[test]
fn mint_audit_failure_revokes_the_grant() {
    let rig = Rig::new();
    std::fs::remove_file(rig.audit_path()).expect("rm");
    std::fs::create_dir(rig.audit_path()).expect("mkdir");
    let err = rig.broker.operator_mint(mint(&[], 1)).expect_err("audit");
    assert_eq!(err["code"], "audit_failed");
    let text = std::fs::read_to_string(rig.state().join("grants.json")).expect("grants");
    let rows: serde_json::Value = serde_json::from_str(&text).expect("json");
    let rows = rows.as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r["revoked"] == true).count(), 1);
}

#[test]
fn expired_idle_and_revoked_grants_are_refused() {
    let dir = short_tmp();
    let mut grants = Grants::load(&dir.path().join("grants.json"), T0).expect("load");
    let g = grants
        .mint(OperatorMint::stub("dev.x", SCOPE), T0)
        .expect("mint");
    assert!(grants.live(&g.id, T0 + 299.0).is_ok());
    let idle = grants.live(&g.id, T0 + 300.0).expect_err("idle");
    assert!(idle.to_string().contains("idle"), "{idle}");
    grants.touch(&g.id, T0 + 1790.0);
    let expired = grants.live(&g.id, T0 + 1800.0).expect_err("ttl");
    assert!(expired.to_string().contains("expired"), "{expired}");
    grants.touch(&g.id, T0 + 10.0);
    grants.revoke(&g.id).expect("revoke");
    let revoked = grants.live(&g.id, T0 + 11.0).expect_err("revoked");
    assert_eq!(revoked.code(), "grant_expired");
    assert_eq!(
        grants.live("g-none", T0).expect_err("unknown").code(),
        "no_grant"
    );
}
