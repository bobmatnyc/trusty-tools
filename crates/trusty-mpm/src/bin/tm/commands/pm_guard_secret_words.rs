//! Cutting a Bash command's text into the candidate PATH WORDS the secret-read
//! guard screens.
//!
//! Why: `pm_guard_secret_read` decides on the FILE a command names, so the
//! question "which words of this text could be a path?" is asked of every
//! surface it guards — an argv token, an unlexable segment, a here-document
//! body, an interpreter's inline program. The answer is one layer with its own
//! failure modes: a `${…}` expansion whose operator is a cut byte (#7266
//! round 7), an operand spliced against the bytes beside it (round 8), and a
//! brace literal the cut splits in two (#7414). Split out of
//! `pm_guard_secret_read.rs` when #7414 pushed that file over the 500-SLOC cap,
//! the same way `pm_guard_bash::path_tokens` came out of its own `mod.rs`.
//!
//! What: [`is_path_byte`] is the cut itself and [`normalize_bracket_classes`]
//! the glob repair that follows it. [`scan_spellings`] is the entry point —
//! it returns every reading of one text the caller must screen, because no
//! single spelling is trusted to be the only way a shell reads it.
//!
//! One residual is named rather than silently allowed: a text whose brace
//! alternatives multiply past [`MAX_BRACE_EXPANSION`] is scanned raw, so a
//! literal brace the cut splits there fails CLOSED. That is a false-positive
//! trade taken deliberately — the alternative is the round-12 hole, where an
//! unbounded group was scanned through the lossy orphan drop alone.
//!
//! Test: `allows_a_parameter_expansion_that_names_no_secret`,
//! `denies_a_parameter_expansion_whose_operand_names_a_secret`,
//! `splits_a_parameter_expansion_into_its_name_and_operand`,
//! `splices_an_operand_against_the_bytes_beside_it`,
//! `denies_a_secret_name_split_across_a_span_boundary`,
//! `allows_a_brace_literal_passed_as_an_argument_value`,
//! `a_real_brace_alternation_in_argv_still_denies`,
//! `drops_only_the_braces_the_cut_orphaned`,
//! `denies_a_secret_hidden_by_nesting_or_cap_overflow`,
//! `normalize_bracket_classes_collapses_a_class_and_drops_a_stray` in
//! `pm_guard_secret_read`'s `tests` submodule, which owns every caller of this
//! layer.

/// Every reading of `text` the word scan must screen.
///
/// Why: a shell can read one string more than one way, and round 8 established
/// that the guard scans ALL of them rather than picking a winner — scanning an
/// extra spelling can only ADD a deny word, while picking wrong loses one.
/// [`drop_split_orphan_braces`] is the exception to that discipline: it REMOVES
/// bytes, so it is the one repair that can lose a name, and round 12's critic
/// found both ways it did. `cat .{e:x,{y,env}}` and
/// `cat .{x:1,<70 more>,env}` each ALLOWED on `cd8c42991` while bash reads
/// `.env` out of the middle alternative.
/// What: [`rewrite_parameter_expansions`] first, then
/// [`bounded_brace_readings`] — every string the shell's own brace expansion
/// can produce — and the orphan drop is applied to EACH of those readings
/// rather than to the raw text. A reading is therefore braceless wherever the
/// expansion resolved, so the drop can only touch a brace that is literal.
/// When the expansion cannot be bounded the readings are unavailable, and the
/// raw text is scanned instead so its unresolved brace fails CLOSED — the same
/// answer [`is_secret_read_target`](super::pm_guard_secret_read::is_secret_read_target) gives a brace it cannot expand.
/// Test: `allows_a_brace_literal_passed_as_an_argument_value`,
/// `a_real_brace_alternation_in_argv_still_denies`,
/// `denies_a_secret_hidden_by_nesting_or_cap_overflow`.
pub(crate) fn scan_spellings(text: &str) -> Vec<String> {
    let scanned = rewrite_parameter_expansions(text);
    // #7414: an expansion this layer cannot bound leaves the orphan drop
    // unchecked, so the raw spelling is scanned and its brace fails closed.
    let Some(readings) = bounded_brace_readings(&scanned) else {
        return vec![scanned];
    };
    readings
        .iter()
        .map(|reading| drop_split_orphan_braces(reading))
        .collect()
}

