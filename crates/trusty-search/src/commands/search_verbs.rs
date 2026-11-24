//! Native CLI verbs for the MCP search tools that had no CLI (#11625).
//!
//! Why: `search_similar`, `get_call_chain`, `grep` and `typeahead` were
//! reachable only through MCP although the daemon already serves each one on
//! its socket. A shell user or a script had no way to call them.
//! What: [`SearchVerb`] is flattened into `Commands`, so each verb is a
//! top-level subcommand. [`SearchVerb::request`] builds the socket method and
//! params the matching MCP tool sends; [`run`] sends them; [`handle`] resolves
//! the index, prints `--json` as the method result, or renders text.
//! Test: `search_verbs_tests.rs`.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::queries::{
    METHOD_GREP, METHOD_GREP_ALL, METHOD_SIMILAR, METHOD_TYPEAHEAD,
};
use trusty_search::service::rpc::reads::METHOD_CALL_CHAIN;

#[cfg(test)]
#[path = "search_verbs_tests.rs"]
mod tests;

/// The `get_call_chain` traversal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Direction {
    /// Callees and callers.
    Both,
    /// Callees only.
    Outgoing,
    /// Callers only.
    Callers,
}

/// The `typeahead` retrieval mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TypeaheadMode {
    /// BM25 only; per-keystroke fast.
    Lexical,
    /// Semantic and KG lanes too; for debounced input.
    Blended,
}

/// The `grep` switches, one per MCP `grep` argument.
///
/// Short flags follow ripgrep except `-i` and `-v`, which the global
/// `--index` and `--verbose` flags already own.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct GrepArgs {
    /// Regex (default) or literal with `--fixed-strings`
    pub pattern: String,
    /// Search every registered index (`search.grep.all`); ignores `--index`
    #[arg(long)]
    pub all: bool,
    /// Case-insensitive match
    #[arg(long)]
    pub ignore_case: bool,
    /// Treat PATTERN as a literal
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,
    /// Require word boundaries
    #[arg(short = 'w', long)]
    pub word_regexp: bool,
    /// Print lines that do NOT match
    #[arg(long)]
    pub invert_match: bool,
    /// Print one path per matching file
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,
    /// Let `.` span newlines
    #[arg(long)]
    pub multiline: bool,
    /// Lines of context before and after each match
    #[arg(short = 'C', long)]
    pub context: Option<u64>,
    /// Lines of context before each match
    #[arg(short = 'B', long)]
    pub context_before: Option<u64>,
    /// Lines of context after each match
    #[arg(short = 'A', long)]
    pub context_after: Option<u64>,
    /// Only files whose index-relative path matches this glob
    #[arg(long)]
    pub glob: Option<String>,
    /// Cap on returned matches
    #[arg(long, visible_alias = "max-count")]
    pub max_results: Option<u64>,
}

/// The four verbs, flattened into the top-level `Commands` enum.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SearchVerb {
    /// Chunks semantically similar to a file or function (MCP `search_similar`)
    ///
    /// Examples:
    ///   trusty-search similar src/auth.rs --function validate_token -k 5
    #[command(display_order = 13)]
    Similar {
        /// File whose chunk seeds the search, index-relative
        file: String,
        /// Function name inside FILE to seed from
        #[arg(long)]
        function: Option<String>,
        /// Number of results (daemon default 10)
        #[arg(short = 'k', long)]
        top_k: Option<u64>,
    },

    /// Annotated call tree for a function (MCP `get_call_chain`)
    ///
    /// Examples:
    ///   trusty-search call-chain handle_query --direction callers
    #[command(display_order = 14)]
    CallChain {
        /// Symbol name, fuzzy substring, or `file:line`
        entry_point: String,
        /// Which edges to walk
        #[arg(long, value_enum)]
        direction: Option<Direction>,
        /// Traversal depth, 1-4
        #[arg(long)]
        max_depth: Option<u64>,
        /// Omit the embedded source at depth <= 1
        #[arg(long)]
        no_source: bool,
    },

    /// Regex or literal search over indexed files (MCP `grep`)
    ///
    /// Examples:
    ///   trusty-search grep 'fn handle_\w+' --glob '*.rs'
    ///   trusty-search grep TODO --all -l
    #[command(display_order = 15)]
    Grep(GrepArgs),

    /// Autocomplete suggestions for a prefix (MCP `typeahead`)
    ///
    /// Examples:
    ///   trusty-search typeahead rrf_fu --limit 10 --mode blended
    #[command(display_order = 16)]
    Typeahead {
        /// Prefix or partial symbol to complete
        query: String,
        /// Max suggestions, 1-25
        #[arg(long)]
        limit: Option<u64>,
        /// Retrieval mode
        #[arg(long, value_enum)]
        mode: Option<TypeaheadMode>,
    },
}

/// Set `obj[key]` to `v` when `v` is present.
fn put(obj: &mut Value, key: &str, v: Option<Value>) {
    if let Some(v) = v {
        obj[key] = v;
    }
}

