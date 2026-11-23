//! The `trusty-summarizer` CLI: `summarize`, `read`, `find`, `size`,
//! `miss`, `misses`, `usage` and `server`.
//!
//! `size`, `summarize`, `find` and `read` append one line per document to
//! the usage log ([`trusty_summarizer::usage`]) in this process, warm or
//! not; a failed usage write is a stderr warning and changes no exit status.
//!
//! `summarize` runs through the warm server ([`trusty_summarizer::server`])
//! unless `--no-warm` or `warm.enabled = false`.
//!
//! Exit status: 0 when every file succeeded, 2 when the batch finished with
//! at least one error record, 1 on a fatal error (bad configuration, bad
//! range, unreadable file for `read`). `size` exits 2 when a file failed;
//! `miss` exits 0 when the line was appended (even for an unreadable
//! document) and 1 when the log cannot be written.

use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use trusty_summarizer::cache::Cache;
use trusty_summarizer::find::{FindOptions, find};
use trusty_summarizer::output::{RenderOptions, render as render_record};
use trusty_summarizer::server::client::{self, WarmError};
use trusty_summarizer::server::{RuntimeDir, daemon};
use trusty_summarizer::trial::{self, Category, MissEntry};
use trusty_summarizer::usage::{self, UsageEntry};
use trusty_summarizer::{BackendRegistry, Config, Converters, LineIndex, Span, Summarizer};

