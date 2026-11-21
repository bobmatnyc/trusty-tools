//! `casefold` parity with Python 3.12 `str.casefold` (`tests/fixtures/casefold.json`,
//! written by `tests/fixtures/capture/gen_casefold.py`).

mod common;

use common::{items, json, s};
use sha2::{Digest, Sha256};
use trusty_browser::casefold::{casefold, py_strip, table_len};
use trusty_browser::pyjson::Json;

#[test]
fn casefold_matches_python_on_every_code_point() {
    let fixture = json("casefold.json");
    let mut h = Sha256::new();
    for cp in (0..0x11_0000u32).filter(|c| !(0xD800..=0xDFFF).contains(c)) {
        let c = char::from_u32(cp).unwrap_or('\0');
        h.update(casefold(&c.to_string()).as_bytes());
        h.update([0u8]);
    }
    assert_eq!(
        hex::encode(h.finalize()),
        s(&fixture, "all_codepoints_sha256")
    );
    assert_eq!(Some(&Json::Int(table_len() as i64)), fixture.get("rows"));
}

#[test]
fn casefold_hard_cases_match_python() {
    let fixture = json("casefold.json");
    let cases = items(fixture.get("cases").unwrap_or(&Json::Null));
    assert!(cases.len() > 30);
    for case in cases {
        let pair = items(case);
        let (input, want) = (
            pair[0].as_str().unwrap_or(""),
            pair[1].as_str().unwrap_or(""),
        );
        assert_eq!(casefold(input), want, "{input:?}");
    }
    // Rust's lowercase is not case folding: these differ.
    assert_ne!(casefold("Straße"), "Straße".to_lowercase());
    assert_eq!(py_strip("\u{1c}a@b\u{3000}"), "a@b");
}
