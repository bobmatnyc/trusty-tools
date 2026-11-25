//! One ledger: the locked read-modify-write cycle and its single commit.
//!
//! Why: `datg.py` runs every write as lock, load, apply on a copy, `commit`. The
//! commit orders its effects archive, state, log so a crash leaves at worst an
//! orphan archive line (dropped by the next lock) or a missing log line, never a
//! half-written state. The port keeps that order and adds what the Python lacks:
//! a bounded lock, validation before the write, a directory fsync, and a log
//! append that is one `write`.
//! What: [`Ledger`] with [`Ledger::write_ops`] (batch of ops, one commit),
//! [`Ledger::init`], [`Ledger::prune`], [`Ledger::rotate`] and the `commit`.
//! A failure at any step is returned, never downgraded: a state that advanced
//! while its log line failed reports exactly that.
//! Test: `tests/il_reliability.rs` (`lock_held_changes_nothing`,
//! `corrupt_state_is_refused_untouched`, `failed_rename_leaves_state_and_reconciles_archive`,
//! `failed_log_append_reports_the_advanced_state`), `binary_round_trip`.

use super::act::act_record;
use super::error::{IlError, IlResult, invalid};
use super::json::{Json, obj};
use super::log::answered_times;
use super::ops::{Ctx, Ev, apply_op, collect_closed};
use super::prune::{archive_lookup, prune_state, reconcile_archive, rotate_plan};
use super::schema::validate;
use super::size::{SIZE_REFUSE, SIZE_WARN, size_report};
use super::store::{
    LOCK_WAIT, LockGuard, Paths, append_lines, atomic_write, load_state, lock, size_of,
};
use super::time::fmt_day;
use std::time::Duration;

/// A step a test makes fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The state replace fails after the temp file is written.
    Rename,
    /// The archive append fails.
    Archive,
    /// The log append fails after the state advanced.
    Log,
}

/// One ledger directory and how to write it.
#[derive(Debug, Clone)]
pub struct Ledger {
    /// The files.
    pub paths: Paths,
    /// How long a writer waits for the lock.
    pub wait: Duration,
    /// A failure to inject (tests only).
    pub fault: Option<Fault>,
}

