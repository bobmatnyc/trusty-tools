//! Tests for the daemon question table (#8433 P2).
//!
//! Why: the table is a Fail-Open Check site. Each test goes red if an arm
//! that must refuse (second resolve, wrong party, poisoned table, second
//! opener, a log a correct table never writes) is changed to serve.
//! What: real files under a hermetic temp dir; faults through the
//! table's own test seam; the policy adapter through a real snapshot.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use trusty_channels::policy::Channel;

use super::{
    Answer, Ask, Conversation, Fault, QuestionTable, QuestionTableError, ResolveOutcome, sha256_hex,
};

const BOB: &str = "U0ABCDEF1";
const EVE: &str = "U0ABCDEF2";

/// A temp dir and the table path inside a not-yet-created `channels/`.
struct Fixture {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = crate::test_support::hermetic_temp_dir();
    let path = tmp.path().join("channels").join("questions.jsonl");
    Fixture { _tmp: tmp, path }
}

fn conv(dm: &str, ts: &str) -> Conversation {
    Conversation {
        dm_channel_id: dm.into(),
        thread_ts: ts.into(),
    }
}

fn bob_conv() -> Conversation {
    conv("D0BOB", "1700000000.000100")
}

fn ask_of(route: &str, recipient: &str, conversation: Conversation) -> Ask {
    Ask {
        channel: Channel::Slack,
        route: route.into(),
        recipient: recipient.into(),
        asker_managed_session: "tm-architect".into(),
        conversation,
        message_ts: "1700000000.000100".into(),
    }
}

fn bob_ask() -> Ask {
    ask_of("bob-dm", BOB, bob_conv())
}

// #8433: no retry on `Locked`; a dropped table releases its flock at once,
// even while a sibling test's forked child holds the descriptor (#9833).
fn open_at(path: &Path) -> QuestionTable {
    QuestionTable::open_at(path).expect("open table")
}

/// Reserve and open one question; its id.
fn ask(table: &QuestionTable, ask: Ask) -> u64 {
    let id = table.reserve().expect("reserve");
    table.open(id, ask).expect("open question");
    id
}

/// Every decoded log line.
fn log_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read log")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("log line is JSON"))
        .collect()
}

/// The `resolve` lines for `id`.
fn resolve_lines(path: &Path, id: u64) -> Vec<serde_json::Value> {
    log_lines(path)
        .into_iter()
        .filter(|v| v["op"] == "resolve" && v["id"] == id)
        .collect()
}

fn set_fault(table: &QuestionTable, fault: Option<Fault>) {
    table.state.lock().expect("state lock").fault = fault;
}

fn append_raw(path: &Path, text: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open log");
    f.write_all(text.as_bytes()).expect("append raw");
}

/// `rounds` questions; per round `threads` threads released together each
/// resolve it with their own answer text.
fn race(rounds: u64, threads: usize) {
    let fx = fixture();
    let table = Arc::new(open_at(&fx.path));
    let mut winners = Vec::new();
    for _ in 0..rounds {
        let id = ask(&table, bob_ask());
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|n| {
                let (table, barrier) = (Arc::clone(&table), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let text = format!("answer {id}/{n}");
                    barrier.wait();
                    (text.clone(), table.resolve(id, BOB, &bob_conv(), &text))
                })
            })
            .collect();
        let outcomes: Vec<(String, ResolveOutcome)> = handles
            .into_iter()
            .map(|h| h.join().expect("resolver thread"))
            .collect();
        let won: Vec<&String> = outcomes
            .iter()
            .filter(|(_, o)| *o == ResolveOutcome::Resolved)
            .map(|(t, _)| t)
            .collect();
        assert_eq!(won.len(), 1, "question {id}: {outcomes:?}");
        let winner = sha256_hex(won[0]);
        for (_, outcome) in outcomes
            .iter()
            .filter(|(_, o)| *o != ResolveOutcome::Resolved)
        {
            match outcome {
                ResolveOutcome::AlreadyResolved { first } => {
                    assert_eq!(
                        first.text_sha256, winner,
                        "question {id}: wrong winner named"
                    );
                }
                other => panic!("question {id}: loser got {other:?}"),
            }
        }
        winners.push((id, winner));
    }
    drop(table);
    let reopened = open_at(&fx.path);
    for (id, winner) in winners {
        let lines = resolve_lines(&fx.path, id);
        assert_eq!(lines.len(), 1, "question {id}: {lines:?}");
        assert_eq!(
            lines[0]["answer"]["text_sha256"],
            winner.as_str(),
            "question {id}"
        );
        assert!(reopened.state(id).expect("known").answered, "question {id}");
    }
}

#[test]
fn concurrent_answers_resolve_exactly_once_n_rounds() {
    race(500, 2);
    race(100, 8);
}

#[test]
fn concurrent_reserve_issues_unique_ids() {
    let fx = fixture();
    let table = Arc::new(open_at(&fx.path));
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (table, barrier) = (Arc::clone(&table), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                (0..100)
                    .map(|_| table.reserve().expect("reserve"))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut ids: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().expect("reserve thread"))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 800, "duplicate ids issued");
    let reserves = log_lines(&fx.path)
        .into_iter()
        .filter(|v| v["op"] == "reserve")
        .count();
    assert_eq!(reserves, 800);
}

