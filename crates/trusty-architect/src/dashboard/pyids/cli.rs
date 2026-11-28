//! `dashboard_ids.py`'s command line (#12029 P2d).
//!
//! Why: the Architect lists the id space and runs the one retirement
//! command from a shell; the port must print the same lines.
//! What: [`run`]: with `retire [--apply] [--now ISO]`, the usage-pass
//! preview (dry run only) and one `retired=` / `would retire=` line per
//! prefix; otherwise one `next= free= retire= kept=` line per prefix over
//! the default root (`--age-days N` adds the old finished items), then the
//! `B` line from the brief registry. Errors the prototype ends in a
//! traceback print one line and exit 1.
//! Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`floor`).

use super::{At, Ctx, PREFIXES, USAGE_REASON, brief_next, brief_used, retire_apply, retire_plan};
use crate::dashboard::pending::from_iso_parts;
use crate::dashboard::pyderive::ids::{self as rules, KEEP_FREE, Retire};
use crate::pyval::{get, py_str};
use crate::spec_drift::Outcome;

fn done(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// The value after `flag` in `args` (`argv[argv.index(flag) + 1]`).
fn after<'a>(args: &'a [String], flag: &str) -> Option<Option<&'a str>> {
    let i = args.iter().position(|a| a == flag)?;
    Some(args.get(i + 1).map(String::as_str))
}

/// `datetime.fromisoformat(s.replace("Z", "+00:00"))` as an aware instant.
fn parse_now(s: &str) -> Result<At, String> {
    from_iso_parts(&s.replace('Z', "+00:00"))
        .map(|(wall, off)| At {
            unix: (wall - off) as f64 / 1e6,
        })
        .ok_or_else(|| {
            format!(
                "ValueError: Invalid isoformat string: {}",
                crate::pystr::py_repr(s)
            )
        })
}

/// `(stdout, stderr)` of `retire`; skip lines go to stderr and the run still succeeds.
fn retire(args: &[String], ctx: &Ctx) -> Result<(String, String), String> {
    let apply = args.iter().any(|a| a == "--apply");
    let now = match after(args, "--now") {
        Some(Some(v)) => Some(parse_now(v)?),
        Some(None) => return Err("IndexError: list index out of range".into()),
        None => None,
    };
    let err = |e: crate::dashboard::store::py::PyStoreError| format!("{}: {e}", e.class());
    let mut out = String::new();
    if !apply {
        let t = now.unwrap_or(At {
            unix: ctx.clock.now_unix(),
        });
        for (p, rows) in retire_plan(&ctx.snap(None).map_err(err)?, t, &super::Plan::default()) {
            let ids: Vec<String> = rows
                .iter()
                .filter(|r| get(r, "reason").map(py_str).as_deref() == Some(USAGE_REASON))
                .map(|r| get(r, "id").map(py_str).unwrap_or_default())
                .collect();
            if !ids.is_empty() {
                out.push_str(&format!(
                    "{p}\tusage pass would retire={}\tids={}\n",
                    ids.len(),
                    ids.join(",")
                ));
            }
        }
    }
    let mut stderr = String::new();
    let applied = retire_apply(ctx, None, apply, now, PREFIXES, &mut stderr).map_err(err)?;
    for (p, planned, retired, skipped, free) in applied {
        let (word, n) = if apply {
            ("retired", retired)
        } else {
            ("would retire", planned)
        };
        out.push_str(&format!(
            "{p}\t{word}={n}\tskipped={skipped}\tfree={free}\n"
        ));
    }
    Ok((out, stderr))
}

fn listing(args: &[String], ctx: &Ctx) -> Result<(String, String), String> {
    let age = match after(args, "--age-days") {
        Some(Some(v)) => Some(crate::pystr::strip(v).parse::<i64>().map_err(|_| {
            format!(
                "ValueError: invalid literal for int() with base 10: {}",
                crate::pystr::py_repr(v)
            )
        })?),
        Some(None) => return Err("IndexError: list index out of range".into()),
        None => None,
    };
    let snap = ctx.snap(None).map_err(|e| format!("{}: {e}", e.class()))?;
    let mut out = String::new();
    for p in PREFIXES.chars().map(String::from) {
        let r = Retire {
            keep_free: KEEP_FREE,
            age_days: age.map(|a| a as f64),
            now: None,
        };
        out.push_str(&format!(
            "{p}\tnext={}\tfree={}\tretire={}\tkept={}\n",
            rules::next_id(&snap, &p, None).unwrap_or_else(|| "None".into()),
            rules::free_count(&snap, &p),
            rules::retire_candidates(&snap, &p, r, ctx.clock.as_ref()).join(","),
            rules::kept_ids(&snap, &p).join(","),
        ));
    }
    let next = brief_next(ctx)?;
    out.push_str(&format!(
        "B\tnext={next}\tused={}\tnever recycled (4 hex digits)\n",
        brief_used(ctx)?
    ));
    Ok((out, String::new()))
}

/// Why: the one entry for `dashboard_ids.py [retire ...]`.
/// What: see the module doc; `args` excludes the program name.
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`floor`).
pub(crate) fn run(args: &[String], ctx: &Ctx) -> Outcome {
    let result = match args.split_first() {
        Some((cmd, rest)) if cmd == "retire" => retire(rest, ctx),
        _ => listing(args, ctx),
    };
    match result {
        Ok((out, err)) => done(out, err, 0),
        Err(last_line) => done(String::new(), format!("{last_line}\n"), 1),
    }
}