/// Bytes a filename can carry, for the purpose of cutting a raw segment into
/// candidate path words.
///
/// Why: the deny must not depend on lexing, because `echo "$(cat .env)"`,
/// `php -r 'readfile(".env")'` and an unbalanced quote all defeat a lexer while
/// still naming the file in plain text. Cutting at every byte a path cannot
/// contain surfaces the name in all three.
/// What: ASCII alphanumerics plus the punctuation a real path uses, INCLUDING
/// the glob metacharacters `*`, `?`, `[` and `]`. A quote, `$`, `(`, `=`, `<`,
/// `:` and whitespace are all cuts, so `if=.env` and `"$(cat .env)"` each yield
/// the bare name.
///
/// The four glob bytes are kept in the word rather than cut at (#7266 round 6,
/// critic CRITICAL 1). Cutting at them threw the wildcard away and left a
/// remainder that matched nothing: `cat .en?` yielded `.en`, `cat .e*` yielded
/// `.e`, `cat id_rs?` yielded `id_rs`, and all three ALLOWED while naming a
/// glob the shell expands onto the real file. Kept in the word, each reaches
/// [`is_secret_read_target`](super::pm_guard_secret_read::is_secret_read_target), which has screened a caller's PATTERN — not just
/// a literal name — since round 3, and which the `Grep` `glob` arm has used all
/// along.
///
/// `{` and `}` are kept for brace ALTERNATION, which is not the only thing a
/// brace spells: [`rewrite_parameter_expansions`] removes every `${…}` span
/// before this cut runs, because an expansion's operator (`:`, `#`, `%`) IS a
/// cut and the `{VAR` it leaves behind fails closed (#7266 round 7). A JSON or
/// jq literal splits the same way — this cut runs THROUGH `{"a":"b"}`, leaving
/// its braces in different fragments — so [`drop_split_orphan_braces`] removes
/// a brace the cut has orphaned before the words are matched (#7414).
/// Test: `secret_files_named_in_finds_a_name_inside_a_program_string`,
/// `denies_a_glob_that_expands_onto_a_secret_file`,
/// `allows_a_parameter_expansion_that_names_no_secret`.
pub(crate) fn is_path_byte(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '.' | '_' | '-' | '/' | '~' | '+' | '@' | '{' | '}' | ',' | '*' | '?' | '[' | ']'
        )
}

/// A name or glob with every bracket class collapsed to a single `?`.
///
/// Why: [`is_path_byte`] now keeps `[` and `]` in the word, and a bracket class
/// is the one glob shape the shared matcher does not implement — `cat id_[r]sa`
/// reached `is_secret_bearing_name` as the literal `id_[r]sa`, matched no
/// pattern, and ALLOWED (#7266 round 6, critic CRITICAL 1). A class matches
/// exactly one character, so `?` is its faithful stand-in, and `?` is a
/// WIDENING of it — every string the class reaches, `?` reaches too — which
/// puts the approximation on the deny side.
/// What: a balanced non-empty `[…]` becomes `?`; a stray `[` or `]` is dropped,
/// so a malformed class falls back to the name around it (`cat .env]` still
/// denies) rather than shielding it.
/// Test: `normalize_bracket_classes_collapses_a_class_and_drops_a_stray`,
/// `denies_a_glob_that_expands_onto_a_secret_file`.
pub(crate) fn normalize_bracket_classes(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => match chars[i + 1..].iter().position(|c| *c == ']') {
                Some(close) if close > 0 => {
                    out.push('?');
                    i += close + 2;
                }
                _ => i += 1,
            },
            ']' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}