#[test]
fn first_answer_wins_second_gets_already_answered_by_x_at_t() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    let line = resolve_lines(&fx.path, id).remove(0);
    let at = line["answer"]["answered_at"]
        .as_str()
        .expect("time")
        .to_owned();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let expected = Answer {
        answerer: BOB.into(),
        answered_at: at,
        text_sha256: sha256_hex("yes"),
        text_len: 3,
    };
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "no, wait"),
        ResolveOutcome::AlreadyResolved { first: expected }
    );
    assert_eq!(resolve_lines(&fx.path, id).len(), 1);
}

#[test]
fn answerer_must_be_the_questions_recipient() {
    let fx = fixture();
    let table = open_at(&fx.path);
    // The question was asked of U1 on route `bob-dm`; the route's recipient
    // later became U2, who answers in their own DM.
    let id = ask(&table, bob_ask());
    let before = std::fs::read(&fx.path).expect("log");
    assert_eq!(
        table.resolve(id, EVE, &conv("D0EVE", "1700000000.000100"), "yes"),
        ResolveOutcome::NotRecipient
    );
    // The recipient's id in the recipient's conversation from anyone else.
    assert_eq!(
        table.resolve(id, EVE, &bob_conv(), "yes"),
        ResolveOutcome::NotRecipient
    );
    assert_eq!(
        std::fs::read(&fx.path).expect("log"),
        before,
        "a line was appended"
    );
    assert!(!table.state(id).expect("known").answered, "question closed");
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
}

#[test]
fn answer_from_other_conversation_is_refused() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    for other in [
        conv("D0OTHER", "1700000000.000100"),
        conv("D0BOB", "1700000099.000100"),
    ] {
        assert_eq!(
            table.resolve(id, BOB, &other, "yes"),
            ResolveOutcome::NotRecipient,
            "{other:?}"
        );
    }
    assert!(resolve_lines(&fx.path, id).is_empty());
    assert!(!table.state(id).expect("known").answered);
}

#[test]
fn table_survives_restart() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let reserved_only = table.reserve().expect("reserve");
    let a = ask(&table, bob_ask());
    let b = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(a, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    let question_b = table.get(b).expect("b");
    drop(table);
    let table = open_at(&fx.path);
    assert!(table.state(a).expect("a").answered);
    assert!(!table.state(b).expect("b").answered);
    assert_eq!(table.get(b), Some(question_b));
    assert_eq!(table.state(reserved_only), None);
    assert_eq!(table.reserve().expect("reserve"), 4);
    assert!(matches!(
        table.resolve(a, BOB, &bob_conv(), "again"),
        ResolveOutcome::AlreadyResolved { .. }
    ));
}

#[test]
fn reserved_never_opened_id_is_not_reused_after_restart() {
    let fx = fixture();
    let id = open_at(&fx.path).reserve().expect("reserve");
    let table = open_at(&fx.path);
    assert_eq!(table.reserve().expect("reserve"), id + 1);
}

#[test]
fn id_reserved_before_send_is_never_reused() {
    let fx = fixture();
    let table = open_at(&fx.path);
    // The send outcome is unknown: the id stays reserved and no row exists.
    let unknown = table.reserve().expect("reserve");
    let next = ask(&table, bob_ask());
    assert_ne!(next, unknown);
    assert!(matches!(
        table.open(next, bob_ask()),
        Err(QuestionTableError::NotReserved(id)) if id == next
    ));
    drop(table);
    let table = open_at(&fx.path);
    let later = table.reserve().expect("reserve");
    assert!(later > next, "{later} reused");
}

#[test]
fn reply_to_reserved_only_id_is_unknown_and_drops() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = table.reserve().expect("reserve");
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Unknown
    );
    assert_eq!(table.state(id), None);
    assert!(resolve_lines(&fx.path, id).is_empty());
}

#[test]
fn open_refuses_unreserved_or_invalid_question() {
    let fx = fixture();
    let table = open_at(&fx.path);
    assert!(matches!(
        table.open(7, bob_ask()),
        Err(QuestionTableError::NotReserved(7))
    ));
    let id = table.reserve().expect("reserve");
    let mut gchat = bob_ask();
    gchat.channel = Channel::Gchat;
    let mut bad_session = bob_ask();
    bad_session.asker_managed_session = "../etc".into();
    let mut no_thread = bob_ask();
    no_thread.conversation.thread_ts.clear();
    for bad in [gchat, bad_session, no_thread] {
        assert!(
            matches!(
                table.open(id, bad.clone()),
                Err(QuestionTableError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert_eq!(table.state(id), None);
}

#[test]
fn torn_final_line_quarantined() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let body_len = std::fs::read(&fx.path).expect("log").len();
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    drop(table);
    // A crash mid-append: half of the resolve line, no newline.
    let full = std::fs::read(&fx.path).expect("log");
    let cut = body_len + (full.len() - body_len) / 2;
    std::fs::write(&fx.path, &full[..cut]).expect("tear");
    let table = open_at(&fx.path);
    assert_eq!(table.quarantined_bytes(), Some(cut - body_len));
    let torn = std::fs::read(fx.path.with_extension("jsonl.torn")).expect("torn file");
    assert_eq!(torn, [&full[body_len..cut], b"\n"].concat());
    assert_eq!(std::fs::read(&fx.path).expect("log"), &full[..body_len]);
    assert!(
        !table.state(id).expect("known").answered,
        "fragment applied"
    );
    // The next append starts its own line.
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    drop(table);
    assert_eq!(resolve_lines(&fx.path, id).len(), 1);
}

#[test]
fn torn_open_line_leaves_the_id_reserved() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = table.reserve().expect("reserve");
    let body_len = std::fs::read(&fx.path).expect("log").len();
    table.open(id, bob_ask()).expect("open");
    drop(table);
    let full = std::fs::read(&fx.path).expect("log");
    std::fs::write(&fx.path, &full[..body_len + 20]).expect("tear");
    let table = open_at(&fx.path);
    assert_eq!(table.quarantined_bytes(), Some(20));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Unknown
    );
    assert_eq!(table.reserve().expect("reserve"), id + 1);
}

#[test]
fn final_line_missing_only_newline_is_kept() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    drop(table);
    let mut full = std::fs::read(&fx.path).expect("log");
    assert_eq!(full.pop(), Some(b'\n'));
    std::fs::write(&fx.path, &full).expect("drop newline");
    let table = open_at(&fx.path);
    assert_eq!(table.quarantined_bytes(), None);
    assert!(table.state(id).expect("known").answered, "answer lost");
    assert!(!fx.path.with_extension("jsonl.torn").exists());
    assert!(std::fs::read(&fx.path).expect("log").ends_with(b"\n"));
}

