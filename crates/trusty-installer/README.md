# trusty-installer

Install, upgrade, health, lifecycle, configuration, and signing control plane
for the Trusty tool stack. The package installs two equivalent binaries:
`trusty-installer` is the canonical name and `tctl` is the transitional alias.

The current implementation manages the application set declared in
[`src/commands/stable_set.rs`](src/commands/stable_set.rs): trusty-search,
trusty-memory, trusty-analyze, trusty-review, tga, trusty-console, and
trusty-mpm. It also installs and updates itself. Required/optional status,
daemon strategy, binary bundles, and dependency expansion are code-owned
rather than copied into this README.

## Installation

The repository bootstrap installs the latest supported release:

```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
```

For a source build:

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-installer --locked
```

Verify either binary name:

```bash
tctl version
trusty-installer version --json
```

## Common operations

```bash
tctl up
tctl install
tctl install trusty-search --dry-run
tctl status
tctl stack health
tctl stack doctor
tctl updates
tctl upgrade --check
tctl ensure --wait
tctl restart trusty-search
```

`up` boots the core stack in dependency order and can include trusty-mpm.
`install` and `upgrade` select named members or the full stable set, expand
required dependencies, verify downloaded artifacts, and run a post-operation
health tail. `ensure` patches project MCP configuration and establishes the
project's search and memory resources.

Other supported surfaces include `start`, `stop`, `config`, `config keys`,
`port`, `doctor`, `ui`, `self-update`, and macOS `sign`. Run `tctl <command>
--help` for the authoritative flags. Generic `<tool> <verb>` passthrough is
still a compatibility stub and must not be treated as a completed dispatch
path.

## Configuration and output

Global options include `--json`, `--yes`, `--timeout`, `--manifest`,
`--scope`, and `--verbose`. Machine-readable mode writes data to stdout;
diagnostics stay on stderr.

On first use, the installer can migrate the former
`~/.trusty-tools/trusty-controller/` directory to
`~/.trusty-tools/trusty-installer/`. The migration is idempotent and
non-destructive.

The behavior contract is [SPEC-INSTALLER-01](../../docs/specs/SPEC-INSTALLER-01.md).
The rename and CLI-contract decisions are recorded in
[ADR-0013](../../docs/adr/0013-rename-trusty-controller-to-trusty-installer.md),
[ADR-0007](../../docs/adr/0007-tool-contract-versioning-and-verb-model.md), and
[ADR-0008](../../docs/adr/0008-project-identity-convention.md).

## Development

```bash
cargo check -p trusty-installer
cargo test -p trusty-installer --no-fail-fast
cargo clippy -p trusty-installer --all-targets --all-features -- -D warnings
cargo run -p trusty-installer --bin tctl -- --help
```

Licensed under the [MIT License](../../LICENSE).
