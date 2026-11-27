//! `build()` and `write_pack()` of `ruling_pack.py`: the pack text and its file (#12028 P1i).
//!
//! Why: the pack is what the Architect reads before a ruling; its sections,
//! notes and confidence line must be the script's, byte for byte.
//! What: [`build`] resolves the project, recalls, searches and greps, then
//! lays out the header, prior rulings, facts, the direct check, backend
//! problems with the MCP calls to make instead, and the label block, capped
//! at [`MAX_CHARS`] with the label block kept. [`write_pack`] writes it to
//! `<packs>/<utc>-<key>.md`.
//! Test: the `tests/replay_ruling_pack.rs` recordings,
//! `write_pack_lands_under_the_given_root`, `suggest_checks_follow_the_script`.

use std::path::Path;

use super::backends::{Recall, Search, grep_root, recall_all, search_index};
use super::pyerr::{PyErr, dict_get};
use super::scan::{identifiers, label, resolve, text_or_empty, trim};
use crate::classify::alerts::civil_from_days;
use crate::clock::Clock;
use crate::pyjson_load::PyValue;
use crate::pystr::py_repr;
use crate::pyval::{get, py_str, truthy};
use crate::writers::os_error;

/// The script's `MAX_CHARS`: the pack's cap, about 1.5k tokens.
pub(crate) const MAX_CHARS: usize = 6000;
/// The script's `HIT_CHARS`: one memory hit's length.
const HIT_CHARS: usize = 300;
/// The key-map flag that adds the `cto` palace (the script's field name).
const CTO_FLAG: &str = "duetto";

/// The inputs of one pack, as `main()` passes them to `build()`.
pub(crate) struct Request<'a> {
    pub(crate) project: &'a str,
    pub(crate) question: &'a str,
    pub(crate) issues: &'a [String],
    pub(crate) prs: &'a [String],
    pub(crate) files: &'a [String],
    pub(crate) symbols: &'a [String],
    pub(crate) qtype: &'a str,
    pub(crate) checked: bool,
}

/// Why `build` produced no pack.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    /// The script's `KeyError`, printed as `ruling_pack: <msg>` (exit 2).
    #[error("{0}")]
    Unknown(String),
    /// An exception the script does not catch (its traceback's last line).
    #[error(transparent)]
    Fatal(#[from] PyErr),
}

/// `strftime` of the clock's instant, UTC, with `fmt` `T` (`%Y-%m-%dT%H:%M:%SZ`)
/// or compact (`%Y%m%dT%H%M%SZ`).
pub(crate) fn stamp(clock: &dyn Clock, compact: bool) -> String {
    // Seconds truncate toward the past, as `datetime.now()` reads them.
    let secs = clock.now_unix().floor() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    if compact {
        format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }
}

/// `suggest_checks(entry, nums, qtype)`: read-only shell lines to run, never run here.
pub(crate) fn suggest_checks(e: &PyValue, nums: &[String], qtype: &str) -> Vec<String> {
    let repo = text_or_empty(get(e, "repo"));
    let gh = match get(e, "gh_config").filter(|v| truthy(v)) {
        Some(cfg) if !repo.is_empty() => format!(
            "env -u GH_TOKEN -u GITHUB_TOKEN GH_CONFIG_DIR=$HOME/.config/{} gh",
            py_str(cfg)
        ),
        _ => String::new(),
    };
    let mut cmds = Vec::new();
    for n in nums.iter().take(3) {
        if !gh.is_empty() && qtype == "merge" {
            cmds.push(format!("{gh} pr view {n} -R {repo} --json state,headRefOid,mergeable,mergeStateStatus,statusCheckRollup,autoMergeRequest"));
        } else if !gh.is_empty() {
            cmds.push(format!(
                "{gh} issue view {n} -R {repo} --json state,title,body,comments  # or pr view {n}"
            ));
        }
    }
    let root = text_or_empty(get(e, "root"));
    if qtype == "merge" && !root.is_empty() {
        cmds.push(format!(
            "git -C {root} fetch origin && git -C {root} log --oneline -5 origin/main"
        ));
    }
    if qtype == "prod-write" {
        cmds.push("read the migration file text and row counts / schema (read-only); confirm the named restore branch or snapshot exists".into());
    }
    if ["spec", "decision", "waiver"].contains(&qtype) && cmds.is_empty() && !root.is_empty() {
        cmds.push(format!(
            "open the cited doc section under {root}; check CODEOWNERS and the issue criteria"
        ));
    }
    if cmds.is_empty() {
        cmds.push("name one read-only check that could falsify the PM's claim (gh, git, curl, grep) and run it".into());
    }
    cmds
}

