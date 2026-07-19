# Security Policy

## Installer trust model

The `curl | sh` bootstrap installer (`install.sh`) follows the same trust model as rustup:

- **Transport integrity:** the script and all assets are fetched over HTTPS only. The installer aborts on any non-HTTPS URL or HTTP error.
- **Binary integrity:** every downloaded binary tarball is verified against its published `.sha256` checksum before installation. A checksum mismatch aborts the install.
- **Installer integrity:** the `install.sh` script itself is **not** cryptographically signed. HTTPS is the only integrity guarantee for the script. If you require higher assurance, download and review the script before executing it:
  ```bash
  curl -sSfO https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh
  less install.sh   # review
  sh install.sh
  ```
- **Idempotent re-runs:** when a matching version is already installed, the installer skips re-download and therefore skips checksum re-verification of the existing binary. Run with `--force` (or `TRUSTY_FORCE=1`) to always re-download and re-verify.

---

## Supported Versions

We maintain security updates for recent releases. The exact version support matrix is maintained in the individual crate CHANGELOG files (located in `crates/*/CHANGELOG.md`).

General guidance:
- Always upgrade to the latest version for security fixes
- Per-crate versioning means updates can be released independently
- Subscribe to [GitHub Security Advisories](https://github.com/bobmatnyc/trusty-tools/security/advisories) to be notified of published vulnerabilities

## Reporting Security Vulnerabilities

We take security seriously. If you discover a security vulnerability, **do not open a public issue**. Instead, please report it privately using one of the following channels:

**Primary (Recommended):** [GitHub Security Advisories](https://github.com/bobmatnyc/trusty-tools/security/advisories/new)

**Secondary:** r@1mc.io

**Include in your report:**
- A clear description of the vulnerability
- Steps to reproduce (if applicable)
- Affected crate(s) and version(s)
- Potential impact and severity
- Any known mitigations

## Response and Disclosure

- **Acknowledgment:** We will acknowledge receipt within 48 hours
- **Triage:** We will assess severity and begin work on a fix
- **Fix timeline:** Critical vulnerabilities are addressed within 7 days; others within 30 days
- **Disclosure:** We will coordinate a responsible disclosure timeline with you before publishing a fix

## Dependency Security

`cargo audit` scans dependencies for known vulnerabilities against the RustSec
advisory database. It runs automatically on a **weekly schedule** via
[`.github/workflows/cargo-audit.yml`](.github/workflows/cargo-audit.yml)
(Monday 03:00 UTC, plus manual `workflow_dispatch`). This scan is
visibility-only — it is not wired to `push`/`pull_request`, so it never
blocks a PR merge; a failing scheduled run means an advisory needs triage,
not that CI is broken.

You can also run it locally at any time:

```bash
cargo audit
```

Dependencies are kept up-to-date as part of regular maintenance.

## Secure Coding Practices

See [CLAUDE.md](CLAUDE.md) for the project's development conventions, including:
- Error handling best practices
- No use of `unsafe` except in carefully justified library code
- No global state or unsynchronized access patterns
- Logging to stderr only (no secrets leak to stdout)

## Questions

For security-related questions or concerns, please use the reporting channels above.
