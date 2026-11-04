//! `Bundle::move_concept` (SPEC-OKF-01 §2.3, §2.10 "Moves", §2.11 link
//! rules).
//!
//! Why: a concept ID is its path, so a move changes the ID. Every link to the
//! old ID must follow it in the same operation, and no link may be rewritten
//! that the move cannot resolve inside the bundle.
//! What: refuses, with no file changed, a missing source (`NotFound`), an
//! existing target (`target_exists`), a target that folds to another file's
//! key (`fold_collision`), a target that already has a verification record
//! (`verification_record_exists`), and a rewrite that `put` would refuse
//! for a fold twin. Then it renames the file and its record, rewrites each
//! markdown link that resolves lexically to the old ID into the
//! bundle-absolute `/<new-id>.md` (keeping a `#fragment`), and the same for a
//! `supersedes`/`superseded_by` value. The moved concept's own relative
//! links that would change target are pinned to bundle-absolute links. Each
//! rewrite is a content change through `put` (actor
//! `process:trusty-okf-move`), so it clears that concept's verification.
//! URLs are never touched. A link that climbs above the root, or sits in a
//! foreign, unparseable or unpatchable file, stays unchanged and is reported.
//! `log.md` gains exactly one entry for the move.
//! Residual risk until slice S5's operation journal (§4.11): a crash after
//! the rename leaves links to the old ID.
//! Test: `c09_move_rewrites_every_link_and_logs_one_entry`,
//! `c55_move_leaves_a_climbing_link_and_reports_it`,
//! `c115_bundle_open_reads_foreign_files_and_writes_never_touch_them`,
//! `c140_fold_collision_on_put_and_move`.

use std::ops::Range;

use chrono::{DateTime, FixedOffset};
use serde_yaml::Value;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::log_md::append_entry;
use crate::bundle::paths::{confined_file, fold_conflict, io_err, restrict_path};
use crate::bundle::put::{PutOutcome, conflict};
use crate::bundle::record::record_rel;
use crate::bundle::walk::scan;
use crate::format::concept::{Concept, parse_concept};
use crate::format::fields::Actor;
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::validate::Finding;
use crate::format::write::WriteMode;

/// A link `move_concept` left unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmovedLink {
    /// Bundle-relative path of the file holding the link.
    pub path: String,
    /// The link target as written.
    pub target: String,
    /// `climbs_above_root`, `foreign_file`, `unparseable` or `unpatchable`.
    pub reason: String,
}

/// What `move_concept` did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MoveReport {
    /// Concepts whose links were rewritten, the moved concept included when
    /// its own links were pinned.
    pub rewritten: Vec<ConceptId>,
    /// Verification events the rewrites moved to history (§2.10 "Moves").
    pub verifications_cleared: usize,
    /// Links left unchanged, with the reason.
    pub unmoved: Vec<UnmovedLink>,
    /// Findings of the rewrite writes.
    pub findings: Vec<Finding>,
}

/// Where a link target leads, resolved lexically.
#[derive(Debug, PartialEq)]
enum Resolved {
    /// A concept, plus the `#fragment` (with `#`) or "".
    Concept(ConceptId, String),
    /// A relative link whose `..` climbs above the root.
    Climbs,
    /// A URL, an anchor, a non-`.md` path or anything else.
    Other,
}

fn dir_of(id: &ConceptId) -> &str {
    id.as_str().rsplit_once('/').map_or("", |(d, _)| d)
}

/// Resolve `target` written in a file whose directory is `base`.
fn resolve(target: &str, base: &str) -> Resolved {
    let scheme = target.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    if target.is_empty() || target.starts_with('#') || scheme || target.contains('?') {
        return Resolved::Other;
    }
    let (path, frag) = match target.find('#') {
        Some(i) => (&target[..i], &target[i..]),
        None => (target, ""),
    };
    let Some(stem) = path.strip_suffix(".md") else {
        return Resolved::Other;
    };
    let mut segs: Vec<&str> = Vec::new();
    let (start, rel) = match stem.strip_prefix('/') {
        Some(abs) => ("", abs),
        None => (base, stem),
    };
    for seg in start.split('/').chain(rel.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                if segs.pop().is_none() {
                    return Resolved::Climbs;
                }
            }
            s => segs.push(s),
        }
    }
    match ConceptId::parse(&segs.join("/")) {
        Ok(id) => Resolved::Concept(id, frag.to_string()),
        Err(_) => Resolved::Other,
    }
}