/// The operator spellings that separate a `${…}` parameter's NAME from the
/// word the expansion can produce, longest spelling first.
///
/// Why: the operand is the only part of an expansion that can carry a
/// filename, and it is only reachable once the operator in front of it is
/// removed. Leaving the operator on glues it to the name — `${F:-.env}` scans
/// as `-.env`, which matches no pattern and ALLOWS the very shape this rule
/// exists to refuse (#7266 round 7).
/// What: the `:`-guarded and bare default/assign/error/alternate forms, the
/// `#`/`%` prefix and suffix trims, the `/` substitutions, the case and
/// transform operators, and the bare `:` that opens a substring range. Order
/// is significant — a two-character spelling is tried before the
/// one-character spelling it starts with.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`,
/// `allows_a_parameter_expansion_that_names_no_secret`.
const PARAMETER_EXPANSION_OPERATORS: &[&str] = &[
    ":-", ":=", ":?", ":+", "##", "%%", "//", ",,", "^^", "#", "%", "/", "^", ",", "@", ":", "-",
    "=", "?", "+",
];

/// One `${…}` expansion's parameter NAME and the operand behind its operator.
///
/// Why: see [`PARAMETER_EXPANSION_OPERATORS`]. Both halves are returned rather
/// than the operand alone, so a parameter whose NAME is itself a secret-shaped
/// filename (`${id_rsa}`) keeps denying exactly as it did before round 7.
/// What: strips a leading `#` (length) or `!` (indirection) sigil, takes the
/// longest run of `[A-Za-z0-9_]` as the name — or one character when the
/// parameter is a special one like `@` or `*` — then removes the first
/// matching operator. Text after an operator this list does not carry is
/// returned whole, so an unrecognised form is scanned rather than skipped.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`.
pub(crate) fn split_parameter_expansion(inner: &str) -> (&str, &str) {
    let body = inner
        .strip_prefix('#')
        .or_else(|| inner.strip_prefix('!'))
        .unwrap_or(inner);
    let name_len = body
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(body.len());
    let (name, rest) = if name_len == 0 {
        let mut chars = body.chars();
        let taken = chars.next().map_or(0, char::len_utf8);
        body.split_at(taken)
    } else {
        body.split_at(name_len)
    };
    for op in PARAMETER_EXPANSION_OPERATORS {
        if let Some(operand) = rest.strip_prefix(op) {
            return (name, operand);
        }
    }
    (name, rest)
}