/// Assert opening `path` fails as `Corrupt` at `line`.
fn assert_corrupt(path: &Path, line: usize, what: &str) {
    // #9833: wait out a forked child's copy of the dropped table's lock fd.
    match QuestionTable::open_at(path) {
        Err(QuestionTableError::Corrupt { line: l, .. }) => assert_eq!(l, line, "{what}"),
        other => panic!("{what}: opened: {other:?}"),
    }
}

#[test]
fn malformed_middle_line_fails_the_open() {
    let fx = fixture();
    let table = open_at(&fx.path);
    ask(&table, bob_ask());
    drop(table);
    let text = std::fs::read_to_string(&fx.path).expect("log");
    let mut lines: Vec<&str> = text.lines().collect();
    lines.insert(1, "{\"v\":1,\"op\":\"reserve\",\"id\":");
    std::fs::write(&fx.path, lines.join("\n") + "\n").expect("write");
    assert_corrupt(&fx.path, 2, "malformed middle line");
}

#[test]
fn resolve_for_unknown_id_in_log_fails_the_open() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    drop(table);
    let resolve = serde_json::to_string(&resolve_lines(&fx.path, id)[0]).expect("encode");
    let pristine = std::fs::read(&fx.path).expect("log");
    let cases = [
        (
            resolve.replace(&format!("\"id\":{id}"), "\"id\":99"),
            "unknown id",
        ),
        (resolve.clone(), "second resolve"),
    ];
    for (line, what) in cases {
        std::fs::write(&fx.path, &pristine).expect("restore");
        append_raw(&fx.path, &format!("{line}\n"));
        assert_corrupt(&fx.path, 4, what);
    }
}

#[test]
fn unknown_channel_or_gchat_row_fails_the_open() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = table.reserve().expect("reserve");
    drop(table);
    let pristine = std::fs::read(&fx.path).expect("log");
    for channel in ["gchat", "irc"] {
        std::fs::write(&fx.path, &pristine).expect("restore");
        let row = serde_json::json!({
            "v": 1, "op": "open", "question": {
                "id": id, "opened_at": "2026-10-12T00:00:00.000Z", "channel": channel,
                "route": "bob-dm", "recipient": BOB, "asker_managed_session": "tm-architect",
                "conversation": {"dm_channel_id": "D0BOB", "thread_ts": "1.2"},
                "message_ts": "1.2"
            }
        });
        append_raw(&fx.path, &format!("{row}\n"));
        assert_corrupt(&fx.path, 2, channel);
    }
}

/// `row` with `edit` applied.
fn edited(row: &serde_json::Value, edit: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
    let mut row = row.clone();
    edit(&mut row);
    row
}

/// Replace the log with `rows`, each newline-terminated.
fn write_log(path: &Path, rows: &[serde_json::Value]) {
    let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(path, text).expect("write log");
}

// #8433 r-078: each case goes red when its `Book::apply` check is removed.
#[test]
fn reopen_or_reused_reserve_in_log_fails_the_open() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    drop(table);
    let log = log_lines(&fx.path);
    let ops: Vec<_> = log.iter().map(|r| r["op"].clone()).collect();
    assert_eq!(ops, ["reserve", "open", "resolve"]);
    let (reserve, open, resolve) = (&log[0], &log[1], &log[2]);
    let reserve_id = |n: u64| edited(reserve, |r| r["id"] = n.into());
    // Each bad row sits mid-file: a row valid after it follows.
    let cases = [
        (
            vec![
                reserve.clone(),
                open.clone(),
                resolve.clone(),
                open.clone(),
                reserve_id(id + 1),
            ],
            4,
            "second open of an answered id",
        ),
        (
            vec![reserve_id(7), reserve_id(5), reserve_id(8)],
            2,
            "reserve below the next id",
        ),
    ];
    for (rows, line, what) in cases {
        write_log(&fx.path, &rows);
        assert_corrupt(&fx.path, line, what);
    }
}