impl Ledger {
    /// A ledger over `paths` with the default lock wait.
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            wait: LOCK_WAIT,
            fault: None,
        }
    }

    fn fail(&self, f: Fault) -> bool {
        self.fault == Some(f)
    }

    /// `Locked.__enter__`: the exclusive lock, then the archive reconcile.
    ///
    /// # Errors
    /// `Locked` past the wait bound; `Io`.
    pub fn locked(&self) -> IlResult<LockGuard> {
        let g = lock(&self.paths, self.wait)?;
        reconcile_archive(&self.paths, false)?;
        Ok(g)
    }

    /// `load_state` plus the full schema check, for a write.
    ///
    /// # Errors
    /// `Invalid` when missing; `Corrupt` when malformed or off-schema.
    pub fn load_for_write(&self) -> IlResult<Json> {
        let st = load_state(&self.paths)?;
        validate(&st)?;
        Ok(st)
    }

    /// `commit`: bumps `v`, stamps the session, archives closed items, replaces `state.json`, appends the log.
    /// `tail_log` (already rendered lines, e.g. `act` records) goes in the same log write after the events,
    /// which is the byte order `datg.py` produces with two appends, for one fsync instead of two.
    ///
    /// # Errors
    /// `Invalid` for a state over the size bar; `Corrupt` when the result fails the schema;
    /// `Io` for a failed step. Nothing is written when the error precedes the archive step.
    pub fn commit(
        &self,
        ctx: &Ctx,
        st: &mut Json,
        events: &[Ev],
        mut archived: Vec<(String, Json)>,
        tail_log: &str,
    ) -> IlResult<()> {
        let old = size_of(&self.paths.state) as usize;
        let v = st.i("v").unwrap_or(0) + 1;
        st.set("v", v);
        if !matches!(st.get("session"), Some(Json::Obj(_))) {
            st.set("session", Json::Obj(Vec::new()));
        }
        if let Some(ses) = st.get_mut("session") {
            ses.set("written_at", ctx.now_iso());
            ses.set("written_epoch", ctx.epoch());
        }
        let mut text = format!("{}\n", st.dump());
        if text.len() > SIZE_REFUSE && text.len() > old {
            let times = answered_times(&self.paths.log);
            archived.extend(prune_state(st, &times, 24.0, false, ctx.clock.now_unix()));
            text = format!("{}\n", st.dump());
            if text.len() > SIZE_REFUSE && text.len() > old {
                return invalid(format!(
                    "state.json would be {} B (> {SIZE_REFUSE} B bar); {}; close or shorten the named items",
                    text.len(),
                    size_report(st, 3)
                ));
            }
        }
        validate(st)?;
        if !archived.is_empty() {
            let t = ctx.now_iso();
            let buf: String = archived
                .iter()
                .map(|(k, o)| {
                    let line = obj([
                        ("archived_at", t.clone().into()),
                        ("closed_at", t.clone().into()),
                        ("kind", k.as_str().into()),
                        ("item", o.clone()),
                        ("cv", v.into()),
                    ]);
                    format!("{}\n", line.dump())
                })
                .collect();
            if self.fail(Fault::Archive) {
                return Err(IlError::Io {
                    what: format!("append {}", self.paths.archive.display()),
                    source: std::io::Error::other("injected archive failure"),
                });
            }
            append_lines(&self.paths.archive, buf.as_bytes())?;
        }
        atomic_write(&self.paths.state, text.as_bytes(), self.fail(Fault::Rename))?;
        let t = ctx.now_iso();
        let mut log = String::new();
        for e in events {
            let mut rec = obj([
                ("t", t.clone().into()),
                ("id", e.id.as_str().into()),
                ("ev", e.ev.as_str().into()),
            ]);
            if !e.note.is_empty() {
                rec.set("note", e.note.as_str());
            }
            log.push_str(&rec.dump());
            log.push('\n');
        }
        if text.len() > SIZE_WARN && SIZE_WARN >= old {
            let rec = obj([
                ("ts", t.into()),
                ("session", ctx.env_session.as_str().into()),
                ("kind", "size_warning".into()),
                (
                    "text",
                    format!(
                        "state.json {} B > {SIZE_WARN} B bar; {}",
                        text.len(),
                        size_report(st, 3)
                    )
                    .into(),
                ),
            ]);
            log.push_str(&rec.dump());
            log.push('\n');
        }
        log.push_str(tail_log);
        self.append_log(log.as_bytes()).map_err(|e| match e {
            IlError::Io { what, source } => IlError::Io {
                what: format!("state.json advanced to v{v} but the log append failed ({what})"),
                source,
            },
            other => other,
        })
    }

    /// Appends already-rendered log lines in one write.
    ///
    /// # Errors
    /// `Io`.
    pub fn append_log(&self, lines: &[u8]) -> IlResult<()> {
        if self.fail(Fault::Log) {
            return Err(IlError::Io {
                what: format!("append {}", self.paths.log.display()),
                source: std::io::Error::other("injected log failure"),
            });
        }
        append_lines(&self.paths.log, lines)
    }

    /// `write_ops_ev`: validates and applies `ops` to a copy of the state, then commits once
    /// (all or nothing). `act` ops are validated with the rest and appended to the log after the commit.
    ///
    /// # Errors
    /// Any op's refusal, the lock, the schema or a failed step; the ledger is then unchanged
    /// except as `commit` documents.
    pub fn write_ops(&self, ctx: &mut Ctx, ops: &[Json]) -> IlResult<(Vec<Ev>, Json)> {
        let _guard = self.locked()?;
        let mut st = self.load_for_write()?;
        let (mut evs, mut acts, mut state_ops) = (Vec::new(), String::new(), 0);
        for a in ops {
            if !matches!(a, Json::Obj(_)) {
                return invalid("each op must be an object");
            }
            if a.get("op").and_then(Json::as_str) == Some("act") {
                let rec = act_record(a, ctx.epoch(), &ctx.env_session)?;
                acts.push_str(&rec.dump());
                acts.push('\n');
                evs.push(Ev {
                    id: "act".into(),
                    ev: "act".into(),
                    ..Ev::default()
                });
                continue;
            }
            state_ops += 1;
            evs.extend(apply_op(&mut st, a, ctx)?);
        }
        if state_ops > 0 {
            let closed = collect_closed(&mut st, &evs);
            let logged: Vec<Ev> = evs.iter().filter(|e| e.ev != "act").cloned().collect();
            self.commit(ctx, &mut st, &logged, closed, &acts)?;
        } else if !acts.is_empty() {
            self.append_log(acts.as_bytes())?;
        }
        Ok((evs, st))
    }

    /// `op_init`: creates `state.json` with code `code`.
    ///
    /// # Errors
    /// `Invalid` for a bad code or an existing state.
    pub fn init(&self, ctx: &Ctx, code: &str) -> IlResult<String> {
        if !super::ids::is_code(code) {
            return invalid("code must be 2-8 chars, lowercase letters/digits, letter first");
        }
        let _guard = self.locked()?;
        if self.paths.state.exists() {
            return invalid("state.json exists; init refuses to overwrite");
        }
        let mut st = obj([
            ("code", code.into()),
            ("v", 0.into()),
            ("config", obj([("block_after", "2026-10-12T13:30Z".into())])),
            ("goals", Json::Arr(vec![])),
            ("items", Json::Arr(vec![])),
            ("tasks", Json::Arr(vec![])),
            (
                "next",
                obj([
                    ("G", "01".into()),
                    ("D", "01".into()),
                    ("A", "01".into()),
                    ("T", "01".into()),
                ]),
            ),
        ]);
        let e = Ev {
            id: format!("{code}-init"),
            ev: "open".into(),
            note: "init".into(),
            ..Ev::default()
        };
        self.commit(ctx, &mut st, &[e], Vec::new(), "")?;
        Ok(format!("initialised {code}"))
    }

    /// `op_prune`: archives done items, old answered items and finished reminders.
    ///
    /// # Errors
    /// As [`Ledger::write_ops`].
    pub fn prune(
        &self,
        ctx: &Ctx,
        dry_run: bool,
        hours: f64,
        require_outcome: bool,
    ) -> IlResult<String> {
        let _guard = self.locked()?;
        let mut st = self.load_for_write()?;
        let before = size_of(&self.paths.state);
        let times = answered_times(&self.paths.log);
        let gone = prune_state(
            &mut st,
            &times,
            hours,
            require_outcome,
            ctx.clock.now_unix(),
        );
        let ids: Vec<&str> = gone.iter().map(|(_, o)| o.s("id")).collect();
        if gone.is_empty() {
            return Ok("nothing to prune".to_owned());
        }
        if dry_run {
            return Ok(format!(
                "dry-run: would archive {}: {}",
                ids.len(),
                ids.join(", ")
            ));
        }
        let n = ids.len();
        let code = st.s("code").to_owned();
        let e = Ev {
            id: format!("{code}-prune"),
            ev: "update".into(),
            note: format!("archived {n}"),
            ..Ev::default()
        };
        self.commit(ctx, &mut st, &[e], gone, "")?;
        Ok(format!(
            "archived {n} items; state.json {before} -> {} B",
            size_of(&self.paths.state)
        ))
    }

    /// `op_rotate`: moves log lines older than seven days to `log-<today>.jsonl`.
    ///
    /// # Errors
    /// `Corrupt` for a non-UTF-8 log; `Io` for a failed write.
    pub fn rotate(&self, ctx: &Ctx) -> IlResult<String> {
        let now = ctx.epoch();
        let _guard = self.locked()?;
        let Some((old, keep)) = rotate_plan(&self.paths.log, now - 7 * 86_400)? else {
            return Ok("no log".to_owned());
        };
        if old.is_empty() {
            let n = old.len() + keep.len();
            return Ok(format!(
                "nothing to rotate ({} B, {n} lines)",
                size_of(&self.paths.log)
            ));
        }
        let name = format!("log-{}.jsonl", fmt_day(now));
        let rp = self.paths.dir.join(&name);
        // Only a missing file starts empty: an unreadable one must not be replaced by this write.
        let prev = match std::fs::read_to_string(&rp) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(IlError::io("read", &rp)(e)),
        };
        atomic_write(&rp, format!("{prev}{}\n", old.join("\n")).as_bytes(), false)?;
        let rest: String = keep.iter().map(|l| format!("{l}\n")).collect();
        atomic_write(&self.paths.log, rest.as_bytes(), self.fail(Fault::Rename))?;
        Ok(format!(
            "rotated {} lines to {name}; log.jsonl {} lines",
            old.len(),
            keep.len()
        ))
    }

    /// `archive_lookup`.
    ///
    /// # Errors
    /// `Invalid` when the id is not archived.
    pub fn archive_show(&self, id: &str) -> IlResult<String> {
        archive_lookup(&self.paths, id)
    }
}
