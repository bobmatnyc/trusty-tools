//! Identifier-filename and fixed-grammar shapes for the secret detector.
//!
//! Why (issues #277, #8589): after #8589 a dry run of the kuzu-memory import
//! still refused 4,230 of 23,016 memories, and a per-token attribution found
//! one credential among them. 70% carried a `<Stem>.<ext>` file name that
//! misses the `is_symbol_path_segment` word floor (`BituKura.java`) or the
//! whole-segment mixed-case test (`BorFuigebWaokdaw7.java`). Most of the rest
//! were four fixed grammars: a GitHub noreply email, `KEY=<url>`, an npm
//! `name@version`, and a ticket key leading a CamelCase title.
//! What: one narrow predicate per shape, consulted by
//! `is_readable_path_segment` and `is_structural_token` in the parent module.
//! Test: the `_after_277` tests in `filter_tests.rs`.

use super::{
    IDENTIFIER_DELIMITERS, MAX_ACRONYM_LEN, MIN_MEAN_CAMEL_WORD_LEN, MIN_VOWEL_PERCENT,
    camel_words, digit_run_count, is_ordinary_url, is_provider_key, looks_like_secret,
    meets_vowel_floor,
};

/// Longest file extension [`is_identifier_file_segment`] accepts (`java`,
/// `woff2`). See #277.
pub(crate) const MAX_FILE_EXTENSION_LEN: usize = 5;

/// Longest numeric id before `+` in a GitHub noreply email. See #277.
pub(crate) const MAX_GITHUB_USER_ID_LEN: usize = 12;

/// Longest GitHub login. See #277.
pub(crate) const MAX_GITHUB_LOGIN_LEN: usize = 39;

/// Domain of a GitHub noreply commit email. See #277.
pub(crate) const GITHUB_NOREPLY_DOMAIN: &str = "users.noreply.github.com";

/// True when the alphabetic run `run` is a sequence of Title-case words or
/// acronyms that meets the #8589 mean-word-length and vowel floors.
///
/// Why (issue #277): base64 changes case about every other letter, so its
/// CamelCase words are short and single letters are common; an identifier's
/// words are not. The floors apply at every length, not only to the runs over
/// 20 letters that `is_readable_alpha_run` judges: with a looser floor below
/// 20 characters, random 16-character stems gained over 200 admits per 20k
/// and the `path_wrapped_encoder_blobs_stay_flagged_after_8589` base64 row
/// rose from 15 to 18. A lowercase word is refused for the same reason: it
/// doubled the random admits at 16 characters. A refused memory is
/// recoverable by re-import; an admitted secret is not.
/// What: splits with `camel_words`; every word opens with a capital, has at
/// least two letters, and carries at most [`MAX_ACRONYM_LEN`] leading
/// capitals; the mean word length is at least [`MIN_MEAN_CAMEL_WORD_LEN`]
/// and at least [`MIN_VOWEL_PERCENT`] of the letters are vowels.
/// Test: `identifier_file_name_clause_boundaries_after_277`,
/// `random_stems_as_file_names_stay_flagged_after_277`.
pub(crate) fn is_identifier_word_run(run: &str) -> bool {
    let mut words = 0usize;
    for w in camel_words(run.as_bytes()) {
        let caps = w.iter().take_while(|b| b.is_ascii_uppercase()).count();
        if caps == 0 || caps > MAX_ACRONYM_LEN || w.len() < 2 {
            return false;
        }
        words += 1;
    }
    words > 0
        && run.len() >= MIN_MEAN_CAMEL_WORD_LEN * words
        && meets_vowel_floor(run, MIN_VOWEL_PERCENT)
}

/// True when `seg` is a `<stem>.<ext>` file name whose stem has identifier
/// shape.
///
/// Why (issue #277): 70% of the memories the import still refused fail the
/// `is_symbol_path_segment` word floor, which asks a segment over 8
/// characters for a CamelCase word of 5 letters (`BituKura.java` has none), or
/// carry a digit in a 20+ character segment, which the whole-segment
/// mixed-case test refuses (`BorFuigebWaokdaw7.java`). A file name is judged
/// by its stem's word structure instead. Random stems are held off by the
/// digit-run and stray-letter caps (the #5043 discriminators) and by
/// [`is_identifier_word_run`]; `random_stems_as_file_names_stay_flagged_after_277`
/// pins the measured admits.
/// Known accepted bound: a word-composed passphrase with one digit group,
/// used as a file stem, is admitted (same class as FN-2, #1484) —
/// `vault/CorrectHorseBatteryStaple7.txt`, pinned in
/// `known_accepted_bounds_after_277`.
/// What: the extension (after the last `.`) is 1 to [`MAX_FILE_EXTENSION_LEN`]
/// lowercase letters or digits, opening with a letter. The stem is
/// alphanumeric plus [`IDENTIFIER_DELIMITERS`] and carries no
/// [`super::SECRET_PREFIXES`] entry. Each delimiter-separated piece holds at
/// most one digit run, at most one single-letter run, no AWS key id, and only
/// alphabetic runs of two or more letters that pass [`is_identifier_word_run`].
/// Test: `identifier_file_names_are_not_flagged_after_277`,
/// `identifier_file_name_clause_boundaries_after_277`,
/// `random_stems_as_file_names_stay_flagged_after_277`.
pub(crate) fn is_identifier_file_segment(seg: &str) -> bool {
    let Some((stem, ext)) = seg.rsplit_once('.') else {
        return false;
    };
    let ext_ok = (1..=MAX_FILE_EXTENSION_LEN).contains(&ext.len())
        && ext.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let stem_charset_ok = !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || IDENTIFIER_DELIMITERS.contains(&c));
    if !ext_ok || !stem_charset_ok || is_provider_key(seg) {
        return false;
    }
    stem.split(IDENTIFIER_DELIMITERS)
        .filter(|p| !p.is_empty())
        .all(|piece| {
            let mut strays = 0usize;
            let runs_ok = piece
                .split(|c: char| !c.is_ascii_alphabetic())
                .filter(|r| !r.is_empty())
                .all(|r| {
                    strays += usize::from(r.len() == 1);
                    r.len() == 1 || is_identifier_word_run(r)
                });
            runs_ok && strays <= 1 && digit_run_count(piece) <= 1 && !is_provider_key(piece)
        })
}

