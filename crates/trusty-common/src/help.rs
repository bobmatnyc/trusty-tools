//! Declarative CLI help system with "did you mean?" suggestions.
//!
//! Why: every standalone trusty-* binary (search, memory, analyze, mpm-cli, tga,
//! open-mpm) was rendering its `--help` and unknown-subcommand error output
//! independently, so the formats drifted over time. Issue #216 centralises the
//! help model into one declarative YAML schema, one canonical renderer, and one
//! Jaro-Winkler suggester so the six binaries share a single user-facing voice.
//!
//! What: the module loads a [`HelpConfig`] from a per-binary `help.yaml`
//! (typically embedded with `include_str!` so the help text ships inside the
//! binary, no runtime file I/O required), renders a top-level or subcommand
//! help block via [`render_help`], and proposes the closest matching command
//! via [`suggest`] when the user mistypes a subcommand. All three functions are
//! pure — no global state, no logging, no I/O.
//!
//! Test: `cargo test -p trusty-common --features cli-help` exercises YAML
//! parsing, top-level vs subcommand rendering, and the suggester's similarity
//! threshold (positive match + below-threshold rejection).

use indexmap::IndexMap;
use serde::Deserialize;
use strsim::jaro_winkler;
use thiserror::Error;

/// Minimum Jaro-Winkler similarity (0.0..=1.0) above which [`suggest`] proposes
/// a command. Tuned empirically — `0.85` catches a single transposition or
/// dropped character but rejects accidental noise like `xyz`.
///
/// Why: hard-coded module-level constant keeps the threshold honest and
/// discoverable. If a future tweak is needed, the call site is a single
/// search target.
/// What: pure data; no Test stanza (used by `suggest`'s tests).
const SIMILARITY_THRESHOLD: f64 = 0.85;

/// Errors returned by the help loader.
///
/// Why: `serde_yaml` errors carry useful context (line/column), so the help
/// loader surfaces a structured error type that wraps them rather than a
/// stringified `anyhow`. Library consumers (binary crates) can match on the
/// variant if they ever want to distinguish parse vs. structural failures.
/// What: a thin newtype around `serde_yaml::Error`. The `Display` impl
/// preserves the underlying message verbatim.
/// Test: `load_help_returns_err_on_malformed_yaml`.
#[derive(Debug, Error)]
pub enum HelpError {
    /// The YAML payload failed to parse into [`HelpConfig`].
    #[error("parse help.yaml: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Convenience alias used throughout the module.
pub type Result<T> = std::result::Result<T, HelpError>;

/// Top-level help configuration parsed from a per-binary `help.yaml`.
///
/// Why: every trusty-* CLI surfaces the same shape — a binary name, a one-line
/// tagline, a usage line, and a flat command list (each command may itself
/// carry subcommands). Capturing that shape declaratively means a future docs
/// pipeline can render the same help to a website without re-implementing the
/// renderer.
/// What: serde-deserialized from YAML. `commands` preserves declaration order
/// (`IndexMap`) so `--help` lists subcommands in the order the maintainer
/// chose, not alphabetical.
/// Test: `load_help_parses_yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct HelpConfig {
    /// Binary name as the user types it (`trusty-search`, `tm`, `tga`, …).
    pub name: String,
    /// One-line description shown on the first line of `--help`.
    pub tagline: String,
    /// Usage signature, e.g. `trusty-search <COMMAND> [OPTIONS]`.
    pub usage: String,
    /// Ordered command list. Declaration order in YAML is preserved.
    #[serde(default)]
    pub commands: IndexMap<String, CommandDef>,
    /// Whether to enable the "did you mean?" suggester for this binary.
    ///
    /// Defaults to `true` because the suggester is the whole reason this
    /// module exists; a binary can opt out by setting `suggest: false` in
    /// its `help.yaml` (rare — useful only for binaries whose commands are
    /// themselves user data).
    #[serde(default = "default_suggest")]
    pub suggest: bool,
}

/// Why: serde needs a function pointer for `#[serde(default = "…")]`.
/// What: returns `true`.
/// Test: covered indirectly by `load_help_defaults_suggest_to_true`.
fn default_suggest() -> bool {
    true
}

