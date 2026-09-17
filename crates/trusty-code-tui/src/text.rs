//! Pure text-shaping helpers shared by [`crate::app`]'s chat mutators and
//! [`crate::widgets::scrollback`]'s renderer.
//!
//! Why: LLM/agent responses routinely arrive with extra leading/trailing
//! blank lines (a model's closing paragraph break) and interior `\n\n`
//! paragraph gaps that read as wasted vertical space once rendered into a
//! fixed-height terminal pane. Both problems are pure string transforms with
//! no dependency on `ReplApp` or ratatui, so they live in their own module
//! rather than being duplicated between the state mutator that trims stored
//! text ([`crate::app::ReplApp::push_assistant`]) and any future consumer
//! that needs the same shaping. Extracted verbatim (behavior-preserving) from
//! `crates/trusty-agents/src/repl/tui/helpers.rs`'s `trim_surrounding_blank_lines`/
//! `strip_interior_blank_lines` (DOC-50 §3.1/§5 Slice 4 migration).
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4).

/// Shorten `text` to `width` columns, preferring `/` boundaries (#8164).
///
/// Why: a splash or status line that names an absolute path routinely
/// overflows its column, and clipping the tail removes the leaf — the one
/// component a reader is checking. Eliding whole path components instead
/// keeps the deepest directories intact, which is where a repository name
/// and a worktree name live.
///
/// What it GUARANTEES, and nothing more: `text` that already fits `width` is
/// returned unchanged, and the result never exceeds `width` — except at a
/// `width` of 0 or 1, where the one-column `…` is kept rather than an empty
/// string. For a path, the deepest components arrive WHOLE behind a leading
/// `…/`, and the repository directory name survives alongside them: when the
/// deepest contiguous run that fits does not reach back to the repository —
/// a linked worktree under `<repo>/.claude/worktrees/<name>` is the
/// motivating case (#8205) — that run is replaced by `…/<repo>/…/<tail>`, so
/// the name a reader identifies the checkout by stays on the row. The two
/// columns each `…/` costs are part of the budget.
///
/// Order of surrender, repository name LAST (owner ruling 2026-09-17): tail
/// components go one at a time, then the tail entirely (`…/<repo>/…`), and
/// only a repository name too wide for `width` itself falls through. That
/// fall-through, a path carrying no repository marker whose last two
/// components alone overflow, and a string with no `/` all land on character
/// elision, which keeps both ends and replaces the middle with `…`. Widths
/// count `char`s, never bytes.
/// Test: `tests::elide_middle_keeps_whole_trailing_path_components`,
/// `tests::elide_middle_keeps_the_repo_name_at_both_render_budgets`,
/// `tests::elide_middle_keeps_a_repo_name_that_is_the_leaf`,
/// `tests::elide_middle_drops_the_tail_before_the_repo_name`,
/// `tests::elide_middle_keeps_a_fitting_path_unchanged`,
/// `tests::elide_middle_falls_back_to_character_elision`,
/// `tests::elide_middle_handles_degenerate_widths`.
pub fn elide_middle(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if text.contains('/')
        && let Some(elided) = elide_path_components(text, width)
    {
        return elided;
    }
    elide_chars(text, width)
}

/// Directory names whose PARENT component names a git repository.
///
/// Why (#8205): a bare path is the only input `elide_middle` gets, so the
/// repository has to be recognised from shape. Both worktree layouts put the
/// repository directly above an administrative directory —
/// `<repo>/.claude/worktrees/<name>` (this workspace's own) and
/// `<repo>/.git/worktrees/<name>` (git's) — which makes the component above
/// one of these names the repository. Nothing here touches the filesystem.
const REPO_MARKERS: [&str; 2] = [".claude", ".git"];

/// Index of the component naming the repository, per [`REPO_MARKERS`].
///
/// What: the component immediately above the LAST marker, so the nearest
/// enclosing repository wins on a path that nests two. Returns an index, not
/// the name, because the caller also has to ask whether a tail it already
/// kept covers it. A marker at index 0 has no parent and yields `None`.
/// Test: `tests::elide_middle_keeps_the_repo_name_at_both_render_budgets`,
/// `tests::elide_middle_keeps_a_repo_name_that_is_the_leaf`.
fn repo_anchor(components: &[&str]) -> Option<usize> {
    components
        .iter()
        .rposition(|c| REPO_MARKERS.contains(c))
        .filter(|&marker| marker > 0)
        .map(|marker| marker - 1)
}

