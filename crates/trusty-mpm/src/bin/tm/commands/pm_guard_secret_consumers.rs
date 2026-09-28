//! `tm hook --pm-guard` — a key file handed to a program that never prints it,
//! and a GET that lists secret names (#8869, #7266 PR-1).
//!
//! Why: the #7266 rule refuses every Bash segment that names a secret-bearing
//! file unless its program is a safe verb, so tm-apex could not upload a GitHub
//! App key with `gh secret set NAME < key.pem`, sign a JWT with
//! `openssl dgst -sign key.pem`, or list an environment's secret names with
//! `gh api …/environments/E/secrets` (whose path word matches `*secrets*`).
//! Each of those programs reads the key into its own process and prints only a
//! signature, a public key, gh's "✓ Set secret" line, or secret NAMES — GitHub
//! never returns a secret value.
//! What: two exemptions the read rule asks after its safe-verb and terraform
//! grants. [`key_only_consumed`] matches the segment against
//! [`KEY_CONSUMERS`]; [`gh_api_lists_secret_names`] against
//! [`SECRET_LISTINGS`]. The verdict depends only on the program, its mode and
//! its flags, so a downstream pipe cannot change it. Every arm that cannot
//! prove the shape — an unlexable segment, an unknown program, subcommand,
//! flag or redirect, a missing flag value, a key that is not a literal path, a
//! nested command outside an exact `NAME=$(…)` capture, an endpoint outside the
//! anchored listing grammar — answers `false`, and the read rule then denies.
//! Test: `allows_each_key_consumer_in_its_real_idiom_8869`,
//! `allows_the_three_reported_commands_8869`,
//! `allows_a_get_that_lists_secret_names_8869`,
//! `denies_every_print_and_copy_sink_of_a_key_8869`,
//! `denies_a_secrets_path_that_is_not_a_get_listing_8869`,
//! `every_consumer_error_arm_fails_closed_8869`; end to end,
//! `pm_guard_allows_a_key_handed_to_a_consumer_8869` and
//! `pm_guard_still_denies_every_key_print_or_copy_sink_8869` in
//! `tests/tm_hook_pm_guard_pem_consumers.rs`.

use crate::commands::pm_guard_bash::{
    RedirectRole, input_redirect_operand, redirect_role, tokenize,
};
use crate::commands::pm_guard_secret_read::{
    NESTED_COMMAND_MARKERS, Scan, command_basename, secret_files_named_in,
};

/// How one consumer mode reads its argv.
///
/// Why: the design's safety argument is "consumer + mode + flags decide", so a
/// mode is a closed description: every flag it may carry is listed, and any
/// other token denies.
struct Consumer {
    /// The program basename and the subcommand words that must follow it.
    words: &'static [&'static str],
    /// Flags whose value is the key file — a literal path, never printed.
    key_flags: &'static [&'static str],
    /// Flags whose value the program WRITES its output to, or reads as an
    /// output filter (`gh api --jq`): a secret-shaped name there is allowed.
    output_flags: &'static [&'static str],
    /// Other flags that take a value.
    value_flags: &'static [&'static str],
    /// Flags whose value must be exactly `GET` (`gh api -X`).
    method_flags: &'static [&'static str],
    /// Flags that take no value.
    switch_flags: &'static [&'static str],
    /// Each group needs at least one member present.
    required: &'static [&'static [&'static str]],
    /// How many positional arguments the mode takes; `None` is any number.
    positionals: Option<usize>,
    /// A test every positional must pass; one that passes is an allowed
    /// position (the `gh api` endpoint).
    positional_check: Option<fn(&str) -> bool>,
    /// Whether an fd-0 `<` redirect is the key position (`gh secret set`).
    stdin_is_key: bool,
    /// Whether a clustered short flag (`-lf`) is split into its letters.
    clusters: bool,
}

impl Consumer {
    /// A mode with every list empty, for the table below to fill in.
    const EMPTY: Self = Self {
        words: &[],
        key_flags: &[],
        output_flags: &[],
        value_flags: &[],
        method_flags: &[],
        switch_flags: &[],
        required: &[],
        positionals: Some(0),
        positional_check: None,
        stdin_is_key: false,
        clusters: false,
    };
}

/// The digest switches `openssl dgst` accepts in front of `-sign`.
const DGST_SWITCHES: &[&str] = &[
    "-binary",
    "-hex",
    "-r",
    "-sha1",
    "-sha224",
    "-sha256",
    "-sha384",
    "-sha512",
    "-sha512-256",
    "-sha3-256",
    "-sha3-384",
    "-sha3-512",
    "-blake2b512",
    "-blake2s256",
];