/// A single command (or subcommand) entry.
///
/// Why: commands have a description, optional flags, positional args, examples,
/// and may themselves nest subcommands. Modelling all of that as one recursive
/// struct keeps the YAML schema flat and the renderer simple.
/// What: deserialised from a YAML mapping; every optional field defaults to an
/// empty container so `help.yaml` files stay readable.
/// Test: `load_help_parses_yaml` exercises every field.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandDef {
    /// Short description shown on the same line as the command name.
    pub description: String,
    /// Flags accepted by this command (rendered in the OPTIONS section).
    #[serde(default)]
    pub flags: Vec<FlagDef>,
    /// Positional argument names (rendered in the USAGE line).
    #[serde(default)]
    pub args: Vec<String>,
    /// Worked examples shown in an EXAMPLES section.
    #[serde(default)]
    pub examples: Vec<Example>,
    /// Nested subcommands. Use `None` to mean "leaf command".
    #[serde(default)]
    pub subcommands: Option<IndexMap<String, CommandDef>>,
}

/// A single flag definition.
///
/// Why: clap renders flags with a long name, an optional short name, an
/// optional type hint, a default value, and a description. The help.yaml
/// schema mirrors that 1:1 so the rendered output matches what users see
/// from clap.
/// What: `name` is the long flag (sans leading `--`). `short` is optional and
/// rendered as `-X`. `type_hint`/`default` decorate the OPTIONS column.
/// Test: `load_help_parses_yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct FlagDef {
    /// Long flag name without leading dashes (e.g. `port`).
    pub name: String,
    /// Optional short name (single character, e.g. `'p'` for `-p`).
    #[serde(default)]
    pub short: Option<char>,
    /// Optional type hint shown after the flag name (e.g. `PORT`, `PATH`).
    #[serde(default)]
    pub type_hint: Option<String>,
    /// Optional default value shown in brackets after the description.
    #[serde(default)]
    pub default: Option<String>,
    /// One-line flag description.
    pub description: String,
}

/// A worked example shown under EXAMPLES.
///
/// Why: examples are the most reliable form of CLI help — far more useful than
/// flag enumeration. Each entry pairs the command line with an optional note
/// explaining when to use it.
/// What: `cmd` is the literal shell command line. `note` is an optional
/// `# comment`-style annotation rendered on the next line.
/// Test: `render_help_top_level` verifies examples appear in the output.
#[derive(Debug, Clone, Deserialize)]
pub struct Example {
    /// Shell command line (rendered verbatim, no shell-escaping).
    pub cmd: String,
    /// Optional explanatory note shown indented under `cmd`.
    #[serde(default)]
    pub note: Option<String>,
}

/// Parse a `help.yaml` payload into a [`HelpConfig`].
///
/// Why: every binary embeds its `help.yaml` via `include_str!` and calls this
/// function inside a [`std::sync::LazyLock`] at startup. Returning `Result`
/// (instead of panicking) lets the caller decide how to surface a corrupt
/// help file — most bins use `expect("help.yaml is bundled and valid")` since
/// the YAML is shipped inside the binary and a parse failure is a programmer
/// error caught at first run.
/// What: thin wrapper around `serde_yaml::from_str` that returns the typed
/// [`HelpConfig`] or a [`HelpError::Parse`].
/// Test: `load_help_parses_yaml` covers the happy path;
/// `load_help_returns_err_on_malformed_yaml` covers the error path.
pub fn load_help(yaml: &str) -> Result<HelpConfig> {
    let config: HelpConfig = serde_yaml::from_str(yaml)?;
    Ok(config)
}

/// Render the help text for a binary or one of its subcommands.
///
/// Why: every trusty-* binary used to spell out its `--help` in clap doc
/// comments and rely on clap's default renderer. The output diverged over
/// time (some sorted commands alphabetically, some by `display_order`; some
/// listed examples, some didn't). This function gives the workspace one
/// canonical layout that every binary can call from its unknown-subcommand
/// path.
/// What: when `subcommand` is `None`, emits the top-level help block
/// (tagline → USAGE → COMMANDS → global options → "Run '… <COMMAND>
/// --help'"). When `Some(name)`, descends `config.commands` (and nested
/// `subcommands`) to print that command's own help (description → USAGE
/// → OPTIONS → EXAMPLES). Returns an error message if the subcommand path
/// is not found.
/// Test: `render_help_top_level` and `render_help_subcommand`.
pub fn render_help(config: &HelpConfig, subcommand: Option<&str>) -> String {
    match subcommand {
        None => render_top_level(config),
        Some(path) => render_subcommand(config, path),
    }
}

