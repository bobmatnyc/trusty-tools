//! `tm compress --tool "<spec>" --chain <nonce>` — the chained-call filter (#11507).
//!
//! Why: before #11507 a chain reached `tm compress` as one stream under one
//! tool name, so a chain mixing output classes (`git diff; cargo test`) or
//! starting with `cd X;` was refused, and 9M tokens a day of chained output
//! went uncompressed (issue #11507, Architect spec).
//! What: `hook_rewrite_chain` keeps the original chain text and inserts a
//! `printf` marker line between steps ([`chain_marker`]). This filter reads
//! stdin, strips the #7384 exit sentinel, cuts the stream at the markers
//! ([`split_chain`]), runs each covered step's piece through that step's own
//! filter, passes inert steps (`cd`, `true`, `export`, `echo`, `date`) through
//! unchanged, writes one savings row per compressed step tagged with the
//! step's tool and the chain's shape, and exits with the sentinel's status, so
//! the pipeline's status is the chain's. stderr never passes through here.
//! Test: `a_split_and_merge_round_trips_byte_for_byte`,
//! `marker_like_text_in_step_output_is_not_a_cut`,
//! `a_short_circuited_chain_yields_fewer_pieces`, `the_spec_parses_into_steps`.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trusty_agents_common::compress::default_rtk_resolver;
use trusty_mpm::core::savings_compress::{ChainTag, record_compress_step};

use super::compress::{
    append_compression_record, compress_with_raw_fallback, compression_log_path,
    init_stats_log_subscriber, log_compression_stats, split_exit_sentinel,
};

/// The marker line `printf` writes before step `step` (1-based past the first).
///
/// Why: a per-call random `nonce` makes the line one no real output carries,
/// and the step index makes a stale or reordered marker no cut.
/// Test: `marker_like_text_in_step_output_is_not_a_cut`.
pub(crate) fn chain_marker(nonce: &str, step: usize) -> String {
    format!("__tm_chain_{nonce}_{step}__")
}

/// The steps a chain spec names: `Some(tool)` for a covered step, `None` inert.
///
/// What: the spec is the steps joined by ` && ` or ` ; ` (the chain's own
/// separators); an inert step is `=<program>`, a covered step is its tool name,
/// optionally followed by ` | <reader>`.
/// Test: `the_spec_parses_into_steps`.
pub(crate) fn parse_chain_spec(spec: &str) -> Vec<Option<String>> {
    spec.replace("&&", ";")
        .split(';')
        .map(|step| {
            let tool = step.split('|').next().unwrap_or("").trim();
            (!tool.starts_with('=')).then(|| tool.to_string())
        })
        .collect()
}

/// The spec as the savings row's `chain_shape`: separators and names only.
///
/// Why (#11507 security scan): a step's tool name is program plus second
/// token, so `ls /Users/<name>/proj` put a home path into the ledger.
/// What: each step becomes its program basename, plus the verb only for
/// `git` in `GIT_READ_VERBS` or `cargo` in `CARGO_VERBS`; a name that is not
/// `[A-Za-z0-9._-]{1,32}` becomes `?`. Separators (`&&`, `;`, `|`) as written.
/// Never a `/`, an argument or a pattern.
/// Test: `the_spec_parses_into_steps`, `a_chain_savings_row_shape_carries_no_path`.
pub(crate) fn chain_shape(spec: &str) -> String {
    use super::hook_rewrite_chain::{CARGO_VERBS, GIT_READ_VERBS};
    let clean = |name: &str| {
        let ok = !name.is_empty()
            && name.len() <= 32
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if ok {
            name.to_string()
        } else {
            "?".to_string()
        }
    };
    let step = |text: &str| {
        let mut words = text.split_whitespace();
        let program = words.next().unwrap_or("").trim_start_matches('=');
        let program = program.rsplit('/').next().unwrap_or("");
        let verb = words.next().filter(|v| match program {
            "git" => GIT_READ_VERBS.contains(v),
            "cargo" => CARGO_VERBS.contains(v),
            _ => false,
        });
        match verb {
            Some(v) => format!("{} {v}", clean(program)),
            None => clean(program),
        }
    };
    let mut shape = String::new();
    let mut rest = spec;
    while let Some(at) = rest.find(['&', ';', '|']) {
        let sep = if rest[at..].starts_with("&&") {
            "&&"
        } else {
            &rest[at..=at]
        };
        shape.push_str(&format!("{} {sep} ", step(&rest[..at])));
        rest = &rest[at + sep.len()..];
    }
    shape.push_str(&step(rest));
    shape
}

