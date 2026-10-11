//! The Bash arm of `tm hook --egress-guard` (#8454 G3, slice S3).
//!
//! Why: S1 denies the outbound-channel MCP tools, but a Bash command reaches
//! the same services with `curl`, a mail client or a channel binary. This arm
//! is a tripwire for those direct forms, for every tm-launched session; S5's
//! `permissions.deny` and an OS sandbox are the boundary. It reads no bypass
//! variable and no `agent_id`.
//! What: [`bash_egress_hit`] names the first outbound-channel token a command
//! carries. A family host denies in any word; a family program denies only in
//! command position, so `grep mail f` is allowed. A command over
//! [`MAX_COMMAND_BYTES`] is denied unread (#9699). It reuses the pm-guard shell
//! parsers — `shell_segments_map` for segments, wrappers and nested bodies,
//! `tokenize`, `resolve_program_word`, `split_heredoc_bodies` and
//! `decode_ansi_c` — and adds no tokenizer.
//! Accepted residuals (plan §4): (1) hosts or programs the shell constructs —
//! from variables, substitutions, brace expansion, curl URL globs or path
//! globs; (2) encoded payloads; (3) scripts run by path; (4) aliases,
//! functions and rc files; (5) self-configured tools (`gh` extensions,
//! `rclone`, `aws sns`, `gcloud`); (6) DNS, IP-literal or shortener fronting;
//! (7) hosts read at runtime; (8) Unicode confusables. Also: a program run by
//! `find -exec`, `ssh`, `watch` or `parallel`, and a `$(…)` inside an
//! unquoted-delimiter here-document body fed to a non-shell, are not read as
//! command position.
//! Test: `egress_guard_bash_tests.rs`; end to end in
//! `tests/tm_hook_egress_guard_8454.rs`.

use std::cell::Cell;

use crate::commands::pm_guard_bash::{
    Decoded, MAX_SUBSTITUTION_DEPTH, UnplaceableWrite, decode_ansi_c, decode_ansi_c_lenient,
    keyword_words, shell_segments_map, split_heredoc_bodies, tokenize,
};
use crate::commands::program_word::{Unresolved, resolve_program_word};

/// The audit slug of a Bash egress deny.
pub(crate) const BASH_CHECK: &str = "egress-bash";

const SLACK: &str = "Slack";
const NOTION: &str = "Notion";
const GMAIL: &str = "Gmail";
const GCHAT: &str = "Google Chat";
const GWORKSPACE: &str = "Google Workspace";
const SMTP: &str = "SMTP mail";

/// Family hosts, each matched as the whole host or a subdomain of it. The
/// bare `googleapis.com` apex is deliberately absent.
const FAMILY_HOSTS: &[(&str, &str)] = &[
    ("slack.com", SLACK),
    ("slack-edge.com", SLACK),
    ("notion.so", NOTION),
    ("notion.com", NOTION),
    ("gmail.googleapis.com", GMAIL),
    ("mail.google.com", GMAIL),
    ("smtp.gmail.com", GMAIL),
    ("chat.googleapis.com", GCHAT),
];

/// URL schemes that speak SMTP.
const SMTP_URLS: &[&str] = &["smtp://", "smtps://"];

/// The mail ports an `smtp.*` host is denied on.
const SMTP_PORTS: &[&str] = &["25", "465", "587"];

/// Programs denied in command position, matched on their lowercased basename.
const FAMILY_PROGRAMS: &[(&str, &str)] = &[
    ("sendmail", SMTP),
    ("mail", SMTP),
    ("mailx", SMTP),
    ("msmtp", SMTP),
    ("mutt", SMTP),
    ("swaks", SMTP),
    ("slack-mcp", SLACK),
    ("gchat-mcp", GCHAT),
    ("gws", GWORKSPACE),
    ("gworkspace-mcp", GWORKSPACE),
];

/// `tm` subcommands that message a channel.
const TM_SUBCOMMANDS: &[(&str, &str)] = &[("slack", SLACK), ("gchat", GCHAT)];

/// `tm` global flags whose value is the next word.
const TM_VALUED_FLAGS: &[&str] = &["--url", "--account"];

/// The one carved-out argv after `gchat-mcp`: the read-only diagnostic.
const GCHAT_DOCTOR: &[&str] = &["doctor", "--offline"];