/// Programs that take a key file as input and print none of its bytes.
///
/// Why: see the module doc. `--body`, `--env-file` and `--no-store` of
/// `gh secret set`, `-text`/`-passin` of `openssl`, and `-p`/`-e`/`-P` of
/// `ssh-keygen` are absent, so each denies as an unknown flag.
/// What: `gh secret set NAME < K`; `openssl dgst -<digest> -sign K`;
/// `openssl pkeyutl -sign -inkey K`; `openssl pkey -in K -pubout`;
/// `ssh-keygen -y|-l -f K`.
/// Test: `allows_each_key_consumer_in_its_real_idiom_8869`,
/// `denies_every_print_and_copy_sink_of_a_key_8869`.
const KEY_CONSUMERS: &[Consumer] = &[
    Consumer {
        words: &["gh", "secret", "set"],
        value_flags: &[
            "-e",
            "--env",
            "-R",
            "--repo",
            "-o",
            "--org",
            "-a",
            "--app",
            "-v",
            "--visibility",
            "-r",
            "--repos",
            "-u",
            "--user",
        ],
        positionals: Some(1),
        stdin_is_key: true,
        ..Consumer::EMPTY
    },
    Consumer {
        words: &["openssl", "dgst"],
        key_flags: &["-sign"],
        output_flags: &["-out"],
        value_flags: &["-sigopt", "-keyform"],
        switch_flags: DGST_SWITCHES,
        required: &[&["-sign"]],
        positionals: None,
        ..Consumer::EMPTY
    },
    Consumer {
        words: &["openssl", "pkeyutl"],
        key_flags: &["-inkey"],
        output_flags: &["-out"],
        value_flags: &["-in", "-digest", "-pkeyopt"],
        switch_flags: &["-sign", "-rawin"],
        required: &[&["-sign"], &["-inkey"]],
        ..Consumer::EMPTY
    },
    Consumer {
        words: &["openssl", "pkey"],
        key_flags: &["-in"],
        output_flags: &["-out"],
        switch_flags: &["-pubout"],
        required: &[&["-in"], &["-pubout"]],
        ..Consumer::EMPTY
    },
    Consumer {
        words: &["ssh-keygen"],
        key_flags: &["-f"],
        switch_flags: &["-y", "-l"],
        required: &[&["-y", "-l"], &["-f"]],
        clusters: true,
        ..Consumer::EMPTY
    },
];

/// `gh api` as a GET of a secret-listing endpoint.
///
/// Why: the endpoint's `secrets` path word is what the read rule refuses, and
/// a GET of it returns names, update times and a public key — never a value.
/// What: `-X`/`--method` must be `GET`; `-f`/`-F`/`--field`/`--raw-field`/
/// `--input` are absent, so a write denies; `--jq`/`--template` are output
/// filters; exactly one endpoint, which [`is_secret_listing_endpoint`] must
/// accept.
/// Test: `allows_a_get_that_lists_secret_names_8869`,
/// `denies_a_secrets_path_that_is_not_a_get_listing_8869`.
const SECRET_LISTINGS: &[Consumer] = &[Consumer {
    words: &["gh", "api"],
    output_flags: &["-q", "--jq", "-t", "--template"],
    value_flags: &["-H", "--header"],
    method_flags: &["-X", "--method"],
    switch_flags: &["--paginate", "--slurp", "-i", "--include", "--silent"],
    positionals: Some(1),
    positional_check: Some(is_secret_listing_endpoint),
    ..Consumer::EMPTY
}];

/// Whether `segment` names secret files only as a [`KEY_CONSUMERS`] key or
/// output (#8869).
///
/// What: the segment itself, or the body of an exact `NAME=$(…)` capture (see
/// [`capture_body`]), must match a consumer mode, and every word of `named`
/// must come from that mode's key or output positions.
/// Test: `allows_each_key_consumer_in_its_real_idiom_8869`,
/// `every_consumer_error_arm_fails_closed_8869`.
pub(crate) fn key_only_consumed(segment: &str, named: &[String]) -> bool {
    capture_body(segment).is_some_and(|body| consumed_by(body, named, KEY_CONSUMERS))
}

/// Whether `segment` is a GET that lists secret names, naming secret words
/// only in its endpoint, its output filter or its output file (#8869).
///
/// What: no nested command at all, then a [`SECRET_LISTINGS`] match.
/// Test: `allows_a_get_that_lists_secret_names_8869`,
/// `denies_a_secrets_path_that_is_not_a_get_listing_8869`.
pub(crate) fn gh_api_lists_secret_names(segment: &str, named: &[String]) -> bool {
    !has_nested_command(segment) && consumed_by(segment.trim(), named, SECRET_LISTINGS)
}

/// Whether `text` carries a [`NESTED_COMMAND_MARKERS`] spelling.
fn has_nested_command(text: &str) -> bool {
    NESTED_COMMAND_MARKERS.iter().any(|m| text.contains(m))
}

