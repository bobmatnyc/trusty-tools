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
    """Render `value` as a safe YAML scalar for frontmatter.

    Why: agent names/models/descriptions are free-form strings that may
    contain YAML-significant characters; emitting them unquoted in that case
    would silently corrupt the frontmatter.
    What: double-quotes and escapes the value when [`_needs_quoting`] says
    so; otherwise emits it bare for readability.
    """
    if _needs_quoting(value):
        escaped = value.replace("\\", "\\\\").replace('"', '\\"')
        return f'"{escaped}"'
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
