//! The ten invariant checks of `spec_drift_check.py` (`inv01`..`inv10`).
//!
//! Why: each check is one SPEC-ARCHITECT-COMPONENTS invariant; the port
//! keeps the script's order of reads and key lookups, so the first exception
//! a malformed manifest raises is the one the script reports (#12028).
//! What: [`CHECKS`] maps the manifest and the root to a [`Status`] and a
//! message, or a [`Raise`]. [`resolve`] is the script's entry-to-path rule.
//! Test: `tests/replay_spec_drift_check.rs` (every recorded case).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::py::{
    Raise, get, is_file, key, list, list_repr, md_tree, number, py_repr_value, py_str, read,
    squash_space, text, truthy, visible,
};
use super::{INDEX_PATH, Options, SPEC, Status};
use crate::dashboard::schema_check::pattern::Pattern;
use crate::dashboard::store::{Format, ListQuery, Store};
use crate::pystr::{is_digit, is_word, py_repr, splitlines, strip};

/// One check's verdict.
pub(super) type Verdict = Result<(Status, String), Raise>;
/// One check: root, manifest, options.
pub(super) type Check = fn(&Path, &Value, &Options) -> Verdict;
/// `CHECKS`, in `ALL_INVS` order.
pub(super) const CHECKS: [Check; 10] = [
    inv01, inv02, inv03, inv04, inv05, inv06, inv07, inv08, inv09, inv10,
];
const KINDS: [&str; 2] = ["generic", "rig"];

fn ok(status: Status, msg: impl Into<String>) -> Verdict {
    Ok((status, msg.into()))
}

/// `candidates(entry)`.
fn candidates(e: &Value) -> Result<Vec<String>, Raise> {
    if truthy(get(e, "path")?) {
        return Ok(vec![text(key(e, "path")?)?.to_owned()]);
    }
    let n = py_str(key(e, "name")?);
    Ok(vec![
        format!(".claude/skills/{n}/SKILL.md"),
        format!(".claude/skills/architect/references/{n}.md"),
    ])
}

/// `resolve(root, entry)`: the first candidate that is a file.
pub(super) fn resolve(root: &Path, e: &Value) -> Result<Option<String>, Raise> {
    Ok(candidates(e)?.into_iter().find(|r| is_file(&root.join(r))))
}

/// `e.get("component") in range(1, 9)` and not a `bool`.
fn component_ok(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Number(n)) => n
            .as_f64()
            .is_some_and(|f| f.fract() == 0.0 && (1.0..=8.0).contains(&f)),
        _ => false,
    }
}

fn inv01(_root: &Path, m: &Value, _o: &Options) -> Verdict {
    let files = list(key(m, "files")?)?;
    let (mut bad, mut seen) = (Vec::new(), BTreeSet::new());
    for e in files {
        let name = get(e, "name")?;
        let n = name.map_or_else(|| "?".to_owned(), py_str);
        if !seen.insert(name.map_or_else(|| py_repr("?"), py_repr_value)) {
            bad.push(format!("{n}: duplicate"));
        }
        let comp = get(e, "component")?;
        if !component_ok(comp) {
            bad.push(format!(
                "{n}: component {}",
                comp.map_or("None".into(), py_repr_value)
            ));
        }
        let kind = get(e, "kind")?;
        match kind.and_then(Value::as_str) {
            Some(k) if KINDS.contains(&k) => {
                if comp.and_then(number) == Some(8.0) && k != "rig" {
                    bad.push(format!("{n}: component 8 must be rig"));
                }
            }
            _ => bad.push(format!(
                "{n}: kind {}",
                kind.map_or("None".into(), py_repr_value)
            )),
        }
    }
    if !bad.is_empty() {
        return ok(Status::Fail, bad.join("; "));
    }
    ok(Status::Pass, format!("{} entries tagged", files.len()))
}

fn inv02(root: &Path, m: &Value, _o: &Options) -> Verdict {
    let files = list(key(m, "files")?)?;
    let (mut missing, mut waiting) = (Vec::new(), Vec::new());
    for e in files {
        if resolve(root, e)?.is_none() {
            let phase = truthy(get(e, "phase")?);
            let n = py_str(key(e, "name")?);
            if phase {
                waiting.push(n)
            } else {
                missing.push(n)
            }
        }
    }
    if !missing.is_empty() {
        return ok(
            Status::Fail,
            format!("{} missing: {}", missing.len(), missing.join(", ")),
        );
    }
    if !waiting.is_empty() {
        let names = waiting.join(", ");
        let msg = format!("{} entries wait for their phase: {names}", waiting.len());
        return ok(Status::Pending, msg);
    }
    ok(Status::Pass, format!("{} entries on disk", files.len()))
}