// #8433 r-078: each case goes red when replay's `validate` or `decode`'s
// version check is removed.
#[test]
fn invalid_ask_or_unknown_version_in_log_fails_the_open() {
    let fx = fixture();
    let table = open_at(&fx.path);
    ask(&table, bob_ask());
    table.reserve().expect("reserve");
    drop(table);
    let log = log_lines(&fx.path);
    let ops: Vec<_> = log.iter().map(|r| r["op"].clone()).collect();
    assert_eq!(ops, ["reserve", "open", "reserve"]);
    let (reserve, open, after) = (&log[0], &log[1], &log[2]);
    let cases = [
        (
            edited(open, |o| {
                o["question"]["conversation"]["thread_ts"] = "".into();
            }),
            "empty thread_ts",
        ),
        (
            edited(open, |o| {
                o["question"]["asker_managed_session"] = "../x".into()
            }),
            "path in asker session id",
        ),
        (edited(open, |o| o["v"] = 2.into()), "record version 2"),
    ];
    for (bad, what) in cases {
        write_log(&fx.path, &[reserve.clone(), bad, after.clone()]);
        assert_corrupt(&fx.path, 2, what);
    }
}

#[test]
fn unknown_question_id_refuses() {
    let fx = fixture();
    let table = open_at(&fx.path);
    ask(&table, bob_ask());
    let before = std::fs::read(&fx.path).expect("log");
    assert_eq!(
        table.resolve(42, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Unknown
    );
    assert_eq!(table.state(42), None);
    assert_eq!(std::fs::read(&fx.path).expect("log"), before);
}

#[test]
fn foreign_route_id_refuses() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let bobs = ask(&table, bob_ask());
    let eves = ask(&table, ask_of("eve-dm", EVE, conv("D0EVE", "1.1")));
    // Eve answers Bob's question from her own conversation, and the reverse.
    assert_eq!(
        table.resolve(bobs, EVE, &conv("D0EVE", "1.1"), "yes"),
        ResolveOutcome::NotRecipient
    );
    assert_eq!(
        table.resolve(eves, BOB, &bob_conv(), "yes"),
        ResolveOutcome::NotRecipient
    );
    assert!(!table.state(bobs).expect("bob's").answered);
    assert!(!table.state(eves).expect("eve's").answered);
}

/// Every operation refuses on `table`.
fn assert_refuses_everything(table: &QuestionTable, id: u64, reserved: u64, what: &str) {
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "later"),
        ResolveOutcome::Poisoned,
        "{what}: resolve"
    );
    assert!(
        matches!(table.reserve(), Err(QuestionTableError::Poisoned)),
        "{what}: reserve"
    );
    assert!(
        matches!(
            table.open(reserved, bob_ask()),
            Err(QuestionTableError::Poisoned)
        ),
        "{what}: open"
    );
    assert_eq!(table.state(id), None, "{what}: state");
    assert!(table.is_poisoned(), "{what}: is_poisoned");
}

#[test]
fn append_error_poisons_table_until_reopen() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    set_fault(&table, Some(Fault::BeforeWrite));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Poisoned
    );
    set_fault(&table, None);
    assert_refuses_everything(&table, id, reserved, "after a failed append");
    drop(table);
    // The reopen replays the log, which holds no answer.
    let table = open_at(&fx.path);
    assert!(!table.is_poisoned());
    assert!(!table.state(id).expect("known").answered);
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
}

#[test]
fn failed_resolve_leaves_question_open_in_memory_only_if_log_agrees() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    // The line reaches the file, then `sync_data` fails.
    set_fault(&table, Some(Fault::AfterWrite));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "first"),
        ResolveOutcome::Poisoned
    );
    set_fault(&table, None);
    assert_refuses_everything(&table, id, reserved, "after a failed sync");
    drop(table);
    let table = open_at(&fx.path);
    let lines = resolve_lines(&fx.path, id);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        lines[0]["answer"]["text_sha256"],
        sha256_hex("first").as_str()
    );
    match table.resolve(id, BOB, &bob_conv(), "second") {
        ResolveOutcome::AlreadyResolved { first } => {
            assert_eq!(first.text_sha256, sha256_hex("first"));
        }
        other => panic!("second answer after reopen: {other:?}"),
    }
}

#[test]
fn second_table_on_same_path_is_refused_while_first_lives() {
    let fx = fixture();
    let first = open_at(&fx.path);
    assert!(matches!(
        QuestionTable::open_at(&fx.path),
        Err(QuestionTableError::Locked { .. })
    ));
    let id = ask(&first, bob_ask());
    assert!(matches!(
        QuestionTable::open_at(&fx.path),
        Err(QuestionTableError::Locked { .. })
    ));
    drop(first);
    // lock_released_on_drop
    let second = open_at(&fx.path);
    assert!(second.state(id).is_some());
}

/// A forked child that holds a copy of every descriptor open at the fork,
/// stopped before exec until released (#9833).
struct HeldChild {
    spawner: std::thread::JoinHandle<std::process::ExitStatus>,
    go_tx: std::io::PipeWriter,
}

impl HeldChild {
    /// Fork the child from another thread; returns once it has forked.
    fn fork() -> Self {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let (mut ready_rx, ready_tx) = std::io::pipe().expect("ready pipe");
        let (go_rx, go_tx) = std::io::pipe().expect("go pipe");
        let (ready_fd, go_rx_fd, go_tx_fd) =
            (ready_tx.as_raw_fd(), go_rx.as_raw_fd(), go_tx.as_raw_fd());
        let mut child = std::process::Command::new("true");
        // SAFETY: the hook runs between fork and exec and makes only the
        // async-signal-safe calls close, write and read on inherited fds.
        unsafe {
            child.pre_exec(move || {
                // Our own copy of the release end closes, so a parent that dies
                // reads as EOF here and the child still execs.
                libc::close(go_tx_fd);
                libc::write(ready_fd, [1u8].as_ptr().cast(), 1);
                let mut byte = 0u8;
                libc::read(go_rx_fd, (&raw mut byte).cast(), 1);
                Ok(())
            });
        }
        let spawner = std::thread::spawn(move || child.status().expect("spawn child"));
        ready_rx.read_exact(&mut [0u8; 1]).expect("child forked");
        Self { spawner, go_tx }
    }