/// Index of the `}` closing the `{` at `open`, or `None` when nothing does.
///
/// What: counts nesting, so `${A:-${B}}` yields the OUTER close.
/// Test: `splits_a_parameter_expansion_into_its_name_and_operand`.
pub(crate) fn matching_close_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, c) in chars.get(open..)?.iter().enumerate() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` with every `${…}` parameter expansion replaced by the words it can
/// actually name.
///
/// Why: #7266 round 7, critic CRITICAL 1. [`is_path_byte`] keeps `{` and `}`
/// for brace ALTERNATION (`cp secret.{tfvars,bak}`) but cuts at `:`, `#` and
/// `%`, so every expansion carrying an operator left an unmatched `{VAR`
/// behind. `expand_brace_alternatives` finds no closing brace for it, answers
/// `None`, and [`is_secret_read_target`](super::pm_guard_secret_read::is_secret_read_target) fails CLOSED — so
/// `mkdir -p "${OUT_DIR:-build}"`, `echo "${1:-default}"` and
/// `cp "${SRC%.rs}.bak" x` were all refused with no secret named.
///
/// A `${…}` span is parameter expansion and a bare `{…}` group is brace
/// alternation; only the first is rewritten here, so the alternation arm is
/// untouched. Failing OPEN on the whole span would reopen `${x:-.env}`, so the
/// span is not skipped — it is REWRITTEN to its name and its operand, and both
/// are scanned. `${VAR}` and `$VAR` reach the same words they always did.
///
/// Round 8, critic CRITICAL: round 7 put a separator on BOTH sides of the
/// operand, so `${F:-.en}v` scanned as the three words `F`, `.en` and `v` and
/// the name the shell actually builds — `.env` — appeared in none of them.
/// `cat "${F:-.en}v"`, `cat "${F:-.e}${G:-nv}"`, `cat ${F:-id_rs}a`,
/// `cat ${F:-id_}rsa` and `cat id_${F:-rsa}` all ALLOWED on `5b7f629e4`. The
/// SPLICED spelling — operand glued to the literal bytes on either side, so two
/// adjacent spans join their operands and an empty operand leaves its
/// neighbours contiguous — closes that.
///
/// Both spellings are scanned, not just the spliced one. The SEPARATED
/// spelling is what makes `cat "${F:-x}.env"` deny: there the secret is the
/// literal TAIL, which is a word of its own only while the span emits a
/// trailing separator (`x.env` matches no pattern). Round 7 shipped that row
/// denying and the round-8 verdict requires it preserved, so the separated
/// spelling stays and the spliced one is added beside it. Scanning both can
/// only ADD deny words, never remove one.
/// What: walks each balanced `${…}` twice. The spliced walk emits the operand
/// with no separator and collects every parameter NAME into a trailing
/// word list, so a name can never glue onto a neighbour; the separated walk is
/// round 7's ` <name> <operand> `. Both recurse into the operand, so a nested
/// expansion resolves in each. An UNBALANCED `${` is left exactly as it stands
/// in both, which keeps that shape failing closed.
/// Test: `allows_a_parameter_expansion_that_names_no_secret`,
/// `denies_a_parameter_expansion_whose_operand_names_a_secret`,
/// `splices_an_operand_against_the_bytes_beside_it`.
pub(crate) fn rewrite_parameter_expansions(text: &str) -> String {
    if !text.contains("${") {
        return text.to_string();
    }
    let mut names = String::new();
    let spliced = walk_parameter_expansions(text, true, &mut names);
    let separated = walk_parameter_expansions(text, false, &mut String::new());
    format!("{spliced}{names} {separated}")
}