/// `set(v)` for a manifest list, as the names it can hold.
fn name_set(v: &Value, into: &mut BTreeSet<String>) -> Result<(), Raise> {
    match v {
        Value::Array(a) => into.extend(a.iter().filter_map(Value::as_str).map(str::to_owned)),
        Value::String(s) => into.extend(s.chars().map(String::from)),
        Value::Object(o) => into.extend(o.keys().cloned()),
        other => {
            return Err(Raise::Crash(format!(
                "TypeError: set() of {}",
                py_repr_value(other)
            )));
        }
    }
    Ok(())
}

/// `_disk_names(root, m)`.
fn disk_names(root: &Path, m: &Value) -> Result<BTreeSet<String>, Raise> {
    let mut skip = BTreeSet::from(["architect".to_owned()]);
    for k in ["outside_scope", "retiring"] {
        if let Some(v) = get(m, k)? {
            name_set(v, &mut skip)?;
        }
    }
    let skills = root.join(".claude/skills");
    let mut found: BTreeSet<String> = visible(&skills, "", "", true)
        .into_iter()
        .filter(|n| skills.join(n).join("SKILL.md").symlink_metadata().is_ok())
        .filter(|n| !skip.contains(n))
        .collect();
    let refs = root.join(".claude/skills/architect/references");
    for f in visible(&refs, "", ".md", false) {
        found.insert(f[..f.len() - 3].to_owned());
    }
    Ok(found)
}

fn inv03(root: &Path, m: &Value, _o: &Options) -> Verdict {
    let files = list(key(m, "files")?)?;
    let mut known = BTreeSet::new();
    for e in files {
        if !truthy(get(e, "path")?)
            && let Some(n) = key(e, "name")?.as_str()
        {
            known.insert(n.to_owned());
        }
    }
    let mut orphans: Vec<String> = disk_names(root, m)?.difference(&known).cloned().collect();
    let mut known_paths = BTreeSet::new();
    for e in files {
        if truthy(get(e, "path")?)
            && let Some(p) = key(e, "path")?.as_str()
        {
            known_paths.insert(p.to_owned());
        }
    }
    let mut globbed: Vec<String> = visible(&root.join("docs/architect"), "codex-", ".md", false)
        .into_iter()
        .map(|n| format!("docs/architect/{n}"))
        .collect();
    if root.join(INDEX_PATH).symlink_metadata().is_ok() {
        globbed.push(INDEX_PATH.to_owned());
    }
    orphans.extend(globbed.into_iter().filter(|r| !known_paths.contains(r)));
    if !orphans.is_empty() {
        let msg = format!(
            "{} not in the manifest: {}",
            orphans.len(),
            orphans.join(", ")
        );
        return ok(Status::Fail, msg);
    }
    ok(Status::Pass, "no orphans")
}

/// `lhs > v` for a budget value.
fn gt(lhs: usize, v: &Value) -> Result<bool, Raise> {
    let rhs = number(v)
        .ok_or_else(|| Raise::Crash(format!("TypeError: '>' against {}", py_repr_value(v))))?;
    Ok(lhs as f64 > rhs)
}

fn inv04(root: &Path, m: &Value, _o: &Options) -> Verdict {
    let text = read(&root.join("AGENTS.md"))?;
    let (lines, size) = (splitlines(&text).len(), text.len());
    let b = key(m, "agents_md_budget")?;
    let (bl, bb) = (key(b, "lines")?, key(b, "bytes")?);
    let info = format!(
        "AGENTS.md {lines} lines, {size} bytes (budget {} lines, {} bytes)",
        py_str(bl),
        py_str(bb)
    );
    if !is_file(&root.join(INDEX_PATH)) {
        return ok(
            Status::Pending,
            format!("{info}; enforced from phase 1 (skill index not present)"),
        );
    }
    if gt(lines, bl)? || gt(size, bb)? {
        return ok(Status::Fail, info);
    }
    ok(Status::Pass, info)
}