    /// Let the child exec after `delay`; the handle yields its exit status.
    fn release_after(self, delay: Duration) -> std::thread::JoinHandle<std::process::ExitStatus> {
        use std::io::Write as _;
        let Self { spawner, mut go_tx } = self;
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            go_tx.write_all(&[1]).expect("release child");
            spawner.join().expect("spawner thread")
        })
    }
}

/// 🔴 #8433: a dropped table releases its flock explicitly, so a forked
/// child's copy of the lock descriptor does not hold it; the reopen takes
/// the lock at once, with no retry. Before the fix it was `Locked` until
/// the child exec'd (#9833).
#[test]
fn dropped_table_releases_its_flock_despite_a_forked_child() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let child = HeldChild::fork();
    drop(table);
    let reopened = QuestionTable::open_at(&fx.path);
    let released = child.release_after(Duration::ZERO);
    assert!(released.join().expect("release thread").success());
    let reopened = reopened.expect("a forked child's lock descriptor held a dropped table's flock");
    assert!(reopened.state(id).is_some());
}

/// 🔴 #8433: a table releases its flock when it poisons, so a reopen takes
/// the lock while stale `Arc` holders still run, and those holders refuse
/// every operation, so the reopened table is the only writer.
#[test]
fn poisoned_table_releases_its_flock_while_arc_held() {
    let fx = fixture();
    let table = Arc::new(open_at(&fx.path));
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    let stale = Arc::clone(&table);
    set_fault(&table, Some(Fault::BeforeWrite));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Poisoned
    );
    set_fault(&table, None);
    drop(table);
    let reopened = QuestionTable::open_at(&fx.path)
        .expect("a poisoned table's stale holder kept the flock from the reopen");
    assert_refuses_everything(&stale, id, reserved, "a stale holder after the reopen");
    assert_eq!(
        reopened.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    assert_eq!(resolve_lines(&fx.path, id).len(), 1);
}

/// #9842: a reopen right after a drop reports `Unsealed`, not `Locked`, while
/// a forked child holds the lock descriptor; the flock is taken before the
/// seal check. #8433: the dropped table released it, so no wait is needed.
#[test]
fn unsealed_reason_is_not_masked_by_a_forked_childs_lock_descriptor() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = fixture();
    let table = open_at(&fx.path);
    let child = HeldChild::fork();
    drop(table);
    std::fs::set_permissions(&fx.path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let reopened = QuestionTable::open_at(&fx.path).map(drop);
    let released = child.release_after(Duration::ZERO);
    assert!(released.join().expect("release thread").success());
    match reopened {
        Err(QuestionTableError::Unsealed { reason, .. }) => {
            assert_eq!(reason, "mode grants access beyond the owner");
        }
        other => panic!("expected Unsealed with a forked child alive, got {other:?}"),
    }
}

/// 🔴 #8433 Fail-Open Check: an unlock that fails at the poison transition
/// leaves the table poisoned and its flock held, so the reopen is refused
/// (`Locked`) until the table drops, and nothing is served meanwhile.
#[test]
fn failed_unlock_at_poison_keeps_the_table_refusing_and_locked() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    {
        let mut state = table.state.lock().expect("state lock");
        state.fault = Some(Fault::BeforeWrite);
        state.unlock_fails = true;
    }
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Poisoned
    );
    set_fault(&table, None);
    assert_refuses_everything(&table, id, reserved, "after a failed unlock");
    assert!(matches!(
        QuestionTable::open_at(&fx.path),
        Err(QuestionTableError::Locked { .. })
    ));
    drop(table);
    assert!(open_at(&fx.path).state(id).is_some());
}

#[test]
fn poisoned_mutex_refuses_every_op() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = table.state.lock().expect("state lock");
        panic!("a panic inside a held section");
    }));
    assert!(panicked.is_err());
    assert_refuses_everything(&table, id, reserved, "after a panic under the lock");
    assert!(resolve_lines(&fx.path, id).is_empty());
    // #8433: a poisoned mutex released the flock; the reopen takes it now.
    let reopened = QuestionTable::open_at(&fx.path).expect("reopen while the table lives");
    assert!(reopened.state(id).is_some());
}

#[test]
fn file_is_0600_and_dir_0700() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    let fx = fixture();
    let table = open_at(&fx.path);
    ask(&table, bob_ask());
    assert_eq!(mode(fx.path.parent().expect("dir")), 0o700, "directory");
    assert_eq!(mode(&fx.path), 0o600, "log");
    assert_eq!(mode(&fx.path.with_extension("jsonl.lock")), 0o600, "lock");
    // #9815: an existing wider log no longer opens (narrowed before P2a);
    // `wider_mode_log_refuses_to_open` covers it.
}

#[test]
fn home_write_fence_is_called() {
    let root = crate::core::home_write_fence::armed_roots()
        .first()
        .expect("the lib test binary arms the fence")
        .clone();
    let dir = root.join(format!("p2-fence-{}", uuid::Uuid::new_v4()));
    let path = dir.join("questions.jsonl");
    let caught = std::panic::catch_unwind(|| QuestionTable::open_at(&path).map(drop));
    let written = dir.exists();
    if written {
        // The fence was bypassed: undo the write before failing.
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        !written,
        "open wrote under the fenced root {}",
        root.display()
    );
    let payload = caught.expect_err("open under a fenced root did not panic");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(
        message.contains("#8545"),
        "not the fence's panic: {message}"
    );
}