/// The command a key-consumer segment runs, or `None` when it cannot be read.
///
/// Why: `SIG=$(openssl dgst -sign k.pem …)` is how a script keeps a signature,
/// and a capture prints nothing. Any other nested command — `<(cat k.pem)`,
/// `"$(cat k.pem)"` as an argument, a second `$(` — can hand key bytes to a
/// printer, so it withdraws the grant.
/// What: the trimmed segment when it has no nested command; the body of an
/// exact `NAME=$( … )` or `NAME="$( … )"` capture whose body has no nested
/// command and no paren of its own; otherwise `None`.
/// Test: `every_consumer_error_arm_fails_closed_8869`.
fn capture_body(segment: &str) -> Option<&str> {
    let text = segment.trim();
    if !has_nested_command(text) {
        return Some(text);
    }
    let (name, value) = text.split_once('=')?;
    let is_identifier = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let body = value
        .strip_prefix("$(")
        .and_then(|v| v.strip_suffix(')'))
        .or_else(|| {
            let quoted = value.strip_prefix("\"$(")?.strip_suffix(")\"")?;
            (!quoted.contains('"')).then_some(quoted)
        })?;
    (is_identifier && !has_nested_command(body) && !body.contains(['(', ')'])).then_some(body)
}

/// Whether `command` matches one of `table`'s modes with every `named` word in
/// an allowed position.
fn consumed_by(command: &str, named: &[String], table: &[Consumer]) -> bool {
    // #8869: an unlexable command is never granted — the read rule denies it.
    let Ok(argv) = tokenize(command) else {
        return false;
    };
    let Some(consumer) = table.iter().find(|c| invokes(&argv, c.words)) else {
        return false;
    };
    let args = expand_clusters(consumer, &argv[consumer.words.len()..]);
    let Some(allowed) = allowed_positions(consumer, &args) else {
        return false;
    };
    let words: Vec<String> = allowed
        .iter()
        .flat_map(|tok| secret_files_named_in(tok, Scan::Argv))
        .collect();
    named.iter().all(|n| words.contains(n))
}

/// Whether `argv` starts with `words` — the program by basename at index 0,
/// then each subcommand word verbatim. A wrapper or env prefix does not match:
/// `xargs` could append a printing flag the mode never saw.
fn invokes(argv: &[String], words: &[&str]) -> bool {
    argv.len() >= words.len()
        && words.iter().enumerate().all(|(i, w)| {
            if i == 0 {
                command_basename(&argv[0]) == *w
            } else {
                argv[i] == *w
            }
        })
}

/// `args` with each clustered short flag of a [`Consumer::clusters`] mode
/// split into one flag per letter (`-lf` → `-l`, `-f`); every other token, and
/// any cluster carrying a letter the mode does not list, is left whole.
fn expand_clusters(consumer: &Consumer, args: &[String]) -> Vec<String> {
    let known =
        |flag: &str| consumer.switch_flags.contains(&flag) || consumer.key_flags.contains(&flag);
    args.iter()
        .flat_map(|tok| {
            let letters = tok.strip_prefix('-').filter(|l| {
                consumer.clusters
                    && l.len() > 1
                    && l.chars()
                        .all(|c| c.is_ascii_alphabetic() && known(&format!("-{c}")))
            });
            match letters {
                Some(l) => l.chars().map(|c| format!("-{c}")).collect(),
                None => vec![tok.clone()],
            }
        })
        .collect()
}

/// The tokens of `args` that sit in a key or output position, or `None` when
/// any token is one the mode does not know.
///
/// Test: `every_consumer_error_arm_fails_closed_8869`.
fn allowed_positions<'a>(consumer: &Consumer, args: &'a [String]) -> Option<Vec<&'a str>> {
    let mut allowed: Vec<&str> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut positionals = 0usize;
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        if is_redirect_shaped(tok) {
            i += read_redirect(consumer, args, i, &mut allowed)?;
            continue;
        }
        if tok.len() > 1 && tok.starts_with('-') {
            let (flag, joined) = match tok.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f, Some(v)),
                _ => (tok, None),
            };
            seen.push(flag);
            if consumer.switch_flags.contains(&flag) && joined.is_none() {
                i += 1;
                continue;
            }
            let value = match joined {
                Some(v) => v,
                None => args.get(i + 1)?.as_str(),
            };
            i += if joined.is_some() { 1 } else { 2 };
            if consumer.key_flags.contains(&flag) {
                allowed.push(is_literal_path(value).then_some(value)?);
            } else if consumer.output_flags.contains(&flag) {
                allowed.push(value);
            } else if consumer.method_flags.contains(&flag) {
                (value == "GET").then_some(())?;
            } else if !consumer.value_flags.contains(&flag) {
                return None;
            }
            continue;
        }
        if let Some(check) = consumer.positional_check {
            allowed.push(check(tok).then_some(tok)?);
        }
        positionals += 1;
        i += 1;
    }
    let required = consumer
        .required
        .iter()
        .all(|group| group.iter().any(|f| seen.contains(f)));
    let count_ok = consumer.positionals.is_none_or(|n| n == positionals);
    (required && count_ok).then_some(allowed)
}

