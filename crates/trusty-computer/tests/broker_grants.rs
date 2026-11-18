//! Grants: consent-only mint, sensitive apps, persistence, expiry (issue #11460, Phases 3-4).
//!
//! Every grant here is minted through the consent flow on `operator.sock`
//! with the fake dialog (`common::rig::consent`).

mod common;

use std::os::unix::fs::PermissionsExt;

use common::rig::{Rig, T0, consent, grant_req, pid};
use common::short_tmp;
use serde_json::{Value, json};
use trusty_computer::grants::{GrantError, Grants};

fn mint_via(rig: &Rig, req: &Value) -> Value {
    consent(&rig.server.operator, &rig.dialog, req)
}

#[test]
fn grants_survive_a_broker_restart() {
    let rig = Rig::new();
    let g = rig.mint(1);
    assert!(g.id.starts_with("g-") && g.id.len() == 14, "{}", g.id);
    let path = rig.state().join("grants.json");
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let again = Grants::load(&path, T0 + 1.0).expect("reload");
    let back = again.get(&g.id).expect("kept");
    assert_eq!(
        (back.scope, &back.bundle_id, &back.tools),
        (g.scope, &g.bundle_id, &g.tools)
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
    let shown = rig.dialog.shown();
    let mut term = grant_req(&[], 1);
    term["bundle_id"] = json!("com.apple.Terminal");
    rig.resolver.set(pid(), "com.apple.Terminal", "Terminal");
    assert_eq!(mint_via(&rig, &term)["code"], "sensitive_app");
    rig.resolver.apps.lock().expect("apps").clear();
    let mut bad = grant_req(&["kill_app"], 1);
    assert_eq!(mint_via(&rig, &bad)["code"], "bad_tool");
    bad["tools"] = json!([]);
    bad["ttl"] = json!(0);
    assert_eq!(mint_via(&rig, &bad)["code"], "bad_ttl");
    bad["bundle_id"] = json!("x\" & do shell script \"id");
    assert_eq!(mint_via(&rig, &bad)["code"], "bad_bundle");
    assert_eq!(rig.dialog.shown(), shown, "no dialog for a refused request");
    term["allow_sensitive"] = json!(true);
    rig.resolver.set(pid(), "com.apple.Terminal", "Terminal");
    let g = mint_via(&rig, &term);
    assert_eq!(g["result"]["grant"]["sensitive"], true, "{g}");
    let mint = rig
        .audit()
        .into_iter()
        .rfind(|l| l["op"] == "mint")
        .expect("mint line");
    assert_eq!(mint["sensitive"], true);
}

#[test]
fn mint_write_failure_refuses() {
    let rig = Rig::new();
    let tmp = rig.state().join("grants.json.tmp");
    std::fs::create_dir_all(tmp.join("x")).expect("block the temp file");
    let reply = mint_via(&rig, &grant_req(&[], 1));
    assert_eq!(reply["code"], "grant_store", "{reply}");
    let path = rig.state().join("grants.json");
    assert_eq!(Grants::load(&path, T0).expect("reload").iter().count(), 1);
}

#[test]
fn mint_audit_failure_revokes_the_grant() {
    let rig = Rig::new();
    let mut conn = common::rig::Conn::open(&rig.server.operator);
    let challenge = conn.send(&grant_req(&[], 1));
    std::fs::remove_file(rig.audit_path()).expect("rm");
    std::fs::create_dir(rig.audit_path()).expect("mkdir");
    let err = conn.send(&json!({"op": "grant_confirm",
        "challenge": challenge["result"]["challenge"], "code": rig.dialog.last_code()}));
    assert_eq!(err["code"], "audit_failed", "{err}");
    let text = std::fs::read_to_string(rig.state().join("grants.json")).expect("grants");
    let rows: Value = serde_json::from_str(&text).expect("json");
    let rows = rows.as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r["revoked"] == true).count(), 1);
}

#[test]
fn expired_idle_and_revoked_grants_are_refused() {
    let rig = Rig::new();
    let g = rig.grant.clone();
    let mut grants = Grants::load(&rig.state().join("grants.json"), T0).expect("load");
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
