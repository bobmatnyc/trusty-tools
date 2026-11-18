//! Phase 0 harness: every fixture captured from Python 0.7.0 parses and holds
//! the shape the port must reproduce. Fixtures and their capture script live
//! in `tests/fixtures/` (see `tests/fixtures/capture/README.md`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;

const MAX_FRAME: usize = 1024 * 1024;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn json(rel: &str) -> Value {
    let text = fs::read_to_string(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} not a string in {v}"))
}

/// Decode one native-endian length-prefixed frame; the whole file is one frame.
fn decode_frame(raw: &[u8]) -> Map<String, Value> {
    assert!(raw.len() >= 4, "frame shorter than its header");
    let size = u32::from_ne_bytes(raw[..4].try_into().expect("4 bytes")) as usize;
    assert!(
        size > 0 && size <= MAX_FRAME,
        "frame size {size} out of range"
    );
    assert_eq!(size, raw.len() - 4, "header length disagrees with body");
    match serde_json::from_slice::<Value>(&raw[4..]).expect("frame body is JSON") {
        Value::Object(m) => m,
        other => panic!("frame body is not an object: {other}"),
    }
}

fn hmac_hex(key: &str, msg: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

fn transcript(role: &str, hello: &Value, server_nonce: &str) -> Vec<u8> {
    let items = [
        "trusty-browser-native-v1",
        role,
        "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc",
        s(hello, "profile"),
        s(hello, "email"),
        s(hello, "incarnation"),
        s(hello, "nonce"),
        server_nonce,
    ];
    serde_json::to_vec(&items).expect("strings serialise")
}

fn is_hex64(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn every_frame_sample_decodes_to_an_object() {
    let index = json("frames/index.json");
    let entries = index.as_array().expect("index is a list");
    assert!(
        entries.len() >= 60,
        "expected the full capture, got {}",
        entries.len()
    );
    let on_disk: BTreeSet<String> = fs::read_dir(fixtures().join("frames"))
        .expect("frames dir")
        .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
        .filter(|n| n.ends_with(".bin"))
        .collect();
    let listed: BTreeSet<String> = entries.iter().map(|e| s(e, "file").to_owned()).collect();
    assert_eq!(on_disk, listed, "index and frames/*.bin disagree");
    let sockets = BTreeSet::from([
        "service.sock",
        "agent.sock",
        "enrollment.sock",
        "operator.sock",
    ]);
    for entry in entries {
        assert!(sockets.contains(s(entry, "socket")), "{entry}");
        assert!(matches!(
            s(entry, "direction"),
            "client_to_broker" | "broker_to_client"
        ));
        let raw = fs::read(fixtures().join("frames").join(s(entry, "file"))).expect("frame file");
        let frame = decode_frame(&raw);
        let message = s(entry, "message");
        if s(entry, "direction") == "broker_to_client" {
            if message.starts_with("worker-command") {
                // Broker -> worker command envelope (activation-codes.md, page commands).
                assert!(
                    frame["id"].is_string()
                        && frame["op"].is_string()
                        && frame["email"].is_string()
                );
            } else if !message.ends_with("challenge") {
                assert!(frame["ok"].is_boolean(), "{}: no ok flag", s(entry, "file"));
            }
        }
    }
    let seen: BTreeSet<&str> = entries.iter().map(|e| s(e, "socket")).collect();
    assert_eq!(seen, sockets, "every socket has samples");
}

#[test]
fn live_handshake_frames_verify_against_the_fixture_key() {
    let frame = |name: &str| {
        let index = json("frames/index.json");
        let file = index
            .as_array()
            .expect("list")
            .iter()
            .find(|e| s(e, "message") == name)
            .map(|e| s(e, "file").to_owned())
            .unwrap_or_else(|| panic!("no frame {name}"));
        Value::Object(decode_frame(
            &fs::read(fixtures().join("frames").join(file)).expect("read"),
        ))
    };
    let hello = frame("pair-hello");
    let keys: BTreeSet<&str> = hello
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["type", "profile", "email", "incarnation", "nonce"])
    );
    let challenge = frame("pair-challenge");
    assert_eq!(s(&challenge, "type"), "challenge");
    assert!(is_hex64(s(&challenge, "nonce")));
    // Vector 0 is this live handshake; its key is the pairing's throwaway key.
    let live = json("proof_vectors.json")[0].clone();
    assert_eq!(live["hello"], hello, "vector 0 records the live hello");
    let key = s(&live, "key").to_owned();
    let nonce = s(&challenge, "nonce");
    assert_eq!(
        s(&challenge, "proof"),
        hmac_hex(&key, &transcript("broker", &hello, nonce))
    );
    let proof = frame("pair-proof");
    assert_eq!(
        s(&proof, "proof"),
        hmac_hex(&key, &transcript("extension", &hello, nonce))
    );
    let ack = frame("pair-ack");
    assert_eq!(ack["ok"], Value::Bool(true));
    assert!(ack["generation"].is_string() && ack["live"].is_array());
}

#[test]
fn proof_vectors_reproduce_transcript_and_mac() {
    let vectors = json("proof_vectors.json");
    let vectors = vectors.as_array().expect("list");
    assert_eq!(vectors.len(), 4, "two cases x two roles");
    assert!(
        vectors.iter().any(|v| !s(&v["hello"], "email").is_ascii()),
        "non-ASCII case present"
    );
    for v in vectors {
        let t = transcript(s(v, "role"), &v["hello"], s(v, "server_nonce"));
        assert_eq!(
            hex::encode(&t),
            s(v, "transcript_hex"),
            "transcript bytes, role {}",
            s(v, "role")
        );
        assert_eq!(hmac_hex(s(v, "key"), &t), s(v, "mac"));
        assert!(is_hex64(s(v, "mac")) && is_hex64(s(v, "key")));
    }
}

