//! Critic round 1 (#9756 P0, DOC-82 LDG-2.2.3): `State::allocate` refuses an
//! id that is live or archived, and a malformed `next` is an error, never a
//! silent `01`.

use trusty_architect::ledger::{Code, LedgerError, LedgerId, Prefix, State};

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

fn task(n: u8) -> Result<LedgerId, LedgerError> {
    Ok(LedgerId::new(Code::parse("xa")?, Prefix::Task, n))
}

#[test]
fn allocate_takes_the_next_free_id() -> R {
    let mut st = State::new(Code::parse("xa")?);
    assert_eq!(st.allocate(Prefix::Task, &[])?.to_string(), "xa-T01");
    assert_eq!(st.allocate(Prefix::Task, &[])?.to_string(), "xa-T02");
    assert_eq!(st.allocate(Prefix::Goal, &[])?.to_string(), "xa-G01");
    assert_eq!(st.next.task, "03");
    Ok(())
}

#[test]
fn allocate_wraps_past_ff_and_skips_archived_ids() -> R {
    let mut st = store(&[0xfe, 0xff], "fe")?;
    let archived = [task(1)?, task(2)?];
    assert_eq!(st.allocate(Prefix::Task, &archived)?.to_string(), "xa-T03");
    assert_eq!(st.next.task, "04");
    // An archived id at `next` is skipped without wrapping.
    let mut st = store(&[], "05")?;
    assert_eq!(
        st.allocate(Prefix::Task, &[task(5)?])?.to_string(),
        "xa-T06"
    );
    Ok(())
}

#[test]
fn allocate_refuses_when_every_id_is_live_or_archived() -> R {
    let live: Vec<u8> = (1..=0x80).collect();
    let mut st = store(&live, "01")?;
    let archived: Vec<LedgerId> = (0x81..=0xff).map(task).collect::<Result<_, _>>()?;
    let err = st.allocate(Prefix::Task, &archived);
    assert!(matches!(err, Err(LedgerError::NoFreeId('T'))), "{err:?}");
    assert_eq!(st.next.task, "01");
    Ok(())
}

#[test]
fn allocate_refuses_a_malformed_next() -> R {
    for bad in ["zz", "00", "1", "0x1", "FF", ""] {
        let mut st = store(&[], bad)?;
        let err = st.allocate(Prefix::Task, &[]);
        assert!(
            matches!(err, Err(LedgerError::Invalid(_))),
            "{bad:?}: {err:?}"
        );
        assert_eq!(st.next.task, bad, "next unchanged");
    }
    Ok(())
}
