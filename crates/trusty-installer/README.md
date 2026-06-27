# trusty-installer (`tctl` transitional alias)

**Phase 0 scaffold — CLI surface wired, backend dispatch deferred to Phase 1.**

`trusty-installer` (formerly `trusty-controller`) is the install/upgrade orchestrator
for the entire claude-mpm stack. It coordinates install, upgrade, restart, health,
and config operations across every `trusty-*` tool and the external `claude-mpm`
orchestrator through a uniform versioned contract (DOC-1) and a manifest-driven
dispatch engine (DOC-5).

- **RFC:** [#920](https://github.com/bobmatnyc/trusty-tools/issues/920)
- **Rename tracking:** [#1757](https://github.com/bobmatnyc/trusty-tools/issues/1757)
- **Design docs:** `docs/trusty-installer/research/02-design/`
- **ADRs:** `docs/adr/0013-rename-trusty-controller-to-trusty-installer.md`,
  `docs/adr/0007-tool-contract-versioning-and-verb-model.md`,
  `docs/adr/0008-project-identity-convention.md`

## Binaries

```
trusty-installer     # primary binary (ADR-0013)
tctl                 # transitional alias — same binary, deprecated in one release cycle
```

Install via `cargo install --path crates/trusty-installer --locked`.

## Phase-0 status

All Phase-0 subcommands are **fully wired** (clap parsing + dispatch table is
complete) but most return a structured `not-yet-implemented` result rather than
executing the real backend logic.  The exceptions:

| Command | Status |
|---|---|
| `trusty-installer version [--json]` | **Fully implemented** (capability discovery, DOC-5 §4.2) |
| `trusty-installer stack health` | Phase-0 stub — returns structured NYI |
| `trusty-installer stack doctor [<member>]` | Phase-0 stub |
| `trusty-installer status` | Phase-0 stub |
| `trusty-installer updates [--latest]` | Phase-0 stub |
| `trusty-installer upgrade [--check] [--latest] [--exclude-self] [<members>…]` | Phase-0 stub |
| `trusty-installer update` | visible alias of `upgrade` |
| `trusty-installer install [<members>…]` | Phase-0 stub |
| `trusty-installer ensure [--wait]` | Phase-0 stub |
| `trusty-installer start / stop / restart [<members>…]` | Phase-0 stubs |
| `trusty-installer config [<members>…]` | Phase-0 stub |
| `trusty-installer port [--addr] [--json]` | Phase-0 stub |
| `trusty-installer doctor [--self-check <member>]` | Phase-0 stub |
| `trusty-installer ui [--print]` | Phase-0 stub |
| `trusty-installer <tool> <verb> [args]` | Generic passthrough — Phase-0 stub |

## Usage

```bash
trusty-installer --help
trusty-installer version
trusty-installer version --json   # capability-discovery JSON
trusty-installer stack health
trusty-installer stack doctor
trusty-installer status
trusty-installer updates
trusty-installer upgrade --check
trusty-installer install
trusty-installer ensure
trusty-installer start
trusty-installer stop
trusty-installer restart
trusty-installer config
trusty-installer port
trusty-installer doctor --self-check trusty-search
trusty-installer ui
trusty-installer trusty-search doctor   # generic passthrough

# Transitional alias (deprecated — use trusty-installer):
tctl --help
tctl version
```

## Global flags

| Flag | Description |
|---|---|
| `--scope <project\|system\|all>` | Override scope (DOC-3 §3; default: `all` inside a project dir, else `system`) |
| `--json` | Machine-readable JSON to stdout |
| `--timeout <secs>` | Per-tool probe deadline override |
| `-y` / `--yes` | Skip blast-radius confirmation |
| `--manifest <path>` | Override manifest path |
| `-v` / `--verbose` | Increase detail |

## Architecture

`trusty-installer` is a **thin coordinator**: it reads a stack manifest (DOC-2),
iterates over the enabled members, invokes each member's contract verbs (DOC-1) at
the appropriate scope (DOC-3), collects the standardised envelopes, and renders the
results (DOC-4 rollup for stack verbs, verbatim for passthrough).

Zero tool-specific logic is compiled in — a new stack member is added by editing
the manifest, not the installer source.  See
`docs/trusty-installer/research/02-design/05-controller-cli.md` for the full
dispatch-engine specification.

## Planned full scope (Phase 1+)

- Manifest-driven parallel probe loop (DOC-4 §1.3)
- DOC-8 install/bootstrap mechanics (`trusty-installer install`, `trusty-installer ensure`)
- DOC-9 upgrade flow (`trusty-installer upgrade`, `trusty-installer updates`)
- DOC-1 capability negotiation + graceful older-contract degrade
- DOC-7 embedded web UI (`trusty-installer ui`, `trusty-installer port`)
- DOC-6 contract conformance self-check (`trusty-installer doctor --self-check`)
- Publishing to crates.io (`cargo publish -p trusty-installer`)

## Development

```bash
# Check
cargo check -p trusty-installer

# Build the binary
cargo build -p trusty-installer

# Run tests
cargo test -p trusty-installer

# Lint
cargo clippy -p trusty-installer --all-targets -- -D warnings

# Try the binary
./target/debug/trusty-installer --help
./target/debug/trusty-installer version --json

# Transitional alias also works:
./target/debug/tctl --help
```

## Config directory migration

On first run, `trusty-installer` automatically migrates
`~/.trusty-tools/trusty-controller/` → `~/.trusty-tools/trusty-installer/`
if the old directory exists and the new one does not. The migration is
idempotent and never destructive (see ADR-0013).

## License

MIT (workspace default; see root `Cargo.toml` and issue #898).  `publish = false`
for the Phase-0 scaffold; will be set to `true` when the Phase-1 dispatch engine ships.