/// Interpreters whose inline program is read, besides every `python*`.
const INTERPRETERS: &[&str] = &["node", "ruby", "perl", "php", "deno", "bun"];

/// The flags whose next word is an interpreter's inline program (ruling 5).
const INLINE_FLAGS: &[&str] = &["-c", "-e"];

/// The mail module an inline program must not import.
const SMTPLIB: &str = "smtplib";

/// The bytes that end a word in the raw scan of an unlexable command.
const RAW_SEPARATORS: &[char] = &[
    ';', '|', '&', '(', ')', '{', '}', '<', '>', '\'', '"', '`', '$', '=', ',', '\\',
];

/// The per-segment error for a segment that will not tokenize.
const UNLEXABLE_SEGMENT: UnplaceableWrite = "a segment whose words do not lex";

/// The per-segment error for shell keywords nested past the depth bound.
const TOO_DEEP: UnplaceableWrite = "shell keywords nested deeper than the guard follows";

/// The work units one [`text_hit`] spends before it fails closed (#8454
/// critic round 3): `time eval S` reads `S` twice per level. #9699 (b): a
/// [`segment_hits`] call costs one unit plus one per [`BYTES_PER_UNIT`] of its
/// segment, so the budget bounds the bytes re-read as well as the calls.
const WORK_BUDGET: usize = 4096;

/// The segment bytes one [`WORK_BUDGET`] unit pays for (#9699 (b)): the walk
/// re-reads at most ~4 MiB, 64 full reads of a command at the cap.
const BYTES_PER_UNIT: usize = 1024;

/// The most bytes of a Bash command the arm reads: 64 KiB (#9699 (b)).
///
/// Why: the walk's cost grows with the bytes it re-reads, and Claude Code
/// kills a hook at its timeout, which is an allow. So a command over the cap
/// is denied whole — never scanned in part, never allowed. The cap is
/// inclusive, as #9699 words it ("a command over a byte cap"): 65 536 bytes is
/// read, 65 537 is denied.
/// Test: `bash_egress_byte_cap_boundary_is_65536_bytes_9699`.
pub(crate) const MAX_COMMAND_BYTES: usize = 64 * 1024;

/// The quoting bytes the shell removes from a word, which the raw scan also
/// strips (#9699 (a)).
const QUOTING: &[char] = &['\'', '"', '\\'];

/// One outbound-channel name a Bash command carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    /// The service the token reaches, as the deny names it.
    pub(crate) service: &'static str,
    /// The text that matched: a host, a program or a module name.
    pub(crate) token: String,
}

/// A [`Hit`] on `service` naming `token`.
fn hit(service: &'static str, token: &str) -> Hit {
    Hit {
        service,
        token: token.to_owned(),
    }
}

/// The first outbound-channel token `command` carries, if any.
///
/// Why (plan §2, §5): the deny must hold against wrappers, nesting and `$'…'`
/// quoting, and must fail closed on a command it cannot read, without
/// refusing an unreadable command that names no channel.
/// What: an undecodable `$'…'`/`$"…"` is judged by [`raw_hit`], then its
/// lenient decode by [`text_hit`]. Otherwise the command, and its decoded form
/// when it carries `$'…'`, each go through [`text_hit`].
/// Test: `bash_egress_denies_ansi_c_quoted_names`,
/// `bash_egress_denies_ansi_c_beside_an_undecodable_quote`,
/// `bash_egress_denies_unlexable_with_family_token`,
/// `bash_allows_unlexable_without_family_token`,
/// `bash_allows_family_words_as_arguments`.
pub(crate) fn bash_egress_hit(command: &str) -> Option<Hit> {
    let decoded = match decode_ansi_c(command) {
        Decoded::Plain => None,
        Decoded::Text(text) => Some(text),
        // #8454: a quote the decoder cannot read is judged as unlexable text,
        // and (critic round 1) the raw scan splits at `\`, so the decodable
        // quotes are judged too.
        Decoded::Undecodable(_) => {
            return raw_hit(command).or_else(|| text_hit(&decode_ansi_c_lenient(command)));
        }
    };
    std::iter::once(command)
        .chain(decoded.as_deref())
        .find_map(text_hit)
}