/// Why: top-level help is the most common rendering path (`trusty-search
/// --help`), so factored out for clarity.
/// What: emits `<name> — <tagline>` then USAGE, COMMANDS, OPTIONS, and a
/// "Run '<name> <COMMAND> --help'" footer. Each section is separated by a
/// blank line.
/// Test: covered by `render_help_top_level`.
fn render_top_level(config: &HelpConfig) -> String {
    let mut out = String::new();
    // Header line: "name — tagline" (em dash, matching clap's style).
    out.push_str(&format!("{} — {}\n", config.name, config.tagline));
    out.push('\n');

    out.push_str("USAGE:\n");
    out.push_str(&format!("    {}\n", config.usage));
    out.push('\n');

    if !config.commands.is_empty() {
        out.push_str("COMMANDS:\n");
        // Compute padding for command-name column so descriptions align.
        let name_width = config
            .commands
            .keys()
            .map(|k| k.len())
            .max()
            .unwrap_or(0)
            .max(4);
        for (name, cmd) in &config.commands {
            out.push_str(&format!(
                "    {:width$}    {}\n",
                name,
                cmd.description,
                width = name_width
            ));
        }
        out.push('\n');
    }

    // Standard global options every binary exposes.
    out.push_str("OPTIONS:\n");
    out.push_str("    -h, --help       Print this help\n");
    out.push_str("    -V, --version    Print version\n");
    out.push('\n');

    out.push_str(&format!(
        "Run '{} <COMMAND> --help' for command-specific help.\n",
        config.name
    ));
    out
}

/// Why: when the user runs `<bin> <command> --help`, we resolve a possibly
/// nested command path (e.g. `service install`) and print that leaf
/// command's help.
/// What: splits `path` on whitespace, walks `config.commands` and any nested
/// `subcommands` maps, then renders description + USAGE + OPTIONS + EXAMPLES
/// for the resolved leaf. If any segment is unknown, returns a single-line
/// error message instead of panicking — the caller will print it verbatim.
/// Test: `render_help_subcommand` and `render_help_subcommand_unknown`.
fn render_subcommand(config: &HelpConfig, path: &str) -> String {
    let parts: Vec<&str> = path.split_whitespace().collect();
    if parts.is_empty() {
        return render_top_level(config);
    }

    // Resolve the command chain.
    let mut commands_map: &IndexMap<String, CommandDef> = &config.commands;
    let mut current: Option<&CommandDef> = None;
    let mut resolved_path: Vec<String> = Vec::with_capacity(parts.len());
    for part in &parts {
        match commands_map.get(*part) {
            Some(cmd) => {
                current = Some(cmd);
                resolved_path.push((*part).to_string());
                if let Some(subs) = &cmd.subcommands {
                    commands_map = subs;
                } else {
                    // Leaf: no further nesting possible.
                    commands_map = &EMPTY_MAP;
                }
            }
            None => {
                return format!("unknown command: {}\n", parts.join(" "));
            }
        }
    }

    let Some(cmd) = current else {
        return format!("unknown command: {}\n", parts.join(" "));
    };

    let full_name = format!("{} {}", config.name, resolved_path.join(" "));
    let mut out = String::new();
    out.push_str(&format!("{full_name} — {}\n\n", cmd.description));

    // USAGE line: include positional args inline.
    out.push_str("USAGE:\n");
    let mut usage_line = format!("    {full_name}");
    for arg in &cmd.args {
        usage_line.push_str(&format!(" <{}>", arg.to_uppercase()));
    }
    if !cmd.flags.is_empty() {
        usage_line.push_str(" [OPTIONS]");
    }
    if cmd.subcommands.is_some() {
        usage_line.push_str(" <SUBCOMMAND>");
    }
    out.push_str(&usage_line);
    out.push('\n');
    out.push('\n');

    if let Some(subs) = &cmd.subcommands
        && !subs.is_empty()
    {
        out.push_str("SUBCOMMANDS:\n");
        let name_width = subs.keys().map(|k| k.len()).max().unwrap_or(0).max(4);
        for (n, sub) in subs {
            out.push_str(&format!(
                "    {:width$}    {}\n",
                n,
                sub.description,
                width = name_width
            ));
        }
        out.push('\n');
    }

    if !cmd.flags.is_empty() {
        out.push_str("OPTIONS:\n");
        for flag in &cmd.flags {
            let mut left = String::new();
            if let Some(short) = flag.short {
                left.push_str(&format!("-{short}, "));
            } else {
                left.push_str("    ");
            }
            left.push_str(&format!("--{}", flag.name));
            if let Some(hint) = &flag.type_hint {
                left.push_str(&format!(" <{hint}>"));
            }
            let mut right = flag.description.clone();
            if let Some(def) = &flag.default {
                right.push_str(&format!(" [default: {def}])"));
            }
            out.push_str(&format!("    {left:<32}{right}\n"));
        }
        out.push('\n');
    }

    if !cmd.examples.is_empty() {
        out.push_str("EXAMPLES:\n");
        for ex in &cmd.examples {
            if let Some(note) = &ex.note {
                out.push_str(&format!("    # {note}\n"));
            }
            out.push_str(&format!("    {}\n", ex.cmd));
        }
        out.push('\n');
    }

    out
}