#[derive(Debug, Parser)]
#[command(
    name = "trusty-summarizer",
    version,
    about = "Fast file summaries, classes and outlines"
)]
struct Cli {
    /// Config file (default: $TRUSTY_SUMMARIZER_CONFIG, then
    /// ~/.config/trusty-summarizer/config.toml if present).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Summarize files and directories (recursive); one JSON line per file.
    Summarize {
        /// Files or directories.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Omit line and paragraph ranges from the outline.
        #[arg(long)]
        no_references: bool,
        /// Plain outline (the default): overrides `outline.describe = true`.
        #[arg(long)]
        no_describe: bool,
        /// Neither read nor write the result cache.
        #[arg(long)]
        no_cache: bool,
        /// Cache directory (overrides the config).
        #[arg(long, value_name = "DIR")]
        cache_dir: Option<PathBuf>,
        /// Files processed at once (overrides the config).
        #[arg(long, value_name = "N")]
        concurrency: Option<usize>,
        /// Run in this process: no warm server, no socket.
        #[arg(long)]
        no_warm: bool,
        /// Print at most N outline entries (default: all). A capped record
        /// keeps the first N and adds `outline_omitted`: the dropped count
        /// and, with references, the lines they cover.
        #[arg(long, value_name = "N")]
        outline_max: Option<usize>,
        /// Also print the per-run `cache` status and `stages` timings and
        /// costs. Without it, a fresh and a cached run print the same bytes.
        #[arg(long)]
        stats: bool,
    },
    /// The warm background server.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Print lines A to B (1-based, inclusive) of a file's text.
    Read {
        /// Range `A-B`, or a single line `A`.
        #[arg(long, value_name = "A-B")]
        lines: Span,
        /// The file.
        file: PathBuf,
    },
    /// Print, as one JSON object, the lines of a file's text where a term
    /// or phrase occurs, with surrounding lines.
    Find {
        /// The term or phrase.
        #[arg(long, value_name = "TEXT")]
        query: String,
        /// At most this many hits (overrides the config).
        #[arg(long, value_name = "N")]
        max_hits: Option<usize>,
        /// Lines of context before and after each hit (overrides the config).
        #[arg(long, value_name = "N")]
        context: Option<usize>,
        /// Cut each shown line to N characters, marking a cut with `…`;
        /// 0 prints whole lines (default: `find.max_line_chars`, 160).
        #[arg(long, value_name = "N")]
        width: Option<usize>,
        /// The file.
        file: PathBuf,
    },
    /// Print each file's converted-text size as one JSON line:
    /// `{"path","format","converter","bytes","lines","text_chars","tokens_est"}`,
    /// or `{"path","error"}`. The values are the ones `summarize` records.
    ///
    /// `tokens_est` = ceil(text_chars / 4): median error about 5 % against a
    /// model's own count, from -20 % to +33 % per document. No model runs.
    /// Text, Markdown and code are read directly; PDF and Office files come
    /// from the converted-text cache, or are converted locally once (seconds)
    /// and cached. Exit 0, or 2 when a file failed.
    Size {
        /// Files.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Report a miss: append one JSON line to the per-user miss log.
    ///
    /// Example:
    ///   trusty-summarizer miss report.pdf --question "What is the annex 17
    ///   budget?" --category find-empty --note "find found no 'annex 17'"
    ///
    /// Records: UTC time, the document's absolute path, sha256, format,
    /// converter, bytes, text_chars and tokens_est (null for a converted
    /// format not yet in the cache), the question, note and category, the
    /// tool version, outline.describe and references in effect, and the
    /// working directory. Never records document text; no network call; no
    /// warm server.
    ///
    /// Log: $TRUSTY_SUMMARIZER_MISS_LOG, else
    /// ~/.local/state/trusty-summarizer/misses.jsonl (directory 0700, file
    /// 0600), the same from any working directory.
    ///
    /// Exit: 0 when the line was written, also for a missing or unreadable
    /// document (a warning on stderr, the reason in `doc_error`); 1 when the
    /// log cannot be written.
    #[command(verbatim_doc_comment)]
    Miss {
        /// The document the question was about.
        file: PathBuf,
        /// What was asked.
        #[arg(long, value_name = "TEXT")]
        question: String,
        /// What went wrong, free text.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// What kind of miss.
        #[arg(long, value_enum, value_name = "CATEGORY")]
        category: Option<Category>,
    },
    /// Show the miss log: `{"log": <path>, "exists": .., "count": N,
    /// "usage_log": <path>, "usage_count": M}` (M documents served, so the
    /// miss rate is N / M); with `--print`, the miss entries instead, one
    /// JSON line each.
    Misses {
        /// Print every entry.
        #[arg(long)]
        print: bool,
    },
    /// Count the usage log: one JSON object with `count`, `documents`,
    /// `errors`, `by_day`, `by_command` and `by_cwd`.
    ///
    /// Every `size`, `summarize`, `find` and `read` call appends one line per
    /// document: ts, command, path, hash, format, tokens_est, lines_served
    /// (`read`), range (`read`, as requested), hits and query (`find`),
    /// outcome (`ok` or the error kind),
    /// tool_version, cwd. Never document text. Log:
    /// $TRUSTY_SUMMARIZER_USAGE_LOG, else usage.jsonl beside the miss log.
    #[command(verbatim_doc_comment)]
    Usage {
        /// Count only entries at or after this UTC date or time
        /// (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ`).
        #[arg(long, value_name = "WHEN", value_parser = usage::parse_since)]
        since: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Print the running server's state as JSON (`"running": false` when
    /// none runs).
    Status,
    /// Stop the running server and wait for it to exit.
    Stop,
    /// Run the server in the foreground (what a `summarize` call starts).
    #[command(hide = true)]
    Run {
        /// Exit after this many seconds without a request.
        #[arg(long, value_name = "SECS")]
        idle_timeout_secs: Option<u64>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("trusty-summarizer: error: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut config = match Config::load(cli.config.as_deref()) {
        Ok((config, _source)) => config,
        // A miss is still worth recording under a broken config.
        Err(e) if matches!(cli.command, Command::Miss { .. }) => {
            eprintln!("trusty-summarizer: warning: config not loaded ({e}); using defaults");
            Config::default()
        }
        Err(e) => return Err(e.into()),
    };
    match cli.command {
        Command::Summarize {
            paths,
            no_references,
            no_describe,
            no_cache,
            cache_dir,
            concurrency,
            no_warm,
            outline_max,
            stats,
        } => {
            let render = RenderOptions {
                max_outline: outline_max,
                stats,
            };
            if no_references {
                config.references = false;
            }
            if no_describe {
                config.outline.describe = false;
            }
            if no_cache {
                config.cache.enabled = false;
            }
            if cache_dir.is_some() {
                config.cache.dir = cache_dir;
            }
            if concurrency.is_some() {
                config.concurrency = concurrency;
            }
            let cwd = std::env::current_dir()?;
            config.resolve_paths(&cwd)?;
            config.validate()?;
            let mut out = BufWriter::new(io::stdout().lock());
            let mut sink = |record: trusty_summarizer::FileRecord| {
                usage::record(&[UsageEntry::for_record(&cwd, &record)]);
                out.write_all(render_record(&record, &render)?.as_bytes())?;
                out.write_all(b"\n")?;
                out.flush()
            };
            let stats = if no_warm || !config.warm.enabled {
                let converters = Converters::standard(&config);
                let summarizer =
                    Summarizer::new(config, &BackendRegistry::with_builtins(), converters)?;
                match summarizer.summarize_batch(&paths, sink) {
                    Ok(stats) => stats,
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                        return Ok(ExitCode::SUCCESS);
                    }
                    Err(e) => return Err(e.into()),
                }
            } else {
                match client::summarize(&RuntimeDir::locate(), &config, &cwd, &paths, &mut sink) {
                    Ok(stats) => stats,
                    Err(WarmError::Sink(e)) if e.kind() == io::ErrorKind::BrokenPipe => {
                        return Ok(ExitCode::SUCCESS);
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            eprintln!(
                "trusty-summarizer: {} files, {} errors, {} from cache",
                stats.files, stats.errors, stats.cache_hits
            );
            Ok(if stats.errors > 0 {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Server { command } => {
            let dir = RuntimeDir::locate();
            let line = match command {
                ServerCommand::Status => match client::status(&dir)? {
                    Some(s) => {
                        let mut v = serde_json::to_value(&s)?;
                        v["running"] = true.into();
                        v
                    }
                    None => serde_json::json!({"running": false, "socket": dir.socket()}),
                },
                ServerCommand::Stop => match client::stop(&dir)? {
                    Some(pid) => serde_json::json!({"stopped": true, "pid": pid}),
                    None => serde_json::json!({"stopped": false, "running": false}),
                },
                ServerCommand::Run { idle_timeout_secs } => {
                    let secs = idle_timeout_secs.unwrap_or(config.warm.idle_timeout_secs);
                    if secs == 0 {
                        return Err("--idle-timeout-secs must be at least 1".into());
                    }
                    daemon::serve(&dir, Duration::from_secs(secs))?;
                    return Ok(ExitCode::SUCCESS);
                }
            };
            println!("{line}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Find {
            query,
            max_hits,
            context,
            width,
            file,
        } => {
            if !config.find.enabled {
                return Err("find is turned off (find.enabled = false)".into());
            }
            let opts = FindOptions {
                max_hits: max_hits.unwrap_or(config.find.max_hits).max(1),
                context: context.unwrap_or(config.find.context_lines),
                max_line_chars: width.unwrap_or(config.find.max_line_chars),
            };
            let mut entry = UsageEntry::new("find", &std::env::current_dir()?, &file);
            entry.query = Some(query.clone());
            let text = load_text(&config, &file, &mut entry);
            let text = match text {
                Ok(t) => t,
                Err(e) => {
                    usage::record(&[entry]);
                    return Err(e);
                }
            };
            let result = find(&text, &query, opts);
            entry.hits = Some(result.total);
            usage::record(&[entry]);
            let mut out = io::stdout().lock();
            let line = serde_json::to_string(&result)? + "\n";
            match out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
                Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
                _ => Ok(ExitCode::SUCCESS),
            }
        }
        Command::Size { files } => {
            let cache = if config.cache.enabled {
                Some(Cache::open(&config.cache_dir()?)?)
            } else {
                None
            };
            let mut failed = false;
            let cwd = std::env::current_dir()?;
            let mut out = BufWriter::new(io::stdout().lock());
            for file in &files {
                let path = file.display().to_string();
                let size = trial::text_size(&config, file, cache.as_ref());
                let kind = size.as_ref().map_err(|e| e.kind().to_string());
                usage::record(&[UsageEntry::for_size(&cwd, file, &kind.cloned())]);
                let line = match size {
                    Ok(size) => {
                        let mut v = serde_json::to_value(&size)?;
                        v["path"] = path.into();
                        v
                    }
                    Err(e) => {
                        failed = true;
                        serde_json::json!({"path": path, "error": e.to_string()})
                    }
                };
                let line = line.to_string() + "\n";
                if let Err(e) = out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
                    if e.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(ExitCode::SUCCESS);
                    }
                    return Err(e.into());
                }
            }
            Ok(if failed {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Miss {
            file,
            question,
            note,
            category,
        } => {
            let log = trial::miss_log_path()?;
            let cwd = std::env::current_dir()?;
            let entry =
                MissEntry::build(&config, &cwd, &file, &question, note.as_deref(), category);
            if let Some(reason) = &entry.doc_error {
                eprintln!(
                    "trusty-summarizer: warning: miss recorded without document facts: {reason}"
                );
            }
            trial::append(&log, &entry)
                .map_err(|e| format!("cannot write the miss log {}: {e}", log.display()))?;
            eprintln!("trusty-summarizer: miss recorded in {}", log.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Misses { print } => {
            let log = trial::miss_log_path()?;
            let lines = trial::read_log(&log)
                .map_err(|e| format!("cannot read the miss log {}: {e}", log.display()))?;
            let text = if print {
                lines.iter().map(|l| format!("{l}\n")).collect::<String>()
            } else {
                let usage_log = usage::usage_log_path()?;
                let used = trial::read_log(&usage_log).map_err(|e| {
                    format!("cannot read the usage log {}: {e}", usage_log.display())
                })?;
                serde_json::json!({
                    "log": log,
                    "exists": trusty_summarizer::logfile::exists(&log),
                    "count": lines.len(),
                    "usage_log": usage_log,
                    "usage_count": usage::tally(&used, None).count,
                })
                .to_string()
                    + "\n"
            };
            let mut out = io::stdout().lock();
            match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
                Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
                _ => Ok(ExitCode::SUCCESS),
            }
        }
        Command::Usage { since } => {
            let log = usage::usage_log_path()?;
            let lines = trial::read_log(&log)
                .map_err(|e| format!("cannot read the usage log {}: {e}", log.display()))?;
            let mut v = serde_json::to_value(usage::tally(&lines, since.as_deref()))?;
            v["log"] = serde_json::to_value(&log)?;
            v["exists"] = trusty_summarizer::logfile::exists(&log).into();
            v["since"] = since.into();
            let text = v.to_string() + "\n";
            let mut out = io::stdout().lock();
            match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
                Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
                _ => Ok(ExitCode::SUCCESS),
            }
        }
        Command::Read { lines, file } => {
            let mut entry = UsageEntry::new("read", &std::env::current_dir()?, &file);
            entry.range = Some(lines.to_string());
            let slice = load_text(&config, &file, &mut entry).and_then(|text| {
                let (slice, served) = LineIndex::new(&text).slice_clamped(lines)?;
                if served != lines {
                    eprintln!(
                        "trusty-summarizer: note: {lines} runs past the last line; served {served}"
                    );
                }
                Ok((slice.to_string(), served))
            });
            match &slice {
                Ok((_, served)) => entry.lines_served = Some(served.len()),
                Err(_) if entry.outcome == "ok" => entry.outcome = "invalid_range".to_string(),
                Err(_) => {}
            }
            usage::record(&[entry]);
            let (slice, _) = slice?;
            let mut out = io::stdout().lock();
            match out.write_all(slice.as_bytes()).and_then(|()| out.flush()) {
                Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
                _ => Ok(ExitCode::SUCCESS),
            }
        }
    }
}

/// A file's converted text, through the converted-text cache when enabled;
/// fills `entry`'s hash, format, `tokens_est`, or its outcome on failure.
fn load_text(
    config: &Config,
    file: &std::path::Path,
    entry: &mut UsageEntry,
) -> Result<String, Box<dyn std::error::Error>> {
    let cache = if config.cache.enabled {
        match config.cache_dir().and_then(|d| Cache::open(&d)) {
            Ok(c) => Some(c),
            Err(e) => {
                entry.outcome = e.kind().to_string();
                return Err(e.into());
            }
        }
    } else {
        None
    };
    let (text, facts) = usage::load_text(config, file, cache.as_ref());
    usage::fill_from_text(entry, &text, facts);
    Ok(text?)
}
