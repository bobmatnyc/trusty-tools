"""verdict_log.py — read, merge and write docs/test-audit/verdicts.jsonl (#11268).

Why: the owner asked that tests are not re-evaluated; a verdict, once given,
  must survive every re-run until the test's normalised source (attributes,
  signature, body) changes.
What: `merge(old_rows, fresh_rows, scope, rev, date)` reconciles the log:
  - a key outside `scope` (its file is under no scanned directory) is kept
    verbatim, so a pilot run never touches another crate's rows;
  - same key, same body_sha, reviewed (verdict not null): kept verbatim;
  - same key, same body_sha, unreviewed: the script-owned fields (rules,
    reason) are refreshed; the old row is kept byte-for-byte when nothing but
    audit_rev, date or evidence (`file:line`, which an edit elsewhere in the
    file moves) would differ;
  - same key, new body_sha: unreviewed -> the fresh row; reviewed -> the
    fresh row with verdict `rereview` and the old verdict and reason kept in
    `reason`, `action` carried over;
  - key gone from the tree: unreviewed -> dropped; reviewed -> verdict
    `rereview`, reason `test vanished; was …` (once, then kept verbatim);
  - a new key: the fresh row.
  Rows are written sorted by key, compact JSON, schema field order.
  `migrate_legacy_sha` runs first: it moves rows written when `body_sha`
  hashed the body alone onto the current hash, so an unchanged test keeps
  its verdict.
Test: scripts/test_audit/tests/test_verdict_log.py
"""
from __future__ import annotations

import json
from pathlib import Path

SCHEMA = ("key", "body_sha", "rules", "verdict", "reason", "evidence", "reviewer",
          "audit_rev", "date", "action")
REREVIEW = "rereview"
VANISHED = "test vanished; was "
CHANGED = "body_sha changed; was "
STAMP = ("audit_rev", "date", "evidence")  # evidence: file:line, moves with unrelated edits


class LogError(ValueError):
    """A verdict-log row that does not match the schema."""


def row(**fields: object) -> dict[str, object]:
    if set(fields) != set(SCHEMA):
        raise LogError(f"row fields {sorted(fields)} differ from the schema")
    return {k: fields[k] for k in SCHEMA}


def read_log(path: Path) -> list[dict[str, object]]:
    if not path.exists():
        return []
    rows: list[dict[str, object]] = []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        data = json.loads(line)
        if not isinstance(data, dict) or tuple(data) != SCHEMA:
            raise LogError(f"{path}:{n}: fields must be exactly {', '.join(SCHEMA)} in order")
        rows.append(data)
    keys = [r["key"] for r in rows]
    if len(keys) != len(set(keys)):
        raise LogError(f"{path}: duplicate key")
    return rows


def dumps(rows: list[dict[str, object]]) -> str:
    ordered = sorted(rows, key=lambda r: str(r["key"]))
    return "".join(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n"
                   for r in ordered)


def in_scope(key: str, scope: list[str]) -> bool:
    parts = key.split("::")
    return len(parts) > 2 and any(parts[1].startswith(s) for s in scope)


def _same_but_stamp(a: dict[str, object], b: dict[str, object]) -> bool:
    return all(a[k] == b[k] for k in SCHEMA if k not in STAMP)


def _was(old: dict[str, object]) -> str:
    return f"{old['verdict']}: {old['reason']}"


def migrate_legacy_sha(old_rows: list[dict[str, object]],
                       shas: dict[str, tuple[str, str]]) -> list[dict[str, object]]:
    """Rewrite a pre-PR0 body-only `body_sha` to the current source hash (#11268).

    `shas` maps key -> (body-only sha, attrs+sig+body sha) for the scanned tests.
    A row whose `body_sha` equals its test's body-only sha has an unchanged
    body, so it takes the new sha and keeps every other field; `merge` then
    sees it as unchanged. Any other row is returned as is. Idempotent: once
    rewritten, a row no longer matches a body-only sha.
    """
    out = []
    for r in old_rows:
        pair = shas.get(str(r["key"]))
        out.append({**r, "body_sha": pair[1]} if pair and r["body_sha"] == pair[0] else r)
    return out


def merge(old_rows: list[dict[str, object]], fresh_rows: list[dict[str, object]],
          scope: list[str], rev: str, date: str) -> list[dict[str, object]]:
    old = {str(r["key"]): r for r in old_rows}
    out: dict[str, dict[str, object]] = {}
    for fresh in fresh_rows:
        key = str(fresh["key"])
        prev = old.get(key)
        if prev is None:
            out[key] = fresh
        elif prev["body_sha"] == fresh["body_sha"]:
            if prev["verdict"] is not None:
                out[key] = prev
            else:
                out[key] = prev if _same_but_stamp(prev, fresh) else fresh
        elif prev["verdict"] is None:
            out[key] = fresh
        else:
            reason = prev["reason"] if prev["verdict"] == REREVIEW else CHANGED + _was(prev)
            out[key] = {**fresh, "verdict": REREVIEW, "reason": reason,
                        "action": prev["action"]}
    for key, prev in old.items():
        if key in out:
            continue
        if not in_scope(key, scope):
            out[key] = prev
        elif prev["verdict"] is None:
            continue
        elif prev["verdict"] == REREVIEW and str(prev["reason"]).startswith(VANISHED):
            out[key] = prev
        else:
            out[key] = {**prev, "verdict": REREVIEW, "reason": VANISHED + _was(prev),
                        "reviewer": "script", "audit_rev": rev, "date": date}
    return [out[k] for k in sorted(out)]


def write_log(path: Path, rows: list[dict[str, object]]) -> bool:
    """Write the log; return True when its bytes changed."""
    text = dumps(rows)
    if path.exists() and path.read_text(encoding="utf-8") == text:
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return True
