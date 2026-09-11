# Crate Abbreviations and Aliases

> Moved out of [`CLAUDE.md`](../../CLAUDE.md) by #7423, unchanged. That file
> keeps the one-line rule — resolve an abbreviation here before acting on it —
> and points at this table.

Resolve any crate abbreviation with this table before taking action — it applies
everywhere: ticket descriptions, build commands, conversation.

🟡 **Crate name ≠ directory name.** `-p <crate>` takes the `name` field from the
crate's `Cargo.toml`; the exceptions are below. On "package not found", read the
crate's `Cargo.toml`.

| Abbreviation | Full crate name | Cargo package flag | Directory |
|---|---|---|---|
| `tga` | trusty-git-analytics | `-p tga` | `crates/trusty-git-analytics/` |
| `tm` | trusty-memory | `-p trusty-memory` | `crates/trusty-memory/` |
| `ts` | trusty-search | `-p trusty-search` | `crates/trusty-search/` |
| `tc` | trusty-common | `-p trusty-common` | `crates/trusty-common/` |
| `ta` | trusty-analyze | `-p trusty-analyze` | `crates/trusty-analyze/` |
| `mpm` | trusty-mpm | `-p trusty-mpm` | `crates/trusty-mpm/` |
| `tagent` or `t-agents` | trusty-agents | `-p trusty-agents` | `crates/trusty-agents/` (bin: `tagent`) |
| `t-agents-common` | trusty-agents-common | `-p trusty-agents-common` | `crates/trusty-agents-common/` |
| `tcode` | trusty-code | `-p trusty-code` | `crates/trusty-code/` |
| `tctl` | trusty-installer | `-p trusty-installer` | `crates/trusty-installer/` |
| `taudit` | trusty-audit | `-p trusty-audit` | `crates/trusty-audit/` (bins: `trusty-audit`, `taudit`) |

> **Auto-resolution:** When connected to trusty-memory MCP, call
> `get_prompt_context()` at the start of each turn to load current aliases and
> conventions. Pass a `query` string to filter to relevant facts only.

What each crate is for: [crate-map.md](crate-map.md).