#[test]
fn resolve_is_durable_before_it_returns() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "ship it"),
        ResolveOutcome::Resolved
    );
    // A fresh reader, with the table still open, sees the answer.
    let lines = resolve_lines(&fx.path, id);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["answer"]["answerer"], BOB);
    assert_eq!(
        lines[0]["answer"]["text_sha256"],
        sha256_hex("ship it").as_str()
    );
    assert!(
        !std::fs::read_to_string(&fx.path)
            .expect("log")
            .contains("ship it"),
        "answer text stored"
    );
}

/// A policy with `bob-dm` (BOB) and `eve-dm` (EVE) on Slack.
struct TwoRoutes(PathBuf);

impl crate::channels::snapshot::PolicySource for TwoRoutes {
    fn refresh(&mut self) -> Option<trusty_channels::policy::LoadReport> {
        use trusty_channels::policy::{ProjectInput, merge_for, parse_host, parse_project_file};
        let home = &self.0;
        let dir = home.join("proj");
        let mut routes = String::from("version = 2\n");
        for (name, who) in [("bob-dm", BOB), ("eve-dm", EVE)] {
            routes.push_str(&format!(
                "\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{who}\"\nkinds = [\"question\"]\n"
            ));
        }
        let host = format!(
            "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: slack, \
             app_ref: slack-app }}\n    projects: [\"{}\"]\n",
            dir.display()
        );
        Some(merge_for(
            parse_host(&host, Some(home)),
            vec![ProjectInput {
                file: dir.join(".trusty-channels/routes.toml"),
                project_dir: dir,
                parsed: parse_project_file(&routes, Some(home)),
            }],
            &[Channel::Slack, Channel::Telegram],
        ))
    }
}

#[test]
fn question_state_adapter_matches_policy() {
    use crate::channels::snapshot::{PolicySnapshot, Refusal};
    use trusty_channels::policy::{DropReason, MonotonicClock};
    let fx = fixture();
    let home = std::fs::canonicalize(fx._tmp.path()).expect("canonical home");
    let snap = PolicySnapshot::new(TwoRoutes(home), MonotonicClock::new());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Err(e) = snap.current() {
        assert!(
            std::time::Instant::now() < deadline,
            "policy never loaded: {e:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    let reserved = table.reserve().expect("reserve");
    let inbound = |sender: &str, id: u64| {
        let states: Vec<_> = table.state(id).into_iter().collect();
        snap.check_inbound(Channel::Slack, sender, &[id], &states)
            .map(|(route, q)| (route.name().to_owned(), q))
    };
    assert_eq!(inbound(BOB, id), Ok(("bob-dm".to_owned(), id)));
    assert_eq!(
        inbound(EVE, id),
        Err(Refusal::Policy(DropReason::NotOwnedByRoute))
    );
    assert_eq!(
        inbound(BOB, reserved),
        Err(Refusal::Policy(DropReason::NoOpenQuestion))
    );
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    assert_eq!(
        inbound(BOB, id),
        Err(Refusal::Policy(DropReason::AlreadyResolved))
    );
}

#[cfg(feature = "daemon")]
#[tokio::test]
async fn table_open_failure_refuses_ask_owner_and_inbound() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = crate::daemon::DaemonState::with_root_isolated_managed(tmp.path().into()).await;
    let home = state.user_home().expect("pinned home");
    let path = super::questions_path(&home);
    let dir = path.parent().expect("channels dir");
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    for attempt in 0..2 {
        assert!(
            matches!(state.question_table(), Err(QuestionTableError::Io { .. })),
            "attempt {attempt}: a table opened in a read-only directory"
        );
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    // The failed open was not cached: this call retries and succeeds.
    let table = state.question_table().expect("retried open");
    assert!(Arc::ptr_eq(&table, &state.question_table().expect("held")));
}

#[cfg(feature = "daemon")]
#[tokio::test]
async fn poisoned_table_is_reopened_by_the_holder() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = crate::daemon::DaemonState::with_root_isolated_managed(tmp.path().into()).await;
    let table = state.question_table().expect("open");
    let id = ask(&table, bob_ask());
    set_fault(&table, Some(Fault::AfterWrite));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Poisoned
    );
    // #8433: the poisoned table still lives here, but it released its flock
    // when it poisoned, so the holder reopens it at once.
    let reopened = state
        .question_table()
        .expect("reopen beside a stale holder");
    assert!(!Arc::ptr_eq(&reopened, &table));
    assert!(!reopened.is_poisoned());
    assert!(reopened.state(id).expect("replayed").answered);
    // The stale holder refuses, so the reopened table is the only writer.
    assert!(matches!(table.reserve(), Err(QuestionTableError::Poisoned)));
}

// ── #9815 (P2a of #8433): the log and its lock open only as sealed ──────────

/// The `Unsealed` reason `open_at(path)` fails with, naming `want`.
fn unsealed_reason(path: &Path, want: &Path) -> &'static str {
    match QuestionTable::open_at(path) {
        Err(QuestionTableError::Unsealed { path, reason }) => {
            assert_eq!(path, want, "the refusal names the unsealed file");
            reason
        }
        other => panic!("expected Unsealed for {}, got {other:?}", want.display()),
    }
}

/// Create the table's `channels/` directory, 0700, as `open_at` would.
fn make_dir(fx: &Fixture) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = fx.path.parent().expect("dir").to_path_buf();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .expect("mkdir");
    dir
}