/// Cut `text` at the markers for steps `1..steps`, in order.
///
/// What: piece `i` is step `i`'s output: the bytes between its own marker (none
/// for step 0) and the next marker that printed. Exactly the markers' bytes are
/// removed, so concatenating the pieces gives the chain's own output. A missing
/// marker (an `&&` short-circuit) is skipped, not an end: #11507 critic, in
/// `a && b; c` with `a` failing, `c`'s marker still prints, so the steps the
/// short-circuit skipped get empty pieces and the search goes on. Pieces stay
/// aligned with steps; there are fewer only when the tail never ran.
/// Test: `a_split_and_merge_round_trips_byte_for_byte`,
/// `a_short_circuited_chain_yields_fewer_pieces`,
/// `a_short_circuit_before_a_semicolon_keeps_later_markers`.
pub(crate) fn split_chain<'a>(text: &'a str, nonce: &str, steps: usize) -> Vec<&'a str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    for step in 1..steps {
        let cut = format!("\n{}\n", chain_marker(nonce, step));
        let Some(at) = rest.find(&cut) else {
            continue;
        };
        pieces.push(&rest[..at]);
        // Steps between the last printed marker and this one never ran.
        pieces.resize(step, "");
        rest = &rest[at + cut.len()..];
    }
    pieces.push(rest);
    pieces
}

/// Run the chain filter; returns the status `tm` exits with.
///
/// What: see the module doc. The status is the sentinel's, or 0 without one.
/// Test: end to end `a_compressed_chain_writes_a_row_and_the_footer_shows_a_number`.
pub(crate) async fn run_chain(spec: &str, nonce: &str) -> anyhow::Result<i32> {
    init_stats_log_subscriber();
    let mut stdin_text = String::new();
    tokio::io::stdin().read_to_string(&mut stdin_text).await?;
    let (input, exit) = split_exit_sentinel(&stdin_text);
    let tools = parse_chain_spec(spec);
    let shape = chain_shape(spec);
    let tag = ChainTag {
        shape: &shape,
        steps: tools.len(),
    };
    let session_id = trusty_mpm::core::savings::claude_code_session_id();
    let mut out = String::with_capacity(input.len());
    let mut gained = false;
    for (piece, tool) in split_chain(input, nonce, tools.len())
        .into_iter()
        .zip(&tools)
    {
        let Some(tool) = tool.as_deref().filter(|_| !piece.is_empty()) else {
            out.push_str(piece); // inert or empty: verbatim
            continue;
        };
        let started = std::time::Instant::now();
        let (compressed, path) =
            compress_with_raw_fallback(default_rtk_resolver(), tool, piece).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        log_compression_stats(tool, piece.len(), compressed.len(), path.as_str(), exit);
        append_compression_record(
            &compression_log_path(),
            session_id.as_deref(),
            tool,
            piece.len(),
            compressed.len(),
            path.as_str(),
            duration_ms,
        )
        .await;
        record_step_savings(
            session_id.as_deref(),
            tool,
            piece,
            &compressed,
            path.as_str(),
            tag,
        );
        gained |= trusty_mpm::core::savings_compress::compress_gains(piece.len(), compressed.len());
        out.push_str(&compressed);
    }
    record_chain_outcome(session_id.as_deref(), spec, gained);
    let mut stdout = tokio::io::stdout();
    stdout.write_all(out.as_bytes()).await?;
    stdout.flush().await?;
    Ok(exit.unwrap_or(0))
}

/// One step's savings row, under the operator's framework root.
///
/// #7514: like `compress::record_compress_savings`, the ambient root and
/// session reads stay in a function no unit test reaches.
fn record_step_savings(
    session_id: Option<&str>,
    tool: &str,
    piece: &str,
    compressed: &str,
    path: &str,
    tag: ChainTag<'_>,
) {
    let Some(session_id) = session_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return;
    };
    match super::managed_root::resolve_managed_paths(None) {
        Ok(paths) => record_compress_step(
            &paths.root,
            session_id,
            tool,
            (piece.len(), compressed.len()),
            path,
            tag,
        ),
        Err(source) => tracing::warn!(%source, "no framework root: writing no chain savings row"),
    }
}