fn inv05(root: &Path, m: &Value, _o: &Options) -> Verdict {
    let mut terms = Vec::new();
    for t in list(key(m, "rig_terms")?)? {
        // See #12028: a pattern outside the supported subset stops the run.
        terms.push(
            Pattern::compile_python(text(t)?)
                .map_err(|e| Raise::Crash(format!("re.error: {e}")))?,
        );
    }
    let (mut fail, mut todo_files, mut todo_hits, mut scrubbed) = (Vec::new(), 0, 0, 0);
    for e in list(key(m, "files")?)? {
        if key(e, "kind")?.as_str() != Some("generic") {
            continue;
        }
        let Some(rel) = resolve(root, e)? else {
            continue;
        };
        let body = read(&root.join(&rel))?;
        let hits = splitlines(&body)
            .iter()
            .filter(|l| terms.iter().any(|t| t.search(l)))
            .count();
        if truthy(get(e, "scrubbed")?) {
            scrubbed += 1;
            if hits > 0 {
                fail.push(format!("{}: {hits}", py_str(key(e, "name")?)));
            }
        } else if hits > 0 {
            todo_files += 1;
            todo_hits += hits;
        }
    }
    if !fail.is_empty() {
        return ok(
            Status::Fail,
            format!("rig terms in scrubbed files: {}", fail.join(", ")),
        );
    }
    if todo_files > 0 {
        let msg = format!(
            "{todo_hits} matching lines in {todo_files} unscrubbed generic files; {scrubbed} scrubbed and clean"
        );
        return ok(Status::Pending, msg);
    }
    ok(
        Status::Pass,
        format!("{scrubbed} scrubbed generic files clean"),
    )
}

/// `_instruction_files(root, m)`, sorted.
fn instruction_files(root: &Path) -> BTreeSet<String> {
    let mut rels: Vec<String> = ["AGENTS.md", "ARCHITECT.md", "CLAUDE.md"]
        .map(String::from)
        .into();
    md_tree(root, ".claude/skills", &mut rels);
    rels.extend(
        visible(&root.join("docs/architect"), "", ".md", false)
            .into_iter()
            .map(|n| format!("docs/architect/{n}")),
    );
    rels.into_iter()
        .filter(|r| is_file(&root.join(r)))
        .collect()
}

