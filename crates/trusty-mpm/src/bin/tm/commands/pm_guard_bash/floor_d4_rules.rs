//! The upload and disk-tool deny-sets of the #8878 D4-remainder floor.
//!
//! Why: the deny-sets proposed on #8878 (comment 5889247312), one argv at a
//! time, so [`super::floor_d4`] owns only the segment walk.
//! What: [`exfiltration_reason`] — `curl` with `-d`/`--data*`/`--json` `@file`,
//! `-T`, or a `-F` file field; `wget --post-file`/`--body-file`; `scp` to a
//! remote destination; `rsync` with a remote operand after the first; `nc`
//! except `-z`; `ssh` reading a pipe or a `<` file. Plain downloads pass.
//! [`disk_tool_reason`] — `diskutil` erase/zero/partition/resize/delete verbs
//! and a forced unmount; `dd of=/dev/…` (not a pseudo-device); `mkfs*`, `newfs*`, `fdisk`, `asr`;
//! `hdiutil burn`/`erasekeys`/`resize` and a forced `detach`/`eject`.
//! Test: `floor_d4_tests.rs`.

use super::credential_print::input_redirect_operand;
use super::floor_d4::{D4_REMEDY, program_positions};

/// `curl` short options that take a value (the value ends a cluster).
const CURL_VALUE_SHORTS: &str = "AbcCdDeEFHKmoPQrtTuUwxXyYz";

/// `scp` short options that take a value.
const SCP_VALUE_SHORTS: &str = "cDFiJloPSX";

/// `rsync` options whose value is a separate token.
const RSYNC_VALUE_OPTS: &[&str] = &[
    "-e",
    "-f",
    "-T",
    "-B",
    "-M",
    "--rsh",
    "--exclude",
    "--include",
    "--filter",
    "--exclude-from",
    "--include-from",
    "--files-from",
    "--port",
];

/// The curl `--data*` family: each reads a file after `@`.
const CURL_DATA_LONGS: &[&str] = &[
    "--data",
    "--data-binary",
    "--data-ascii",
    "--data-urlencode",
    "--json",
];

/// The upload deny for one segment's argv, or `None`.
///
/// Test: `an_upload_of_local_content_is_denied`, `plain_downloads_pass`.
pub(super) fn exfiltration_reason(argv: &[String], piped: bool) -> Option<String> {
    for (i, program) in program_positions(argv) {
        let tail = &argv[i + 1..];
        let upload = match program.as_str() {
            "curl" => curl_uploads(tail),
            "wget" => tail.iter().any(|t| {
                ["--post-file", "--body-file"]
                    .iter()
                    .any(|o| t == o || t.starts_with(&format!("{o}=")))
            }),
            "scp" => scp_uploads(tail),
            "rsync" => rsync_uploads(tail),
            "nc" | "ncat" | "netcat" => !short_flag_present(tail, 'z'),
            "ssh" => piped || reads_a_file(tail),
            _ => false,
        };
        if upload {
            return Some(format!(
                "Hard-floor deny (#8878 D4): `{program}` here uploads local content (a file body, \
                 a piped stream or a copy to a remote host) — network exfiltration. Plain \
                 downloads are not covered. {D4_REMEDY}"
            ));
        }
    }
    None
}

/// Whether a curl argv sends a local file or stdin.
fn curl_uploads(tail: &[String]) -> bool {
    let reads = |opt: char, value: &str| match opt {
        'd' => value.starts_with('@'),
        'F' => value.contains(['@', '<']),
        'T' => true,
        _ => false,
    };
    let mut i = 0;
    while i < tail.len() {
        let tok = tail[i].as_str();
        let next = tail.get(i + 1).map(String::as_str).unwrap_or_default();
        if let Some(long) = tok.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (format!("--{name}"), Some(value)),
                None => (tok.to_string(), None),
            };
            let value = value.unwrap_or(next);
            // `--data-urlencode name@file` reads a file after a name.
            let data_file = if name == "--data-urlencode" {
                value.contains('@')
            } else {
                value.starts_with('@')
            };
            if (CURL_DATA_LONGS.contains(&name.as_str()) && data_file)
                || name == "--upload-file"
                || (name == "--form" && value.contains(['@', '<']))
            {
                return true;
            }
        } else if let Some(cluster) = tok.strip_prefix('-') {
            for (at, c) in cluster.char_indices() {
                if CURL_VALUE_SHORTS.contains(c) {
                    let rest = &cluster[at + c.len_utf8()..];
                    if reads(c, if rest.is_empty() { next } else { rest }) {
                        return true;
                    }
                    break;
                }
            }
        }
        i += 1;
    }
    false
}

