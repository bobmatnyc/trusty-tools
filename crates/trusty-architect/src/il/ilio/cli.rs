//! The `ilio` verbs of `trusty-architect-il`.
//!
//! Why: `ilio.py` is what a session calls each turn (`apply`) and each resume
//! (`table`); the verbs, stdin, stdout, stderr and exit codes are kept so a caller
//! swaps the program name only.
//! What: [`run_ilio`] runs `table`, `parse`, `check`, `expand`, `relay`, `apply`
//! and `collapse` against `$ILIO_PROJECT` (the ledger project directory) and
//! `$ILIO_DIR` (R library, `epoch.json`). `check` exits 1 with
//! `IL-REJECT ...` on stdout; a refused write exits 2 as `ilio: <msg>`. `record`
//! is not ported. `apply` writes the ledger only (see `apply.rs`).
//! Test: `tests/il_ilio.rs`.

use super::apply::apply_block;
use super::check::check;
use super::collapse::collapse;
use super::{Lib, expand, fit_rows, parse_block, relay_text, table_rows};
use crate::il::args::Spec;
use crate::il::cli::{Cx, parse_args};
use crate::il::error::{IlError, IlResult, invalid};
use crate::il::json::Json;
use crate::il::store::{Where, load_state};
use std::path::PathBuf;

/// Runs one `ilio` verb; returns its exit code.
pub fn run_ilio(cx: &mut Cx, rest: &[String]) -> IlResult<i32> {
    let Some((verb, args)) = rest.split_first() else {
        return invalid("a verb is required (table, parse, check, expand, relay, apply, collapse)");
    };
    let ilio_dir = cx.env.get("ILIO_DIR").map(PathBuf::from);
    let lib = Lib::load(ilio_dir.as_deref());
    if verb == "relay" {
        let p = parse_args(args, &Spec::default())?;
        let Some(reference) = p.pos.first() else {
            return invalid("the following arguments are required: ref");
        };
        let slots: Vec<(String, String)> = p.pos[1..]
            .iter()
            .filter_map(|s| s.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        return Ok(match relay_text(reference, &slots, &lib) {
            Some(t) if !t.is_empty() => {
                cx.out.println(&t);
                0
            }
            Some(t) => {
                cx.out.eprintln(&t);
                0
            }
            None => {
                cx.out.eprintln(&format!("unknown R reference {reference}"));
                1
            }
        });
    }
    let Some(project) = cx.env.get("ILIO_PROJECT") else {
        return invalid("ILIO_PROJECT is not set (the ledger project directory)");
    };
    let w = Where {
        project: PathBuf::from(project),
        env_session: cx.env.get("DATG_SESSION").map(str::to_owned),
        tmux_pane: cx.env.get("TMUX_PANE").map(str::to_owned),
        state_dir: cx.env.get("DATG_STATE_DIR").map(PathBuf::from),
        ..Where::default()
    };
    let ledger = cx.ledger(&w)?;
    if verb == "collapse" {
        let p = parse_args(
            args,
            &Spec {
                values: &[],
                flags: &["dry-run"],
                repeat: &[],
            },
        )?;
        let (ok, msg) = collapse(&ledger, &cx.ctx(), ilio_dir.as_deref(), p.flag("dry-run"))?;
        cx.out.println(&msg);
        return Ok(i32::from(!ok));
    }
    let st = load_state(&ledger.paths)?;
    match verb.as_str() {
        "table" => {
            let p = parse_args(
                args,
                &Spec {
                    values: &["max-tokens"],
                    flags: &[],
                    repeat: &[],
                },
            )?;
            let max = p
                .int("max-tokens")
                .map_err(IlError::Invalid)?
                .unwrap_or(3000);
            let budget = usize::try_from(max.saturating_mul(4)).unwrap_or(0);
            cx.out
                .println(&fit_rows(&table_rows(&st, &lib), budget).join("\n"));
            Ok(0)
        }
        "parse" | "check" | "expand" | "apply" => {
            let p = parse_args(
                args,
                &Spec {
                    values: &[],
                    flags: &["no-dashboard"],
                    repeat: &[],
                },
            )?;
            let text = (cx.stdin)();
            stdin_verb(cx, verb, &text, &st, &lib, &ledger, p.flag("no-dashboard"))
        }
        "record" => {
            invalid("record is not ported to trusty-architect-il; keep calling ilio.py record")
        }
        other => invalid(format!("argument verb: invalid choice: {other:?}")),
    }
}

fn stdin_verb(
    cx: &mut Cx,
    verb: &str,
    text: &str,
    st: &Json,
    lib: &Lib,
    ledger: &crate::il::Ledger,
    no_dash: bool,
) -> IlResult<i32> {
    match verb {
        "parse" => {
            let Some(rows) = parse_block(text) else {
                cx.out.eprintln("no il block");
                return Ok(1);
            };
            let doc = Json::Arr(
                rows.iter()
                    .map(|(k, v)| Json::Arr(vec![k.as_str().into(), v.as_str().into()]))
                    .collect(),
            );
            cx.out.println(&doc.dump_spaced());
            Ok(0)
        }
        "check" => {
            let err = check(text, st, lib);
            cx.out.println(err.as_deref().unwrap_or("OK"));
            Ok(i32::from(err.is_some()))
        }
        "expand" => {
            cx.out.println(&expand(text, st, lib));
            Ok(0)
        }
        _ => {
            if let Some(err) = check(text, st, lib) {
                cx.out.println(&err);
                return Ok(1);
            }
            let who = cx.env.get("ILIO_WHO").unwrap_or("tm-architect").to_owned();
            let mut ctx = cx.ctx();
            let msg = apply_block(ledger, &mut ctx, text, &who, no_dash)?;
            cx.out.println(&msg);
            cx.flush_warnings(&ctx, "datg");
            Ok(0)
        }
    }
}