/// One walk of [`rewrite_parameter_expansions`], in either spelling.
///
/// What: with `splice`, each `${…}` contributes only its operand to the
/// returned text and pushes its NAME onto `names`; without it, the span becomes
/// ` <name> <operand> `. `names` is untouched by the separated walk.
/// Test: `splices_an_operand_against_the_bytes_beside_it`.
pub(crate) fn walk_parameter_expansions(text: &str, splice: bool, names: &mut String) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$'
            && chars.get(i + 1) == Some(&'{')
            && let Some(close) = matching_close_brace(&chars, i + 1)
        {
            let inner: String = chars[i + 2..close].iter().collect();
            let (name, operand) = split_parameter_expansion(&inner);
            if splice {
                names.push(' ');
                names.push_str(name);
            } else {
                out.push(' ');
                out.push_str(name);
                out.push(' ');
            }
            out.push_str(&walk_parameter_expansions(operand, splice, names));
            if !splice {
                out.push(' ');
            }
            i = close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Upper bound on how many readings [`bounded_brace_readings`] will build.
///
/// Why: the expansion is a cartesian product over every alternation group in
/// the text, so a crafted argument with many comma-heavy groups could ask for
/// an unbounded allocation.
/// What: 64 — far above any real `cp a.{x,y,z} dst`, and the recursion stops
/// the moment the product reaches it, so the refusal costs one allocation per
/// reading already built. Over the bound the readings are NOT available and
/// [`scan_spellings`] falls back to the raw text, which fails closed; round 12
/// dropped the expansion spelling and kept scanning, and that is the hole
/// `cat .{x:1,<70 more>,env}` walked through.
const MAX_BRACE_EXPANSION: usize = 64;

/// `text` with every brace the CUT orphaned removed (#7414).
///
/// Why: [`is_path_byte`] keeps `{` and `}` in a word for brace ALTERNATION but
/// cuts at `"` and `:`, so a JSON or jq literal passed as a plain argument
/// value splits with its `{` and its `}` in different fragments. The orphaned
/// fragment reaches [`is_secret_read_target`](super::pm_guard_secret_read::is_secret_read_target), which answers `None` for a
/// brace it cannot resolve and therefore fails CLOSED. Live on tm 1.5.27 that
/// refused `curl -d '{"position":"above"}'` (naming `` `{` ``),
/// `gh issue view -q '{title,labels:[…]}'` (naming `` `{title,labels` ``) and
/// `gh issue create --body '… {p+=$4} …'` (naming `` `{p+` ``) — three
/// commands naming no file at all. Round 9's `Scan::ProgramText` leniency
/// does not reach them: `curl` and `gh` take argv, not an interpreter body.
/// What: within each maximal run of path bytes — one fragment of the cut — a
/// `{` is matched against a `}` with a stack. Every brace left unmatched is
/// REMOVED, which glues the prefix to the first alternative and the last
/// alternative to the suffix, so a name the cut split across the brace still
/// surfaces (`.{env,x:y}` reads `.env,x:y`, which `.env*` matches). A brace
/// pair that survives the cut whole is untouched, so `cp secret.{tfvars,bak}`
/// and `cat {.env,.env.prod}` expand and deny exactly as before. A `{`
/// preceded by `$` is a PARAMETER expansion, not an alternation, and is never
/// dropped — that is what keeps an unbalanced `${VAR` failing closed
/// (#7266 round 7).
///
/// Removing bytes can lose a name, so [`scan_spellings`] runs this over each
/// [`bounded_brace_readings`] reading rather than over the raw text: by then
/// every alternation has already been resolved, and the braces this sees are
/// literal ones.
/// Test: `drops_only_the_braces_the_cut_orphaned`,
/// `allows_a_brace_literal_passed_as_an_argument_value`,
/// `a_real_brace_alternation_in_argv_still_denies`.
pub(crate) fn drop_split_orphan_braces(text: &str) -> String {
    if !text.contains('{') && !text.contains('}') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = vec![true; chars.len()];
    let mut run_start = 0usize;
    for index in 0..=chars.len() {
        if index < chars.len() && is_path_byte(chars[index]) {
            continue;
        }
        mark_orphan_braces(&chars, run_start, index, &mut keep);
        run_start = index + 1;
    }
    chars
        .iter()
        .zip(keep)
        .filter_map(|(c, keep)| keep.then_some(*c))
        .collect()
}

/// Clear `keep` for every brace in `chars[start..end]` with no partner there.
///
/// What: the stack walk [`drop_split_orphan_braces`] documents, over one
/// fragment. A `{` whose preceding character is `$` opens no alternation, so it
/// is neither pushed nor droppable.
/// Test: `drops_only_the_braces_the_cut_orphaned`.
fn mark_orphan_braces(chars: &[char], start: usize, end: usize, keep: &mut [bool]) {
    let mut open: Vec<usize> = Vec::new();
    for index in start..end {
        match chars[index] {
            // #7414: `${` is a parameter expansion — round 7 owns that shape.
            '{' if index == 0 || chars[index - 1] != '$' => open.push(index),
            '}' if open.pop().is_none() => keep[index] = false,
            _ => {}
        }
    }
    for index in open {
        keep[index] = false;
    }
}

/// Every string the shell's brace expansion can read `text` as, or `None` past
/// [`MAX_BRACE_EXPANSION`].
///
/// Why: [`drop_split_orphan_braces`] reconstructs only the first and last
/// alternative of a group the cut split, so a group whose MIDDLE alternative
/// carries the secret is lost — bash reads `cat .{e:x,env}` as `.e:x` and
/// `.env`, and the dropped spelling yields neither. Round 12 answered that with
/// the shared [`expand_brace_alternatives`](super::pm_guard_bash::expand_brace_alternatives), which resolves neither a NESTED
/// group nor one past the bound and returned `None` for both; the orphan-drop
/// spelling was then trusted alone and `cat .{e:x,{y,env}}` and
/// `cat .{x:1,<70 more>,env}` both ALLOWED. Resolving nesting here is what
/// makes the drop safe to apply afterwards.
/// What: bash's own rule, so a reading is a string the shell really produces.
/// The first `{` that has a matching `}` AND a comma at its own depth is the
/// alternation; every other `{` is literal and the scan continues INSIDE it, so
/// `{"outer":{"inner":1}}` — no comma at any depth — reads as itself while
/// `{a:{b,c}}` reads as `{a:b}` and `{a:c}`. A `{` behind `$` is a parameter
/// expansion and is never a group. An UNBALANCED brace is literal text, as it
/// is in a shell, so that shape needs no refusal here — it still fails closed
/// later, where [`drop_split_orphan_braces`] leaves it inside one fragment.
/// `None` is reserved for the one thing the reading cannot survive: a cartesian
/// product past [`MAX_BRACE_EXPANSION`].
/// Test: `drops_only_the_braces_the_cut_orphaned`,
/// `a_real_brace_alternation_in_argv_still_denies`,
/// `denies_a_secret_hidden_by_nesting_or_cap_overflow`.
pub(crate) fn bounded_brace_readings(text: &str) -> Option<Vec<String>> {
    let Some((open, close, alternatives)) = alternation_group(text) else {
        return Some(vec![text.to_string()]);
    };
    let prefix = text.get(..open)?;
    let tails = bounded_brace_readings(text.get(close + 1..)?)?;
    let mut out = Vec::new();
    for alternative in alternatives {
        for head in bounded_brace_readings(alternative)? {
            for tail in &tails {
                if out.len() >= MAX_BRACE_EXPANSION {
                    return None;
                }
                out.push(format!("{prefix}{head}{tail}"));
            }
        }
    }
    Some(out)
}

/// The first alternation group in `text`: its `{`, its `}`, and its
/// alternatives.
///
/// What: the leftmost `{` that is not a `${`, has a matching `}`, and carries a
/// comma at its own depth. A `{` failing any of those is literal, and the walk
/// continues at the next byte — which is how a group NESTED inside a
/// comma-less one is still found.
/// Test: `drops_only_the_braces_the_cut_orphaned`.
fn alternation_group(text: &str) -> Option<(usize, usize, Vec<&str>)> {
    let bytes = text.as_bytes();
    for (open, byte) in bytes.iter().enumerate() {
        // #7414: `${` is a parameter expansion — round 7 owns that shape.
        if *byte != b'{' || (open > 0 && bytes[open - 1] == b'$') {
            continue;
        }
        let Some(close) = matching_brace_byte(bytes, open) else {
            continue;
        };
        let alternatives = top_level_alternatives(text.get(open + 1..close)?);
        if alternatives.len() > 1 {
            return Some((open, close, alternatives));
        }
    }
    None
}

/// Byte index of the `}` closing the `{` at `open`, or `None` when nothing
/// does.
///
/// What: the byte-index twin of [`matching_close_brace`], which the parameter
/// walk needs in char indices. `{` and `}` are ASCII, so every index it returns
/// is a char boundary.
/// Test: `drops_only_the_braces_the_cut_orphaned`.
fn matching_brace_byte(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// One group's alternatives: `inner` split at every comma at depth zero.
///
/// What: a comma inside a nested `{…}` belongs to that group, not this one, so
/// `"outer":{"inner":1,"z":2}` is ONE alternative and the outer braces stay
/// literal. A single element means the group is not an alternation at all.
/// Test: `drops_only_the_braces_the_cut_orphaned`.
fn top_level_alternatives(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, byte) in inner.as_bytes().iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(&inner[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&inner[start..]);
    out
}