/// `mcp_hints(palaces, query, index, entry)`: the MCP calls to make by hand.
fn mcp_hints(palaces: &[String], query: &str, index: Option<&str>) -> Vec<String> {
    let q = trim(query, 80);
    let mut h: Vec<String> = palaces
        .iter()
        .map(|p| format!("memory_recall(palace=\"{p}\", query=\"{q}\", top_k=3, min_score=0.4)"))
        .collect();
    if let Some(i) = index {
        h.push(format!("grep(index_id=\"{i}\", pattern=\"<identifier>\", compact=true) / search(index_id=\"{i}\", query=\"{q}\", mode=\"text\", compact=true, top_k=5)"));
    }
    h
}

/// The first `n` items of a list value (`v[:n]`); a value of another shape
/// gives none (a script crash the port does not reproduce).
fn head(v: &PyValue, n: usize) -> &[PyValue] {
    match v {
        PyValue::List(items) => &items[..items.len().min(n)],
        _ => &[],
    }
}

/// `x or y` on two looked-up values, `None` standing for Python's `None`.
fn or<'a>(x: Option<&'a PyValue>, y: Option<&'a PyValue>) -> Option<&'a PyValue> {
    x.filter(|v| truthy(v)).or(y)
}

/// `str(v)` of a looked-up value, `None` printing as `None`.
fn show(v: Option<&PyValue>) -> String {
    v.map_or_else(|| "None".into(), py_str)
}

