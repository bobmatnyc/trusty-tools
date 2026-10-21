//! #9756 P0: the PM ledger contract (#9757, #10062) — types, ids, limits,
//! layout, code registry and the `mpm.dbatg.*` request and response types.
//!
//! Goldens under `tests/fixtures/ledger/` are scrubbed copies of four live
//! prototype stores, written by `scrub.py` (see its README).

use std::fs;
use std::path::{Path, PathBuf};

use trusty_architect::ledger::{
    self, ArchiveLine, Code, LedgerError, LedgerId, LogLine, Op, Prefix, Registration, SizeVerdict,
    State, StorePaths,
};

type R = Result<(), Box<dyn std::error::Error>>;

const STORES: [&str; 4] = ["pm-small", "pm-medium", "pm-over-warn", "pm-over-refuse"];

fn fixture(store: &str, file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ledger")
        .join(store)
        .join(file)
}

fn lines(store: &str, file: &str) -> Vec<String> {
    fs::read_to_string(fixture(store, file))
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

#[test]
fn ledger_goldens_round_trip_byte_identical() -> R {
    for store in STORES {
        let raw = fs::read_to_string(fixture(store, "state.json"))?;
        let state = State::from_json(&raw)?;
        assert_eq!(state.to_json()?, raw, "{store}/state.json");
        for line in lines(store, "archive.jsonl") {
            assert_eq!(
                ArchiveLine::from_line(&line)?.to_line()?,
                line,
                "{store} archive"
            );
        }
        for line in lines(store, "log.jsonl") {
            assert_eq!(LogLine::from_line(&line)?.to_line()?, line, "{store} log");
        }
    }
    Ok(())
}

#[test]
fn ledger_reader_keeps_unknown_keys() -> R {
    let raw = concat!(
        r#"{"code":"xa","v":3,"config":{},"goals":[{"id":"xa-G01","rank":1,"title":"t","#,
        r#""finish":"","status":"active","maps":"","later":[1]}],"items":[],"tasks":[],"#,
        r#""next":{"G":"02","D":"01","A":"01","T":"01"},"future":{"k":true}}"#,
        "\n"
    );
    let state = State::from_json(raw)?;
    assert_eq!(state.goals.len(), 1);
    assert_eq!(state.to_json()?, raw);
    Ok(())
}

/// Critic round 1: a `stale_il` notice keeps the prototype's
/// `ts, session, kind, n, text` order (datg.py `stamp_turn`).
#[test]
fn ledger_stale_il_line_round_trips() -> R {
    let stale = r#"{"ts":"2026-01-01T00:00Z","session":"pm-1","kind":"stale_il","n":2,"text":"t"}"#;
    assert_eq!(LogLine::from_line(stale)?.to_line()?, stale);
    Ok(())
}

/// Critic round 1: an option object keeps unknown keys.
#[test]
fn ledger_option_extras_round_trip() -> R {
    let raw = concat!(
        r#"{"code":"xa","v":1,"config":{},"goals":[],"items":[{"id":"xa-D01","kind":"question","#,
        r#""status":"open","goal":"xa-G01","title":"t","opts":[{"l":"a","rec":true,"why":"x"}],"#,
        r#""answer":"","outcome":"","esc":null}],"tasks":[],"#,
        r#""next":{"G":"02","D":"02","A":"01","T":"01"}}"#,
        "\n"
    );
    assert_eq!(State::from_json(raw)?.to_json()?, raw);
    Ok(())
}

#[test]
fn ledger_request_refuses_unknown_keys() {
    let ok = r#"{"op":"add","kind":"D","title":"t","goal":"xa-G01","opts":[{"l":"a","rec":true}]}"#;
    assert!(serde_json::from_str::<Op>(ok).is_ok());
    for bad in [
        r#"{"op":"add","kind":"D","title":"t","goal":"xa-G01","opts":[{"label":"a"}]}"#,
        r#"{"op":"close","id":"xa-T01","outcome":"o","extra":1}"#,
        r#"{"op":"session","task":"t","waiting_on":[{"who":"owner","what":"w","x":1}]}"#,
        r#"{"op":"rename","id":"xa-T01"}"#,
    ] {
        assert!(serde_json::from_str::<Op>(bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn ledger_writer_refuses_newer_schema_and_leaves_bytes() -> R {
    let dir = tempfile::tempdir()?;
    let paths = StorePaths::new(dir.path(), "pm-1")?;
    fs::create_dir_all(&paths.dir)?;
    let mut raw = fs::read_to_string(fixture("pm-small", "state.json"))?;
    raw.insert_str(1, &format!(r#""schema":{},"#, ledger::SCHEMA_VERSION + 1));
    fs::write(&paths.state, &raw)?;
    let err = ledger::load_for_write(&paths);
    assert!(
        matches!(err, Err(LedgerError::NewerSchema { .. })),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&paths.state)?, raw);
    // A store without the key is schema 1 and writable.
    fs::write(&paths.state, fs::read(fixture("pm-small", "state.json"))?)?;
    assert_eq!(ledger::load_for_write(&paths)?.schema_version(), 1);
    Ok(())
}

#[test]
fn ledger_id_grammar_accepts_exactly_the_one_form() -> R {
    let id = LedgerId::parse("ab-G0f")?;
    assert_eq!(
        (id.code().as_str(), id.prefix(), id.number()),
        ("ab", Prefix::Goal, 0x0f)
    );
    assert_eq!(id.to_string(), "ab-G0f");
    assert!(LedgerId::parse("abcdefgh-B10").is_ok());
    // Exhaustive over a small alphabet: the parser agrees with the grammar
    // `^[a-z][a-z0-9]{1,7}-[GDATB][0-9a-f]{2}$` on every string.
    let alpha = ['a', '1', 'G', 'B', 'X', 'f', 'g', 'F', '-'];
    for a in alpha {
        for b in alpha {
            for c in alpha {
                for p in ['G', 'D', 'A', 'T', 'B', 'P', 'g'] {
                    let s = format!("{a}{b}-{p}{c}{a}");
                    let code_ok =
                        a.is_ascii_lowercase() && (b.is_ascii_lowercase() || b.is_ascii_digit());
                    let hex = |x: char| x.is_ascii_digit() || ('a'..='f').contains(&x);
                    let want = code_ok && "GDATB".contains(p) && hex(c) && hex(a);
                    assert_eq!(LedgerId::parse(&s).is_ok(), want, "{s}");
                    let bare = format!("{p}{c}{a}");
                    let bare_want = p.is_ascii_uppercase() && hex(c) && hex(a);
                    assert_eq!(ledger::is_bare_id(&bare), bare_want, "{bare}");
                }
            }
        }
    }
    for bad in [
        "",
        "a-G01",
        "Ab-G01",
        "abcdefghi-G01",
        "ab-G1",
        "ab-G001",
        "ab_G01",
        " ab-G01",
    ] {
        assert!(LedgerId::parse(bad).is_err(), "{bad}");
    }
    assert!(Code::parse("ab").is_ok() && Code::parse("1b").is_err());
    Ok(())
}

#[test]
fn ledger_title_and_paragraph_limits() -> R {
    assert_eq!(ledger::check_title(&"t".repeat(120))?, "t".repeat(120));
    assert_eq!(ledger::check_title("  padded  ")?, "padded");
    for bad in [
        "t".repeat(121),
        String::new(),
        "two\nlines".into(),
        "  ".into(),
    ] {
        assert!(
            matches!(ledger::check_title(&bad), Err(LedgerError::Invalid(_))),
            "{bad:?}"
        );
    }
    // 120 characters, not bytes.
    assert!(ledger::check_title(&"ü".repeat(120)).is_ok());
    assert!(ledger::check_paragraph("outcome", "one\nparagraph", 300).is_ok());
    assert!(ledger::check_paragraph("outcome", "two\n\nparagraphs", 300).is_err());
    assert!(ledger::check_paragraph("outcome", &"o".repeat(301), 300).is_err());
    // An over-long title in an add op is refused before any write.
    let op: Op = serde_json::from_str(&format!(
        r#"{{"op":"add","kind":"G","title":"{}"}}"#,
        "t".repeat(121)
    ))?;
    assert!(matches!(op.check(), Err(LedgerError::Invalid(_))));
    Ok(())
}

#[test]
fn ledger_size_bar_on_fixture_sizes() -> R {
    let size = |s: &str| fs::metadata(fixture(s, "state.json")).map(|m| m.len());
    let (small, warn, over) = (
        size("pm-small")?,
        size("pm-over-warn")?,
        size("pm-over-refuse")?,
    );
    assert_eq!(ledger::size_verdict(small, small + 10), SizeVerdict::Ok);
    assert_eq!(ledger::size_verdict(warn - 10, warn), SizeVerdict::Warn);
    // Refused only when over 16 KB AND larger than before.
    assert_eq!(ledger::size_verdict(over - 1, over), SizeVerdict::Refuse);
    assert_eq!(ledger::size_verdict(over, over - 1), SizeVerdict::Warn);
    assert_eq!(ledger::size_verdict(0, 16 * 1024), SizeVerdict::Warn);
    assert_eq!(ledger::size_verdict(0, 16 * 1024 + 1), SizeVerdict::Refuse);
    assert_eq!(ledger::size_verdict(0, 8 * 1024), SizeVerdict::Ok);
    Ok(())
}

#[test]
fn ledger_timestamps_normalize_to_seconds() -> R {
    assert_eq!(
        ledger::normalize_ts("2026-10-19T12:00Z")?,
        "2026-10-19T12:00:00Z"
    );
    assert_eq!(
        ledger::normalize_ts("2026-10-19T12:00:07Z")?,
        "2026-10-19T12:00:07Z"
    );
    for bad in [
        "2026-10-19 12:00Z",
        "2026-10-19T12:00",
        "2026-13-19T12:00Z",
        "2026-10-19T24:00Z",
        "",
    ] {
        assert!(ledger::normalize_ts(bad).is_err(), "{bad}");
    }
    Ok(())
}

fn live(_: &Registration) -> bool {
    true
}

#[test]
fn ledger_two_sessions_in_one_checkout_get_two_stores() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let reg = home.path().join("codes.json");
    let a = ledger::init_store(
        &reg,
        project.path(),
        "pm-1",
        "xa",
        "2026-10-21T00:00:00Z",
        &live,
    )?;
    let b = ledger::init_store(
        &reg,
        project.path(),
        "pm-2",
        "xb",
        "2026-10-21T00:00:00Z",
        &live,
    )?;
    assert_ne!(a.dir, b.dir);
    let mut sa = ledger::load_for_write(&a)?;
    let mut sb = ledger::load_for_write(&b)?;
    assert_eq!(sa.allocate(Prefix::Task, &[])?.to_string(), "xa-T01");
    assert_eq!(sa.allocate(Prefix::Task, &[])?.to_string(), "xa-T02");
    assert_eq!(sb.allocate(Prefix::Task, &[])?.to_string(), "xb-T01");
    // Same code and session again is a no-op that keeps the store.
    let before = fs::read(&a.state)?;
    ledger::init_store(
        &reg,
        project.path(),
        "pm-1",
        "xa",
        "2026-10-22T00:00:00Z",
        &live,
    )?;
    assert_eq!(fs::read(&a.state)?, before);
    Ok(())
}

#[test]
fn ledger_init_refuses_a_code_held_by_another_live_session() -> R {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let reg = home.path().join("codes.json");
    ledger::init_store(
        &reg,
        project.path(),
        "pm-1",
        "xa",
        "2026-10-21T00:00:00Z",
        &live,
    )?;
    let registry = fs::read(&reg)?;
    let err = ledger::init_store(
        &reg,
        project.path(),
        "pm-2",
        "xa",
        "2026-10-21T00:00:00Z",
        &live,
    );
    assert!(matches!(err, Err(LedgerError::CodeTaken { .. })), "{err:?}");
    assert_eq!(fs::read(&reg)?, registry);
    assert!(!StorePaths::new(project.path(), "pm-2")?.dir.exists());
    // A dead holder's code may be taken over.
    ledger::init_store(
        &reg,
        project.path(),
        "pm-2",
        "xa",
        "2026-10-21T00:00:00Z",
        &|_| false,
    )?;
    Ok(())
}

#[test]
fn ledger_store_anchors_to_the_launch_dir_from_a_worktree() -> R {
    let root = tempfile::tempdir()?;
    let main = root.path().join("repo");
    let wt = main.join(".claude/worktrees/w1");
    fs::create_dir_all(main.join(".git/worktrees/w1"))?;
    fs::create_dir_all(wt.join("crates/x"))?;
    let gitdir = main.join(".git/worktrees/w1");
    fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display()))?;
    assert_eq!(ledger::launch_dir(&wt.join("crates/x")), main);
    assert_eq!(ledger::launch_dir(&main.join(".claude")), main);
    let paths = StorePaths::new(&ledger::launch_dir(&wt), "pm-1")?;
    assert_eq!(paths.state, main.join(".trusty-mpm/dbatg/pm-1/state.json"));
    assert!(StorePaths::new(&main, "../escape").is_err());
    Ok(())
}

#[test]
fn ledger_rpc_types_use_the_mpm_prefix_and_round_trip() -> R {
    assert!(
        ledger::rpc::METHODS
            .iter()
            .all(|m| m.starts_with("mpm.dbatg."))
    );
    assert!(ledger::rpc::METHODS.contains(&"mpm.dbatg.batch"));
    let req: ledger::rpc::BatchRequest = serde_json::from_str(concat!(
        r#"{"store":{"project":"/p","session":"pm-1"},"request_id":"r1","if_v":4,"#,
        r#""ops":[{"op":"close","id":"xa-T01","outcome":"merged"},{"op":"prune"}]}"#
    ))?;
    assert_eq!((req.ops.len(), req.if_v), (2, Some(4)));
    let back: ledger::rpc::BatchRequest = serde_json::from_str(&serde_json::to_string(&req)?)?;
    assert_eq!(back, req);
    assert_eq!(ledger::rpc::RpcErrorCode::Invalid.exit_code(), 2);
    Ok(())
}

/// Every string in a fixture is a filler, a synthetic id or code, a
/// timestamp, a counter or an enum value; an unscrubbed id or title fails.
#[test]
fn ledger_fixtures_are_scrubbed() {
    const FILLER: [&str; 10] = [
        "lorem", "ipsum", "dolor", "sit", "amet", "elit", "sed", "tempor", "über", "magna",
    ];
    const ENUMS: [&str; 20] = [
        "open",
        "answered",
        "done",
        "running",
        "active",
        "parked",
        "question",
        "action",
        "architect",
        "owner",
        "ci",
        "agent",
        "other",
        "warn",
        "block",
        "item",
        "task",
        "update",
        "size_warning",
        "digest",
    ];
    let filler = |s: &str| {
        s.split([' ', '\n', '-'])
            .all(|t| FILLER.iter().any(|w| w.starts_with(t)))
    };
    let synthetic_id = |s: &str| {
        let rest = s
            .strip_prefix('x')
            .and_then(|r| r.get(1..))
            .and_then(|r| r.strip_prefix('-'));
        let tail = rest.unwrap_or(s);
        (rest.is_some() || !s.starts_with('x'))
            && (ledger::is_bare_id(tail) || tail == "init" || tail == "session")
    };
    let ok = |s: &str| {
        s.is_empty()
            || ledger::normalize_ts(s).is_ok()
            || filler(s)
            || synthetic_id(s)
            || (s.len() == 2 && s.starts_with('x'))
            || (s.len() == 2 && s.chars().all(|c| c.is_ascii_hexdigit()))
            || s.strip_prefix("pm-")
                .is_some_and(|n| n.parse::<u32>().is_ok())
            || ENUMS.contains(&s)
    };
    fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }
    for store in STORES {
        for file in ["state.json", "archive.jsonl", "log.jsonl"] {
            for (n, line) in lines(store, file).iter().enumerate() {
                let mut found = Vec::new();
                let value: serde_json::Value =
                    serde_json::from_str(line).expect("fixture line is JSON");
                strings(&value, &mut found);
                for s in found {
                    // Never print the value: an unscrubbed one is private.
                    let at = format!("{store}/{file} line {}", n + 1);
                    assert!(ok(&s), "{at}: unscrubbed {}-char string", s.chars().count());
                }
            }
        }
    }
}

/// Critic round 1: `scrub.py` renumbers ids per (code, prefix) in first-seen
/// order, so in each fixture store every id group is exactly `01..n` and its
/// `next` is `n + 1`. A raw live id (a gap, or a number past the count)
/// fails here.
#[test]
fn ledger_fixture_ids_are_dense_per_prefix() -> R {
    use std::collections::{BTreeMap, BTreeSet};
    fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }
    for store in STORES {
        let mut groups: BTreeMap<(String, char), BTreeSet<u32>> = BTreeMap::new();
        for file in ["state.json", "archive.jsonl", "log.jsonl"] {
            for line in lines(store, file) {
                let mut found = Vec::new();
                strings(&serde_json::from_str(&line)?, &mut found);
                for s in found {
                    let (code, tail) = match LedgerId::parse(&s) {
                        Ok(id) => (id.code().as_str().to_owned(), s[s.len() - 3..].to_owned()),
                        Err(_) if ledger::is_bare_id(&s) => (String::new(), s.clone()),
                        Err(_) => continue,
                    };
                    let letter = tail.chars().next().unwrap_or('?');
                    let n = u32::from_str_radix(&tail[1..], 16)?;
                    groups.entry((code, letter)).or_default().insert(n);
                }
            }
        }
        assert!(!groups.is_empty(), "{store}: no ids");
        for ((code, letter), nums) in &groups {
            let want: BTreeSet<u32> = (1..=nums.len() as u32).collect();
            assert_eq!(
                nums, &want,
                "{store}: {code}-{letter} ids are not dense 01..n"
            );
        }
        let state = State::from_json(&fs::read_to_string(fixture(store, "state.json"))?)?;
        let next = serde_json::to_value(&state.next)?;
        for letter in ['G', 'D', 'A', 'T'] {
            let used = groups.get(&(state.code.as_str().to_owned(), letter));
            let want = format!("{:02x}", used.map_or(0, BTreeSet::len) + 1);
            assert_eq!(next[letter.to_string()], want, "{store}: next {letter}");
        }
    }
    Ok(())
}

/// With TRUSTY_LEDGER_DENYLIST naming a file outside the repository (one
/// real value per line, written by `scrub.py --deny-out`), no fixture holds
/// any of its values. Unset, the structural check above is the CI gate.
#[test]
fn ledger_fixtures_carry_no_denied_value() -> R {
    let Some(path) = std::env::var_os("TRUSTY_LEDGER_DENYLIST") else {
        eprintln!("TRUSTY_LEDGER_DENYLIST unset: deny-list check skipped");
        return Ok(());
    };
    let deny = fs::read_to_string(path)?;
    for store in STORES {
        for file in ["state.json", "archive.jsonl", "log.jsonl"] {
            let text = fs::read_to_string(fixture(store, file)).unwrap_or_default();
            for value in deny.lines().filter(|l| !l.is_empty()) {
                assert!(!text.contains(value), "{store}/{file} holds a denied value");
            }
        }
    }
    Ok(())
}
