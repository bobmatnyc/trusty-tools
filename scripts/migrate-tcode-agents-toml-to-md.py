#!/usr/bin/env python3
"""migrate-tcode-agents-toml-to-md.py — convert tcode agent configs from the
retired TOML format to the Markdown+frontmatter format (#2897 Slice D).

Why: trusty-code's TOML agent loader (`AgentConfig::from_toml_str` /
`AgentConfig::load`) was retired in #2897 Slice D — `.claude/agents/*.toml`
is no longer read at all. A project with pre-existing `.toml` agents gets a
one-time WARN-level log naming this script (see
`crates/trusty-code/src/agents/mod.rs`'s `discover_agents` doc comment) but
the actual migration has to happen out-of-band, since the harness itself no
longer understands the old format. This script does that conversion.

What: reads one or more tcode agent `.toml` files and emits the equivalent
`.md`+frontmatter document tcode's Markdown loader
(`agents::md_loader::load_md_agent`) understands. Field mapping:

    [agent]
    name         -> frontmatter `name:`
    role         -> frontmatter `role:`
    model        -> frontmatter `model:`        (wins over [llm].model_override,
                                                   matching AgentConfig::agent.model's
                                                   higher precedence in resolve_model)
    description  -> frontmatter `description:`

    [llm]
    max_tokens      -> frontmatter `max_tokens:`
    model_override  -> frontmatter `model:` (ONLY if [agent].model is absent)
    temperature     -> DROPPED (no consumer reads AgentConfig.llm.temperature
                                 anywhere in trusty-code; see md_loader's own
                                 docs for the same conclusion)

    [system_prompt]
    content         -> the Markdown body (after the closing `---` fence)
    append_skills   -> frontmatter `skills:` (only emitted when non-empty —
                                                the shared frontmatter grammar
                                                has a `skills:` field even
                                                though tcode itself has no
                                                consumer for it yet; see
                                                md_loader::project_to_agent_config's
                                                doc comment)

    [tools]
    allowed         -> frontmatter `tools:` (direct list map; an explicit
                                               empty list `tools: []` is
                                               preserved as deny-all, not
                                               dropped, since `Some([])` and
                                               `None` are NOT the same thing)

    [runner]        -> DROPPED entirely (dead config: nothing in trusty-code
                                          ever reads AgentConfig.runner; the
                                          in-process runner is the only
                                          runtime backend)

Frontmatter reader contract (why `yaml_scalar` never backslash-escapes):
trusty-agents-common's frontmatter reader is NOT a full YAML parser. For
every field this script emits (`name`, `role`, `description`, `model`, and
each element of `tools:`/`skills:`), it only ever strips AT MOST ONE
balanced leading/trailing quote-character pair
(`agents::frontmatter::strip_one_quote_pair`, used by both `parse_kv_line`
and `parse_list_value`) and does nothing else to the value. It does NOT
backslash-unescape — that unescaping (`unescape_yaml_double_quoted`, in
`agents/builder.rs`) is applied to exactly one field, `initial_prompt`,
which this script never emits (tcode's `AgentConfig` has no
`initial_prompt` concept). So `yaml_scalar` must emit a plain,
un-escaped `"`..`"` wrap: since the reader always removes EXACTLY the
first and last byte of a quoted value and nothing more, wrapping ANY
string verbatim in one fresh quote pair round-trips correctly no matter
what characters it contains — including embedded quotes, backslashes, or
a value that itself starts/ends with a quote character. Backslash-escaping
inner quotes (as a naive YAML emitter would) is WRONG here: the reader
would leave the literal backslashes in the loaded value instead of
resolving them.

Verification (this script has no automated test harness, so this is a
manually-reproducible check rather than a pinned regression test): a TOML
fixture with `description = "\"Featured\""` (i.e. a decoded value that
itself both starts AND ends with a literal `"`) was converted, then loaded
through the REAL production parser
(`trusty_agents_common::agents::metadata::agent_metadata_from_str`, the
exact function `agents::md_loader::load_md_agent` calls) via a throwaway
`cargo run` harness depending on `trusty-agents-common` by path. The
loaded `description` was confirmed to equal the original `"Featured"`
byte-for-byte, with no stray backslashes — re-run this check after editing
`yaml_scalar`/`_needs_quoting`.

Usage:
    # Convert a single file, writing <name>.md next to <name>.toml
    python3 scripts/migrate-tcode-agents-toml-to-md.py .claude/agents/engineer.toml

    # Convert every *.toml in a directory
    python3 scripts/migrate-tcode-agents-toml-to-md.py .claude/agents/

    # Write elsewhere / preview without writing
    python3 scripts/migrate-tcode-agents-toml-to-md.py engineer.toml -o /tmp/engineer.md
    python3 scripts/migrate-tcode-agents-toml-to-md.py engineer.toml --dry-run

Requires Python 3.11+ (stdlib `tomllib`, no external dependencies).
"""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path

_LEADING_SPECIAL = set("-?:,[]{}#&*!|>'\"%@`")


def _needs_quoting(value: str) -> bool:
    """Whether `value` is unsafe to emit as a bare YAML scalar.

    Why: agent names/models/descriptions are plain identifiers or slugs
    (`python-engineer`, `openai/gpt-4o-mini`) that read far more naturally
    unquoted, matching every hand-authored `.md` fixture in the repo — a
    blanket "quote if it contains a hyphen" rule would needlessly quote the
    common case. Only the actual YAML hazards force quoting.
    What: true when `value` is empty, starts/ends with whitespace, starts
    with a YAML indicator character, contains `: ` (a mapping-key
    look-alike), contains a flow-collection character (`[`, `]`, `{`, `}`,
    `,`), or contains a newline.
    """
    if not value:
        return True
    if value[0] in _LEADING_SPECIAL or value[0].isspace() or value[-1].isspace():
        return True
    if ": " in value or value.endswith(":"):
        return True
    if any(ch in value for ch in "[]{},\n"):
        return True
    return False