/// [`elide_middle`]'s path arm: the deepest components that fit, with the
/// repository name kept alongside them.
///
/// Why kept separate: it can legitimately fail (a lone component wider than
/// `width`), and `None` is what routes the caller to character elision rather
/// than to a path rendering that does not fit.
/// What: prefers [`deepest_contiguous`], which has no interior gap, and takes
/// it whenever the path names no repository or the run already reaches the
/// repository. Otherwise the repository name is spliced back in by
/// [`anchored_on_repo`] — the #8205 defect was returning the gapless run even
/// when it had elided the one name identifying the checkout.
/// Test: `tests::elide_middle_keeps_the_repo_name_at_both_render_budgets`,
/// `tests::elide_middle_keeps_whole_trailing_path_components`,
/// `tests::elide_middle_falls_back_to_character_elision`.
fn elide_path_components(text: &str, width: usize) -> Option<String> {
    let components: Vec<&str> = text.split('/').filter(|c| !c.is_empty()).collect();
    let anchor = repo_anchor(&components);
    let contiguous = deepest_contiguous(&components, width);
    let reaches_repo = |kept: usize| anchor.is_none_or(|a| a >= components.len() - kept);
    if let Some((kept, rendered)) = &contiguous
        && reaches_repo(*kept)
    {
        return Some(rendered.clone());
    }
    // #8205: the repository name outlives the tail, so an anchored rendering
    // that fits beats the gapless run that dropped the name.
    match anchor.and_then(|a| anchored_on_repo(&components, a, width)) {
        Some(anchored) => Some(anchored),
        None => contiguous.map(|(_, rendered)| rendered),
    }
}

/// `…/<tail>`: the deepest run of whole components that fits `width`.
///
/// What: walks from the leaf backwards, accepting each component while
/// `…/<tail>` stays within `width`, and reports how many it kept so the
/// caller can tell whether the repository name is among them. `None` unless
/// at least the last TWO were accepted — one lone directory name behind an
/// ellipsis says less than head-and-tail character elision would.
/// Test: `tests::elide_middle_keeps_a_repo_name_that_is_the_leaf`,
/// `tests::elide_middle_falls_back_to_character_elision`.
fn deepest_contiguous(components: &[&str], width: usize) -> Option<(usize, String)> {
    let mut kept = 0usize;
    let mut rendered = String::new();
    for count in 1..=components.len() {
        let tail = components[components.len() - count..].join("/");
        let candidate = format!("…/{tail}");
        if candidate.chars().count() > width {
            break;
        }
        kept = count;
        rendered = candidate;
    }
    (kept >= 2).then_some((kept, rendered))
}

/// `…/<repo>/…/<tail>`: the repository name, then the deepest tail that fits.
///
/// What: tries tails from as deep as the path allows down to the leaf alone,
/// never reaching back into the repository component itself, and returns the
/// longest that fits. When no tail fits, the bare `…/<repo>/…` keeps the name
/// on its own — the repository name is the last thing surrendered (owner
/// ruling 2026-09-17). `None` only when even that bare form overflows, which
/// hands a repository name wider than `width` to character elision.
/// Test: `tests::elide_middle_keeps_the_repo_name_at_both_render_budgets`,
/// `tests::elide_middle_drops_the_tail_before_the_repo_name`.
fn anchored_on_repo(components: &[&str], anchor: usize, width: usize) -> Option<String> {
    let repo = components[anchor];
    let mut rendered: Option<String> = None;
    for count in 1..components.len() - anchor {
        let tail = components[components.len() - count..].join("/");
        let candidate = format!("…/{repo}/…/{tail}");
        if candidate.chars().count() > width {
            break;
        }
        rendered = Some(candidate);
    }
    rendered.or_else(|| {
        let bare = format!("…/{repo}/…");
        (bare.chars().count() <= width).then_some(bare)
    })
}

