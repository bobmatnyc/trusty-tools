//! Where a gated tool call lands, for the CTO tripwire (#8454 G3, S6 prep).
//!
//! Why: the tripwire trips on a path in a listed project, and a call can name
//! that path many ways — relative to its cwd, through `..` or a symlink, in
//! another case on a case-insensitive APFS volume, or inside a Bash command.
//! Each of those must land where the kernel would open it, and a path the
//! guard cannot place must trip rather than pass (plan §5).
//! What: [`judge`] reads the listed projects, then the call's paths by tool:
//! `file_path` (Read, Edit, MultiEdit, Write), `notebook_path` (NotebookEdit),
//! the search root and `pattern` (Glob, Grep; the root is `path`, else the
//! cwd), and for Bash the cwd, every write the trust-anchor splitter names
//! ([`anchor_writes`]), every path-shaped word of every segment, and any
//! literal mention of a project dir in the command text (an interpreter
//! one-liner or a here-document body). A path is judged by its lexical form
//! with `..` collapsed and by its resolved form, each compared component-wise
//! without case ([`starts_with_ci`]); it trips when it is inside a project or
//! above one (a search from `~` reaches every project). A missing cwd under a
//! relative path, an unplaceable Bash write, an expansion the guard does not
//! evaluate and a path that will not resolve each trip as unplaceable.
//! Test: `egress_guard_tripwire_tests.rs` (`tripwire_places_each_tool_path`);
//! end to end in `tests/tm_hook_tripwire_8454.rs`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::Touch;
use crate::commands::pm_guard_bash::{
    AnchorWrite, anchor_writes, shell_segments_map, split_heredoc_bodies, tokenize,
};
use crate::commands::pm_guard_trust_anchor_paths::{
    Placed, Resolved, dequote, fold_case, place, resolve, starts_with_ci,
};

/// The characters that start a glob in a tool's `pattern`.
const GLOB_CHARS: [char; 4] = ['*', '?', '[', '{'];

/// The characters that start a glob or an expansion in a shell word.
const SHELL_CHARS: [char; 6] = ['$', '`', '*', '?', '[', '{'];

/// A shell segment whose words do not lex.
const UNLEXABLE: &str = "a Bash segment whose words do not lex";

/// The listed projects, each by its lexical and resolved spelling.
struct Projects {
    dirs: Vec<PathBuf>,
}

impl Projects {
    fn new(listed: &[PathBuf]) -> Self {
        let mut dirs = Vec::new();
        for dir in listed {
            dirs.push(normalize(dir));
            if let Resolved::Path(real) = resolve(dir) {
                dirs.push(real);
            }
        }
        Self { dirs }
    }

    /// Whether `path` is inside a project or above one.
    fn overlaps(&self, path: &Path) -> bool {
        self.dirs
            .iter()
            .any(|dir| starts_with_ci(path, dir) || starts_with_ci(dir, path))
    }

    /// The touch of the placed path `at`, spelled `spelling`.
    fn at(&self, at: &Path, spelling: &str) -> Touch {
        if self.overlaps(&normalize(at)) {
            return Touch::Inside(spelling.to_owned());
        }
        match resolve(at) {
            Resolved::Path(real) if self.overlaps(&real) => Touch::Inside(spelling.to_owned()),
            Resolved::Path(_) => Touch::Clear,
            Resolved::Unresolvable => Touch::Unplaceable(spelling.to_owned()),
        }
    }

    /// The first project dir `text` names literally, in any case. The touch
    /// keeps the dir, never the command text, which may carry a secret.
    fn mentioned_in(&self, text: &str) -> Option<String> {
        let text = fold_case(text);
        self.dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .find(|dir| text.contains(&fold_case(dir)))
    }
}

/// `path` with `.` dropped and `..` collapsed, never above the root.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The first touch that is not [`Touch::Clear`], else `Clear`.
fn first(touches: impl IntoIterator<Item = Touch>) -> Touch {
    touches
        .into_iter()
        .find(|t| *t != Touch::Clear)
        .unwrap_or(Touch::Clear)
}

