//! The `brief_ids.py` port: durable brief ids and their registry (#12028 P1e).
//!
//! Why: a brief is cited for months, so its id (`MB` + four hex digits) must
//! never be reused; the registry `local/briefs/registry.jsonl` is append-only
//! and a brief file carries its id in a marker line and its H1.
//! What: the library the `brief-id` command calls. [`allocate`] is one past
//! the highest id in the registry or the dashboard media store (legacy `B`
//! ids share the number space); [`assign`] stamps a file (marker after any
//! YAML frontmatter, `MBxxxx: ` before the first H1 outside a code fence),
//! keeping an id the file or the registry already gives it, rewrites the
//! file in place and appends one `json.dumps` row; [`check`] lists every
//! disagreement between the files and the registry. `Err` is the last line
//! of the traceback the script ends with.
//! Test: the `brief-id` recordings in `tests/replay_writers.rs`.

pub mod cli;
mod text;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub use text::{marker_id, stamp, title_of};

use crate::Clock;
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{py_repr, strip};
use crate::pyval::{dumps, py_str, repr, type_name};
use crate::writers::{os_error, read_text};

/// The highest brief number plus one; ids never wrap.
const SPACE: u32 = 1 << 16;

/// The checkout the module's paths hang off (`REPO`).
#[derive(Debug, Clone)]
pub struct Paths {
    /// `REPO`, absolute and normalised.
    pub repo: PathBuf,
}