/// `build(...)`: the pack text, its label and the resolved key.
///
/// Why: the one call `main()` makes; the replay recordings exercise every arm.
/// What: see the module doc. An unknown project is [`BuildError::Unknown`]
/// naming the known keys, sorted.
/// Test: the `tests/replay_ruling_pack.rs` recordings.
pub(crate) fn build(
    req: &Request<'_>,
    projects: &[(String, PyValue)],
    clock: &dyn Clock,
) -> Result<(String, &'static str, String), BuildError> {
    let Some((key, e)) = resolve(req.project, projects) else {
        let mut keys: Vec<&str> = projects.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        let msg = format!(
            "unknown project {}; keys: {}",
            py_repr(req.project),
            keys.join(", ")
        );
        return Err(BuildError::Unknown(msg));
    };
    let ids = identifiers(req.question, req.issues, req.prs, req.files, req.symbols);
    let mut palaces: Vec<String> = Vec::new();
    let cto = get(e, CTO_FLAG)
        .is_some_and(truthy)
        .then(|| "cto".to_owned());
    let candidates = [
        get(e, "palace").filter(|p| truthy(p)).map(py_str),
        Some("supervisor".into()),
        cto,
    ];
    for p in candidates.into_iter().flatten() {
        if !palaces.contains(&p) {
            palaces.push(p);
        }
    }
    let mut notes = Vec::new();

    let mem: Vec<Recall> = recall_all(&palaces, req.question);
    let (mut prior, mut mem_lines) = (false, Vec::new());
    for (p, r) in palaces.iter().zip(&mem) {
        if let Some(err) = &r.error {
            notes.push(format!("{p}: {err}"));
        }
        for h in head(&r.hits, 3) {
            prior = true;
            let content = dict_get(h, "content")?.unwrap_or(h);
            mem_lines.push(format!("- [{p}] {}", trim(&py_str(content), HIT_CHARS)));
        }
    }
    if matches!(get(e, "palace"), None | Some(PyValue::Null)) {
        notes
            .push("no project palace in the key map; project rulings may sit in supervisor".into());
    }

    let (mut fact_lines, mut last_indexed, mut fact) = (Vec::new(), None, false);
    let index = get(e, "index").filter(|i| truthy(i)).map(py_str);
    if let Some(idx) = &index {
        match search_index(idx, req.question)? {
            Err(err) => notes.push(format!("{idx}: {err}")),
            Ok(Search {
                hits,
                last_indexed: li,
            }) => {
                last_indexed = li;
                fact = truthy(&hits);
                for h in head(&hits, 5) {
                    let path = or(dict_get(h, "path")?, dict_get(h, "file")?);
                    let line = dict_get(h, "start_line")?;
                    let empty = PyValue::Str(String::new());
                    let snip = or(
                        dict_get(h, "compact_snippet")?,
                        Some(dict_get(h, "content")?.unwrap_or(&empty)),
                    );
                    fact_lines.push(format!(
                        "- {}:{} {}",
                        show(path),
                        show(line),
                        trim(&show(snip), 200)
                    ));
                }
            }
        }
    } else {
        notes.push("no search index in the key map for this project".into());
    }
    let gterms: Vec<String> = ids.files.iter().chain(&ids.symbols).cloned().collect();
    if !gterms.is_empty() {
        let (g, gerr) = grep_root(get(e, "root"), &gterms);
        if let Some(err) = gerr {
            notes.push(err);
        }
        for (t, ls) in g {
            let found = if ls.is_empty() {
                "0 matches".to_owned()
            } else {
                ls.join("; ")
            };
            fact_lines.push(format!("- grep `{t}`: {found}"));
            fact = fact || !ls.is_empty();
        }
    }

    let lab = label(prior, fact, req.checked);
    let li = last_indexed
        .filter(truthy)
        .map_or_else(|| "n/a".into(), |v| py_str(&v));
    let none = || vec!["- none found".to_owned()];
    let mut parts = vec![
        format!("# Ruling pack: {key} @ {}", stamp(clock, false)),
        format!("Question: {}", trim(req.question, 240)),
        format!(
            "Type: {}. Repo: {}. Palaces: {}. Index: {} (last_indexed {li}).",
            req.qtype,
            show(get(e, "repo")),
            palaces.join(", "),
            index.clone().unwrap_or_else(|| show(get(e, "index")))
        ),
    ];
    parts.extend(["".into(), "## Prior rulings (memory)".into()]);
    parts.extend(if mem_lines.is_empty() {
        none()
    } else {
        mem_lines
    });
    parts.extend(["".into(), "## Facts (search + exact grep)".into()]);
    parts.extend(if fact_lines.is_empty() {
        none()
    } else {
        fact_lines
    });
    parts.extend(["".into(), "## Direct read-only check to run".into()]);
    parts.extend(
        suggest_checks(e, &ids.nums, req.qtype)
            .into_iter()
            .map(|c| format!("- {c}")),
    );
    if !notes.is_empty() {
        parts.extend([
            "".into(),
            "## Backend problems (counted as not found)".into(),
        ]);
        parts.extend(notes.iter().map(|n| format!("- {n}")));
        parts.push("Make these MCP calls instead:".into());
        let hints = mcp_hints(&palaces, req.question, index.as_deref());
        parts.extend(hints.into_iter().map(|h| format!("- {h}")));
    }
    let yes = |b: bool| if b { "yes" } else { "no" };
    let done = if req.checked {
        "yes"
    } else {
        "NO (run it, then rerun with --checked)"
    };
    parts.extend([
        String::new(),
        format!("Prior ruling: {}. Fact: {}. Direct check done: {done}.", yes(prior), yes(fact)),
        format!("Confidence: {lab}. Rule: HIGH = prior ruling + fact + direct check agree; MEDIUM = two of three; LOW = PM report only. Downgrade if sources disagree."),
        "Relay line: `Confidence: <label>. Checked: <items I read>. From PM report: <items>. Prior rulings: <ids or none found>.`".into(),
    ]);
    Ok((cap(parts.join("\n")), lab, key.to_owned()))
}

/// The script's cap: past [`MAX_CHARS`], the head, a marker and the last
/// 1400 characters (the label block survives).
fn cap(text: String) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX_CHARS {
        return text;
    }
    let keep: String = chars[chars.len() - 1400..].iter().collect();
    let head: String = chars[..MAX_CHARS - 1400 - 40].iter().collect();
    format!("{head}\n…(pack truncated)…\n{keep}")
}

/// `write_pack(text, key, packs=...)`: writes `<packs>/<utc>-<key>.md` (the
/// text and a newline), creating `packs`; returns the path as printed.
///
/// Why: a pack on disk outlives the session that built it.
/// What: the file name stamps the clock's instant, compact, UTC. A failure is
/// the `OSError` traceback line the script would end in.
/// Test: `write_pack_lands_under_the_given_root`, the `write` recording.
pub(crate) fn write_pack(
    text: &str,
    key: &str,
    clock: &dyn Clock,
    packs: &Path,
) -> Result<String, PyErr> {
    let fatal = |e: std::io::Error, p: &str| PyErr::from_line(&os_error(&e, p));
    let dir = packs.to_string_lossy().into_owned();
    std::fs::create_dir_all(packs).map_err(|e| fatal(e, &dir))?;
    let path = format!("{dir}/{}-{key}.md", stamp(clock, true));
    std::fs::write(&path, format!("{text}\n")).map_err(|e| fatal(e, &path))?;
    Ok(path)
}