/// Set `obj[key] = true` when `flag` is set, so an unset switch stays absent.
fn flag(obj: &mut Value, key: &str, flag: bool) {
    if flag {
        obj[key] = Value::Bool(true);
    }
}

impl SearchVerb {
    /// True unless the verb fans out over every index (`grep --all`).
    pub fn needs_index(&self) -> bool {
        !matches!(self, Self::Grep(g) if g.all)
    }

    /// The socket method and params the matching MCP tool sends.
    ///
    /// Why: the CLI and MCP must hit one daemon contract, so the params copy
    /// the shapes in `mcp/tools/{search,misc}.rs` field for field.
    /// What: `similar` and index-scoped `grep` use the `{index_id, body}`
    /// envelope; `call-chain` and `typeahead` put fields beside `index_id`;
    /// `grep --all` sends the bare body. An unset option is omitted, so the
    /// daemon default applies. `index` is ignored when [`Self::needs_index`]
    /// is false.
    /// Test: `each_verb_sends_its_method_and_params`,
    /// `grep_sends_every_switch_the_mcp_tool_sends`.
    pub fn request(&self, index: &str) -> (&'static str, Value) {
        match self {
            Self::Similar {
                file,
                function,
                top_k,
            } => {
                let mut body = json!({ "file": file });
                put(&mut body, "function", function.clone().map(Value::from));
                put(&mut body, "top_k", top_k.map(Value::from));
                (METHOD_SIMILAR, json!({ "index_id": index, "body": body }))
            }
            Self::CallChain {
                entry_point,
                direction,
                max_depth,
                no_source,
            } => {
                let mut p = json!({ "index_id": index, "entry_point": entry_point });
                let dir = direction.map(|d| match d {
                    Direction::Both => "both",
                    Direction::Outgoing => "outgoing",
                    Direction::Callers => "callers",
                });
                put(&mut p, "direction", dir.map(Value::from));
                put(&mut p, "max_depth", max_depth.map(Value::from));
                if *no_source {
                    p["include_source"] = Value::Bool(false);
                }
                (METHOD_CALL_CHAIN, p)
            }
            Self::Grep(g) => {
                let body = grep_body(g);
                if g.all {
                    (METHOD_GREP_ALL, body)
                } else {
                    (METHOD_GREP, json!({ "index_id": index, "body": body }))
                }
            }
            Self::Typeahead { query, limit, mode } => {
                let mut p = json!({ "index_id": index, "q": query });
                put(&mut p, "limit", limit.map(Value::from));
                let mode = mode.map(|m| match m {
                    TypeaheadMode::Lexical => "lexical",
                    TypeaheadMode::Blended => "blended",
                });
                put(&mut p, "mode", mode.map(Value::from));
                (METHOD_TYPEAHEAD, p)
            }
        }
    }
}

/// The `GrepRequest` body for `g`, with only the switches the user set.
fn grep_body(g: &GrepArgs) -> Value {
    let mut b = json!({ "pattern": g.pattern });
    flag(&mut b, "case_insensitive", g.ignore_case);
    flag(&mut b, "fixed_strings", g.fixed_strings);
    flag(&mut b, "word_regexp", g.word_regexp);
    flag(&mut b, "invert_match", g.invert_match);
    flag(&mut b, "files_with_matches", g.files_with_matches);
    flag(&mut b, "multiline", g.multiline);
    put(&mut b, "context", g.context.map(Value::from));
    put(&mut b, "context_before", g.context_before.map(Value::from));
    put(&mut b, "context_after", g.context_after.map(Value::from));
    put(&mut b, "glob", g.glob.clone().map(Value::from));
    put(&mut b, "max_results", g.max_results.map(Value::from));
    b
}

/// Send `verb` to the daemon and return the method result.
///
/// # Errors
///
/// "could not reach daemon: …" naming the socket when nothing answers;
/// "daemon returned …" for a refusal (see `daemon_rpc::rpc_error`).
pub(crate) async fn run(client: &DaemonClient, verb: &SearchVerb, index: &str) -> Result<Value> {
    let (method, params) = verb.request(index);
    super::daemon_rpc::call(client, method, params).await
}

/// Execute one of the [`SearchVerb`] subcommands.
///
/// Why: one entry point keeps index resolution and output identical across
/// the four verbs.
/// What: resolves the index (`-i` wins, else the cwd project) unless the verb
/// fans out, starts the daemon if needed, calls it, and prints the result as
/// JSON under `--json` or as text. Any failure returns `Err`, which `main()`
/// prints and turns into exit code 1.
/// Test: `a_verb_names_the_socket_when_no_daemon_answers`.
pub async fn handle(verb: SearchVerb, explicit: &Option<String>, json_out: bool) -> Result<()> {
    let index = if verb.needs_index() {
        let (id, warned) = super::index_resolve::resolve_index(explicit)?;
        super::index_resolve::print_index_header(&id, warned);
        id
    } else {
        String::new()
    };
    let client = super::daemon_rpc::connect().await?;
    let body = run(&client, &verb, &index).await?;
    if json_out {
        println!("{body}");
    } else {
        super::search_verbs_render::render(&verb, &body);
    }
    Ok(())
}
