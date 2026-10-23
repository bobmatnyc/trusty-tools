#!/usr/bin/env python3
"""dashboard_schema.py: JSON Schema validation for dashboard documents (stdlib only).

Why: the owner (2026-10-11 04:12Z) wants every dashboard data structure deterministic and described by a schema.
The schemas in schemas/dashboard/ are plain draft 2020-12 files so the Rust trusty-architect crate can load
them unchanged; the host Python may lack `jsonschema`, so this module carries a small validator for exactly
the keywords those files use.
What: schema_for(coll, doc_id) -> schema dict; validate(coll, doc_id, data) -> list of error strings
"<coll>/<id>: <json pointer>: <message>" (empty = valid); validate_store(docs_by_coll) adds the cross-document
checks (goal ids exist, step_progress items exist). Keywords: type (string or list), enum, const, required,
properties, additionalProperties (bool or schema), items, pattern, minimum, maximum, minLength, oneOf, $ref
into the file's own $defs. bool is never an integer or number. An unknown keyword raises SchemaError at load,
so a schema edit cannot silently weaken a check. The code invariants "goal lifecycle parked <=> parked is an
object" and "factory goal has a board" are checked here, not in JSON Schema. Mode helpers: mode() reads DASHBOARD_SCHEMA (off|warn|enforce).
Test: scripts/test_dashboard_schema.py
"""
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
SCHEMA_DIR = os.path.join(REPO, "schemas", "dashboard")

COLLECTIONS = ("items", "sessions", "goals", "goal_reorders", "goal_removals", "step_progress", "accounts",
               "effort_tunes", "il_steers", "briefs", "questions", "media", "meta")
META_IDS = ("state", "settings", "seen", "gh_track")
MODES = ("off", "warn", "enforce")

KNOWN_KEYWORDS = {"$schema", "$id", "$ref", "$defs", "title", "description", "type", "enum", "const", "required",
                  "properties", "additionalProperties", "items", "pattern", "minimum", "maximum", "minLength",
                  "oneOf"}

_cache = {}


class SchemaError(Exception):
    """A schema file uses a keyword this validator does not implement, or a $ref it cannot resolve."""


def mode(env=None):
    """Validation mode from DASHBOARD_SCHEMA; None when unset (the caller picks its default)."""
    value = (env if env is not None else os.environ).get("DASHBOARD_SCHEMA")
    if value is None or value == "":
        return None
    if value not in MODES:
        raise SchemaError(f"DASHBOARD_SCHEMA={value!r}: use one of {', '.join(MODES)}")
    return value


def _check_keywords(node, where):
    if isinstance(node, dict):
        for key, sub in node.items():
            if key in ("properties", "$defs"):
                for name, child in sub.items():
                    _check_keywords(child, f"{where}/{key}/{name}")
                continue
            if key not in KNOWN_KEYWORDS:
                raise SchemaError(f"{where}: unsupported keyword {key!r}")
            if key in ("additionalProperties", "items") and isinstance(sub, dict):
                _check_keywords(sub, f"{where}/{key}")
            elif key == "oneOf":
                for i, child in enumerate(sub):
                    _check_keywords(child, f"{where}/oneOf/{i}")


def load_schema(name, schema_dir=None):
    """Load schemas/dashboard/<name>.schema.json (cached per directory)."""
    d = schema_dir or SCHEMA_DIR
    key = (d, name)
    if key not in _cache:
        path = os.path.join(d, f"{name}.schema.json")
        try:
            with open(path, encoding="utf-8") as f:
                schema = json.load(f)
        except (OSError, ValueError) as e:
            raise SchemaError(f"cannot load schema {path}: {e}")
        _check_keywords(schema, name)
        _cache[key] = schema
    return _cache[key]


def schema_name(coll, doc_id=None):
    if coll == "meta":
        return f"meta-{doc_id}" if doc_id in META_IDS else None
    return coll if coll in COLLECTIONS else None


def schema_for(coll, doc_id=None, schema_dir=None):
    name = schema_name(coll, doc_id)
    return load_schema(name, schema_dir) if name else None


# ---- validator ---------------------------------------------------------------
def _type_ok(value, t):
    if t == "null":
        return value is None
    if t == "boolean":
        return isinstance(value, bool)
    if t == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if t == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if t == "string":
        return isinstance(value, str)
    if t == "array":
        return isinstance(value, list)
    if t == "object":
        return isinstance(value, dict)
    raise SchemaError(f"unsupported type {t!r}")


def _resolve(root, ref):
    if not ref.startswith("#/"):
        raise SchemaError(f"unsupported $ref {ref!r}")
    node = root
    for part in ref[2:].split("/"):
        if not isinstance(node, dict) or part not in node:
            raise SchemaError(f"unresolved $ref {ref!r}")
        node = node[part]
    return node


