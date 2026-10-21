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
//! ([`anchor_writes`]), every `cd`/`pushd`/`popd` operand when the command
//! changes directory, every path-shaped word and every word holding `$` or a
//! backtick, and any literal mention of a project dir in the command text (an
//! interpreter one-liner or a here-document body). A path is judged by its
//! lexical form with `..` collapsed and by its resolved form, each compared
//! component-wise without case ([`starts_with_ci`]); it trips when it is
//! inside a project or above one (a search from `~` reaches every project). A
//! missing cwd under a relative path, an unplaceable Bash write, a `cd` with no
//! operand, `cd -`, a `popd` or `pushd` stack move, an expansion the guard
//! does not evaluate and a path that will not resolve each trip as
//! unplaceable. #8454 fix round: a touch carries a listed project dir or a
//! fixed description, never a word of the call, which may hold a credential.
//! Test: `egress_guard_tripwire_tests.rs` (`tripwire_places_each_tool_path`);
//! end to end in `tests/tm_hook_tripwire_8454.rs`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::Touch;
use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::{
    AnchorWrite, anchor_writes, keyword_words, shell_segments_map, split_heredoc_bodies, tokenize,
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

/// A path that exists but will not resolve.
const UNRESOLVABLE: &str = "a path that will not resolve";

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

    /// The project `path` is inside or above, if any.
    fn overlapping(&self, path: &Path) -> Option<&PathBuf> {
        self.dirs
            .iter()
            .find(|dir| starts_with_ci(path, dir) || starts_with_ci(dir, path))
    }

    /// The touch of the placed path `at`: the project it overlaps, never the
    /// path as the call spelled it.
    fn at(&self, at: &Path) -> Touch {
        if let Some(dir) = self.overlapping(&normalize(at)) {
            return Touch::Inside(dir.clone());
        }
        match resolve(at) {
            Resolved::Path(real) => self
                .overlapping(&real)
                .map_or(Touch::Clear, |dir| Touch::Inside(dir.clone())),
            Resolved::Unresolvable => Touch::Unplaceable(UNRESOLVABLE),
        }
    }

    /// The first project dir `text` names literally, in any case.
    fn mentioned_in(&self, text: &str) -> Option<PathBuf> {
        let text = fold_case(text);
        self.dirs
            .iter()
            .find(|dir| text.contains(&fold_case(&dir.to_string_lossy())))
            .cloned()
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
    /// A string field of `tool_input`, or the unplaceable touch.
    fn field(&self, key: &str) -> Result<&str, Touch> {
        match self.input.get(key).and_then(Value::as_str) {
            Some(text) if !text.is_empty() => Ok(text),
            _ => Err(Touch::Unplaceable("a missing or non-string path field")),
        }
    }

    /// An edit or read tool's one path field, placed against the cwd.
    fn file(&self, key: &str) -> Touch {
        let word = match self.field(key) {
            Ok(word) => word,
            Err(touch) => return touch,
        };
        match place(word, self.cwd, self.home, false) {
            Placed::At(at) => self.projects.at(&at),
            Placed::Unknown => Touch::Unplaceable("a relative or `~user` path with no cwd"),
        }
    }

    /// Glob or Grep: the search root, then `pattern` (Grep's only when it is
    /// an absolute or `~` path, since it is a regex).
    fn search(&self, grep: bool) -> Touch {
        let root = match self.input.get("path") {
            None | Some(Value::Null) => match self.cwd {
                Some(cwd) => cwd.to_path_buf(),
                None => return Touch::Unplaceable("a search with no `path` and no cwd"),
            },
            Some(_) => match self
                .field("path")
                .map(|w| place(w, self.cwd, self.home, false))
            {
                Ok(Placed::At(at)) => at,
                Ok(Placed::Unknown) => {
                    return Touch::Unplaceable("a search `path` the guard cannot place");
                }
                Err(touch) => return touch,
            },
        };
        let touch = self.projects.at(&root);
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
            Placed::At(at) => self.projects.at(&at),
            Placed::Unknown => Touch::Unplaceable("a search `pattern` the guard cannot place"),
        }
    }

    /// Bash: the cwd, its writes, its directory changes, its words, and
    /// project mentions.
    fn bash(&self) -> Touch {
        let command = match self.field("command") {
            Ok(command) => command,
            Err(touch) => return touch,
        };
        let Some(cwd) = self.cwd else {
            return Touch::Unplaceable("a Bash call with no absolute cwd");
        };
        let here = self.projects.at(cwd);
        if here != Touch::Clear {
            return here;
        }
        if let Some(dir) = self.projects.mentioned_in(command) {
            return Touch::Inside(dir);
        }
        let writes = match anchor_writes(command) {
            Ok(writes) => writes,
            Err(what) => return Touch::Unplaceable(what),
        };
        // #8454 round 2: `moved` reads the same op list `dir_changes` judges,
        // so a `cd` one sees the other cannot miss.
        let ops = match shell_segments_map(command, &segment_dir_changes) {
            Ok(ops) => ops,
            Err(what) => return Touch::Unplaceable(what),
        };
        // After a `cd`, a relative word no longer names the hook's cwd.
        let moved = !ops.is_empty();
        let base = (!moved).then_some(cwd);
        let touch = first(writes.iter().flat_map(write_words).map(|word| {
            match place(&word, base, self.home, true) {
                Placed::At(at) => self.projects.at(&at),
                Placed::Unknown => Touch::Unplaceable("a Bash write the guard cannot place"),
            }
        }));
        if touch != Touch::Clear {
            return touch;
        }
        // #8454 fix round: every directory the command moves to is judged, so
        // a bare name read after the `cd` lands under a judged directory.
        if moved {
            let touch = self.dir_changes(ops, cwd);
            if touch != Touch::Clear {
                return touch;
            }
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
            Err(what) => Touch::Unplaceable(what),
        }
    }

    /// Every `cd`/`pushd`/`popd` op of a command, judged in order from `cwd`.
    ///
    /// What: an operand is placed against the directory the previous one
    /// reached. A missing operand (`cd` goes to `$HOME`, a bare `pushd` swaps
    /// the stack), `cd -`, a `popd` and a `pushd +N`/`-N` move to a directory
    /// the command does not name, and an operand the guard cannot place, are
    /// each [`Touch::Unplaceable`].
    fn dir_changes(&self, ops: Vec<DirOp>, cwd: &Path) -> Touch {
        let mut here = cwd.to_path_buf();
        for op in ops {
            let word = match op {
                DirOp::To(word) => word,
                DirOp::Missing => return Touch::Unplaceable("a `cd` or `pushd` with no operand"),
                DirOp::Back => return Touch::Unplaceable("a `cd -`"),
                DirOp::Stack => return Touch::Unplaceable("a `popd` or `pushd` stack move"),
            };
            let Placed::At(at) = place(&word, Some(&here), self.home, true) else {
                return Touch::Unplaceable("a `cd` operand the guard cannot place");
            };
            let touch = self.projects.at(&at);
            if touch != Touch::Clear {
                return touch;
            }
            here = normalize(&at);
        }
        Touch::Clear
    }

    /// One shell word a command reads, judged by where it lands; a glob is
    /// judged by its literal parent, an expansion it cannot place trips.
    fn word(&self, word: &str, base: Option<&Path>) -> Touch {
        let unplaced = Touch::Unplaceable("a Bash word behind an expansion or after a `cd`");
        if let Placed::At(at) = place(word, base, self.home, true) {
            return self.projects.at(&at);
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
                None => return unplaced,
            },
        };
        match place(parent, base, self.home, true) {
            Placed::At(at) => self.projects.at(&at),
            Placed::Unknown => unplaced,
        }
    }
}