/// The deny reason for a command over [`MAX_COMMAND_BYTES`]; `None` when the
/// command fits.
///
/// Why (#9699 (b)): the caller runs this before any parser reads the command,
/// the trust-anchor floor's included, so no read of an over-cap command can
/// run the hook past its timeout.
/// Test: `bash_egress_denies_a_command_over_the_byte_cap_9699`,
/// `bash_egress_cap_precedes_the_trust_anchor_floor_9699`.
pub(crate) fn bash_oversize_reason(command: &str) -> Option<String> {
    (command.len() > MAX_COMMAND_BYTES).then(|| {
        format!(
            "tm hook --egress-guard denies this Bash command: it is {} bytes, over the 64 KiB \
             ({MAX_COMMAND_BYTES}-byte) cap the guard reads, so the guard cannot rule out an \
             outbound channel in it (#9699). Write the payload to a file first, or split the \
             command.",
            command.len()
        )
    })
}

/// The plan §6 deny reason for `hit`.
pub(crate) fn bash_deny_reason(hit: &Hit) -> String {
    format!(
        "tm hook --egress-guard denies this Bash command: it reaches {} ({}) outside the \
         owner channel, and no tm-launched session, PM or subagent may message Slack, Gmail, \
         Notion or Google Chat directly (#8454). Ask the operator instead.",
        hit.service, hit.token
    )
}

/// [`host_hit`] over the raw text, then [`segment_hits`] over every segment
/// and nested body, within [`WORK_BUDGET`] units; a text the walk cannot
/// parse, or one that spends the budget, falls back to [`raw_hit`].
fn text_hit(text: &str) -> Option<Hit> {
    if let Some(found) = host_hit(text) {
        return Some(found);
    }
    let budget = Cell::new(WORK_BUDGET);
    match shell_segments_map(text, &|segment: &str| segment_hits(segment, 0, &budget)) {
        Ok(hits) => hits.into_iter().next(),
        // #8454 plan §5: unlexable or too deep denies only on a family token.
        Err(_) => raw_hit(text),
    }
}

/// [`host_hit`], then [`raw_word_hit`], over `text` and over `text` with its
/// [`QUOTING`] bytes removed: the fail-closed judgement of a text the parsers
/// cannot read.
///
/// Why (#9699 (a)): the raw words split at quotes, so `se''ndmail` read as
/// `se` and `ndmail`; the shell removes those quotes and runs `sendmail`.
/// Test: `bash_egress_denies_quote_split_names_in_the_raw_scan_9699`,
/// `bash_allows_quote_separated_words_in_the_raw_scan_9699`.
fn raw_hit(text: &str) -> Option<Hit> {
    let unquoted: String = text.chars().filter(|c| !QUOTING.contains(c)).collect();
    [text, unquoted.as_str()]
        .into_iter()
        .find_map(|text| host_hit(text).or_else(|| raw_word_hit(text)))
}

/// The first family host, SMTP URL or `smtp.*` host on a mail port in `text`.
///
/// What: lowercases `text`, then reads every run of ASCII letters, digits, `.`
/// and `-` as a host candidate, its leading and trailing `.`/`-` trimmed, so
/// a family host is matched on whole labels.
/// Test: `bash_egress_denies_curl_slack_host`,
/// `bash_allows_unrelated_notion_substring`, `bash_allows_googleapis_storage`.
fn host_hit(text: &str) -> Option<Hit> {
    let lower = text.to_ascii_lowercase();
    if let Some(url) = SMTP_URLS.iter().find(|url| lower.contains(**url)) {
        return Some(hit(SMTP, url));
    }
    let bytes = lower.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !is_host_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let end = (start..bytes.len())
            .find(|&i| !is_host_byte(bytes[i]))
            .unwrap_or(bytes.len());
        let piece = lower[start..end].trim_matches(['.', '-']);
        if let Some(found) = family_host(piece).or_else(|| smtp_port_hit(piece, &lower[end..])) {
            return Some(found);
        }
        start = end;
    }
    None
}

/// Whether `byte` can be part of a host name.
fn is_host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

/// The family `piece` is, or is a subdomain of.
fn family_host(piece: &str) -> Option<Hit> {
    FAMILY_HOSTS
        .iter()
        .find(|(host, _)| {
            piece == *host
                || piece
                    .strip_suffix(host)
                    .is_some_and(|sub| sub.ends_with('.'))
        })
        .map(|(_, service)| hit(service, piece))
}