def yaml_scalar(value: str) -> str:
    """Render `value` as a frontmatter scalar for a field the reader only
    STRIPS a quote pair from, never unescapes (see the module docstring's
    "Frontmatter reader contract" section — this covers every field this
    script emits: `name`/`role`/`description`/`model`/list elements).

    Why: `agents::frontmatter::strip_one_quote_pair` removes exactly the
    first and last byte of a quoted value with no further interpretation.
    Backslash-escaping interior quotes/backslashes here (the naive YAML-
    emitter move) would therefore be WRONG for this reader — those escapes
    are never resolved back, so the loaded value would retain literal
    backslashes. Wrapping the RAW value in one fresh, un-escaped quote pair
    is the correct inverse of `strip_one_quote_pair` for ANY content
    (including a value that itself starts/ends with a quote character),
    because the strip always removes exactly the pair we just added and
    returns everything between untouched.
    What: double-quotes the value verbatim (no escaping) when
    [`_needs_quoting`] says quoting is required; otherwise emits it bare for
    readability.
    """
    if _needs_quoting(value):
        return f'"{value}"'
    return value


def yaml_list(items: list[str]) -> str:
    """Render a flow-style YAML list, e.g. `[a, b, c]` or `[]`."""
    return "[" + ", ".join(yaml_scalar(i) for i in items) + "]"


def convert(toml_path: Path) -> str:
    """Convert one TOML agent document's bytes into a `.md`+frontmatter string."""
    with toml_path.open("rb") as f:
        data = tomllib.load(f)

    agent = data.get("agent", {})
    llm = data.get("llm", {})
    system_prompt = data.get("system_prompt", {})
    tools = data.get("tools", {})
    dropped: list[str] = []

    if "runner" in data:
        dropped.append("[runner] (dead config — no consumer reads AgentConfig.runner)")
    if "temperature" in llm:
        dropped.append(
            "[llm].temperature (dead config — no consumer reads AgentConfig.llm.temperature)"
        )

    name = agent.get("name")
    if not name:
        raise ValueError(f"{toml_path}: missing required [agent].name")

    frontmatter: list[tuple[str, str]] = [("name", yaml_scalar(name))]

    role = agent.get("role")
    if role:
        frontmatter.append(("role", yaml_scalar(role)))

    description = agent.get("description")
    if description:
        frontmatter.append(("description", yaml_scalar(description)))

    # [agent].model wins over [llm].model_override — mirrors
    # provider::routing::resolve_model's precedence (agent-level model beats
    # llm.model_override), so the converted frontmatter's single `model:`
    # field preserves whichever value actually drove the agent before.
    model = agent.get("model") or llm.get("model_override")
    if model:
        frontmatter.append(("model", yaml_scalar(model)))

    max_tokens = llm.get("max_tokens")
    if max_tokens is not None:
        frontmatter.append(("max_tokens", str(max_tokens)))

    if "allowed" in tools:
        frontmatter.append(("tools", yaml_list(tools["allowed"])))

    skills = system_prompt.get("append_skills") or []
    if skills:
        frontmatter.append(("skills", yaml_list(skills)))

    body = (system_prompt.get("content") or "").strip()

    lines = ["---"]
    lines.extend(f"{key}: {value}" for key, value in frontmatter)
    lines.append("---")
    lines.append("")
    if body:
        lines.append(body)
        lines.append("")

    if dropped:
        print(f"{toml_path}: dropped (no equivalent in .md): {'; '.join(dropped)}", file=sys.stderr)

    return "\n".join(lines)


def discover_toml_files(target: Path) -> list[Path]:
    if target.is_dir():
        return sorted(target.glob("*.toml"))
    return [target]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Convert a tcode agent .toml config to .md+frontmatter (#2897 Slice D).",
    )
    parser.add_argument(
        "input",
        type=Path,
        help="a .toml agent file, or a directory to convert every *.toml file in",
    )
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="output .md path (single-file mode only; default: same name, .md extension)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the converted .md to stdout instead of writing a file",
    )
    args = parser.parse_args(argv)

    if args.output and args.input.is_dir():
        parser.error("-o/--output is only valid when converting a single file")

    toml_files = discover_toml_files(args.input)
    if not toml_files:
        print(f"no .toml files found under {args.input}", file=sys.stderr)
        return 1

    exit_code = 0
    for toml_path in toml_files:
        try:
            md_text = convert(toml_path)
        except (tomllib.TOMLDecodeError, ValueError, OSError) as e:
            print(f"{toml_path}: {e}", file=sys.stderr)
            exit_code = 1
            continue

        if args.dry_run:
            print(f"# --- {toml_path} -> .md ---")
            print(md_text)
            continue

        out_path = args.output or toml_path.with_suffix(".md")
        out_path.write_text(md_text, encoding="utf-8")
        print(f"wrote {out_path}")

    if exit_code == 0 and not args.dry_run:
        print(
            "Done. Review the .md file(s), then delete the source .toml file(s) — "
            "trusty-code no longer reads them.",
            file=sys.stderr,
        )
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