/// Byte ranges of every markdown link target in `text`: inline `](target)`
/// outside code spans and fences, and reference definitions `[x]: target`.
fn link_spans(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let base = line_start;
        line_start += line.len();
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if t.starts_with('[')
            && let Some(close) = t.find("]:")
        {
            let after = &t[close + 2..];
            let lead = after.len() - after.trim_start().len();
            let target_len = after
                .trim_start()
                .find(char::is_whitespace)
                .unwrap_or(after.trim_start().len());
            let at = base + (line.len() - t.len()) + close + 2 + lead;
            if target_len > 0 {
                out.push(at..at + target_len);
            }
            continue;
        }
        let bytes = line.as_bytes();
        let mut in_code = false;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'`' => in_code = !in_code,
                b']' if !in_code && bytes.get(i + 1) == Some(&b'(') => {
                    let s = i + 2;
                    let (s, end_char) = if bytes.get(s) == Some(&b'<') {
                        (s + 1, b'>')
                    } else {
                        (s, b')')
                    };
                    let mut e = s;
                    while e < bytes.len()
                        && bytes[e] != end_char
                        && (end_char == b'>' || !bytes[e].is_ascii_whitespace())
                    {
                        e += 1;
                    }
                    if e > s {
                        out.push(base + s..base + e);
                    }
                    i = e;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
    out
}

/// Replace ranges in `text` (sorted, disjoint) with the paired strings.
fn splice(text: &str, edits: &[(Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (r, s) in edits {
        out.push_str(&text[pos..r.start]);
        out.push_str(s);
        pos = r.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// The planned rewrite of one file's body, plus links it must report.
struct BodyPlan {
    edits: Vec<(Range<usize>, String)>,
    climbing: Vec<String>,
    to_old: Vec<String>,
}

/// Plan a body rewrite. `moved` is `Some(new_dir)` for the moved concept
/// itself, whose relative links are pinned when the new directory would
/// change their target.
fn plan_body(
    body: &str,
    base: &str,
    from: &ConceptId,
    to: &ConceptId,
    moved: Option<&str>,
) -> BodyPlan {
    let mut plan = BodyPlan {
        edits: Vec::new(),
        climbing: Vec::new(),
        to_old: Vec::new(),
    };
    for r in link_spans(body) {
        let target = &body[r.clone()];
        match resolve(target, base) {
            Resolved::Climbs => plan.climbing.push(target.to_string()),
            Resolved::Concept(id, frag) if &id == from => {
                plan.to_old.push(target.to_string());
                plan.edits.push((r, format!("/{}{frag}", to.file_name())));
            }
            Resolved::Concept(id, frag) => {
                if let Some(new_dir) = moved
                    && resolve(target, new_dir) != Resolved::Concept(id.clone(), frag.clone())
                {
                    plan.edits.push((r, format!("/{}{frag}", id.file_name())));
                }
            }
            Resolved::Other => {}
        }
    }
    plan
}

impl Bundle {
    /// Move a concept to a new ID and rewrite every link to it (§2.3).
    ///
    /// Why/What: see the module doc.
    /// Test: `c09_move_rewrites_every_link_and_logs_one_entry`,
    /// `c55_move_leaves_a_climbing_link_and_reports_it`,
    /// `c140_fold_collision_on_put_and_move`,
    /// `a3_every_bundle_write_path_leaves_files_0600`.
    pub fn move_concept(
        &mut self,
        from: &ConceptId,
        to: &ConceptId,
        now: DateTime<FixedOffset>,
    ) -> Result<MoveReport, OkfError> {
        let from_path = confined_file(&self.root, &from.file_name())?;
        let to_path = confined_file(&self.root, &to.file_name())?;
        if !from_path.is_file() {
            return Err(OkfError::NotFound(from.clone()));
        }
        // The fold check runs first: on a case-insensitive file system a
        // fold twin also makes `exists()` true, and the answer must not
        // depend on the file system (§2.11).
        if fold_conflict(&self.root, &to.file_name())?.is_some() {
            return Err(conflict(to, "fold_collision"));
        }
        if from == to || to_path.exists() {
            return Err(conflict(to, "target_exists"));
        }
        let to_record = confined_file(&self.root, &record_rel(to))?;
        if to_record.exists() {
            return Err(conflict(to, "verification_record_exists"));
        }
        let from_record = confined_file(&self.root, &record_rel(from))?;
        let mut report = MoveReport::default();
        let mut rewrites: Vec<Concept> = Vec::new();
        let mut moved_text = String::new();
        let unmoved = |path: &str, target: &str, reason: &str| UnmovedLink {
            path: path.to_string(),
            target: target.to_string(),
            reason: reason.to_string(),
        };
        for entry in scan(&self.root)?.files {
            let Ok(text) = std::fs::read_to_string(self.root.join(&entry.rel)) else {
                continue;
            };
            let id = match &entry.r {
                ConceptRef::Id(id) => id.clone(),
                ConceptRef::Foreign(_) => {
                    let base = entry.rel.rsplit_once('/').map_or("", |(d, _)| d);
                    for t in plan_body(&text, base, from, to, None).to_old {
                        report.unmoved.push(unmoved(&entry.rel, &t, "foreign_file"));
                    }
                    continue;
                }
            };
            let is_moved = &id == from;
            let Ok((mut c, _)) = parse_concept(id.clone(), &text) else {
                for t in plan_body(&text, dir_of(&id), from, to, None).to_old {
                    report.unmoved.push(unmoved(&entry.rel, &t, "unparseable"));
                }
                continue;
            };
            if is_moved {
                moved_text = text.clone();
            }
            let plan = plan_body(&c.body, dir_of(&id), from, to, is_moved.then(|| dir_of(to)));
            for t in &plan.climbing {
                report
                    .unmoved
                    .push(unmoved(&entry.rel, t, "climbs_above_root"));
            }
            let mut changed = !plan.edits.is_empty();
            c.body = splice(&c.body, &plan.edits);
            for key in ["supersedes", "superseded_by"] {
                let points_here =
                    c.get(key).and_then(Value::as_str) == Some(&format!("/{}", from.file_name()));
                if points_here {
                    c.set(key, Value::String(format!("/{}", to.file_name())));
                    changed = true;
                }
            }
            if !changed {
                continue;
            }
            if !c.raw().text.is_empty() && !c.raw().patchable {
                for t in plan.to_old {
                    report.unmoved.push(unmoved(&entry.rel, &t, "unpatchable"));
                }
                continue;
            }
            if !is_moved && fold_conflict(&self.root, &id.file_name())?.is_some() {
                return Err(conflict(&id, "fold_collision"));
            }
            rewrites.push(c);
        }
        // Execute: the file and its record move first, then the rewrites.
        if let Some(dir) = to_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        std::fs::rename(&from_path, &to_path).map_err(|e| io_err(&from_path, e))?;
        // #10746: a file moved to a new bundle path ends up 0600 like any write.
        restrict_path(&to_path)?;
        if from_record.exists() {
            if let Some(dir) = to_record.parent() {
                std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
            }
            std::fs::rename(&from_record, &to_record).map_err(|e| io_err(&from_record, e))?;
            restrict_path(&to_record)?;
        }
        let actor = Actor::Process("trusty-okf-move".to_string());
        for c in rewrites {
            let c = if matches!(&c.id, ConceptRef::Id(id) if id == from) {
                // Re-read under the new ID: same bytes, so the stale check holds.
                let (mut moved, _) = parse_concept(to.clone(), &moved_text)?;
                for key in ["supersedes", "superseded_by"] {
                    if let Some(v) = c.get(key).cloned() {
                        moved.set(key, v);
                    }
                }
                moved.body = c.body.clone();
                moved
            } else {
                c
            };
            // #10746: a link rewrite carries no human confirmation (§2.4).
            let (outcome, findings, id) =
                self.put_inner(c, &actor, false, WriteMode::Lenient, &now, true)?;
            report.findings.extend(findings);
            if let PutOutcome::Updated {
                verifications_cleared,
                ..
            } = outcome
            {
                report.verifications_cleared += verifications_cleared;
            }
            report.rewritten.push(id);
        }
        report.rewritten.sort();
        append_entry(
            &self.root,
            &now,
            &format!(
                "**Moved** `/{}` to `/{}`; links rewritten in {} concept(s), {} verification \
                 event(s) moved to history",
                from.file_name(),
                to.file_name(),
                report.rewritten.len(),
                report.verifications_cleared
            ),
        )?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ConceptId {
        ConceptId::parse(s).unwrap()
    }

    /// Why: the link rules decide what a move may rewrite (§2.11).
    /// What: relative, absolute, fragment, climbing, URL and code-span
    /// links resolve as expected.
    /// Test: self-contained.
    #[test]
    fn links_resolve_lexically() {
        assert_eq!(
            resolve("../b/x.md#h", "a"),
            Resolved::Concept(id("b/x"), "#h".into())
        );
        assert_eq!(
            resolve("/b/x.md", "a/c"),
            Resolved::Concept(id("b/x"), String::new())
        );
        assert_eq!(resolve("../../x.md", "a"), Resolved::Climbs);
        assert_eq!(resolve("https://e.com/x.md", ""), Resolved::Other);
        assert_eq!(resolve("x.txt", ""), Resolved::Other);
        let text = "a [x](y.md) `[c](z.md)` [d](<w.md>)\n```\n[e](v.md)\n```\n[r]: q.md\n";
        let spans: Vec<&str> = link_spans(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(spans, vec!["y.md", "w.md", "q.md"]);
    }
}