/// An `smtp.*` host followed by a mail port, as `:587` or as the next word.
fn smtp_port_hit(piece: &str, after: &str) -> Option<Hit> {
    piece
        .strip_prefix("smtp.")
        .filter(|rest| !rest.is_empty())?;
    let port_text = match after.strip_prefix(':') {
        Some(port) => port,
        None => after.trim_start_matches(|c: char| c.is_whitespace() || "'\",".contains(c)),
    };
    let digits = port_text.len()
        - port_text
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let port = &port_text[..digits];
    SMTP_PORTS
        .contains(&port)
        .then(|| hit(SMTP, &format!("{piece}:{port}")))
}

/// A family program, `smtplib` or `tm slack` among the raw words of `text`.
///
/// Why (plan §5): the fail-closed arm for a command the parsers cannot read.
/// It reads every word, not only command position, because the command
/// position is unknown.
/// Test: `bash_egress_denies_unlexable_with_family_token`.
fn raw_word_hit(text: &str) -> Option<Hit> {
    let lower = text.to_ascii_lowercase();
    if lower.contains(SMTPLIB) {
        return Some(hit(SMTP, SMTPLIB));
    }
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || RAW_SEPARATORS.contains(&c))
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().enumerate().find_map(|(i, word)| {
        let name = basename(word);
        match name.as_str() {
            "tm" => words.get(i + 1).and_then(|next| tm_subcommand_hit(next)),
            _ => program_hit(&name),
        }
    })
}

/// The hits of ONE command segment, for `shell_segments_map`.
///
/// What: tokenizes the segment with its data here-document bodies blanked; a
/// segment that will not tokenize is an `Err`. Every word is a host candidate.
/// The program word ([`resolve_program_word`]) is judged by
/// [`program_position_hit`]; a lookup (`command -v`) runs nothing; when an
/// unmeasurable wrapper option hides it, every word stands in for it.
/// Leading shell keywords ([`after_keywords`]) are skipped and the rest mapped
/// again one `depth` deeper; past [`MAX_SUBSTITUTION_DEPTH`] that is an `Err`.
/// Each call spends one unit of `budget` plus one per [`BYTES_PER_UNIT`] of
/// the segment; a budget that cannot pay is an `Err`.
/// Test: `bash_egress_denies_wrapped_in_sh_c`, `bash_egress_denies_pipe_and_xargs`,
/// `bash_egress_denies_command_substitution_body`, `bash_egress_denies_eval_string`,
/// `bash_egress_denies_program_behind_a_shell_keyword`,
/// `bash_egress_denies_keyword_nesting_past_the_depth_bound`,
/// `bash_egress_bounds_work_for_time_eval_nesting`,
/// `bash_egress_judges_a_near_cap_time_eval_payload_within_bound_9699`.
fn segment_hits(
    segment: &str,
    depth: usize,
    budget: &Cell<usize>,
) -> Result<Vec<Hit>, UnplaceableWrite> {
    // #8454 critic round 3: the walk also re-reads a wrapper's inner command,
    // so depth alone does not bound the total work. #9699 (b): nor does a
    // count of calls, each of which may read the whole command.
    let cost = 1 + segment.len() / BYTES_PER_UNIT;
    let left = budget.get().checked_sub(cost).ok_or(TOO_DEEP)?;
    budget.set(left);
    if segment.is_empty() {
        return Ok(Vec::new());
    }
    // #8454 critic rounds 1 and 2: `then sendmail …`, `coproc sendmail …` and
    // `time -p sendmail …` each run `sendmail`.
    let mut rest = segment;
    while let Some(next) = after_keywords(rest) {
        rest = next;
    }
    if rest.len() < segment.len() {
        // #8454 critic round 2: each re-map starts the walk's own depth at 0,
        // so this depth bounds the recursion and too deep fails closed.
        if depth >= MAX_SUBSTITUTION_DEPTH {
            return Err(TOO_DEEP);
        }
        return shell_segments_map(rest, &|inner: &str| segment_hits(inner, depth + 1, budget));
    }
    let (text, bodies) = split_heredoc_bodies(segment);
    let argv = tokenize(&text).map_err(|_| UNLEXABLE_SEGMENT)?;
    // #8454: quoting the shell joins (`"sl""ack.com"`) is judged as one word.
    if let Some(found) = argv.iter().find_map(|word| host_hit(word)) {
        return Ok(vec![found]);
    }
    let found = match resolve_program_word(&argv) {
        Ok(word) if word.lookup => None,
        Ok(word) => program_position_hit(&argv, word.index, &bodies),
        Err(Unresolved) => argv.iter().find_map(|word| program_hit(&basename(word))),
    };
    Ok(found.into_iter().collect())
}