/// Where one `cd`, `pushd` or `popd` goes.
enum DirOp {
    /// To this operand.
    To(String),
    /// No operand.
    Missing,
    /// `cd -`: the previous directory.
    Back,
    /// A directory-stack move.
    Stack,
}

/// The directory change one segment makes, if it runs `cd`, `pushd` or
/// `popd`; found past leading shell keywords (`if`, `then`, `do`, `{`, `!`),
/// env assignments and wrappers, as the splitter does (#8454 round 2).
fn segment_dir_changes(segment: &str) -> Result<Vec<DirOp>, &'static str> {
    let words = tokenize(&split_heredoc_bodies(segment).0).map_err(|_| UNLEXABLE)?;
    let argv = &words[keyword_words(&words, 0).0..];
    let verb = |w: &str| {
        let w = w.strip_prefix('\\').unwrap_or(w);
        matches!(w.rsplit('/').next(), Some("cd" | "pushd" | "popd"))
    };
    let at = strip_wrapper_prefix(argv).or_else(|| argv.iter().position(|w| verb(w)));
    let Some(at) = at.filter(|at| argv.get(*at).is_some_and(|w| verb(w))) else {
        return Ok(Vec::new());
    };
    let name = argv[at].rsplit('/').next().unwrap_or_default();
    if name.ends_with("popd") {
        return Ok(vec![DirOp::Stack]);
    }
    let mut rest = argv[at + 1..].iter();
    let mut operand = None;
    while let Some(w) = rest.next() {
        // #8454 round 2: a bare redirect operator's target is not an operand.
        if is_bare_redirect(w) {
            rest.next();
        } else if !w.contains(['<', '>']) && (w == "-" || !w.starts_with('-') || is_stack_index(w))
        {
            operand = Some(w);
            break;
        }
    }
    Ok(vec![match operand {
        None if name.ends_with("pushd") => DirOp::Stack,
        None => DirOp::Missing,
        Some(w) if w == "-" => DirOp::Back,
        Some(w) if is_stack_index(w) => DirOp::Stack,
        Some(w) => DirOp::To(w.clone()),
    }])
}

/// Whether `word` is a redirect operator with its target in the next word:
/// `>`, `2>>`, `&>`, `<`, `>|` and the like.
fn is_bare_redirect(word: &str) -> bool {
    let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
    op.contains(['<', '>']) && op.chars().all(|c| matches!(c, '<' | '>' | '&' | '|'))
}

/// Whether a `pushd`/`cd` argument is a stack index, `+N` or `-N`.
fn is_stack_index(word: &str) -> bool {
    word.strip_prefix(['+', '-'])
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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

/// The spellings in one lexed word the tripwire judges: the word with a
/// redirect operator stripped, and an option or assignment value after `=`,
/// each when it is path-shaped or holds `$` or a backtick (#8454 fix round).
fn word_paths(word: &str) -> Vec<String> {
    let digits = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let word = if digits.starts_with(['<', '>']) || word.starts_with("&>") {
        digits.trim_start_matches(['<', '>', '&', '|'])
    } else {
        word
    };
    let judged =
        |w: &str| w.contains(['/', '$', '`']) || w.starts_with('~') || w == "." || w == "..";
    let mut out = Vec::new();
    if judged(word) {
        out.push(word.to_owned());
    }
    if let Some((_, value)) = word.split_once('=')
        && judged(value)
    {
        out.push(value.to_owned());
    }
    out
}
