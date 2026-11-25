"""Tests for the candidate rules R1-R6 (scripts/test_audit/rules.py, dupes.py) (#11268).

Every rule has a firing case and at least one near-miss that must not fire,
so the false-flag behaviour is pinned, not only the hit.
"""
import pytest

from test_audit.assertions import asserting_helpers
from test_audit.dupes import duplicate_rules
from test_audit.extract import extract_tests
from test_audit.rules import ALL_RULES, per_test_rules

PRELUDE = '''
const GUIDE: &str = include_str!("assets/guide.md");
const GOLDEN: &str = include_str!("testdata/out.plist.golden");
fn prompt() -> &'static str { include_str!("assets/prompt.md") }
fn check_ok(x: u8) { assert_eq!(x, 1); }
fn check_via(x: u8) { check_ok(x) }
'''


def rules_for(test_src, prelude=PRELUDE, enabled=ALL_RULES):
    """Every rule, dropped ones included, so their definitions stay pinned."""
    scan = extract_tests("demo", "crates/demo/src/lib.rs", prelude + test_src)
    helpers = asserting_helpers(scan.helper_bodies)
    return per_test_rules(scan.tests[-1], scan.asset_names, helpers, enabled)


def fired(test_src):
    return sorted(rules_for(test_src))


CASES = [
    # R2 text pin
    ("r2_include_str_const", '#[test] fn t() { assert!(GUIDE.contains("## Usage")); }', ["R2"]),
    ("r2_loader_fn", '#[test] fn t() { assert!(prompt().contains("x")); }', ["R2"]),
    ("r2_local_from_asset", '#[test] fn t() { let s = GUIDE.to_lowercase(); assert!(s.contains("x")); }',
     ["R2"]),
    ("r2_inline_include", '#[test] fn t() { assert!(include_str!("a/b.md").contains("x")); }', ["R2"]),
    ("r2_prose_literal",
     '#[test] fn t() { assert_eq!(hint(), "Install tmux with brew before you start the session please"); }',
     ["R2"]),
    ("r2_markdown_literal", '#[test] fn t() { assert!(render().contains("**bold**")); }', ["R2"]),
    ("r2_not_golden_plist", '#[test] fn t() { assert_eq!(render(), GOLDEN); }', []),
    ("r2_not_message_arg",
     '#[test] fn t() { assert_eq!(f(), 1, "this long failure message is not what the test compares"); }', []),
    ("r2_not_input_literal",
     '#[test] fn t() { assert!(parse("an input sentence that is long but passed to code")); }', []),
    ("r2_not_short_literal", '#[test] fn t() { assert!(render().contains("ok")); }', []),
    # R3 no behavioural assertion
    ("r3_no_assert", "#[test] fn t() { run(); }", ["R3"]),
    ("r3_unwrap_only", "#[test] fn t() { run().unwrap(); }", ["R3"]),
    ("r3_is_ok_only", "#[test] fn t() { assert!(run().is_ok()); }", ["R3"]),
    ("r3_not_eq", "#[test] fn t() { assert_eq!(run(), 3); }", []),
    ("r3_not_is_ok_and", "#[test] fn t() { assert!(run().is_ok() && done()); }", []),
    ("r3_not_should_panic", "#[test]\n#[should_panic] fn t() { run(); }", []),
    ("r3_not_unwrap_err", "#[test] fn t() { run().unwrap_err(); }", []),
    ("r3_not_panic_branch", "#[test] fn t() { match run() { Ok(_) => {}, Err(e) => panic!(\"{e}\") } }", []),
    ("r3_not_file_helper", "#[test] fn t() { check_via(run()); }", []),
    ("r3_not_turbofish_helper", "#[test] fn t() { assert_send::<Foo>(); }", []),
    # R4 mock-only
    ("r4_on_mock", "#[test] fn t() { let fake = FakeCargo::new(); run(&fake); assert_eq!(fake.calls(), 2); }",
     ["R4"]),
    ("r4_wrapped_mock", "#[test] fn t() { let m = Arc::new(MockStore::default()); go(&m); assert!(m.hit()); }",
     ["R4"]),
    ("r4_not_mock_as_input", "#[test] fn t() { let fake = FakeHost::default(); assert_eq!(installed(&fake), 1); }",
     []),
    ("r4_not_mixed",
     "#[test] fn t() { let fake = FakeCargo::new(); let r = run(&fake); assert_eq!(r, 1); assert_eq!(fake.calls(), 2); }",
     []),
    ("r4_not_sut_built_from_mock",
     "#[test] fn t() { let sut = Service::new(MockRepo::new()); assert_eq!(sut.get(), 1); }", []),
    ("r4_not_fake_named_dir", "#[test] fn t() { let fake_home = tempdir(); assert_eq!(fake_home.n(), 1); }", []),
    # R5 constant / derive
    ("r5_constant", "#[test] fn t() { assert_eq!(DEFAULT_PORT, 7788); }", ["R5"]),
    ("r5_two_constants", "#[test] fn t() { assert_eq!(a::SOCKET, b::SOCKET); }", ["R5"]),
    ("r5_default", "#[test] fn t() { let c = Config::default(); assert_eq!(c.port, 8080); assert!(!c.on); }",
     ["R5"]),
    ("r5_default_inline", "#[test] fn t() { assert!(!Boot::default().controller.check); }", ["R5"]),
    ("r5_round_trip",
     "#[test] fn t() { let a = S { x: 1 }; let s = serde_json::to_string(&a).unwrap();"
     " let b: S = serde_json::from_str(&s).unwrap(); assert_eq!(b, a); }", ["R5"]),
    ("r5_not_fn_result", "#[test] fn t() { assert_eq!(port_for(\"x\"), 7788); }", []),
    ("r5_not_default_then_behaviour",
     "#[test] fn t() { let c = Config::default(); assert_eq!(c.validate(), Ok(())); }", []),
    ("r5_not_wire_format",
     "#[test] fn t() { let a = S { x: 1 }; let s = serde_json::to_string(&a).unwrap();"
     " let b: S = serde_json::from_str(&s).unwrap(); assert_eq!(b, a); assert!(s.contains(\"\\\"x\\\"\")); }", []),
    ("r5_not_local", "#[test] fn t() { let n = compute(); assert_eq!(n, MAX); }", []),
    # R6 ignore
    ("r6_bare_ignore", "#[test]\n#[ignore]\nfn t() { assert_eq!(f(), 1); }", ["R6"]),
    # The marker is split so check_demo_intentions.sh does not match this fixture.
    ("r6_demo_intent", '#[test]\n#[ignore = "intent(demo' + '): wire later"]\nfn t() { assert_eq!(f(), 1); }', ["R6"]),
    ("r6_not_reasoned", '#[test]\n#[ignore = "touches the real Keychain"]\nfn t() { assert_eq!(f(), 1); }', []),
]