/// The non-option operands of an argv, skipping each option's value.
fn operands<'a>(tail: &'a [String], value_short: &str, value_long: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tail.len() {
        let tok = tail[i].as_str();
        if tok == "--" {
            out.extend(tail[i + 1..].iter().map(String::as_str));
            break;
        }
        if value_long.contains(&tok) {
            i += 2;
            continue;
        }
        if let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            let takes = cluster
                .char_indices()
                .find(|(_, c)| value_short.contains(*c));
            let attached = takes.is_some_and(|(at, c)| at + c.len_utf8() < cluster.len());
            i += if takes.is_some() && !attached { 2 } else { 1 };
            continue;
        }
        if !tok.starts_with('-') {
            out.push(tok);
        }
        i += 1;
    }
    out
}

/// Whether an operand names a remote host (`host:path`, `rsync://`, `::`).
fn is_remote(operand: &str) -> bool {
    if operand.contains("://") || operand.contains("::") {
        return true;
    }
    match (operand.find(':'), operand.find('/')) {
        (Some(colon), Some(slash)) => colon < slash,
        (Some(_), None) => true,
        _ => false,
    }
}

/// `scp` uploads when its destination, the last operand, is remote.
fn scp_uploads(tail: &[String]) -> bool {
    let ops = operands(tail, SCP_VALUE_SHORTS, &[]);
    ops.len() >= 2 && ops.last().is_some_and(|dest| is_remote(dest))
}

/// `rsync` uploads when a remote operand follows the first operand.
fn rsync_uploads(tail: &[String]) -> bool {
    let ops = operands(tail, "efTBM", RSYNC_VALUE_OPTS);
    ops.iter().skip(1).any(|op| is_remote(op))
}

/// Whether a single-dash flag cluster in `tail` carries `flag`.
fn short_flag_present(tail: &[String], flag: char) -> bool {
    tail.iter().any(|t| {
        t.strip_prefix('-')
            .is_some_and(|c| !c.starts_with('-') && c.contains(flag))
    })
}

/// Whether an argv reads its stdin from a file (`< f`, `<f`, `0<f`).
fn reads_a_file(tail: &[String]) -> bool {
    tail.iter()
        .any(|t| input_redirect_operand(t).is_some_and(|(fd, _)| fd == 0))
}

/// The destructive-disk-tool deny for one segment's argv, or `None`.
///
/// Test: `a_destructive_disk_tool_is_denied`, `read_only_disk_tools_pass`.
pub(super) fn disk_tool_reason(argv: &[String]) -> Option<String> {
    for (i, program) in program_positions(argv) {
        let tail = &argv[i + 1..];
        let lower: Vec<String> = tail.iter().map(|t| t.to_ascii_lowercase()).collect();
        let has = |word: &str| lower.iter().any(|t| t == word);
        let destructive = match program.as_str() {
            "diskutil" => lower.iter().any(|t| diskutil_destroys(t, &lower)),
            "dd" => tail
                .iter()
                .any(|t| t.strip_prefix("of=/dev/").is_some_and(is_a_device)),
            "fdisk" | "asr" => true,
            "hdiutil" => {
                has("burn")
                    || has("erasekeys")
                    || has("resize")
                    || ((has("detach") || has("eject")) && has("-force"))
            }
            p => p.starts_with("mkfs") || p.starts_with("newfs"),
        };
        if destructive {
            return Some(format!(
                "Hard-floor deny (#8878 D4): `{program}` here can erase, repartition or force \
                 off a disk — a destructive disk tool. {D4_REMEDY}"
            ));
        }
    }
    None
}

/// Whether one lowercased `diskutil` word is a destroying verb.
fn diskutil_destroys(word: &str, all: &[String]) -> bool {
    const PREFIXES: &[&str] = &[
        "erase",
        "zero",
        "random",
        "secureerase",
        "reformat",
        "partition",
        "resize",
        "delete",
    ];
    PREFIXES.iter().any(|p| word.starts_with(p))
        || word.contains("partition")
        || (word.starts_with("unmount") && all.iter().any(|t| t == "force"))
}

/// Whether a `/dev/` name (without the prefix) is a device `dd` could erase,
/// rather than a stream: `stdout`, `stderr`, `null`, `zero`, `tty*`, `fd/*`.
fn is_a_device(name: &str) -> bool {
    const STREAMS: &[&str] = &[
        "stdout", "stderr", "stdin", "null", "zero", "random", "urandom",
    ];
    !(STREAMS.contains(&name) || name.starts_with("tty") || name.starts_with("fd/"))
}
