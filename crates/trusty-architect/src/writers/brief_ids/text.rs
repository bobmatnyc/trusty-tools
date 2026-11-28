//! The brief-file text rules of `brief_ids.py` (#12028 P1e).
//!
//! What: `FRONT_RE` (`\A---\n.*?\n---\n`), `MARKER_RE`, the first H1 outside
//! a ``` or ~~~ fence (`^#\s+(.*?)\s*$`), the `MBxxxx: ` H1 prefix
//! (`H1_ID_RE`) and the title from a file name, matched by hand with
//! Python's `\s` and `\d` classes.
//! Known gap: `str.capitalize` title-cases a first letter such as `ǆ`;
//! Rust upper-cases it.
//! Test: `brief_text_rules_match_python` and the `brief-id` recordings.

use super::id_like;
use crate::pystr::{is_digit, is_space, lstrip, strip};

/// `_split_front(text)`: the YAML frontmatter block (or `""`) and the rest.
fn split_front(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(i) = rest.find("\n---\n")
    {
        let end = 4 + i + 5;
        return (&text[..end], &text[end..]);
    }
    ("", text)
}

/// `MARKER_RE.match(line)`: the id in `<!-- brief-id: MBxxxx -->`.
fn marker(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("<!--")?.trim_start_matches(is_space);
    let rest = rest.strip_prefix("brief-id:")?.trim_start_matches(is_space);
    let len = if rest.starts_with('M') { 6 } else { 5 };
    let id = rest.get(..len)?;
    if !id_like(id, false) || id.ends_with('\n') {
        return None;
    }
    let tail = rest[len..]
        .trim_start_matches(is_space)
        .strip_prefix("-->")?;
    tail.chars().all(is_space).then_some(id)
}

/// `marker_id(text)`: the id in the first line after any frontmatter.
pub fn marker_id(text: &str) -> Option<String> {
    let (_, body) = split_front(text);
    marker(body.split('\n').next().unwrap_or("")).map(str::to_owned)
}

/// `H1_RE.match(line).group(1)`: `#`, whitespace, the text without its
/// trailing whitespace.
fn h1(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('#')?;
    if !rest.starts_with(is_space) {
        return None;
    }
    Some(strip(rest))
}

/// `_h1_index(lines)`: the first H1 outside a fenced block.
fn h1_index(lines: &[&str]) -> Option<usize> {
    let mut fence = false;
    for (i, ln) in lines.iter().enumerate() {
        let l = lstrip(ln);
        if l.starts_with("```") || l.starts_with("~~~") {
            fence = !fence;
        } else if !fence && h1(ln).is_some() {
            return Some(i);
        }
    }
    None
}

/// `H1_ID_RE.sub("", h, count=1)`: a leading `MBxxxx:` and its whitespace removed.
fn drop_id(h: &str) -> &str {
    let len = if h.starts_with('M') { 6 } else { 5 };
    let Some(id) = h.get(..len) else { return h };
    if !id_like(id, false) || id.ends_with('\n') {
        return h;
    }
    match h[len..].strip_prefix(':') {
        Some(rest) if rest.starts_with(is_space) => rest.trim_start_matches(is_space),
        _ => h,
    }
}

/// `slug_title(path)`: the file name without a date prefix, as a title.
fn slug_title(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(i) if base[..i].chars().any(|c| c != '.') => &base[..i],
        _ => base,
    };
    let c: Vec<char> = stem.chars().collect();
    let dated = c.len() >= 11
        && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|&i| is_digit(c[i]))
        && [4, 7, 10].iter().all(|&i| c[i] == '-');
    let name: String = if dated {
        c[11..].iter().collect()
    } else {
        stem.to_owned()
    };
    let name = name.replace(['-', '_'], " ");
    let name = strip(&name);
    let mut chars = name.chars();
    match chars.next() {
        Some(f) => f
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => "Brief".into(),
    }
}

/// `title_of(text, path)`: the first H1 without a leading id, or a title
/// from the file name.
pub fn title_of(text: &str, path: &str) -> String {
    let (_, body) = split_front(text);
    let lines: Vec<&str> = body.split('\n').collect();
    match h1_index(&lines).and_then(|i| h1(lines[i])) {
        Some(h) => drop_id(h).to_owned(),
        None => slug_title(path),
    }
}

/// `stamp(text, bid, path)`: the marker line and the `bid: ` H1 prefix.
pub fn stamp(text: &str, bid: &str, path: &str) -> String {
    let (front, body) = split_front(text);
    let mut lines: Vec<String> = body.split('\n').map(str::to_owned).collect();
    if marker(&lines[0]).is_some() {
        lines.remove(0);
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    match h1_index(&refs) {
        None => {
            let mut head = vec![format!("# {bid}: {}", slug_title(path)), String::new()];
            head.append(&mut lines);
            lines = head;
        }
        Some(i) => {
            let h = h1(&lines[i]).map(drop_id).unwrap_or_default().to_owned();
            lines[i] = format!("# {bid}: {h}");
        }
    }
    format!("{front}<!-- brief-id: {bid} -->\n{}", lines.join("\n"))
}

/// The first H1 (after any frontmatter) starts with `prefix`.
pub(super) fn h1_has_prefix(text: &str, prefix: &str) -> bool {
    let (_, body) = split_front(text);
    let lines: Vec<&str> = body.split('\n').collect();
    h1_index(&lines).is_some_and(|i| lines[i].starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_text_rules_match_python() {
        // Expected values printed by the frozen brief_ids.py under Python 3.14.8.
        assert_eq!(split_front("---\n\n---\nx"), ("---\n\n---\n", "x"));
        assert_eq!(split_front("---\n---\nx"), ("", "---\n---\nx"));
        assert_eq!(
            marker_id("<!--brief-id:MB00a1  -->  ").as_deref(),
            Some("MB00a1")
        );
        assert_eq!(marker_id("<!-- brief-id: MB00A1 -->"), None);
        assert_eq!(
            title_of("```\n# not\n```\n#\tMB0001:  Real  ", "x.md"),
            "Real"
        );
        assert_eq!(
            title_of("no h1", "a/2026-11-27-my_brief-note.md"),
            "My brief note"
        );
        assert_eq!(title_of("", ".md"), ".md");
        assert_eq!(
            stamp("text", "MB0002", "b.md"),
            "<!-- brief-id: MB0002 -->\n# MB0002: B\n\ntext"
        );
    }
}
