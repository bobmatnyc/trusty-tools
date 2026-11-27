//! The port of `il_timeline.py`: which task a project was on at a given time (#12028 P1d).
//!
//! Why: the Tasks view attributes a message to the task its project was
//! working on, read from the project's Intent Ledger log; the answer must
//! match the library's for every log.
//! What: [`Timeline::load`] sweeps each project's `log*.jsonl` files (sorted,
//! events sorted by time, file order within a time). A task id is active from
//! its `open` to its `done`; `open` and `update` touch it. A re-opened id is a
//! new incarnation keyed `(id, open time)`. After each event the current task
//! is the active, not parked, task touched last; ties go to a task named in
//! the session's `task`/`next` text, then the higher id. [`Timeline::at`] is
//! the current incarnation at a time. The active set is kept ordered, so a
//! sweep costs O(events log active) rather than the script's
//! O(events x active).
//! Known gap: `repo_datg_dirs` (the glob over checkouts) is not ported.
//! Test: `il_timeline_recordings_replay_clean_against_the_port`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::pyjson_load::{PyValue, loads};
use crate::pystr::is_word;
use crate::pystrptime::strptime;
use crate::pyval::{get, is_str, py_str, truthy};

use super::read_json;

const FORMATS: [&str; 3] = [
    "%Y-%m-%dT%H:%M:%S.%fZ",
    "%Y-%m-%dT%H:%M:%SZ",
    "%Y-%m-%dT%H:%MZ",
];

/// One incarnation of a task id.
#[derive(Debug, Clone, PartialEq)]
pub struct Incarnation {
    /// The task id.
    pub id: String,
    /// Its `open` time, Unix seconds.
    pub open: f64,
    /// Its `done` time, if any.
    pub close: Option<f64>,
    /// The state title, else the open event's note.
    pub title: String,
    title_repr: String,
}

impl Incarnation {
    /// `repr(title)`.
    pub fn title_repr(&self) -> &str {
        &self.title_repr
    }
}

/// An incarnation key: the id and the open time in microseconds.
type Key = (String, i64);

/// The loaded timelines of every project.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    inc: BTreeMap<Key, Incarnation>,
    newest: BTreeMap<String, Key>,
    pts: HashMap<String, (Vec<f64>, Vec<Option<Key>>)>,
}

fn secs(us: i64) -> f64 {
    us as f64 / 1e6
}

/// `[0-9a-km-np-z]`.
fn id_char(c: char) -> bool {
    c.is_ascii_digit() || (c.is_ascii_lowercase() && c != 'l' && c != 'o')
}

/// `[a-z]{2}-T` then `n` id characters at `c[i..]`.
fn id_shape(c: &[char], i: usize, n: usize) -> bool {
    c.len() >= i + 4 + n
        && c[i].is_ascii_lowercase()
        && c[i + 1].is_ascii_lowercase()
        && c[i + 2] == '-'
        && c[i + 3] == 'T'
        && c[i + 4..i + 4 + n].iter().all(|x| id_char(*x))
}

/// `TASK_ID.match(s)`: `^[a-z]{2}-T[0-9a-km-np-z]{2,3}$`.
fn task_id(s: &str) -> bool {
    let c: Vec<char> = s.strip_suffix('\n').unwrap_or(s).chars().collect();
    (c.len() == 6 || c.len() == 7) && id_shape(&c, 0, c.len() - 4)
}

/// `MENTION.findall(text)`.
fn mentions(text: &str) -> HashSet<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < c.len() {
        let start_ok = i == 0 || !is_word(c[i - 1]);
        let n = [3, 2].into_iter().find(|n| {
            start_ok && id_shape(&c, i, *n) && c.get(i + 4 + n).is_none_or(|x| !is_word(*x))
        });
        match n {
            Some(n) => {
                out.insert(c[i..i + 4 + n].iter().collect());
                i += 4 + n;
            }
            None => i += 1,
        }
    }
    out
}

/// One event: time (microseconds), id, `ev`, note.
type Event = (i64, String, PyValue, PyValue);

/// `glob(<datg>/log*.jsonl)`, sorted.
fn log_files(datg: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(datg) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("log") && n.ends_with(".jsonl") && n.len() >= 9)
        .collect();
    names.sort();
    names.into_iter().map(|n| datg.join(n)).collect()
}

/// `_events(datg)`; `Err` is the class of an exception the script does not catch.
fn events(datg: &Path) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    for p in log_files(datg) {
        let Ok(bytes) = std::fs::read(&p) else {
            continue; // OSError: the script skips the file
        };
        let text = String::from_utf8_lossy(&bytes)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        for line in text.split_inclusive('\n') {
            if !line.contains("\"ev\"") || !line.contains("-T") {
                continue;
            }
            let Ok(d) = loads(line) else {
                continue;
            };
            if !matches!(d, PyValue::Dict(_)) {
                return Err("AttributeError".into());
            }
            let t = match get(&d, "t") {
                Some(PyValue::Str(s)) => {
                    FORMATS.iter().find_map(|f| strptime(s, f)).map(|(l, _)| l)
                }
                _ => None,
            };
            if let (Some(t), Some(PyValue::Str(id))) = (t, get(&d, "id"))
                && task_id(id)
            {
                let ev = get(&d, "ev").cloned().unwrap_or(PyValue::Null);
                let note = get(&d, "note").filter(|v| truthy(v)).cloned();
                out.push((
                    t,
                    id.clone(),
                    ev,
                    note.unwrap_or(PyValue::Str(String::new())),
                ));
            }
        }
    }
    out.sort_by_key(|e| e.0); // stable: file order within one time
    Ok(out)
}

