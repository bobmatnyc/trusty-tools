//! `os.path.abspath(os.path.expanduser(p))` as `posixpath` computes it.
//!
//! Why: `projects_root` and `claude_config_dir` are returned absolute and
//! must name the same directory the script names (#12028).
//! What: [`expanduser`] replaces a leading `~` with `HOME` (trailing `/`
//! dropped); [`abspath`] joins a relative path to the current directory and
//! applies `normpath`'s lexical rules (`.`, `..`, repeated `/`, a kept `//`).
//! Known gap: `~user` and an unset `HOME` need a password-database lookup;
//! the path is returned unexpanded there.
//! Test: `abspath_and_expanduser_follow_posixpath`.

/// `os.path.expanduser(path)` with `home` as `os.environ.get("HOME")`.
pub(super) fn expanduser(path: &str, home: Option<&str>) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_owned();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return path.to_owned();
    }
    let Some(home) = home else {
        return path.to_owned();
    };
    let joined = format!("{}{rest}", home.trim_end_matches('/'));
    if joined.is_empty() {
        "/".into()
    } else {
        joined
    }
}

/// `posixpath.normpath(path)`.
pub(super) fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let slashes = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else {
        usize::from(path.starts_with('/'))
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (slashes == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let out = format!("{}{}", "/".repeat(slashes), comps.join("/"));
    if out.is_empty() { ".".into() } else { out }
}

/// `os.path.abspath(path)` against the working directory `cwd`.
pub(super) fn abspath(path: &str, cwd: &str) -> String {
    if path.starts_with('/') {
        normpath(path)
    } else {
        normpath(&format!("{cwd}/{path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values from CPython 3.14.8 `posixpath`.
    #[test]
    fn abspath_and_expanduser_follow_posixpath() {
        assert_eq!(expanduser("~/a", Some("/h/")), "/h/a");
        assert_eq!(expanduser("~", Some("/")), "/");
        assert_eq!(expanduser("~x/a", Some("/h")), "~x/a");
        assert_eq!(expanduser("a/~", Some("/h")), "a/~");
        assert_eq!(abspath("x/../y/./z/", "/w"), "/w/y/z");
        assert_eq!(abspath("../../..", "/w"), "/");
        assert_eq!(normpath("//a//b"), "//a/b");
        assert_eq!(normpath("///a"), "/a");
        assert_eq!(normpath("../a"), "../a");
    }
}