/// The chain's outcome for the observer, keyed by the `--tool` spec (#11560).
///
/// #7514: the ambient root read stays out of unit tests, like
/// [`record_step_savings`].
fn record_chain_outcome(session_id: Option<&str>, spec: &str, gained: bool) {
    let Some(session_id) = session_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return;
    };
    if let Ok(paths) = super::managed_root::resolve_managed_paths(None) {
        // #11560: a chain saved when any step saved.
        trusty_mpm::core::savings_compress_outcome::record_outcome(
            &paths.root,
            session_id,
            spec,
            gained,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes the producer's `printf` adds before step `i`.
    fn cut(nonce: &str, i: usize) -> String {
        format!("\n{}\n", chain_marker(nonce, i))
    }

    #[test]
    fn a_split_and_merge_round_trips_byte_for_byte() {
        let steps = ["", "a\nb\n", "no newline", "\n\n", "last\n"];
        let mut text = steps[0].to_string();
        for (i, s) in steps.iter().enumerate().skip(1) {
            text.push_str(&cut("ab12", i));
            text.push_str(s);
        }
        let pieces = split_chain(&text, "ab12", steps.len());
        assert_eq!(pieces, steps);
        assert_eq!(pieces.concat(), steps.concat());
    }

    #[test]
    fn marker_like_text_in_step_output_is_not_a_cut() {
        // Another call's nonce, the wrong index, and a bare marker with no
        // surrounding newlines all stay in the step's output.
        let first = format!(
            "x\n{}\n{}\n{}y\n",
            chain_marker("ffff", 1),
            chain_marker("ab12", 2),
            chain_marker("ab12", 1)
        );
        let text = format!("{first}{}second\n", cut("ab12", 1));
        assert_eq!(split_chain(&text, "ab12", 2), [first.as_str(), "second\n"]);
    }

    #[test]
    fn a_short_circuited_chain_yields_fewer_pieces() {
        let text = format!("one\n{}two\n", cut("n", 1));
        assert_eq!(split_chain(&text, "n", 4), ["one\n", "two\n"]);
        assert_eq!(split_chain("only\n", "n", 3), ["only\n"]);
    }

    /// #11507 critic: `a && b; c` with `a` failing prints only marker 2.
    #[test]
    fn a_short_circuit_before_a_semicolon_keeps_later_markers() {
        let text = format!("a out\n{}c out\n", cut("n", 2));
        assert_eq!(split_chain(&text, "n", 3), ["a out\n", "", "c out\n"]);
        let text = format!("a\n{}{}d\n", cut("n", 1), cut("n", 3));
        assert_eq!(split_chain(&text, "n", 4), ["a\n", "", "", "d\n"]);
    }

    /// #11507 security scan: on d2c6960 the row's `chain_shape` was the raw
    /// spec, `ls /Users/x/p && cargo test`.
    #[test]
    fn a_chain_savings_row_shape_carries_no_path() {
        use trusty_mpm::core::savings::SavingsRow;
        use trusty_mpm::core::savings_opportunity::{TOOL_TYPE_BASH, TaggedSavingsRow};
        let rewrite = super::super::hook_rewrite::rewrite_bash_command_for_compression(
            "cd x; ls /Users/x/p && cargo test -p y; git diff /Users/x/hosts | grep -n secret",
        )
        .expect("the chain is rewritten");
        let spec = rewrite
            .split_once("--tool \"")
            .and_then(|(_, t)| t.split_once('"'))
            .map(|(s, _)| s)
            .expect("a spec");
        let shape = chain_shape(spec);
        let row = SavingsRow {
            ts: "2026-11-20T00:00:00Z".to_string(),
            session_id: "s1".to_string(),
            technique: trusty_mpm::core::savings::TECHNIQUE_COMPRESS.to_string(),
            tokens_saved: 900,
            tokens_before: 1_000,
            cost_saved_usd: 0.01,
            basis: "b".to_string(),
            model_source: String::new(),
        };
        let tagged = TaggedSavingsRow {
            row: &row,
            tool_type: TOOL_TYPE_BASH,
            tool_name: "cargo test",
            chain_shape: Some(&shape),
            steps: Some(parse_chain_spec(spec).len()),
        };
        let json: serde_json::Value = serde_json::to_value(&tagged).expect("the row serialises");
        let written = json["chain_shape"].as_str().expect("a chain_shape");
        assert_eq!(written, "ls && cargo test ; git diff | grep");
        for leak in ["/", "x/p", "hosts", "secret", "-n"] {
            assert!(!written.contains(leak), "{leak:?} in {written:?}");
        }
    }

    #[test]
    fn the_spec_parses_into_steps() {
        let spec = "=cd ; git diff && cargo test | tail";
        assert_eq!(
            parse_chain_spec(spec),
            [None, Some("git diff".into()), Some("cargo test".into())]
        );
        assert_eq!(chain_shape(spec), "cd ; git diff && cargo test | tail");
    }
}