/// True when `token` is a GitHub noreply commit email,
/// `<digits>+<login>@users.noreply.github.com`.
///
/// Why (issue #277): `git log` output in a memory carries these, and the `+`
/// sends the token to the base64 branch, whose `has_digit` floor the numeric
/// id satisfies.
/// What: exact [`GITHUB_NOREPLY_DOMAIN`] after the first `@`; before it, 1 to
/// [`MAX_GITHUB_USER_ID_LEN`] digits, `+`, and a GitHub login (1 to
/// [`MAX_GITHUB_LOGIN_LEN`] alphanumerics or single inner hyphens) that is
/// not itself credential-shaped, as [`is_key_equals_url`] screens its key.
/// Test: `noreply_key_url_npm_and_ticket_shapes_are_not_flagged_after_277`,
/// `noreply_key_url_npm_and_ticket_boundaries_after_277`.
pub(crate) fn is_github_noreply_email(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    let Some((id, login)) = local.split_once('+') else {
        return false;
    };
    domain == GITHUB_NOREPLY_DOMAIN
        && (1..=MAX_GITHUB_USER_ID_LEN).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && (1..=MAX_GITHUB_LOGIN_LEN).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--")
        // #277 review: an unscreened login let a 39-char base62 key through.
        && !looks_like_secret(login)
}

/// True when `token` is `KEY=<ordinary URL>`, an environment variable holding
/// a URL with no userinfo.
///
/// Why (issue #277): the `=` branch of `is_structural_token` refuses any
/// `/`-bearing right-hand side that is not a slash path, and the `//` of a
/// URL is never one, so `REDIS_URL=redis://cache:6379` fell to the base64
/// branch. The URL keeps every check `is_ordinary_url` makes; only the
/// `KEY=` in front is new.
/// What: the key is 1 to 64 ASCII alphanumerics or `_`, not opening with a
/// digit, and not itself credential-shaped; the value passes
/// `is_ordinary_url` and has no `@` in its authority, so neither `user:pass@`
/// nor `user@` rides in.
/// Test: `noreply_key_url_npm_and_ticket_shapes_are_not_flagged_after_277`,
/// `noreply_key_url_npm_and_ticket_boundaries_after_277`.
pub(crate) fn is_key_equals_url(token: &str) -> bool {
    let Some((key, url)) = token.split_once('=') else {
        return false;
    };
    let key_ok = (1..=64).contains(&key.len())
        && key.bytes().next().is_some_and(|b| !b.is_ascii_digit())
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let no_userinfo = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .is_some_and(|authority| !authority.contains('@'));
    key_ok && no_userinfo && !looks_like_secret(key) && is_ordinary_url(url)
}

/// True when `seg` is an npm `name@version` with a semver version, such as
/// `left-pad@1.3.0` or `react-dom@18.2.0-rc.1`.
///
/// Why (issue #277): `@` is outside `is_word_segment`'s charset, so a path
/// through `node_modules/<name>@<version>` failed its segment test and fell
/// to the base64 branch.
/// What: the name is lowercase letters, digits, `-`, `.` and `_`, opening
/// with a letter or digit, with no [`super::SECRET_PREFIXES`] entry; the
/// version is `MAJOR.MINOR.PATCH` of 1-6 digits each, optionally followed by
/// `-` and a prerelease of 1-32 lowercase letters, digits, `.` or `-`. Build
/// metadata (`+…`) is not accepted.
/// Test: `noreply_key_url_npm_and_ticket_shapes_are_not_flagged_after_277`,
/// `noreply_key_url_npm_and_ticket_boundaries_after_277`.
pub(crate) fn is_npm_package_version(seg: &str) -> bool {
    let Some((name, version)) = seg.split_once('@') else {
        return false;
    };
    let name_ok = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
        })
        && !is_provider_key(name);
    let (core, prerelease) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let core_ok = core.split('.').count() == 3
        && core
            .split('.')
            .all(|n| (1..=6).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()));
    let prerelease_ok = prerelease.is_none_or(|p| {
        (1..=32).contains(&p.len())
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
    });
    name_ok && core_ok && prerelease_ok
}

/// True when `token` is a ticket key and number leading a CamelCase title,
/// `[A-Z]{2,10}-[0-9]{1,6}-<CamelTitle>`, such as `QX-4821-TelvoMarun`.
///
/// Why (issue #277): a branch or document name built from a JIRA-style key
/// mixes case and carries a digit, and its title holds several capitals, so
/// the segmented-identifier branch declines it and the mixed-case branch
/// fires.
/// What: an uppercase key of 2-10 letters, `-`, 1-6 digits, `-`, then a title
/// of ASCII letters that passes [`is_identifier_word_run`].
/// Test: `noreply_key_url_npm_and_ticket_shapes_are_not_flagged_after_277`,
/// `noreply_key_url_npm_and_ticket_boundaries_after_277`.
pub(crate) fn is_ticket_camel_title(token: &str) -> bool {
    let mut parts = token.splitn(3, '-');
    let (Some(key), Some(number), Some(title)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    (2..=10).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_uppercase())
        && (1..=6).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
        && title.bytes().all(|b| b.is_ascii_alphabetic())
        && is_identifier_word_run(title)
}