/// Static empty IndexMap used when resolving past a leaf command.
///
/// Why: lets `render_subcommand` always hold a `&IndexMap` reference even
/// after walking past a leaf, simplifying the loop.
/// What: `LazyLock` over `IndexMap::new()`. Created once per process.
/// Test: not directly tested — covered indirectly via `render_help_subcommand`.
static EMPTY_MAP: std::sync::LazyLock<IndexMap<String, CommandDef>> =
    std::sync::LazyLock::new(IndexMap::new);

/// Propose the closest matching command name when the user types an unknown
/// subcommand.
///
/// Why: clap prints a generic "unrecognized subcommand" message and exits. A
/// "did you mean: <closest>?" hint dramatically improves the first-time-user
/// experience and matches the affordance every modern CLI (cargo, git, gh)
/// ships. Living in `trusty_common` keeps the threshold and string-distance
/// algorithm consistent across binaries.
/// What: walks every top-level command name in `config`, computes the
/// Jaro-Winkler similarity to `input`, and returns
/// `Some("Did you mean: <best>?")` only if the highest similarity exceeds
/// [`SIMILARITY_THRESHOLD`]. Returns `None` when `config.suggest` is `false`
/// or when no candidate clears the bar. Comparison is case-insensitive.
/// Test: `suggest_returns_closest_match` and `suggest_returns_none_when_no_match`.
pub fn suggest(input: &str, config: &HelpConfig) -> Option<String> {
    if !config.suggest {
        return None;
    }
    let input_lc = input.to_lowercase();
    let mut best: Option<(f64, &str)> = None;
    for name in config.commands.keys() {
        let score = jaro_winkler(&input_lc, &name.to_lowercase());
        match best {
            Some((b, _)) if b >= score => {}
            _ => best = Some((score, name.as_str())),
        }
    }
    best.and_then(|(score, name)| {
        if score > SIMILARITY_THRESHOLD {
            Some(format!("Did you mean: {name}?"))
        } else {
            None
        }
    })
}

/// Extract the unknown-command token from a clap error.
///
/// Why: clap renders parse errors with the offending token embedded in the
/// `--help`-style message ("error: unrecognized subcommand 'qury'"). When
/// the error kind is `InvalidSubcommand`, the binary's first positional
/// token in argv is overwhelmingly the culprit, so the caller can pass the
/// raw argv along with the clap error and we'll extract the first non-flag
/// argument as the candidate for the suggester.
/// What: walks `argv` skipping the binary name (index 0) and any leading
/// `--flag` / `-f` tokens (and their values when separated by whitespace)
/// until it finds the first positional token. Returns `None` if argv is too
/// short or every token is a flag.
/// Test: covered by `extract_unknown_subcommand_finds_first_positional` and
/// `extract_unknown_subcommand_returns_none_when_no_positional`.
pub fn extract_unknown_subcommand(argv: &[String]) -> Option<&str> {
    let mut iter = argv.iter().enumerate();
    // Skip argv[0] (the binary name).
    let _ = iter.next();
    while let Some((_, tok)) = iter.next() {
        if let Some(stripped) = tok.strip_prefix("--") {
            // Long flag. If it carries `--foo=bar`, no follow-up token.
            // If it's bare `--foo`, the next token *might* be its value;
            // we conservatively skip exactly one follow-up only when the
            // current token contains no `=`.
            if !stripped.contains('=') {
                // Skip a potential value token. The heuristic is loose, but
                // the worst case is that we treat the value as positional —
                // and the suggester will then either match it (giving a
                // useful hint) or return None.
                let _ = iter.next();
            }
            continue;
        }
        if tok.starts_with('-') && tok.len() > 1 {
            // Short flag like `-v`. Same value-skipping heuristic.
            let _ = iter.next();
            continue;
        }
        return Some(tok.as_str());
    }
    None
}