/// `segment` past its leading shell keywords as [`keyword_words`] counts them,
/// `coproc NAME` and `time -p` included; `None` when it has none.
fn after_keywords(segment: &str) -> Option<&str> {
    let first = segment.split_whitespace().next()?;
    // Most segments lead with no keyword; only those split every word.
    if keyword_words(&[first.to_owned()], 0).0 == 0 {
        return None;
    }
    let words: Vec<String> = segment.split_whitespace().map(str::to_owned).collect();
    let mut rest = segment.trim_start();
    for _ in 0..keyword_words(&words, 0).0 {
        rest = rest
            .split_once(char::is_whitespace)
            .map_or("", |(_, tail)| tail)
            .trim_start();
    }
    Some(rest)
}

/// The hit of the program at `argv[at]`, given the segment's data bodies.
///
/// What: the exact argv `gchat-mcp doctor --offline` is allowed; any other
/// [`FAMILY_PROGRAMS`] entry denies; `tm` denies on a [`TM_SUBCOMMANDS`]
/// subcommand; an interpreter denies when its `-c`/`-e` text or a here-document
/// body it reads names `smtplib`.
/// Test: `bash_egress_denies_channel_binaries`,
/// `bash_egress_denies_gchat_mcp_doctor_without_offline`,
/// `bash_allows_gchat_mcp_doctor_offline`, `bash_egress_denies_tm_slack_subcommand`,
/// `bash_egress_denies_python_one_liner`, `bash_egress_denies_heredoc_to_interpreter`.
fn program_position_hit(argv: &[String], at: usize, bodies: &[String]) -> Option<Hit> {
    let name = basename(argv.get(at)?);
    let rest = argv.get(at + 1..).unwrap_or_default();
    // #8454 ruling 3: the exact read-only diagnostic argv only.
    if name == "gchat-mcp"
        && rest
            .iter()
            .map(String::as_str)
            .eq(GCHAT_DOCTOR.iter().copied())
    {
        return None;
    }
    if let Some(found) = program_hit(&name) {
        return Some(found);
    }
    if name == "tm" {
        return tm_subcommand(rest).and_then(tm_subcommand_hit);
    }
    if !is_interpreter(&name) {
        return None;
    }
    // #8454 ruling 5: only `-c`/`-e` text, never a script operand.
    let inline = rest
        .windows(2)
        .filter(|pair| INLINE_FLAGS.contains(&pair[0].as_str()))
        .map(|pair| pair[1].as_str());
    inline
        .chain(bodies.iter().map(String::as_str))
        .any(|program| program.to_ascii_lowercase().contains(SMTPLIB))
        .then(|| hit(SMTP, SMTPLIB))
}

/// The [`FAMILY_PROGRAMS`] hit for a lowercased basename.
fn program_hit(name: &str) -> Option<Hit> {
    FAMILY_PROGRAMS
        .iter()
        .find(|(program, _)| *program == name)
        .map(|(program, service)| hit(service, program))
}

/// `tm`'s subcommand word: the first word that is neither a flag nor a
/// [`TM_VALUED_FLAGS`] value.
fn tm_subcommand(rest: &[String]) -> Option<&str> {
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        if TM_VALUED_FLAGS.contains(&word.as_str()) {
            words.next();
        } else if !word.starts_with('-') {
            return Some(word);
        }
    }
    None
}

/// The [`TM_SUBCOMMANDS`] hit for a `tm` subcommand word.
fn tm_subcommand_hit(word: &str) -> Option<Hit> {
    let word = word.to_ascii_lowercase();
    TM_SUBCOMMANDS
        .iter()
        .find(|(sub, _)| *sub == word)
        .map(|(sub, service)| hit(service, &format!("tm {sub}")))
}

/// Whether `name` is an interpreter whose inline program is read.
fn is_interpreter(name: &str) -> bool {
    INTERPRETERS.contains(&name)
        || name
            .strip_prefix("python")
            .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// A word's lowercased program name: past any `/` and a leading `\`.
fn basename(word: &str) -> String {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
}

#[cfg(test)]
#[path = "egress_guard_bash_tests.rs"]
mod tests;