/// [`elide_middle`]'s fallback: keep both ends, replace the middle with `…`.
///
/// What: splits the `width - 1` remaining budget between the two ends, the
/// odd character going to the head. Counts chars, not bytes. A `width` of 0
/// or 1 returns the bare `…`, so a zero budget is overshot by one column
/// rather than answered with an empty string.
/// Test: `tests::elide_middle_falls_back_to_character_elision`,
/// `tests::elide_middle_handles_degenerate_widths`.
fn elide_chars(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let keep = width - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// Strip leading and trailing whitespace-only lines from a multi-line string.
///
/// Why: Assistant responses regularly include extra `\n\n` at the head or
/// tail. Rendering them verbatim leaves visible blank rows in the chat
/// scrollback, which reads as a UI bug. Trimming once at the boundary keeps
/// interior blank lines (which carry paragraph-break meaning) intact.
/// What: Trims trailing whitespace first, then drops leading whitespace-only
/// lines one at a time. Interior blank lines are untouched.
/// Test: `tests::trim_surrounding_blank_lines_strips_leading_and_trailing`,
/// `tests::trim_surrounding_blank_lines_preserves_when_no_blanks`,
/// `tests::trim_surrounding_blank_lines_empty_input`.
pub fn trim_surrounding_blank_lines(s: &str) -> String {
    let trimmed_end = s.trim_end();
    let mut start = 0usize;
    let bytes = trimmed_end.as_bytes();
    while start < bytes.len() {
        let nl = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        let line_end = nl.unwrap_or(bytes.len());
        let line = &trimmed_end[start..line_end];
        if line.trim().is_empty() {
            start = match nl {
                Some(p) => p + 1,
                None => bytes.len(),
            };
        } else {
            break;
        }
    }
    trimmed_end[start..].to_string()
}

/// Drop every whitespace-only line from within a response.
///
/// Why: Markdown-style double-newline paragraph breaks accumulate into
/// wasted vertical space in a terminal chat panel — the user already reads
/// consecutive paragraphs as one flowing thought, so the gap just pushes
/// later content off-screen sooner. Removing every interior blank produces a
/// tight, compact response block.
/// What: Returns a string where every whitespace-only line is dropped;
/// non-blank lines are preserved verbatim and rejoined with single `\n`s.
/// Test: `tests::strip_interior_blank_lines_drops_all_blanks`,
/// `tests::strip_interior_blank_lines_drops_single_blank`,
/// `tests::strip_interior_blank_lines_treats_whitespace_only_as_blank`.
pub fn strip_interior_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The motivating case (#8164): a worktree path on an 80-column banner
    /// gets 54 columns. The deepest components must arrive WHOLE — a clipped
    /// tail would name neither the worktree nor the directory holding it.
    #[test]
    fn elide_middle_keeps_whole_trailing_path_components() {
        let path = "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.claude/worktrees/agent-af14d84dd34577cee";
        let out = elide_middle(path, 54);
        assert!(out.chars().count() <= 54, "{out}");
        assert!(out.starts_with("…/"), "{out}");
        assert!(
            out.ends_with("/worktrees/agent-af14d84dd34577cee"),
            "the last two components must survive whole: {out}"
        );
        // Nothing is clipped mid-component: every retained segment is either
        // one of the original path's own components or the `…` standing in
        // for the run #8205 replaced with the repository name.
        for segment in out.trim_start_matches("…/").split('/') {
            assert!(
                segment == "…" || path.split('/').any(|c| c == segment),
                "{segment:?} in {out}"
            );
        }
        // #8205: the repository the worktree belongs to is named too.
        assert!(out.contains("trusty-tools"), "{out}");
    }

    /// #8205, the owner-reported defect: at the banner's 54-column budget the
    /// repository name vanished while columns went unused, and only the
    /// connect line's roomier 60 kept it. Both budgets must name the repo.
    #[test]
    fn elide_middle_keeps_the_repo_name_at_both_render_budgets() {
        let path =
            "/private/tmp/q8230/deep/trusty-tools-demo/.claude/worktrees/agent-0123456789abcdef";
        for width in [54usize, 60] {
            let out = elide_middle(path, width);
            assert!(
                out.chars().count() <= width,
                "must fit {width}: {out} ({})",
                out.chars().count()
            );
            assert!(
                out.contains("trusty-tools-demo"),
                "the repository name must survive at {width}: {out}"
            );
            assert!(
                out.ends_with("agent-0123456789abcdef"),
                "the worktree's own name must survive at {width}: {out}"
            );
        }
    }

    /// Order of surrender (owner ruling 2026-09-17): squeezed hard enough,
    /// the tail goes and the repository name stays — never the reverse.
    #[test]
    fn elide_middle_drops_the_tail_before_the_repo_name() {
        let path =
            "/private/tmp/q8230/deep/trusty-tools-demo/.claude/worktrees/agent-0123456789abcdef";
        let out = elide_middle(path, 26);
        assert!(out.chars().count() <= 26, "{out}");
        assert!(
            out.contains("trusty-tools-demo"),
            "the repo name outlives the tail: {out}"
        );
        assert_eq!(out, "…/trusty-tools-demo/…", "{out}");
    }

    /// A path already inside its budget is handed back byte-for-byte — no
    /// ellipsis, no reshaping.
    #[test]
    fn elide_middle_keeps_a_fitting_path_unchanged() {
        let path = "/private/tmp/q8230/repoA";
        assert_eq!(elide_middle(path, 60), path);
        assert_eq!(elide_middle(path, path.chars().count()), path);
    }

    /// A plain repository root — the common case — carries its own name as
    /// the leaf, so keeping the tail already keeps the repo name and no
    /// anchoring is needed. True for a deep path with no repository marker
    /// too: its final component is what survives.
    #[test]
    fn elide_middle_keeps_a_repo_name_that_is_the_leaf() {
        let path = "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools";
        let out = elide_middle(path, 30);
        assert!(out.chars().count() <= 30, "{out}");
        assert!(out.ends_with("/trusty-tools"), "{out}");
        assert!(!out.contains("/…/"), "no anchor splice is needed: {out}");

        let deep = "/private/var/folders/zz/T/very/deeply/nested/scratch/workspace/repoZ";
        let out = elide_middle(deep, 40);
        assert!(out.chars().count() <= 40, "{out}");
        assert!(
            out.ends_with("/repoZ"),
            "the final component survives: {out}"
        );
    }

    /// A string with no `/`, and a path whose last two components alone
    /// overflow, both fall back to character elision — head and tail kept.
    #[test]
    fn elide_middle_falls_back_to_character_elision() {
        let out = elide_middle("abcdefghijklmnopqrstuvwxyz", 11);
        assert_eq!(out.chars().count(), 11, "{out}");
        assert_eq!(out, "abcde…vwxyz", "head and tail both kept");
        assert!(out.contains('…'), "{out}");

        let deep = "/a/averyveryverylongdirectoryname/anotherverylongleafname";
        let out = elide_middle(deep, 20);
        assert_eq!(out.chars().count(), 20, "{out}");
        assert!(out.starts_with("/a/"), "{out}");
        assert!(out.ends_with("leafname"), "{out}");
    }

    /// Degenerate widths must not panic and must not exceed the budget.
    #[test]
    fn elide_middle_handles_degenerate_widths() {
        assert_eq!(elide_middle("/a/b/c", 0), "…");
        assert_eq!(elide_middle("/a/b/c", 1), "…");
        assert_eq!(elide_middle("short", 99), "short");
    }

    #[test]
    fn trim_surrounding_blank_lines_strips_leading_and_trailing() {
        let input = "\n\n  \nhello\n\nworld\n\n  \n";
        let out = trim_surrounding_blank_lines(input);
        assert_eq!(out, "hello\n\nworld");
    }

    #[test]
    fn trim_surrounding_blank_lines_preserves_when_no_blanks() {
        assert_eq!(trim_surrounding_blank_lines("hi"), "hi");
        assert_eq!(trim_surrounding_blank_lines("a\nb"), "a\nb");
    }

    #[test]
    fn trim_surrounding_blank_lines_empty_input() {
        assert_eq!(trim_surrounding_blank_lines(""), "");
        assert_eq!(trim_surrounding_blank_lines("\n\n  \n"), "");
    }

    #[test]
    fn strip_interior_blank_lines_drops_all_blanks() {
        let input = "para1\n\n\npara2\n\n\n\npara3";
        let out = strip_interior_blank_lines(input);
        assert_eq!(out, "para1\npara2\npara3");
    }

    #[test]
    fn strip_interior_blank_lines_drops_single_blank() {
        let input = "para1\n\npara2";
        assert_eq!(strip_interior_blank_lines(input), "para1\npara2");
    }

    #[test]
    fn strip_interior_blank_lines_treats_whitespace_only_as_blank() {
        let input = "a\n   \n\t\nb";
        assert_eq!(strip_interior_blank_lines(input), "a\nb");
    }
}
