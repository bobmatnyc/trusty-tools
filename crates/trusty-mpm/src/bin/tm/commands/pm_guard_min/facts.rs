//! Shell text to structured facts, through tree-sitter-bash (#11431).
//!
//! Why: the old guard judged command text with a hand-written lexer, so a
//! heredoc body, a `python -c` string or prose in `--body` read as commands
//! (#10598, #10567). A real grammar knows which bytes are commands and which
//! are data, so the rules can decide on argv alone.
//! What: [`parse_bash`] walks the syntax tree and returns one [`Cmd`] per
//! simple command (nested command substitutions and `bash -c` literals
//! included) plus every output-redirect target. A word the shell computes at
//! run time is [`Word::Dynamic`]; a command holding a substitution, an `eval`
//! or a parse error is `unparsed`.
//! Test: `facts_tests.rs`.

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

use super::paths::resolve;

/// Deepest syntax-tree recursion; deeper input is unparsed, never a stack overflow.
const MAX_DEPTH: usize = 256;
/// How many nested `bash -c '<literal>'` layers are parsed.
const MAX_SHELL_NESTING: usize = 3;

/// One shell word after quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Word {
    /// The word's value is fixed by the text (`~` and `$HOME` expanded).
    Lit(String),
    /// The shell computes the value at run time (`$F`, `$(..)`, `$((..))`).
    Dynamic,
}

impl Word {
    /// The literal value, `None` when dynamic.
    pub(crate) fn lit(&self) -> Option<&str> {
        match self {
            Word::Lit(s) => Some(s),
            Word::Dynamic => None,
        }
    }
}

/// One simple command.
#[derive(Debug, Clone)]
pub(crate) struct Cmd {
    /// argv with wrappers (`sudo`, `env`, `nohup`, ...) removed; `argv[0]` is a basename.
    pub argv: Vec<Word>,
    /// The directory the command runs in (`cd` earlier in the input followed).
    pub cwd: PathBuf,
    /// A substitution, `eval`, a dynamic `argv[0]` or a parse error touched it.
    pub unparsed: bool,
}

impl Cmd {
    /// `argv[0]`, or `""` when dynamic.
    pub(crate) fn name(&self) -> &str {
        self.argv.first().and_then(Word::lit).unwrap_or("")
    }

    /// The literal words after `argv[0]`; dynamic words are skipped.
    pub(crate) fn lit_args(&self) -> Vec<&str> {
        self.argv.iter().skip(1).filter_map(Word::lit).collect()
    }
}

/// One output-redirect target and the directory it resolves against.
#[derive(Debug, Clone)]
pub(crate) struct Redirect {
    pub target: Word,
    pub cwd: PathBuf,
}

/// Everything [`parse_bash`] learned from one command string.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub cmds: Vec<Cmd>,
    pub redirects: Vec<Redirect>,
    /// Commands marked unparsed, plus one when the grammar reported an error.
    pub unparsed: usize,
}

struct Ctx<'a> {
    src: &'a [u8],
    home: &'a Path,
    cwd: PathBuf,
    has_error: bool,
    nesting: usize,
    out: Facts,
}

/// Parse `src` as bash run from `cwd`.
///
/// Why: see the module doc.
/// What: a grammar that fails to load or a tree with an ERROR node still
/// yields every command it found, each marked unparsed.
/// Test: `heredoc_body_is_data_not_commands`, `substitution_marks_unparsed_and_still_yields_inner`.
pub(crate) fn parse_bash(src: &str, cwd: &Path, home: &Path) -> Facts {
    parse_nested(src, cwd, home, 0)
}