def _name(value):
    return "boolean" if isinstance(value, bool) else type(value).__name__


def _walk(root, schema, value, ptr, errs):
    if "$ref" in schema:
        return _walk(root, _resolve(root, schema["$ref"]), value, ptr, errs)
    where = ptr or "/"
    if "type" in schema:
        types = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(_type_ok(value, t) for t in types):
            errs.append(f"{where}: expected {'|'.join(types)}, got {_name(value)}")
            return
    if "enum" in schema and not any(value == e and type(value) is type(e) for e in schema["enum"]):
        errs.append(f"{where}: {json.dumps(value, ensure_ascii=False)[:60]} is not one of {json.dumps(schema['enum'])}")
    if "const" in schema and not (value == schema["const"] and type(value) is type(schema["const"])):
        errs.append(f"{where}: must be {json.dumps(schema['const'])}")
    if "oneOf" in schema:
        hits = 0
        for sub in schema["oneOf"]:
            trial = []
            _walk(root, sub, value, ptr, trial)
            hits += not trial
        if hits != 1:
            errs.append(f"{where}: matches {hits} of the {len(schema['oneOf'])} allowed shapes (need exactly 1)")
    if isinstance(value, str):
        if "pattern" in schema and not re.search(schema["pattern"], value):
            errs.append(f"{where}: {value[:60]!r} does not match {schema['pattern']}")
        if "minLength" in schema and len(value) < schema["minLength"]:
            errs.append(f"{where}: shorter than {schema['minLength']}")
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema and value < schema["minimum"]:
            errs.append(f"{where}: {value} is below {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            errs.append(f"{where}: {value} is above {schema['maximum']}")
    if isinstance(value, list) and "items" in schema:
        for i, v in enumerate(value):
            _walk(root, schema["items"], v, f"{ptr}/{i}", errs)
    if isinstance(value, dict):
        for req in schema.get("required", []):
            if req not in value:
                errs.append(f"{ptr}/{req}: required property is missing")
        props = schema.get("properties", {})
        extra = schema.get("additionalProperties", True)
        for key in sorted(value):
            sub = props.get(key)
            child = f"{ptr}/{key}"
            if sub is not None:
                _walk(root, sub, value[key], child, errs)
            elif extra is False:
                errs.append(f"{child}: property is not allowed")
            elif isinstance(extra, dict):
                _walk(root, extra, value[key], child, errs)


def check_schema(schema, data):
    """Errors ('<pointer>: <message>') of data against schema; [] when valid."""
    errs = []
    _walk(schema, schema, data, "", errs)
    return errs


def _invariants(coll, data):
    errs = []
    if coll == "goals" and isinstance(data, dict):
        life, pk = data.get("lifecycle"), data.get("parked")
        if life == "parked" and not isinstance(pk, dict):
            errs.append("/lifecycle: 'parked' needs a parked object")
        if data.get("kind") == "factory" and not isinstance(data.get("board"), dict):
            errs.append("/board: required when kind is 'factory'")
        if life != "parked" and isinstance(pk, dict) and life in ("active",):
            errs.append("/parked: set while lifecycle is 'active' (park writes lifecycle 'parked')")
    return errs


def validate(coll, doc_id, data, schema_dir=None):
    """Errors for one document body as '<coll>/<id>: <pointer>: <message>'; [] when valid."""
    schema = schema_for(coll, doc_id, schema_dir)
    if schema is None:
        return [f"{coll}/{doc_id}: no schema for this collection" + (" or meta id" if coll == "meta" else "")]
    errs = check_schema(schema, data) + _invariants(coll, data)
    return [f"{coll}/{doc_id}: {e}" for e in errs]


def validate_store(docs_by_coll, schema_dir=None):
    """Validate every document, then cross-check references. docs_by_coll: {coll: [{id, data}, ...]}."""
    errs = []
    for coll in sorted(docs_by_coll):
        for d in docs_by_coll[coll]:
            errs.extend(validate(coll, d["id"], d["data"], schema_dir))
    goals = {d["id"] for d in docs_by_coll.get("goals", [])}
    items = {d["id"] for d in docs_by_coll.get("items", [])}
    if "goals" in docs_by_coll:
        for d in docs_by_coll.get("items", []):
            g = d["data"].get("goal")
            if isinstance(g, str) and g not in goals:
                errs.append(f"items/{d['id']}: /goal: {g} is not a goal id in the store")
        for d in docs_by_coll.get("goal_removals", []):
            if d["data"].get("goal") not in goals:
                errs.append(f"goal_removals/{d['id']}: /goal: {d['data'].get('goal')} is not a goal id in the store")
    if "items" in docs_by_coll:
        for d in docs_by_coll.get("step_progress", []):
            if d["data"].get("item") not in items:
                errs.append(f"step_progress/{d['id']}: /item: {d['data'].get('item')} is not an item id in the store")
    return errs