fn inv06(root: &Path, m: &Value, _o: &Options) -> Verdict {
    let mut texts = BTreeMap::new();
    for r in instruction_files(root) {
        let t = squash_space(&read(&root.join(&r))?);
        texts.insert(r, t);
    }
    let mut by_name = BTreeMap::new();
    for e in list(key(m, "files")?)? {
        let name = py_repr_value(key(e, "name")?);
        by_name.insert(name, resolve(root, e)?);
    }
    let sources = list(key(m, "single_source")?)?;
    let (mut fail, mut warn, mut good_count) = (Vec::new(), Vec::new(), 0);
    for s in sources {
        let sent = squash_space(text(key(s, "sentence")?)?);
        let place: Vec<(&String, usize)> = texts
            .iter()
            .filter(|(_, t)| t.contains(&sent))
            .map(|(r, t)| (r, t.matches(sent.as_str()).count()))
            .collect();
        let home_v = key(s, "home")?;
        let (home_key, home_shown) = match by_name.get(&py_repr_value(home_v)) {
            Some(Some(rel)) => (Some(rel.clone()), rel.clone()),
            Some(None) => (None, "None".to_owned()),
            None => (home_v.as_str().map(str::to_owned), py_str(home_v)),
        };
        let total: usize = place.iter().map(|(_, c)| c).sum();
        if total == 1 && place.iter().any(|(r, _)| Some(*r) == home_key.as_ref()) {
            good_count += 1;
            continue;
        }
        let shown = if place.is_empty() {
            "no file".to_owned()
        } else {
            let parts: Vec<String> = place
                .iter()
                .map(|(r, c)| format!("{}: {c}", py_repr(r)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        };
        let msg = format!("{} in {shown} (home {home_shown})", py_str(key(s, "id")?));
        let enforce = get(s, "enforce")?.is_none_or(|v| truthy(Some(v)));
        if enforce {
            fail.push(msg)
        } else {
            warn.push(msg)
        }
    }
    if !fail.is_empty() {
        return ok(Status::Fail, fail.join("; "));
    }
    if !warn.is_empty() {
        let msg = format!(
            "{good_count} of {} single-source; not yet single: {}",
            sources.len(),
            warn.join("; ")
        );
        return ok(Status::Warn, msg);
    }
    ok(
        Status::Pass,
        format!("{good_count} key sentences each in exactly one file"),
    )
}

/// `re.findall(r"instance-[a-z0-9-]+", s)`.
fn instance_names(s: &str) -> BTreeSet<String> {
    let in_class = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(i) = rest.find("instance-") {
        let tail = &rest[i + 9..];
        let len = tail.find(|c| !in_class(c)).unwrap_or(tail.len());
        if len > 0 {
            out.insert(rest[i..i + 9 + len].to_owned());
        }
        rest = &rest[i + 9 + len..];
    }
    out
}

fn inv07(root: &Path, _m: &Value, _o: &Options) -> Verdict {
    if !is_file(&root.join(INDEX_PATH)) {
        return ok(Status::Pending, "skill index not present (phase 1)");
    }
    let mut named = instance_names(&read(&root.join(INDEX_PATH))?);
    named.remove("instance-index");
    if !named.is_empty() {
        let names: Vec<String> = named.into_iter().collect();
        return ok(
            Status::Fail,
            format!("index names rig files: {}", names.join(", ")),
        );
    }
    ok(Status::Pass, "index names no rig file")
}

/// `set(re.findall(r"INV-\d\d", s))`.
fn inv_ids(s: &str) -> BTreeSet<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i + 6 <= chars.len() {
        if chars[i..i + 4] == ['I', 'N', 'V', '-']
            && is_digit(chars[i + 4])
            && is_digit(chars[i + 5])
        {
            out.insert(chars[i..i + 6].iter().collect());
            i += 6;
        } else {
            i += 1;
        }
    }
    out
}

/// `re.search(rf"^### C{n}\b", s, re.M)`.
fn has_section(s: &str, n: u32) -> bool {
    let head = format!("### C{n}");
    std::iter::once(s)
        .chain(s.match_indices('\n').map(|(i, _)| &s[i + 1..]))
        .any(|line| {
            line.strip_prefix(&head)
                .is_some_and(|after| !after.chars().next().is_some_and(is_word))
        })
}

fn inv08(root: &Path, _m: &Value, _o: &Options) -> Verdict {
    if !is_file(&root.join(SPEC)) {
        return ok(Status::Fail, format!("{SPEC} missing"));
    }
    let text = read(&root.join(SPEC))?;
    let in_spec = inv_ids(&text);
    let all: BTreeSet<String> = (1..=10).map(|n| format!("INV-{n:02}")).collect();
    let missing_c: Vec<String> = (1..=8)
        .filter(|n| !has_section(&text, *n))
        .map(|n| n.to_string())
        .collect();
    let mut problems = Vec::new();
    if in_spec != all {
        let spec_only: Vec<&str> = in_spec.difference(&all).map(String::as_str).collect();
        let script_only: Vec<&str> = all.difference(&in_spec).map(String::as_str).collect();
        problems.push(format!(
            "invariant ids differ: spec-only {}, script-only {}",
            list_repr(&spec_only),
            list_repr(&script_only)
        ));
    }
    if !missing_c.is_empty() {
        problems.push(format!("no section for C{}", missing_c.join(", C")));
    }
    if !problems.is_empty() {
        return ok(Status::Fail, problems.join("; "));
    }
    ok(Status::Pass, "10 invariants and 8 component sections match")
}

fn inv09(root: &Path, _m: &Value, _o: &Options) -> Verdict {
    let entry = serde_json::json!({"name": "architect-drift-guard"});
    let guard =
        resolve(root, &entry)?.unwrap_or_else(|| "architect-drift-guard (not found)".into());
    let mut missing = Vec::new();
    for p in [SPEC, guard.as_str()] {
        if !is_file(&root.join(p)) || !read(&root.join(p))?.contains("spec_drift_check.py") {
            missing.push(p);
        }
    }
    if !missing.is_empty() {
        return ok(
            Status::Fail,
            format!("no spec_drift_check.py call in: {}", missing.join(", ")),
        );
    }
    ok(Status::Pass, "spec and drift-guard skill both call it")
}

/// `dashboard_store.default_format()`; a bad value is the store's `StoreError`.
fn store_format(o: &Options) -> Result<Format, Raise> {
    match o.dashboard_format.as_deref() {
        None | Some("") | Some("dir") => Ok(Format::Dir),
        Some("jsonl") => Ok(Format::Jsonl),
        Some(v) => Err(Raise::Crash(format!(
            "StoreError: DASHBOARD_FORMAT={}: use dir or jsonl",
            py_repr(v)
        ))),
    }
}

fn inv10(root: &Path, m: &Value, o: &Options) -> Verdict {
    let store = Store::new(root.join("local/owner-queue"), store_format(o)?);
    let docs = store
        .list("items", &ListQuery::default())
        .map_err(|e| Raise::Crash(format!("StoreError: {e}")))?;
    if docs.is_empty() {
        return ok(
            Status::Pending,
            "no dashboard items found; enforced when session_scope_enforced is true",
        );
    }
    let unscoped = docs
        .iter()
        .filter(|d| match d.data.get("session") {
            Some(Value::String(s)) => strip(s).is_empty(),
            other => !truthy(other),
        })
        .count();
    let info = format!(
        "{unscoped} of {} dashboard items have no session scope",
        docs.len()
    );
    if !truthy(get(m, "session_scope_enforced")?) {
        return ok(Status::Pending, format!("{info}; enforced after backfill"));
    }
    ok(
        if unscoped > 0 {
            Status::Fail
        } else {
            Status::Pass
        },
        info,
    )
}
