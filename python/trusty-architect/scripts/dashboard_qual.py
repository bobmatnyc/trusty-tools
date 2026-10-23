"""dashboard_qual.py: fleet-wide qualified ids "<S id>:<local id>" (the owner 2026-10-20, ar-T44).

Why: two stores (Architect on 7890, tm-apex on 7891) both hold bare D01-style ids, so a bare id can name
different entries. A reference shown next to another session's entries must say which session owns it.
What: the qualifier is the OWNING STORE's session, not the item's `session` field (which names the project an item
concerns): the Architect store is S14, a session store is its session's S id (meta/state.session_id; tm-apex = S0c).
store_sid(store); qualify(sid, id) -> "S14:Ddb"; split(ref); resolve(store, coll, ref) -> local id, refusing a qualified
ref with another store's S id (hint names the owner). Derived, never stored, never a rename (r-038).
Test: scripts/test_dashboard_qualified.py
"""
import os
import re

import dashboard_store as ds

QUAL_RE = re.compile(r"^(S[0-9a-f]{2}):([A-Za-z0-9_.-]+)$")


def split(ref):
    m = QUAL_RE.match(ref or "")
    return (m.group(1), m.group(2)) if m else (None, ref)


ARCHITECT_SID = "S14"   # tm-architect: the Architect store (local/owner-queue) is always this session's


def store_sid(store):
    """S id of the session that owns this store. meta/state.session_id wins (a session store sets it); the default
    Architect root is S14; anything else is unknown (None) and qualifies nothing."""
    try:
        sid = (store.get("meta", "state")["data"] or {}).get("session_id")
        if isinstance(sid, str) and QUAL_RE.match(sid + ":x"):
            return sid
    except ds.StoreError:
        pass
    if os.path.realpath(store.root) == os.path.realpath(ds.DEFAULT_ROOT):
        return ARCHITECT_SID
    return None


def qualify(sid, local_id):
    """"<S id>:<id>" for the owning store's session; the bare id when the store's session is unknown."""
    return f"{sid}:{local_id}" if sid else local_id


def resolve(store, coll, ref):
    """Local id for a bare or qualified ref. A qualified ref must carry this store's own S id, else NotFound with a hint."""
    sid, local = split(ref)
    if sid is None:
        return local
    own = store_sid(store)
    if own is not None and sid != own:
        raise ds.NotFound(f"{coll}/{ref} not found: this store belongs to {own}; use {own}:{local}")
    return local


def with_qualifier(store, coll, docs):
    """Copy of get/list results with a derived `qualified` key on each doc of the items collection."""
    if coll != "items":
        return docs
    sid = store_sid(store)
    one = isinstance(docs, dict)
    out = [dict(d, qualified=qualify(sid, d["id"])) for d in ([docs] if one else docs)]
    return out[0] if one else out
