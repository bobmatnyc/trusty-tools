//! The Bash arm of `tm hook --egress-guard` (#8454 G3, slice S3).
//!
//! Why: S1 denies the outbound-channel MCP tools, but a Bash command reaches
//! the same services with `curl`, a mail client or a channel binary. This arm
//! is a tripwire for those direct forms, for every tm-launched session; S5's
//! `permissions.deny` and an OS sandbox are the boundary. It reads no bypass
//! variable and no `agent_id`.
//! What: [`bash_egress_hit`] names the first outbound-channel token a command
//! carries. A family host denies in any word; a family program denies only in
//! command position, so `grep mail f` is allowed. It reuses the pm-guard shell
//! parsers — `shell_segments_map` for segments, wrappers and nested bodies,
//! `tokenize`, `resolve_program_word`, `split_heredoc_bodies` and
//! `decode_ansi_c` — and adds no tokenizer.
//! Accepted residuals (plan §4): hosts built from variables or substitutions;
//! encoded payloads; scripts run by path; aliases, functions and rc files;
//! self-configured tools (`gh` extensions, `rclone`, `aws sns`, `gcloud`); DNS,
//! IP-literal or shortener fronting; hosts read at runtime; Unicode
//! confusables. Also: a program run by `find -exec`, `ssh`, `watch` or
//! `parallel`, and a `$(…)` inside an unquoted-delimiter here-document body
//! fed to a non-shell, are not read as command position.
//! Test: `egress_guard_bash_tests.rs`; end to end in
//! `tests/tm_hook_egress_guard_8454.rs`.

use crate::commands::pm_guard_bash::{
    Decoded, UnplaceableWrite, decode_ansi_c, shell_segments_map, split_heredoc_bodies, tokenize,
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
/// What: an undecodable `$'…'`/`$"…"` is judged by [`raw_hit`]. Otherwise the
/// command, and its decoded form when it carries `$'…'`, each go through
/// [`text_hit`].
/// Test: `bash_egress_denies_ansi_c_quoted_names`,
/// `bash_egress_denies_unlexable_with_family_token`,
/// `bash_allows_unlexable_without_family_token`,
/// `bash_allows_family_words_as_arguments`.
pub(crate) fn bash_egress_hit(command: &str) -> Option<Hit> {
    let decoded = match decode_ansi_c(command) {
        Decoded::Plain => None,
        Decoded::Text(text) => Some(text),
        // #8454: a quote the decoder cannot read is judged as unlexable text.
        Decoded::Undecodable(_) => return raw_hit(command),
    };
    std::iter::once(command)
        .chain(decoded.as_deref())
        .find_map(text_hit)
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
/// and nested body; a text the walk cannot parse falls back to [`raw_hit`].
fn text_hit(text: &str) -> Option<Hit> {
    if let Some(found) = host_hit(text) {
        return Some(found);
    }
    match shell_segments_map(text, &segment_hits) {
        Ok(hits) => hits.into_iter().next(),
        // #8454 plan §5: unlexable or too deep denies only on a family token.
        Err(_) => raw_hit(text),
    }
}

/// [`host_hit`], then [`raw_word_hit`]: the fail-closed judgement of a text
/// the parsers cannot read.
fn raw_hit(text: &str) -> Option<Hit> {
    host_hit(text).or_else(|| raw_word_hit(text))
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
/// Test: `bash_egress_denies_wrapped_in_sh_c`, `bash_egress_denies_pipe_and_xargs`,
/// `bash_egress_denies_command_substitution_body`, `bash_egress_denies_eval_string`.
fn segment_hits(segment: &str) -> Result<Vec<Hit>, UnplaceableWrite> {
    if segment.is_empty() {
        return Ok(Vec::new());
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