/// Judge where a tainted gated call lands. See the module doc.
///
/// What: no home, and a project list `projects` cannot read, are
/// [`Touch::ProjectsUnknown`]; an empty list is [`Touch::Clear`]; otherwise
/// the tool's paths, judged as the module doc says.
/// Test: `tripwire_places_each_tool_path`.
pub(crate) fn judge(
    payload: &Value,
    home: Option<&Path>,
    projects: impl FnOnce(&Path) -> Result<Vec<PathBuf>, String>,
) -> Touch {
    let Some(home) = home else {
        return Touch::ProjectsUnknown("the home directory is unknown".into());
    };
    let listed = match projects(home) {
        Ok(listed) => listed,
        Err(why) => return Touch::ProjectsUnknown(why),
    };
    if listed.is_empty() {
        return Touch::Clear;
    }
    let projects = Projects::new(&listed);
    let tool = payload.get("tool_name").and_then(Value::as_str);
    let input = payload.get("tool_input").unwrap_or(&Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(Path::new)
        .filter(|cwd| cwd.is_absolute());
    let call = Call {
        projects: &projects,
        input,
        cwd,
        home,
    };
    match tool {
        Some("NotebookEdit") => call.file("notebook_path"),
        Some("Glob") => call.search(false),
        Some("Grep") => call.search(true),
        Some("Bash") => call.bash(),
        _ => call.file("file_path"),
    }
}

/// One call's inputs, for the per-tool judges.
struct Call<'a> {
    projects: &'a Projects,
    input: &'a Value,
    cwd: Option<&'a Path>,
    home: &'a Path,
}

