//! DOC-82 LDG-2.2.3 (#10235): `State::allocate` never issues a live id, reuses
//! an archived one, and a malformed `next` is an error, never a silent `01`.

use trusty_architect::ledger::{Code, LedgerError, Prefix, State};

type R = Result<(), Box<dyn std::error::Error>>;

/// A store with code `xa`, the given live task numbers and `next.T`.
fn store(live: &[u8], next_t: &str) -> Result<State, LedgerError> {
    let tasks: Vec<String> = live
        .iter()
        .map(|n| {
            format!(
                r#"{{"id":"xa-T{n:02x}","goal":"xa-G01","title":"t","status":"open","to":"","outcome":""}}"#
            )
        })
        .collect();
    State::from_json(&format!(
        r#"{{"code":"xa","v":1,"config":{{}},"goals":[],"items":[],"tasks":[{}],"next":{{"G":"01","D":"01","A":"01","T":"{next_t}"}}}}"#,
        tasks.join(",")
    ))
}

#[test]
fn allocate_takes_the_next_free_id() -> R {
    let mut st = State::new(Code::parse("xa")?);
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T01");
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T02");
    assert_eq!(st.allocate(Prefix::Goal)?.to_string(), "xa-G01");
    assert_eq!(st.next.task, "03");
    Ok(())
}

#[test]
fn allocate_wraps_past_ff_to_the_lowest_free_id() -> R {
    let mut st = store(&[0xfe, 0xff], "fe")?;
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T01");
    assert_eq!(st.next.task, "02");
    Ok(())
}

// #10235: ids are ephemeral; an archived id is not in the state, so it is free
// and a counter sitting on it hands it out again.
#[test]
fn allocate_reuses_an_id_that_left_the_state() -> R {
    let mut st = store(&[], "05")?;
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T05");
    Ok(())
}

#[test]
fn allocate_never_reissues_a_live_id() -> R {
    let mut st = store(&[5, 6], "05")?;
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T07");
    let mut st = store(&[1, 2, 3, 0xff], "ff")?;
    assert_eq!(st.allocate(Prefix::Task)?.to_string(), "xa-T04");
    Ok(())
}

#[test]
fn allocate_refuses_when_every_id_is_live() -> R {
    let live: Vec<u8> = (1..=0xff).collect();
    let mut st = store(&live, "01")?;
    let err = st.allocate(Prefix::Task);
    assert!(matches!(err, Err(LedgerError::NoFreeId('T'))), "{err:?}");
    assert_eq!(st.next.task, "01");
    Ok(())
}

#[test]
fn allocate_refuses_a_malformed_next() -> R {
    for bad in ["zz", "00", "1", "0x1", "FF", ""] {
        let mut st = store(&[], bad)?;
        let err = st.allocate(Prefix::Task);
        assert!(
            matches!(err, Err(LedgerError::Invalid(_))),
            "{bad:?}: {err:?}"
        );
        assert_eq!(st.next.task, bad, "next unchanged");
    }
    Ok(())
}
