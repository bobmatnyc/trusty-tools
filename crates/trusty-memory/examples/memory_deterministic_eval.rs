//! Isolated deterministic memory evaluation over explicit JSON input only.
//! Why: measure lexical maintenance without a daemon or model.
//! What: stdin requests, portable state, stdout responses; no filesystem state.
//! Test: `support::tests` exercises maintenance, retrieval, and legacy snapshots.

#[path = "support/memory_deterministic/mod.rs"]
mod support;

use std::io::{BufRead, Read, Write};

fn main() {
    if let Err((code, error)) = run() {
        eprintln!("{error}");
        std::process::exit(code);
    }
}

fn run() -> Result<(), (i32, String)> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let jsonl = match args.as_slice() {
        [] => false,
        [flag, value] if flag == "--format" && value == "json" => false,
        [flag, value] if flag == "--format" && value == "jsonl" => true,
        _ => {
            return Err((
                2,
                "usage: memory_deterministic_eval [--format json|jsonl]".into(),
            ))
        }
    };
    let mut output = std::io::stdout().lock();
    let mut failed = false;
    let mut emit = |input: &str| -> Result<(), (i32, String)> {
        let response = support::respond(input);
        failed |= !response["ok"].as_bool().unwrap_or(false);
        serde_json::to_writer(&mut output, &response).map_err(|e| (1, e.to_string()))?;
        writeln!(output).map_err(|e| (1, e.to_string()))?;
        output.flush().map_err(|e| (1, e.to_string()))
    };
    if jsonl {
        for line in std::io::stdin().lock().lines() {
            emit(&line.map_err(|e| (1, e.to_string()))?)?;
        }
    } else {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| (1, e.to_string()))?;
        emit(&input)?;
    }
    if failed {
        Err((2, "one or more requests failed validation".into()))
    } else {
        Ok(())
    }
}
