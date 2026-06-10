# trusty-controller (`tctl`)

**Phase 0 scaffold — CLI surface wired, backend dispatch deferred to Phase 1.**

`trusty-controller` is the thin control plane for the entire claude-mpm stack.
It coordinates install, upgrade, restart, health, and config operations across
every `trusty-*` tool and the external `claude-mpm` orchestrator through a
uniform versioned contract (DOC-1) and a manifest-driven dispatch engine (DOC-5).

- **RFC:** [#920](https://github.com/bobmatnyc/trusty-tools/issues/920)
- **Design docs:** `docs/trusty-controller/research/02-design/`
- **ADRs:** `docs/adr/0006-trusty-controller-naming.md`,
  `docs/adr/0007-tool-contract-versioning-and-verb-model.md`,
  `docs/adr/0008-project-identity-convention.md`

## Binary

```
tctl
```

Install via `cargo install --path crates/trusty-controller --locked`.

## Phase-0 status

All Phase-0 subcommands are **fully wired** (clap parsing + dispatch table is
complete) but most return a structured `not-yet-implemented` result rather than
executing the real backend logic.  The exceptions:

| Command | Status |
|---|---|
| `tctl version [--json]` | **Fully implemented** (capability discovery, DOC-5 §4.2) |
| `tctl stack health` | Phase-0 stub — returns structured NYI |
| `tctl stack doctor [<member>]` | Phase-0 stub |
| `tctl status` | Phase-0 stub |
| `tctl updates [--latest]` | Phase-0 stub |
| `tctl upgrade [--check] [--latest] [--exclude-self] [<members>…]` | Phase-0 stub |
| `tctl update` | visible alias of `upgrade` |
| `tctl install [<members>…]` | Phase-0 stub |
| `tctl ensure [--wait]` | Phase-0 stub |
| `tctl start / stop / restart [<members>…]` | Phase-0 stubs |
| `tctl config [<members>…]` | Phase-0 stub |
| `tctl port [--addr] [--json]` | Phase-0 stub |
| `tctl doctor [--self-check <member>]` | Phase-0 stub |
| `tctl ui [--print]` | Phase-0 stub |
| `tctl <tool> <verb> [args]` | Generic passthrough — Phase-0 stub |

## Usage

```bash
tctl --help
tctl version
tctl version --json   # capability-discovery JSON
tctl stack health
tctl stack doctor
tctl status
tctl updates
tctl upgrade --check
tctl install
tctl ensure
tctl start
tctl stop
tctl restart
tctl config
tctl port
tctl doctor --self-check trusty-search
tctl ui
tctl trusty-search doctor   # generic passthrough
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

`tctl` is a **thin coordinator**: it reads a stack manifest (DOC-2), iterates
over the enabled members, invokes each member's contract verbs (DOC-1) at the
appropriate scope (DOC-3), collects the standardised envelopes, and renders the
results (DOC-4 rollup for stack verbs, verbatim for passthrough).

Zero tool-specific logic is compiled in — a new stack member is added by editing
the manifest, not the controller source.  See `docs/trusty-controller/research/02-design/05-controller-cli.md` for
the full dispatch-engine specification.

## Planned full scope (Phase 1+)

- Manifest-driven parallel probe loop (DOC-4 §1.3)
- DOC-8 install/bootstrap mechanics (`tctl install`, `tctl ensure`)
- DOC-9 upgrade flow (`tctl upgrade`, `tctl updates`)
- DOC-1 capability negotiation + graceful older-contract degrade
- DOC-7 embedded web UI (`tctl ui`, `tctl port`)
- DOC-6 contract conformance self-check (`tctl doctor --self-check`)
- Publishing to crates.io (`SKIP_UI_BUILD=1 cargo publish -p trusty-controller`)

## Development

```bash
# Check
cargo check -p trusty-controller

# Build the binary
cargo build -p trusty-controller

# Run tests
cargo test -p trusty-controller

# Lint
cargo clippy -p trusty-controller --all-targets -- -D warnings

# Try the binary
./target/debug/tctl --help
./target/debug/tctl version --json
```

## License

MIT (workspace default; see root `Cargo.toml` and issue #898).  `publish = false`
for the Phase-0 scaffold; will be set to `true` when the Phase-1 dispatch engine ships.
