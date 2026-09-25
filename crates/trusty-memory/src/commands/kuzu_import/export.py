# #277: read-only export bridge for a kuzu-memory store.
#
# Run by `trusty-memory import kuzu` with the interpreter kuzu-memory itself
# uses, as `python export.py <memories.db> <out.json>`. Opens the database with
# `read_only=True`, so it never migrates, indexes or backs up the store the way
# `kuzu-memory memory export` does on open. Memory rows use the column list of
# kuzu-memory's `export_memories_to_json`, intersected with the columns the
# store actually has (older stores lack some). Writes one JSON document to
# <out.json>; stdout is never used, so log noise cannot corrupt the payload.
import json
import sys
from datetime import datetime

import kuzu

MEMORY_COLUMNS = [
    "id", "content", "content_hash", "created_at", "accessed_at",
    "access_count", "memory_type", "knowledge_type", "importance",
    "confidence", "source_type", "source_speaker", "project_tag",
    "agent_id", "user_id", "session_id", "metadata",
]


def main(db_path, out_path):
    conn = kuzu.Connection(kuzu.Database(db_path, read_only=True))

    def rows(query):
        result = conn.execute(query)
        names = result.get_column_names()
        out = []
        while result.has_next():
            out.append({k: ser(v) for k, v in zip(names, result.get_next())})
        return out

    tables = {r["name"] for r in rows("CALL SHOW_TABLES() RETURN name")}

    def columns(table):
        if table not in tables:
            return []
        return [r["name"] for r in rows(f"CALL TABLE_INFO('{table}') RETURN name")]

    def select(pattern, alias, table, wanted, prefix=""):
        have = set(columns(table))
        picked = [c for c in wanted if c in have]
        if not picked:
            return []
        ret = ", ".join(f"{alias}.{c} AS {prefix}{c}" for c in picked)
        return rows(f"MATCH {pattern} RETURN {ret}")

    mem_cols = [c for c in MEMORY_COLUMNS if c in set(columns("Memory"))]
    memories = select("(m:Memory)", "m", "Memory", MEMORY_COLUMNS)
    entities = select("(e:Entity)", "e", "Entity", ["id", "name", "entity_type"])
    mentions = []
    if "MENTIONS" in tables:
        # #277 L3: older stores may lack the column, like RELATES_TO.strength.
        conf = "r.confidence" if "confidence" in set(columns("MENTIONS")) else "NULL"
        mentions = rows(
            "MATCH (m:Memory)-[r:MENTIONS]->(e:Entity) "
            f"RETURN m.id AS memory_id, e.id AS entity_id, {conf} AS confidence"
        )
    relates = []
    if "RELATES_TO" in tables:
        rel_cols = set(columns("RELATES_TO"))
        kind = "r.relationship_type" if "relationship_type" in rel_cols else "NULL"
        strength = "r.strength" if "strength" in rel_cols else "NULL"
        relates = rows(
            "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) "
            f"RETURN a.id AS from_id, b.id AS to_id, {kind} AS relationship_type, "
            f"{strength} AS strength"
        )
    # #277 LOW-2: rows in relationship tables the import does not map are
    # counted, so the report can say what was left behind. A failure here is
    # reported by exception class and never stops the export.
    other_edges = {}
    other_edges_error = None
    try:
        for t in rows("CALL SHOW_TABLES() RETURN name, type"):
            name = t["name"]
            if str(t["type"]).upper().startswith("REL") and name not in ("MENTIONS", "RELATES_TO"):
                n = rows(f"MATCH ()-[r:`{name}`]->() RETURN count(r) AS n")
                other_edges[name] = n[0]["n"] if n else 0
    except Exception as exc:  # noqa: BLE001 - reported, not swallowed
        other_edges_error = type(exc).__name__
    doc = {
        "format": "trusty-kuzu-export/1",
        "schema_version": "1.0",
        "exported_at": datetime.now().isoformat(),
        "memory_columns": mem_cols,
        "memory_count": len(memories),
        "memories": memories,
        "entities": entities,
        "mentions": mentions,
        "relates_to": relates,
        "other_edges": other_edges,
        "other_edges_error": other_edges_error,
    }
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, default=str)


def ser(value):
    return value.isoformat() if hasattr(value, "isoformat") else value


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: export.py <memories.db> <out.json>")
    main(sys.argv[1], sys.argv[2])