/// 🔴 #9815 (r-078): a log replaced by a symlink is refused, and nothing is
/// read or written through it.
#[test]
fn symlinked_log_refuses_to_open() {
    let fx = fixture();
    let dir = make_dir(&fx);
    let elsewhere = dir.parent().expect("root").join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "").expect("target");
    std::fs::set_permissions(
        &elsewhere,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .expect("chmod");
    std::os::unix::fs::symlink(&elsewhere, &fx.path).expect("symlink");
    assert_eq!(
        unsealed_reason(&fx.path, &fx.path),
        "not a regular file (a symlink, directory or device)"
    );
    assert_eq!(std::fs::read(&elsewhere).expect("target"), b"");
}

/// 🔴 #9815 (r-078): a log with group, other or special mode bits is
/// refused, not narrowed.
#[test]
fn wider_mode_log_refuses_to_open() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = fixture();
    drop(open_at(&fx.path));
    for mode in [0o644, 0o620, 0o4600] {
        std::fs::set_permissions(&fx.path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        assert_eq!(
            unsealed_reason(&fx.path, &fx.path),
            "mode grants access beyond the owner",
            "mode {mode:o}"
        );
        let now = std::fs::metadata(&fx.path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(now, mode, "the refused open changed the mode");
    }
}

/// 🔴 #9815 (r-078): a log with a second hard link — a name outside the
/// sealed path that writes the same file — is refused.
#[test]
fn hard_linked_log_refuses_to_open() {
    let fx = fixture();
    drop(open_at(&fx.path));
    let link = fx
        .path
        .parent()
        .expect("dir")
        .parent()
        .expect("root")
        .join("alias.jsonl");
    std::fs::hard_link(&fx.path, &link).expect("hard link");
    assert_eq!(
        unsealed_reason(&fx.path, &fx.path),
        "hard-linked under another name"
    );
    std::fs::remove_file(&link).expect("unlink alias");
    drop(open_at(&fx.path));
}

/// 🔴 #9815 (r-078): a lock replaced by a symlink is refused before it is
/// opened, so the flock is never taken on a file outside the seal.
#[test]
fn symlinked_lock_refuses_to_open() {
    let fx = fixture();
    let dir = make_dir(&fx);
    let elsewhere = dir.parent().expect("root").join("elsewhere.lock");
    std::os::unix::fs::symlink(&elsewhere, super::lock_path(&fx.path)).expect("symlink");
    assert_eq!(
        unsealed_reason(&fx.path, &super::lock_path(&fx.path)),
        "not a regular file (a symlink, directory or device)"
    );
    assert!(
        !elsewhere.exists(),
        "the lock was created through the symlink"
    );
}

/// 🔴 #9815 (r-078): a lock with group or other access is refused.
#[test]
fn wider_mode_lock_refuses_to_open() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = fixture();
    drop(open_at(&fx.path));
    let lock = super::lock_path(&fx.path);
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    assert_eq!(
        unsealed_reason(&fx.path, &lock),
        "mode grants access beyond the owner"
    );
}

/// 🔴 #9815 (r-078): a file owned by another user is refused; the file the
/// daemon leaves (regular, its own, 0600, one link) passes.
#[test]
fn seal_refuses_a_file_owned_by_another_user() {
    use super::seal::{Stat, verdict};
    let sealed = Stat {
        regular: true,
        uid: 501,
        mode: 0o100_600,
        nlink: 1,
    };
    assert_eq!(verdict(sealed, 501), Ok(()));
    assert_eq!(
        verdict(Stat { uid: 0, ..sealed }, 501),
        Err("owned by another user")
    );
}

/// A table with one question and a torn final line, so the next open moves
/// that line to the torn file; the log's bytes.
fn table_with_torn_tail(fx: &Fixture) -> Vec<u8> {
    let table = open_at(&fx.path);
    ask(&table, bob_ask());
    drop(table);
    append_raw(&fx.path, "{\"v\":1,\"op\":\"res");
    std::fs::read(&fx.path).expect("log")
}

/// 🔴 #9815 (r-078): a symlink planted at the torn-line file is refused by
/// the seal, and the torn-line append itself does not follow one (a link
/// swapped in after the seal check): the link's target is untouched.
#[test]
fn torn_file_symlink_is_not_followed() {
    let fx = fixture();
    let log = table_with_torn_tail(&fx);
    let outside = fx
        .path
        .parent()
        .expect("dir")
        .parent()
        .expect("root")
        .join("outside");
    std::fs::write(&outside, "keep\n").expect("outside");
    let torn = super::torn_path(&fx.path);
    std::os::unix::fs::symlink(&outside, &torn).expect("symlink");
    assert_eq!(
        unsealed_reason(&fx.path, &torn),
        "not a regular file (a symlink, directory or device)"
    );
    assert!(
        matches!(
            super::append_once(&torn, b"x\n"),
            Err(QuestionTableError::Io { .. })
        ),
        "the torn-line append followed a symlink"
    );
    assert_eq!(std::fs::read(&outside).expect("outside"), b"keep\n");
    assert_eq!(
        std::fs::read(&fx.path).expect("log"),
        log,
        "the log was cut"
    );
}

/// How long a FIFO test lets an open run before calling it blocked.
const FIFO_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// Make a 0600 FIFO at `path`: owned, one link, but not a regular file.
fn mkfifo(path: &Path) {
    use nix::sys::stat::Mode;
    nix::unistd::mkfifo(path, Mode::S_IRUSR | Mode::S_IWUSR).expect("mkfifo");
}