impl Paths {
    /// `BRIEFS`: `<repo>/local/briefs`.
    pub fn briefs(&self) -> PathBuf {
        self.repo.join("local/briefs")
    }
    /// `REGISTRY`: `<repo>/local/briefs/registry.jsonl`.
    pub fn registry(&self) -> PathBuf {
        self.repo.join("local/briefs/registry.jsonl")
    }
    /// `MEDIA_DIR`: `<repo>/local/owner-queue/media`.
    pub fn media_dir(&self) -> PathBuf {
        self.repo.join("local/owner-queue/media")
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `re.match(r"^M?B[0-9a-f]{4}$", v)` (`$` also before one final `\n`);
/// `media` drops the optional `M` (`ID_RE`).
pub(crate) fn id_like(v: &str, media: bool) -> bool {
    let v = v.strip_suffix('\n').unwrap_or(v);
    let v = if media {
        v
    } else {
        v.strip_prefix('M').unwrap_or(v)
    };
    v.strip_prefix('B').is_some_and(|h| {
        h.len() == 4
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// `number(bid)`: `int(bid[-4:], 16)` of an id that passed [`id_like`].
pub(crate) fn number(bid: &str) -> u32 {
    let tail: String = bid
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    u32::from_str_radix(strip(&tail), 16).unwrap_or(0)
}

/// `r[key]` on a registry row, or the exception the script raises.
pub(crate) fn field<'a>(r: &'a PyValue, key: &str) -> Result<&'a PyValue, String> {
    match r {
        PyValue::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .ok_or_else(|| format!("KeyError: {}", py_repr(key))),
        PyValue::List(_) => {
            Err("TypeError: list indices must be integers or slices, not str".into())
        }
        PyValue::Str(_) => Err("TypeError: string indices must be integers, not 'str'".into()),
        other => Err(format!(
            "TypeError: '{}' object is not subscriptable",
            type_name(other)
        )),
    }
}

/// `r["id"]` as the `str` a regex match needs.
pub(crate) fn id_str(r: &PyValue) -> Result<&str, String> {
    match field(r, "id")? {
        PyValue::Str(v) => Ok(v),
        other => Err(format!(
            "TypeError: expected string or bytes-like object, got '{}'",
            type_name(other)
        )),
    }
}

/// `read_registry(registry)`: every non-blank line's JSON value.
pub(crate) fn read_registry(registry: &Path) -> Result<Vec<PyValue>, String> {
    if !registry.is_file() {
        return Ok(Vec::new());
    }
    let text = read_text(registry).map_err(|e| e.last_line(&s(registry)))?;
    let mut rows = Vec::new();
    for line in text.split_inclusive('\n') {
        if strip(line).is_empty() {
            continue;
        }
        let v = loads(line).map_err(|e| format!("json.decoder.JSONDecodeError: {}", e.py_str()))?;
        rows.push(v);
    }
    Ok(rows)
}

/// `media_ids(media_dir)`: the numbers of the `Bxxxx.json` media documents.
fn media_ids(media_dir: &Path) -> Result<HashSet<u32>, String> {
    if !media_dir.is_dir() {
        return Ok(HashSet::new());
    }
    let list = std::fs::read_dir(media_dir).map_err(|e| os_error(&e, &s(media_dir)))?;
    let mut out = HashSet::new();
    for entry in list {
        let entry = entry.map_err(|e| os_error(&e, &s(media_dir)))?;
        let n = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = n.strip_suffix(".json")
            && id_like(stem, true)
        {
            out.insert(number(&stem.chars().take(5).collect::<String>()));
        }
    }
    Ok(out)
}

/// `allocate()`: the next id, `MB` + four lowercase hex digits.
///
/// Why: ids are never reused, so the next one is past every id ever used.
/// What: the highest number in the registry (`B` or `MB` ids) or the media
/// store, plus one; `MB0000` when none; an error past `MBffff`.
/// Test: the `brief-id/next-*` recordings.
pub fn allocate(paths: &Paths) -> Result<String, String> {
    let mut used = media_ids(&paths.media_dir())?;
    for r in read_registry(&paths.registry())? {
        let id = id_str(&r)?;
        if id_like(id, false) {
            used.insert(number(id));
        }
    }
    let n = used.iter().max().map_or(0, |m| m + 1);
    if n >= SPACE {
        return Err("RuntimeError: brief ids exhausted (MBffff reached); ids never wrap".into());
    }
    Ok(format!("MB{n:04x}"))
}

/// `os.path.abspath(path)`: joined to the working directory and
/// normalised lexically (`.`, `..`, repeated `/`), never resolved.
pub(crate) fn abspath(path: &str, cwd: &Path) -> String {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", s(cwd))
    };
    let lead = if joined.starts_with("//") && !joined.starts_with("///") {
        "//"
    } else {
        "/"
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("{lead}{}", parts.join("/"))
}

/// `_rel(path)`: the path relative to `REPO` when it lies under it, else absolute.
pub(crate) fn rel(path: &str, paths: &Paths, cwd: &Path) -> String {
    let ap = abspath(path, cwd);
    let repo = s(&paths.repo);
    match ap.strip_prefix(&format!("{repo}/")) {
        Some(r) if !r.is_empty() => r.to_owned(),
        Some(_) => ".".to_owned(),
        None => ap,
    }
}

/// `datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")`.
fn now_text(clock: &dyn Clock) -> String {
    let t = clock.now_unix().floor() as i64;
    let (days, sec) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, m, d) = crate::classify::alerts::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sec / 3600,
        sec / 60 % 60,
        sec % 60
    )
}

/// `_append(registry, row)`: one `json.dumps` line appended, directory made first.
fn append(registry: &Path, row: &[(&str, &str)]) -> Result<(), String> {
    if let Some(dir) = registry.parent() {
        std::fs::create_dir_all(dir).map_err(|e| os_error(&e, &s(dir)))?;
    }
    let v = PyValue::Dict(
        row.iter()
            .map(|(k, v)| ((*k).to_owned(), PyValue::Str((*v).to_owned())))
            .collect(),
    );
    let line = format!("{}\n", dumps(&v, None, false));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(registry)
        .map_err(|e| os_error(&e, &s(registry)))?;
    f.write_all(line.as_bytes())
        .map_err(|e| os_error(&e, &s(registry)))
}

/// `assign(path, project)`: `(id, created)`.
///
/// Why: a brief keeps its id for life; a rewritten file whose path the
/// registry lists gets that id back rather than a new one.
/// What: a file that already has a marker keeps it (a registry row is
/// appended only when the id is missing there; the file is not rewritten);
/// else the last registry id for the file's path is stamped back; else a new
/// id is allocated, stamped, and appended with the H1 title, the time on
/// `clock` and `project`. The file is rewritten in place before the append.
/// Test: the `brief-id/assign-*` recordings.
pub fn assign(
    path: &str,
    project: &str,
    paths: &Paths,
    cwd: &Path,
    clock: &dyn Clock,
) -> Result<(String, bool), String> {
    let file = cwd.join(path);
    let text = read_text(&file).map_err(|e| e.last_line(path))?;
    let have = marker_id(&text);
    let relp = rel(path, paths, cwd);
    let rows = read_registry(&paths.registry())?;
    let reg = paths.registry();
    if let Some(have) = have {
        let mut known = false;
        for r in &rows {
            if matches!(field(r, "id")?, PyValue::Str(v) if *v == have) {
                known = true;
                break;
            }
        }
        if !known {
            let title = title_of(&text, path);
            let row = [
                ("id", have.as_str()),
                ("path", &relp),
                ("title", &title),
                ("created", &now_text(clock)),
                ("project", project),
            ];
            append(&reg, &row)?;
        }
        return Ok((have, false));
    }
    let mut known = None;
    for r in &rows {
        if matches!(field(r, "path")?, PyValue::Str(p) if *p == relp) {
            let id = id_str(r)?;
            if id_like(id, false) {
                known = Some(id.to_owned());
            }
        }
    }
    if let Some(id) = known {
        std::fs::write(&file, stamp(&text, &id, path)).map_err(|e| os_error(&e, path))?;
        return Ok((id, false));
    }
    let bid = allocate(paths)?;
    let new = stamp(&text, &bid, path);
    std::fs::write(&file, &new).map_err(|e| os_error(&e, path))?;
    let title = title_of(&new, path);
    let row = [
        ("id", bid.as_str()),
        ("path", &relp),
        ("title", &title),
        ("created", &now_text(clock)),
        ("project", project),
    ];
    append(&reg, &row)?;
    Ok((bid, true))
}

/// `local_briefs()`: the `*.md` files in `local/briefs`, sorted.
fn local_briefs(briefs: &Path) -> Result<Vec<PathBuf>, String> {
    let list = std::fs::read_dir(briefs).map_err(|e| os_error(&e, &s(briefs)))?;
    let mut out = Vec::new();
    for entry in list {
        let p = entry.map_err(|e| os_error(&e, &s(briefs)))?.path();
        if p.to_string_lossy().ends_with(".md") && p.is_file() {
            out.push(p);
        }
    }
    out.sort_by_key(|p| s(p));
    Ok(out)
}

/// `check()`: the problem lines; empty when files and registry agree.
///
/// Why: `brief-id check` is the guard a skill runs before citing briefs.
/// What: duplicate registry ids and paths with two ids first, then per
/// file (sorted): no marker, a duplicate marker id, an id the registry lacks
/// or lists at another path, and an H1 without the `MBxxxx: ` prefix.
/// Test: the `brief-id/check-*` recordings.
pub fn check(paths: &Paths, cwd: &Path) -> Result<Vec<String>, String> {
    let rows = read_registry(&paths.registry())?;
    let mut probs = Vec::new();
    let mut by_id: HashMap<String, &PyValue> = HashMap::new();
    let mut seen: HashMap<String, &PyValue> = HashMap::new();
    for r in &rows {
        let (id, p) = (field(r, "id")?, field(r, "path")?);
        if by_id.contains_key(&repr(id)) {
            probs.push(format!("duplicate registry id {}", py_str(id)));
        }
        by_id.insert(repr(id), r);
        if let Some(prev) = seen.get(&repr(p))
            && repr(prev) != repr(id)
        {
            probs.push(format!(
                "path {} has two ids {} and {}",
                py_str(p),
                py_str(prev),
                py_str(id)
            ));
        }
        seen.insert(repr(p), id);
    }
    let mut ids_in_files: HashMap<String, String> = HashMap::new();
    for p in local_briefs(&paths.briefs())? {
        let text = read_text(&p).map_err(|e| e.last_line(&s(&p)))?;
        let (bid, relp) = (marker_id(&text), rel(&s(&p), paths, cwd));
        let Some(bid) = bid else {
            probs.push(format!("no brief id: {relp}"));
            continue;
        };
        if let Some(prev) = ids_in_files.get(&bid) {
            probs.push(format!("duplicate id {bid}: {prev} and {relp}"));
        }
        ids_in_files.insert(bid.clone(), relp.clone());
        match by_id.get(&py_repr(&bid)) {
            None => probs.push(format!("{bid} ({relp}) not in registry")),
            Some(r) => {
                let rp = field(r, "path")?;
                if !matches!(rp, PyValue::Str(x) if *x == relp) {
                    probs.push(format!("{bid} registry path {} != file {relp}", py_str(rp)));
                }
            }
        }
        if !text::h1_has_prefix(&text, &format!("# {bid}: ")) {
            probs.push(format!("{bid} ({relp}) H1 does not start with '{bid}: '"));
        }
    }
    Ok(probs)
}