fn parse_nested(src: &str, cwd: &Path, home: &Path, nesting: usize) -> Facts {
    let mut parser = Parser::new();
    let tree = parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()
        .and_then(|()| parser.parse(src, None));
    let Some(tree) = tree else {
        return Facts {
            unparsed: 1,
            ..Facts::default()
        };
    };
    let root = tree.root_node();
    let mut ctx = Ctx {
        src: src.as_bytes(),
        home,
        cwd: cwd.to_path_buf(),
        has_error: root.has_error(),
        nesting,
        out: Facts::default(),
    };
    visit(root, &mut ctx, 0);
    if ctx.has_error {
        for cmd in &mut ctx.out.cmds {
            cmd.unparsed = true;
        }
    }
    let mut out = ctx.out;
    out.unparsed = out.cmds.iter().filter(|c| c.unparsed).count() + usize::from(ctx.has_error);
    out
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn visit(node: Node, ctx: &mut Ctx, depth: usize) {
    if depth > MAX_DEPTH {
        ctx.has_error = true;
        return;
    }
    match node.kind() {
        "command" => visit_command(node, ctx, depth),
        "file_redirect" => visit_redirect(node, ctx, depth),
        // Heredoc bodies are data the command reads on stdin, never commands.
        "heredoc_body" | "heredoc_start" | "heredoc_end" => {}
        _ => {
            for child in named_children(node) {
                visit(child, ctx, depth + 1);
            }
        }
    }
}

fn visit_redirect(node: Node, ctx: &mut Ctx, depth: usize) {
    let mut cursor = node.walk();
    let op = node
        .children(&mut cursor)
        .find(|c| !c.is_named())
        .map(|c| text(c, ctx.src).to_owned())
        .unwrap_or_default();
    let Some(dest) = node.child_by_field_name("destination") else {
        return;
    };
    let writes = matches!(op.as_str(), ">" | ">>" | "&>" | "&>>" | ">|")
        || (op == ">&"
            && !text(dest, ctx.src)
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-'));
    if writes {
        let (target, _) = eval_word(dest, ctx);
        ctx.out.redirects.push(Redirect {
            target,
            cwd: ctx.cwd.clone(),
        });
    }
    // A substitution in the target still runs.
    visit_words(dest, ctx, depth);
}

/// Visit the commands nested in a word (`$(..)`, `<(..)`).
fn visit_words(node: Node, ctx: &mut Ctx, depth: usize) {
    if depth > MAX_DEPTH {
        ctx.has_error = true;
        return;
    }
    if matches!(node.kind(), "command_substitution" | "process_substitution") {
        for child in named_children(node) {
            visit(child, ctx, depth + 1);
        }
        return;
    }
    for child in named_children(node) {
        visit_words(child, ctx, depth + 1);
    }
}

fn visit_command(node: Node, ctx: &mut Ctx, depth: usize) {
    let mut argv = Vec::new();
    let mut unparsed = false;
    for child in named_children(node) {
        match child.kind() {
            "variable_assignment" => visit_words(child, ctx, depth),
            "file_redirect" => visit_redirect(child, ctx, depth),
            "herestring_redirect" => visit_words(child, ctx, depth),
            _ => {
                let (word, subst) = eval_word(child, ctx);
                unparsed |= subst;
                argv.push(word);
                visit_words(child, ctx, depth);
            }
        }
    }
    let argv = strip_wrappers(argv);
    if let Some(Word::Lit(first)) = argv.first() {
        let base = first.rsplit('/').next().unwrap_or(first).to_owned();
        unparsed |= base == "eval";
        let mut argv = argv;
        argv[0] = Word::Lit(base);
        follow_cd(&argv, ctx);
        parse_shell_literal(&argv, ctx);
        let cwd = ctx.cwd.clone();
        ctx.out.cmds.push(Cmd {
            argv,
            cwd,
            unparsed,
        });
    } else if !argv.is_empty() {
        let cwd = ctx.cwd.clone();
        ctx.out.cmds.push(Cmd {
            argv,
            cwd,
            unparsed: true,
        });
    }
}

/// `cd <literal>` moves the cwd of every later command in the input.
fn follow_cd(argv: &[Word], ctx: &mut Ctx) {
    if argv.len() == 2
        && argv[0].lit() == Some("cd")
        && let Some(dir) = argv[1].lit()
    {
        ctx.cwd = resolve(&ctx.cwd, dir);
    }
}

/// `bash -c '<literal>'` runs its argument as a script: parse it too.
fn parse_shell_literal(argv: &[Word], ctx: &mut Ctx) {
    let shell = matches!(argv[0].lit(), Some("bash" | "sh" | "zsh" | "dash"));
    if !shell || ctx.nesting >= MAX_SHELL_NESTING {
        return;
    }
    let Some(i) = argv.iter().position(|w| w.lit() == Some("-c")) else {
        return;
    };
    if let Some(script) = argv.get(i + 1).and_then(Word::lit) {
        let inner = parse_nested(script, &ctx.cwd, ctx.home, ctx.nesting + 1);
        ctx.has_error |= inner.unparsed > inner.cmds.iter().filter(|c| c.unparsed).count();
        ctx.out.cmds.extend(inner.cmds);
        ctx.out.redirects.extend(inner.redirects);
    }
}

/// Drop leading wrappers that run their argument list as the real command.
fn strip_wrappers(argv: Vec<Word>) -> Vec<Word> {
    let mut i = 0;
    while let Some(Word::Lit(w)) = argv.get(i) {
        let base = w.rsplit('/').next().unwrap_or(w);
        match base {
            "sudo" | "command" | "exec" | "nohup" | "time" | "builtin" => {
                i += 1;
                while argv
                    .get(i)
                    .and_then(Word::lit)
                    .is_some_and(|a| a.starts_with('-'))
                {
                    i += 1;
                }
            }
            "env" => {
                i += 1;
                while argv
                    .get(i)
                    .and_then(Word::lit)
                    .is_some_and(|a| a.starts_with('-') || a.contains('='))
                {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    argv.into_iter().skip(i).collect()
}

/// Evaluate one word node; the flag says a substitution was inside it.
fn eval_word(node: Node, ctx: &Ctx) -> (Word, bool) {
    match node.kind() {
        "command_name" => named_children(node)
            .first()
            .map_or((Word::Dynamic, false), |c| eval_word(*c, ctx)),
        "word" | "number" => (
            Word::Lit(expand_tilde(&unescape(text(node, ctx.src)), ctx.home)),
            false,
        ),
        "raw_string" => (
            Word::Lit(text(node, ctx.src).trim_matches('\'').to_owned()),
            false,
        ),
        "string" => eval_parts(node, ctx, true),
        "concatenation" => eval_parts(node, ctx, false),
        "simple_expansion" | "expansion" => (home_var(text(node, ctx.src), ctx.home), false),
        "command_substitution" | "process_substitution" => (Word::Dynamic, true),
        _ => (Word::Dynamic, false),
    }
}

/// A `"..."` string or a concatenation: literal only when every part is.
fn eval_parts(node: Node, ctx: &Ctx, quoted: bool) -> (Word, bool) {
    let mut out = String::new();
    let mut dynamic = false;
    let mut subst = false;
    let mut pos = node.start_byte() + usize::from(quoted);
    let end = node.end_byte() - usize::from(quoted);
    for child in named_children(node) {
        // Unnamed bytes between parts (`"a $X b"` spacing) are literal text.
        if quoted && child.start_byte() > pos {
            out.push_str(std::str::from_utf8(&ctx.src[pos..child.start_byte()]).unwrap_or(""));
        }
        pos = child.end_byte();
        let (word, s) = if quoted && child.kind() == "string_content" {
            (Word::Lit(text(child, ctx.src).to_owned()), false)
        } else {
            eval_word(child, ctx)
        };
        subst |= s;
        match word {
            Word::Lit(s) => out.push_str(&s),
            Word::Dynamic => dynamic = true,
        }
    }
    if quoted && end > pos {
        out.push_str(std::str::from_utf8(&ctx.src[pos..end]).unwrap_or(""));
    }
    if dynamic {
        (Word::Dynamic, subst)
    } else {
        (Word::Lit(out), subst)
    }
}

fn home_var(raw: &str, home: &Path) -> Word {
    if raw == "$HOME" || raw == "${HOME}" {
        Word::Lit(home.display().to_string())
    } else {
        Word::Dynamic
    }
}

fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn expand_tilde(word: &str, home: &Path) -> String {
    if word == "~" {
        home.display().to_string()
    } else if let Some(rest) = word.strip_prefix("~/") {
        home.join(rest).display().to_string()
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