@pytest.mark.parametrize("name,src,want", CASES, ids=[c[0] for c in CASES])
def test_rule_fires_only_where_it_should(name, src, want):
    assert fired(src) == want, rules_for(src)


DROPPED_HITS = [c for c in CASES if c[2] and c[2][0] in ("R2", "R4")]


@pytest.mark.parametrize("name,src,want", DROPPED_HITS, ids=[c[0] for c in DROPPED_HITS])
def test_dropped_rules_r2_r4_never_fire_by_default(name, src, want):
    scan = extract_tests("demo", "crates/demo/src/lib.rs", PRELUDE + src)
    assert per_test_rules(scan.tests[-1], scan.asset_names) == {}


def dup_rules(src):
    scan = extract_tests("demo", "crates/demo/src/lib.rs", src)
    return duplicate_rules(scan.tests, threshold=0.85, min_tokens=20)


LONG = "let v = build(1, 2, 3); let w = transform(&v, true); assert_eq!(w.len(), 3); assert_eq!(w[0], {x});"


def test_r1_identical_bodies_flag_both_sides():
    src = "#[test] fn a() { run(1); assert_eq!(x(), 1); }\n#[test] fn b() { run(1); assert_eq!(x(), 1); }\n"
    got = dup_rules(src)
    assert set(got) == {"demo::crates/demo/src/lib.rs::a", "demo::crates/demo/src/lib.rs::b"}
    assert "identical body to demo::crates/demo/src/lib.rs::b" in got["demo::crates/demo/src/lib.rs::a"]


def test_r1_near_identical_within_threshold():
    body = LONG + " assert!(w.iter().all(|e| e.ok())); assert_eq!(w.last(), Some(&9));"
    src = f"#[test] fn a() {{ {body.format(x=1)} }}\n#[test] fn b() {{ {body.format(x=2)} }}\n"
    got = dup_rules(src)
    assert len(got) == 2 and "near-identical" in next(iter(got.values()))


def test_r1_does_not_flag_distinct_or_short_bodies():
    src = ("#[test] fn a() { " + LONG.format(x=1) + " }\n"
           "#[test] fn b() { let q = parse(\"z\"); assert!(q.is_none()); assert_eq!(other(7), 8); }\n"
           "#[test] fn c() { assert_eq!(f(1), 1); }\n#[test] fn d() { assert_eq!(f(2), 2); }\n")
    assert dup_rules(src) == {}