impl Timeline {
    /// Why: `load(datg_dirs)`, one timeline per project.
    /// What: see the module doc; projects load in name order.
    /// Test: `il_timeline_recordings_replay_clean_against_the_port`.
    ///
    /// # Errors
    /// The class name of an exception the script does not catch (a
    /// `state.json` or log line that is not an object, an unhashable id).
    pub fn load(dirs: &BTreeMap<String, PathBuf>) -> Result<Self, String> {
        let mut tl = Self::default();
        for (project, datg) in dirs {
            tl.add(project, datg)?;
        }
        Ok(tl)
    }

    fn add(&mut self, project: &str, datg: &Path) -> Result<(), String> {
        let st = read_json(&datg.join("state.json")).unwrap_or(PyValue::Dict(Vec::new()));
        if !matches!(st, PyValue::Dict(_)) {
            return Err("AttributeError".into());
        }
        let mut tasks: HashMap<String, &PyValue> = HashMap::new();
        match get(&st, "tasks").filter(|v| truthy(v)) {
            None | Some(PyValue::Dict(_) | PyValue::Str(_)) => {}
            Some(PyValue::List(list)) => {
                for t in list.iter().filter(|t| matches!(t, PyValue::Dict(_))) {
                    match get(t, "id").filter(|v| truthy(v)) {
                        Some(PyValue::Str(id)) => {
                            tasks.insert(id.clone(), t);
                        }
                        Some(PyValue::List(_) | PyValue::Dict(_)) => return Err("TypeError".into()),
                        _ => {}
                    }
                }
            }
            Some(_) => return Err("TypeError".into()),
        }
        let empty = PyValue::Dict(Vec::new());
        let ses = get(&st, "session")
            .filter(|s| matches!(s, PyValue::Dict(_)))
            .unwrap_or(&empty);
        let part = |k: &str| {
            get(ses, k)
                .filter(|v| truthy(v))
                .map_or(String::new(), py_str)
        };
        let named = mentions(&format!("{} {}", part("task"), part("next")));
        let parked = |id: &str| {
            tasks
                .get(id)
                .is_some_and(|t| is_str(get(t, "status"), "parked"))
        };
        let mut active: HashMap<String, (i64, Key)> = HashMap::new();
        let mut order: BTreeSet<(i64, bool, String)> = BTreeSet::new();
        let (mut ts_l, mut best_l) = (Vec::new(), Vec::new());
        for (t, tid, ev, note) in events(datg)? {
            let is = |s: &str| is_str(Some(&ev), s);
            let touched = if is("open") {
                if !active.contains_key(&tid) {
                    let key = (tid.clone(), t);
                    let title = tasks
                        .get(&tid)
                        .and_then(|x| get(x, "title"))
                        .filter(|v| truthy(v))
                        .unwrap_or(&note);
                    self.inc.insert(
                        key.clone(),
                        Incarnation {
                            id: tid.clone(),
                            open: secs(t),
                            close: None,
                            title: py_str(title),
                            title_repr: crate::pyval::repr(title),
                        },
                    );
                    self.newest.insert(tid.clone(), key.clone());
                    active.insert(tid.clone(), (t, key));
                }
                true
            } else if is("update") && active.contains_key(&tid) {
                true
            } else if is("done") && active.contains_key(&tid) {
                if let Some((last, key)) = active.remove(&tid) {
                    order.remove(&(last, named.contains(&tid), tid.clone()));
                    if let Some(i) = self.inc.get_mut(&key) {
                        i.close = Some(secs(t));
                    }
                }
                false
            } else {
                continue;
            };
            if touched && let Some(entry) = active.get_mut(&tid) {
                order.remove(&(entry.0, named.contains(&tid), tid.clone()));
                entry.0 = t;
                if !parked(&tid) {
                    order.insert((t, named.contains(&tid), tid.clone()));
                }
            }
            ts_l.push(secs(t));
            best_l.push(
                order
                    .last()
                    .and_then(|(_, _, id)| active.get(id))
                    .map(|a| a.1.clone()),
            );
        }
        self.pts.insert(project.to_owned(), (ts_l, best_l));
        Ok(())
    }

    /// Why: `at(project, ts)`: the incarnation current at `ts`.
    /// What: the last sweep point at or before `ts` (`bisect_right`), or
    /// `None` before the first event or for an unknown project.
    /// Test: `il_timeline_recordings_replay_clean_against_the_port`.
    pub fn at(&self, project: &str, ts: f64) -> Option<(&str, f64)> {
        let (times, best) = self.pts.get(project)?;
        let (mut lo, mut hi) = (0, times.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if ts < times[mid] {
                hi = mid
            } else {
                lo = mid + 1
            }
        }
        let key = best.get(lo.checked_sub(1)?)?.as_ref()?;
        Some((key.0.as_str(), secs(key.1)))
    }

    /// Every incarnation, sorted by `(id, open)`.
    pub fn incarnations(&self) -> impl Iterator<Item = &Incarnation> {
        self.inc.values()
    }

    /// `newest`: each id's latest incarnation, sorted by id.
    pub fn newest(&self) -> impl Iterator<Item = (&str, f64)> {
        self.newest.iter().map(|(id, k)| (id.as_str(), secs(k.1)))
    }
}