impl Call<'_> {
    /// A string field of `tool_input`, or the unplaceable touch naming it.
    fn field(&self, key: &str) -> Result<&str, Touch> {
        match self.input.get(key).and_then(Value::as_str) {
            Some(text) if !text.is_empty() => Ok(text),
            _ => Err(Touch::Unplaceable(format!("no string `{key}`"))),
        }
    }

    /// A literal path (not a shell word), placed against the cwd and judged.
    fn literal(&self, word: &str) -> Touch {
        match place(word, self.cwd, self.home, false) {
            Placed::At(at) => self.projects.at(&at, word),
            Placed::Unknown => Touch::Unplaceable(word.to_owned()),
        }
    }

    /// An edit or read tool's one path field.
    fn file(&self, key: &str) -> Touch {
        match self.field(key) {
            Ok(word) => self.literal(word),
            Err(touch) => touch,
        }
    }

    /// Glob or Grep: the search root, then `pattern` (Grep's only when it is
    /// an absolute or `~` path, since it is a regex).
    fn search(&self, grep: bool) -> Touch {
        let root = match self.input.get("path") {
            None | Some(Value::Null) => match self.cwd {
                Some(cwd) => cwd.to_path_buf(),
                None => return Touch::Unplaceable("a search with no `path` and no cwd".into()),
            },
            Some(_) => match self
                .field("path")
                .map(|w| (w, place(w, self.cwd, self.home, false)))
            {
                Ok((_, Placed::At(at))) => at,
                Ok((word, Placed::Unknown)) => return Touch::Unplaceable(word.to_owned()),
                Err(touch) => return touch,
            },
        };
        let spelled = root.display().to_string();
        let touch = self.projects.at(&root, &spelled);
        if touch != Touch::Clear {
            return touch;
        }
        let pattern = match self.field("pattern") {
            Ok(pattern) => pattern,
            Err(touch) => return touch,
        };
        let pathlike = pattern.starts_with('/') || pattern.starts_with('~');
        if grep && !pathlike {
            return Touch::Clear;
        }
        let prefix = match pattern.find(GLOB_CHARS) {
            None => pattern,
            Some(at) => match pattern[..at].rfind('/') {
                Some(0) => "/",
                Some(slash) => &pattern[..slash],
                // The glob is in the first segment: it searches the root.
                None => return Touch::Clear,
            },
        };
        match place(prefix, Some(&root), self.home, false) {
            Placed::At(at) => self.projects.at(&at, pattern),
            Placed::Unknown => Touch::Unplaceable(pattern.to_owned()),
        }
    }

    /// Bash: the cwd, its writes, its path-shaped words, and project mentions.
    fn bash(&self) -> Touch {
        let command = match self.field("command") {
            Ok(command) => command,
            Err(touch) => return touch,
        };
        let Some(cwd) = self.cwd else {
            return Touch::Unplaceable("a Bash call with no absolute cwd".into());
        };
        let here = self.projects.at(cwd, &cwd.display().to_string());
        if here != Touch::Clear {
            return here;
        }
        if let Some(dir) = self.projects.mentioned_in(command) {
            return Touch::Inside(dir);
        }
        let writes = match anchor_writes(command) {
            Ok(writes) => writes,
            Err(what) => return Touch::Unplaceable(what.to_owned()),
        };
        // After a `cd`, a relative word no longer names the hook's cwd.
        let base = (!writes.contains(&AnchorWrite::DirChange)).then_some(cwd);
        let touch = first(writes.iter().flat_map(write_words).map(|word| {
            match place(&word, base, self.home, true) {
                Placed::At(at) => self.projects.at(&at, &word),
                Placed::Unknown => Touch::Unplaceable(word),
            }
        }));
        if touch != Touch::Clear {
            return touch;
        }
        let words = shell_segments_map(command, &|segment| {
            tokenize(&split_heredoc_bodies(segment).0).map_err(|_| UNLEXABLE)
        });
        match words {
            Ok(words) => first(
                words
                    .iter()
                    .flat_map(|w| word_paths(w))
                    .map(|w| self.word(&w, base)),
            ),
            Err(what) => Touch::Unplaceable(what.to_owned()),
        }
    }

    /// One path-shaped shell word a command reads, judged by where it lands;
    /// a glob is judged by its literal parent.
    fn word(&self, word: &str, base: Option<&Path>) -> Touch {
        if let Placed::At(at) = place(word, base, self.home, true) {
            return self.projects.at(&at, word);
        }
        let parent = match word.find(SHELL_CHARS) {
            None => word,
            Some(at) => match word[..at].rfind('/') {
                Some(0) => "/",
                Some(slash) => &word[..slash],
                // A glob in the first segment reads the cwd, already judged.
                None if !word[at..].starts_with(['$', '`']) && base.is_some() => {
                    return Touch::Clear;
                }
                None => return Touch::Unplaceable(word.to_owned()),
            },
        };
        match place(parent, base, self.home, true) {
            Placed::At(at) => self.projects.at(&at, word),
            Placed::Unknown => Touch::Unplaceable(word.to_owned()),
        }
    }
}

/// The shell words one splitter write names, dequoted.
fn write_words(write: &AnchorWrite) -> Vec<String> {
    match write {
        AnchorWrite::File(word)
        | AnchorWrite::Source(word)
        | AnchorWrite::Delete(word)
        | AnchorWrite::IntoUnnamed(word) => vec![dequote(word)],
        AnchorWrite::Literal(path) => vec![path.clone()],
        AnchorWrite::Into { dest, names, .. } => {
            let dir = dest.trim_end_matches('/');
            let mut out = vec![dequote(dest)];
            out.extend(names.iter().map(|n| dequote(&format!("{dir}/{n}"))));
            out
        }
        AnchorWrite::DirChange => Vec::new(),
    }
}

/// The path-shaped spellings in one lexed word: the word with a redirect
/// operator stripped, and an option or assignment value after `=`.
fn word_paths(word: &str) -> Vec<String> {
    let digits = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let word = if digits.starts_with(['<', '>']) || word.starts_with("&>") {
        digits.trim_start_matches(['<', '>', '&', '|'])
    } else {
        word
    };
    let pathlike = |w: &str| w.contains('/') || w.starts_with('~') || w == "." || w == "..";
    let mut out = Vec::new();
    if pathlike(word) {
        out.push(word.to_owned());
    }
    if let Some((_, value)) = word.split_once('=')
        && pathlike(value)
    {
        out.push(value.to_owned());
    }
    out
}