/// Whether `tok` opens with a redirect operator, past an fd number or `&`.
fn is_redirect_shaped(tok: &str) -> bool {
    let rest = tok.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    rest.starts_with(['<', '>'])
}

/// Read the redirect at `args[i]`, pushing an output target or an fd-0 key onto
/// `allowed`; the answer is how many tokens it spans, or `None` when it is a
/// form the mode cannot vouch for (`<<<`, `<>`, `<&`, `3<`, a dangling `>`).
fn read_redirect<'a>(
    consumer: &Consumer,
    args: &'a [String],
    i: usize,
    allowed: &mut Vec<&'a str>,
) -> Option<usize> {
    let tok = args[i].as_str();
    let follows = || args.get(i + 1).map(String::as_str);
    match redirect_role(tok) {
        RedirectRole::FileDescriptor => return Some(1),
        RedirectRole::Target(target) => {
            allowed.push(target);
            return Some(1);
        }
        RedirectRole::TargetFollows => {
            allowed.push(follows()?);
            return Some(2);
        }
        RedirectRole::None => {}
    }
    let (fd, attached) = input_redirect_operand(tok)?;
    let (source, width) = if attached.is_empty() {
        (follows()?, 2)
    } else {
        (attached, 1)
    };
    if fd != 0 {
        return None;
    }
    // #8869: stdin is the key only for `gh secret set`; elsewhere it is data,
    // and a secret named there stays outside `allowed`, so the rule denies.
    if consumer.stdin_is_key {
        allowed.push(is_literal_path(source).then_some(source)?);
    }
    Some(width)
}

/// Whether `value` is a literal path: no glob, brace, quote, expansion or
/// operator byte, and not a flag.
///
/// Why: a key position is granted on the name written there; a glob or a
/// variable could resolve to a different file than the one screened.
fn is_literal_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@')
        })
}

/// Whether `endpoint` is a GitHub secret-listing path.
///
/// What: an optional leading `/`, then
/// `(repos/O/R|repositories/N|orgs/O|user)(/environments/E)?`
/// `(/actions|/dependabot|/codespaces)?/secrets(/public-key|/NAME)?`, with no
/// `..` and a query of only `per_page`/`page` numbers. A `%` escape fails the
/// segment charset of [`is_name`] and the paging digits, so it never matches.
/// Test: `allows_a_get_that_lists_secret_names_8869`,
/// `denies_a_secrets_path_that_is_not_a_get_listing_8869`.
fn is_secret_listing_endpoint(endpoint: &str) -> bool {
    if endpoint.contains("..") {
        return false;
    }
    let (path, query) = endpoint.split_once('?').unwrap_or((endpoint, ""));
    let paging = query.is_empty()
        || query.split('&').all(|pair| {
            pair.split_once('=').is_some_and(|(k, v)| {
                matches!(k, "per_page" | "page")
                    && !v.is_empty()
                    && v.bytes().all(|b| b.is_ascii_digit())
            })
        });
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let rest = match segments.as_slice() {
        ["repos", owner, repo, rest @ ..] if is_name(owner) && is_name(repo) => rest,
        ["repositories", id, rest @ ..] if id.bytes().all(|b| b.is_ascii_digit()) => rest,
        ["orgs", org, rest @ ..] if is_name(org) => rest,
        ["user", rest @ ..] => rest,
        _ => return false,
    };
    let rest = match rest {
        ["environments", env, tail @ ..] if is_name(env) => tail,
        _ => rest,
    };
    let rest = match rest {
        [area, tail @ ..] if matches!(*area, "actions" | "dependabot" | "codespaces") => tail,
        _ => rest,
    };
    let listing = match rest {
        ["secrets"] | ["secrets", "public-key"] => true,
        ["secrets", name] => is_name(name),
        _ => false,
    };
    paging && listing
}

/// One endpoint path segment: an owner, repo, environment or secret name, or
/// gh's `{owner}`/`{repo}` placeholder.
fn is_name(segment: &str) -> bool {
    matches!(segment, "{owner}" | "{repo}")
        || (!segment.is_empty()
            && segment != "."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
}

#[cfg(test)]
#[path = "pm_guard_secret_consumers_tests.rs"]
mod tests;
