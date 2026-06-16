# Security Policy

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

The project uses `cargo audit` to scan for known vulnerabilities in dependencies:

```bash
cargo audit
```

This is run in CI on every commit. Dependencies are kept up-to-date as part of regular maintenance.

## Secure Coding Practices

See [CLAUDE.md](CLAUDE.md) for the project's development conventions, including:
- Error handling best practices
- No use of `unsafe` except in carefully justified library code
- No global state or unsynchronized access patterns
- Logging to stderr only (no secrets leak to stdout)

## Questions

For security-related questions or concerns, please use the reporting channels above.