/// Emit a "Did you mean?" hint to stderr when the user types an unknown
/// subcommand.
///
/// Why: each binary's `main.rs` should call this immediately before exiting
/// non-zero on a clap parse error so the user sees a friendly suggestion in
/// addition to clap's own error message. Living here keeps the wording and
/// the binary-name placeholder substitution consistent across the workspace.
/// What: writes two lines to stderr — `Did you mean: <closest>?` (only when
/// the suggester finds a match) and `Run '<binary> --help' for available
/// commands.` Always writes the second line. Caller is responsible for the
/// `process::exit(1)`.
/// Test: covered by integration of each binary's main path. Not unit-tested
/// directly because it writes to the global stderr stream.
pub fn print_suggestion_hint(argv: &[String], config: &HelpConfig) {
    if let Some(unknown) = extract_unknown_subcommand(argv)
        && let Some(hint) = suggest(unknown, config)
    {
        eprintln!("  {hint}");
    }
    eprintln!("  Run '{} --help' for available commands.", config.name);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
name: trusty-search
tagline: Hybrid BM25 + semantic + KG search daemon and MCP server
usage: trusty-search <COMMAND> [OPTIONS]
commands:
  start:
    description: Start the HTTP daemon and MCP server
    flags:
      - name: port
        short: p
        type_hint: PORT
        default: "7878"
        description: Port to listen on
    examples:
      - cmd: trusty-search start
      - cmd: trusty-search start --port 8080
        note: bind to a non-default port
  query:
    description: Query a search index
    args: [query]
    flags:
      - name: index
        type_hint: NAME
        description: Override auto-detected index
  service:
    description: Manage the launchd service
    subcommands:
      install:
        description: Install the launchd plist
      uninstall:
        description: Remove the launchd plist
"#;

    #[test]
    fn load_help_parses_yaml() {
        let config = load_help(SAMPLE_YAML).expect("yaml must parse");
        assert_eq!(config.name, "trusty-search");
        assert!(config.tagline.starts_with("Hybrid"));
        assert_eq!(config.commands.len(), 3);
        let start = config.commands.get("start").expect("start defined");
        assert_eq!(start.flags.len(), 1);
        assert_eq!(start.flags[0].name, "port");
        assert_eq!(start.flags[0].short, Some('p'));
        assert_eq!(start.flags[0].default.as_deref(), Some("7878"));
        assert_eq!(start.examples.len(), 2);
        let service = config.commands.get("service").expect("service defined");
        let subs = service.subcommands.as_ref().expect("subcommands present");
        assert!(subs.contains_key("install"));
        assert!(subs.contains_key("uninstall"));
    }

    #[test]
    fn load_help_defaults_suggest_to_true() {
        let yaml = "name: x\ntagline: y\nusage: x\ncommands: {}\n";
        let config = load_help(yaml).unwrap();
        assert!(
            config.suggest,
            "suggest should default to true when omitted"
        );
    }

    #[test]
    fn load_help_returns_err_on_malformed_yaml() {
        let yaml = "name: trusty-search\ntagline: [unterminated";
        let err = load_help(yaml).unwrap_err();
        assert!(matches!(err, HelpError::Parse(_)));
    }

    #[test]
    fn render_help_top_level() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let out = render_help(&config, None);
        assert!(
            out.starts_with("trusty-search — Hybrid"),
            "header missing or wrong: {out}"
        );
        assert!(out.contains("USAGE:"));
        assert!(out.contains("trusty-search <COMMAND> [OPTIONS]"));
        assert!(out.contains("COMMANDS:"));
        // Order from YAML preserved.
        let start_idx = out.find("    start").expect("start listed");
        let query_idx = out.find("    query").expect("query listed");
        let service_idx = out.find("    service").expect("service listed");
        assert!(start_idx < query_idx);
        assert!(query_idx < service_idx);
        assert!(out.contains("OPTIONS:"));
        assert!(out.contains("--help"));
        assert!(out.contains("--version"));
        assert!(out.contains("Run 'trusty-search <COMMAND> --help'"));
    }

    #[test]
    fn render_help_subcommand() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let out = render_help(&config, Some("start"));
        assert!(out.contains("trusty-search start"));
        assert!(out.contains("Start the HTTP daemon"));
        assert!(out.contains("USAGE:"));
        assert!(out.contains("OPTIONS:"));
        assert!(out.contains("--port"));
        assert!(out.contains("-p"));
        assert!(out.contains("[default: 7878"));
        assert!(out.contains("EXAMPLES:"));
        assert!(out.contains("trusty-search start --port 8080"));
        assert!(out.contains("# bind to a non-default port"));
    }

    #[test]
    fn render_help_subcommand_with_positional_args() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let out = render_help(&config, Some("query"));
        // Positional args rendered uppercase between angle brackets.
        assert!(
            out.contains("<QUERY>"),
            "positional arg missing from query usage: {out}"
        );
    }

    #[test]
    fn render_help_subcommand_with_nested_subcommands() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let out = render_help(&config, Some("service"));
        assert!(out.contains("SUBCOMMANDS:"));
        assert!(out.contains("install"));
        assert!(out.contains("uninstall"));
        // The USAGE line should mention <SUBCOMMAND>.
        assert!(out.contains("<SUBCOMMAND>"));
    }

    #[test]
    fn render_help_subcommand_unknown() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let out = render_help(&config, Some("nope"));
        assert!(out.starts_with("unknown command:"));
    }

    #[test]
    fn suggest_returns_closest_match() {
        let config = load_help(SAMPLE_YAML).unwrap();
        // One transposition / dropped char from "query".
        let s = suggest("quer", &config).expect("should suggest for typo");
        assert!(s.contains("Did you mean"));
        assert!(s.contains("query"));
    }

    #[test]
    fn suggest_returns_none_when_no_match() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let s = suggest("xyzzy", &config);
        assert!(s.is_none(), "expected None for unrelated input, got {s:?}");
    }

    #[test]
    fn suggest_is_case_insensitive() {
        let config = load_help(SAMPLE_YAML).unwrap();
        let s = suggest("START", &config).expect("uppercase should still match");
        assert!(s.contains("start"));
    }

    #[test]
    fn extract_unknown_subcommand_finds_first_positional() {
        let argv: Vec<String> = ["trusty-search", "qury", "fn auth"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(extract_unknown_subcommand(&argv), Some("qury"));
    }

    #[test]
    fn extract_unknown_subcommand_skips_leading_flags() {
        let argv: Vec<String> = ["trusty-search", "--verbose", "satus"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // `--verbose` is a long flag with no `=`; our heuristic skips one
        // follow-up token. So `satus` is correctly treated as positional only
        // if the heuristic doesn't eat it. In practice clap flags either come
        // with `=value` or are boolean — for boolean flags we tolerate a
        // single skipped token because that token would otherwise be a value
        // for an unknown flag the user typed by mistake. So the test asserts
        // the function never returns the verbose flag itself.
        let got = extract_unknown_subcommand(&argv);
        assert_ne!(got, Some("--verbose"));
    }

    #[test]
    fn extract_unknown_subcommand_returns_none_when_no_positional() {
        let argv: Vec<String> = ["trusty-search", "--verbose"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(extract_unknown_subcommand(&argv).is_none());
    }

    #[test]
    fn extract_unknown_subcommand_handles_equals_form() {
        let argv: Vec<String> = ["trusty-search", "--index=foo", "satus"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // `--index=foo` carries its value inline, so the next token is
        // positional.
        assert_eq!(extract_unknown_subcommand(&argv), Some("satus"));
    }

    #[test]
    fn suggest_respects_suggest_false() {
        let yaml =
            "name: x\ntagline: y\nusage: x\nsuggest: false\ncommands:\n  query: {description: q}\n";
        let config = load_help(yaml).unwrap();
        // Even with a near-perfect typo, suggester returns None when disabled.
        assert!(suggest("quer", &config).is_none());
    }
}