/// Run `f` on its own thread and return its result, failing the test if it
/// has not returned within [`FIFO_BOUND`]. On a timeout a reader is opened on
/// `fifo`, which releases a writer blocked there, so the thread ends and the
/// suite does not hang.
fn bounded_on_fifo<T: Send + 'static>(fifo: &Path, f: impl FnOnce() -> T + Send + 'static) -> T {
    use std::os::unix::fs::OpenOptionsExt as _;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(FIFO_BOUND) {
        Ok(out) => out,
        Err(_) => {
            let _reader = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo);
            let _ = rx.recv_timeout(FIFO_BOUND);
            panic!(
                "the open blocked on the FIFO at {} for {FIFO_BOUND:?}",
                fifo.display()
            );
        }
    }
}

/// 🔴 #9815 (r-078): a FIFO at the torn-line file is refused by the seal,
/// fast, and the log keeps its torn tail. Before the fix the open blocked
/// forever: a FIFO with no reader blocks its writer.
#[test]
fn fifo_at_the_torn_file_refuses_to_open_without_blocking() {
    let fx = fixture();
    let log = table_with_torn_tail(&fx);
    let torn = super::torn_path(&fx.path);
    mkfifo(&torn);
    let path = fx.path.clone();
    let opened = bounded_on_fifo(&torn, move || QuestionTable::open_at(&path).map(drop));
    match opened {
        Err(QuestionTableError::Unsealed { path, reason }) => {
            assert_eq!(path, torn, "the refusal names the torn file");
            assert_eq!(
                reason,
                "not a regular file (a symlink, directory or device)"
            );
        }
        other => panic!("expected Unsealed for the torn FIFO, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&fx.path).expect("log"),
        log,
        "the log was cut"
    );
}

/// 🔴 #9815 (r-078): the torn-line append fails fast on a FIFO swapped in
/// after the seal check, instead of blocking for a reader.
#[test]
fn torn_line_append_on_a_fifo_fails_fast() {
    let fx = fixture();
    let dir = make_dir(&fx);
    let fifo = dir.join("questions.jsonl.torn");
    mkfifo(&fifo);
    let target = fifo.clone();
    let appended = bounded_on_fifo(&fifo, move || super::append_once(&target, b"x\n"));
    assert!(
        matches!(appended, Err(QuestionTableError::Io { .. })),
        "the torn-line append on a FIFO: {appended:?}"
    );
}

/// 🔴 #9815 (r-078): an `lstat` that fails for a reason other than absence
/// (here a `channels/` the daemon cannot search) is `Io`, never "absent".
#[test]
fn seal_reports_an_unreadable_directory_as_io() {
    use std::os::unix::fs::PermissionsExt as _;
    // Root reads through a mode-000 directory, so the lstat would succeed.
    if nix::unistd::geteuid().is_root() {
        return;
    }
    /// Restores the directory's mode on every exit, so cleanup can run.
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let fx = fixture();
    drop(open_at(&fx.path));
    let dir = fx.path.parent().expect("dir").to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let _restore = Restore(dir);
    let checked = super::seal::check_path(&fx.path);
    assert!(
        matches!(&checked, Err(QuestionTableError::Io { path, .. }) if *path == fx.path),
        "an unreadable directory: {checked:?}"
    );
}

/// 🔴 #9873 (Q2): an answer binds by the conversation its question was
/// posted in. Another thread or channel binds nothing, a reserved-only id is
/// never found, and an answered question is still found, so a second answer
/// reaches `AlreadyResolved` rather than "no question".
#[test]
fn conversation_lookup_finds_the_question_posted_there() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let d1t1 = conv("D1", "1700000001.000100");
    let id = ask(&table, ask_of("bob-dm", BOB, d1t1.clone()));
    let _reserved = table.reserve().expect("reserve");
    assert_eq!(table.find_by_conversation(&d1t1), Some(id));
    assert_eq!(
        table.find_by_conversation(&conv("D1", "1700000002.000100")),
        None,
        "another thread in the same DM"
    );
    assert_eq!(
        table.find_by_conversation(&conv("D2", "1700000001.000100")),
        None,
        "the same ts in another DM"
    );
    assert_eq!(table.answer(id), None, "unanswered");
    assert_eq!(
        table.resolve(id, BOB, &d1t1, "yes"),
        ResolveOutcome::Resolved
    );
    assert_eq!(table.find_by_conversation(&d1t1), Some(id), "answered");
    let first = table.answer(id).expect("the first answer");
    assert_eq!(first.answerer, BOB);
    assert_eq!(first.text_sha256, sha256_hex("yes"));
}

/// 🔴 #9873: a poisoned table binds nothing and reports no answer, so the
/// inbound path drops rather than reading a book memory may not match.
#[test]
fn conversation_lookup_on_a_poisoned_table_is_none() {
    let fx = fixture();
    let table = open_at(&fx.path);
    let id = ask(&table, bob_ask());
    assert_eq!(table.find_by_conversation(&bob_conv()), Some(id));
    assert_eq!(
        table.resolve(id, BOB, &bob_conv(), "yes"),
        ResolveOutcome::Resolved
    );
    set_fault(&table, Some(Fault::BeforeWrite));
    assert!(table.reserve().is_err(), "the failed append poisons");
    set_fault(&table, None);
    assert_eq!(table.find_by_conversation(&bob_conv()), None);
    assert_eq!(table.answer(id), None);
}