#[test]
fn refusal_shapes_carry_only_contract_fields() {
    let refusals = json("refusals.json");
    let list = refusals.as_array().expect("list");
    let allowed = BTreeSet::from(["ok", "error", "reason", "field", "profiles"]);
    let mut errors = BTreeSet::new();
    for r in list {
        let reply = r["reply"].as_object().expect("reply object");
        assert_eq!(reply["ok"], Value::Bool(false), "{r}");
        errors.insert(s(&r["reply"], "error").to_owned());
        for k in reply.keys() {
            assert!(
                allowed.contains(k.as_str()),
                "unexpected refusal field {k} in {r}"
            );
        }
    }
    for want in [
        "Unauthorized",
        "EnrollmentConflict",
        "InvalidEnrollment",
        "InvalidRequest",
        "ProfileMismatch",
        "InvalidNonce",
        "NotConnected",
        "UnknownGrant",
        "UnknownProfile",
    ] {
        assert!(errors.contains(want), "missing refusal {want}");
    }
}

#[test]
fn audit_lines_use_the_field_whitelist() {
    let text = fs::read_to_string(fixtures().join("audit.jsonl")).expect("audit");
    let fields = BTreeSet::from([
        "ts",
        "event",
        "grant",
        "scope",
        "profile",
        "op",
        "tab",
        "outcome",
        "origin",
        "site",
        "reason",
        "email_domain",
        "replaced",
    ]);
    let mut events = BTreeSet::new();
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).expect("audit line is JSON");
        let obj = v.as_object().expect("object");
        assert!(obj["ts"].is_number());
        events.insert(s(&v, "event").to_owned());
        for k in obj.keys() {
            assert!(
                fields.contains(k.as_str()),
                "field {k} outside the whitelist"
            );
        }
        // Python writes ensure_ascii JSON: no raw non-ASCII bytes on disk.
        assert!(line.is_ascii(), "audit line must be ASCII-escaped: {line}");
    }
    for want in [
        "refused",
        "pair",
        "connect",
        "attach",
        "operation",
        "grant_end",
        "revoke",
        "pair_replaced",
    ] {
        assert!(events.contains(want), "missing audit event {want}");
    }
}

#[test]
fn python_registry_schema_and_pairings_are_readable() {
    let db = rusqlite::Connection::open_with_flags(
        fixtures().join("registry.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open registry");
    let mut stmt = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("prepare");
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(
        tables,
        [
            "pairings",
            "pending_credentials",
            "pending_removals",
            "revoked",
            "route_versions",
            "routes"
        ]
    );
    let mut cols = db
        .prepare("SELECT name FROM pragma_table_info('pairings')")
        .expect("prepare");
    let cols: Vec<String> = cols
        .query_map([], |r| r.get(0))
        .expect("q")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(cols, ["profile", "email", "browser_key"]);
    let mut rows = db
        .prepare("SELECT profile, email, browser_key FROM pairings ORDER BY email")
        .expect("prepare");
    let rows: Vec<(String, String, String)> = rows
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("q")
        .collect::<Result<_, _>>()
        .expect("rows");
    let vault = json("vault.json");
    let mut want: Vec<(String, String)> = vault["pairings"]
        .as_array()
        .expect("list")
        .iter()
        .map(|p| (s(p, "profile").to_owned(), s(p, "email").to_owned()))
        .collect();
    want.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        rows.iter()
            .map(|r| (r.0.clone(), r.1.clone()))
            .collect::<Vec<_>>(),
        want
    );
    let key = base64::engine::general_purpose::STANDARD
        .decode(s(&vault, "key_b64"))
        .expect("key b64");
    assert_eq!(key.len(), 32, "AES-256 key");
    for (_, _, sealed) in &rows {
        let body = sealed.strip_prefix("tb1.").expect("tb1. prefix");
        let raw = base64::engine::general_purpose::URL_SAFE
            .decode(body)
            .expect("urlsafe b64");
        // nonce(12) + ciphertext(64-char hex key) + tag(16)
        assert_eq!(raw.len(), 12 + 64 + 16);
    }
}

#[test]
fn mcp_initialize_and_tools_list_parse_in_both_modes() {
    let names = |mode: &str| -> Vec<String> {
        let v = json(&format!("mcp/{mode}.tools_list.json"));
        assert_eq!(v["id"], 2);
        v["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| {
                assert!(t["inputSchema"]["type"] == "object", "{t}");
                s(t, "name").to_owned()
            })
            .collect()
    };
    for mode in ["connect", "workflow"] {
        let init = json(&format!("mcp/{mode}.initialize.json"));
        assert_eq!(init["result"]["serverInfo"]["name"], "trusty-browser");
        assert!(init["result"]["protocolVersion"].is_string());
    }
    assert_eq!(
        names("workflow"),
        [
            "browser_claim",
            "browser_snapshot",
            "browser_click",
            "browser_release"
        ]
    );
    let connect = names("connect");
    for want in [
        "browser_profiles",
        "browser_connect",
        "browser_disconnect",
        "page_read",
        "page_upload",
    ] {
        assert!(
            connect.iter().any(|n| n == want),
            "connect mode lacks {want}: {connect:?}"
        );
    }
}

#[test]
fn manifest_pins_the_python_versions() {
    let m = json("manifest.json");
    assert_eq!(
        m["captured_from"]["trusty-browser"],
        trusty_browser::PYTHON_PARITY_VERSION
    );
    assert!(m["captured_from"]["mcp"].is_string());
}
