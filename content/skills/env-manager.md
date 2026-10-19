---
name: env-manager
description: "Environment variable validation, security scanning, and management for Next.js, Vite, React, and Node.js applications"
user-invocable: false
metadata:
  version: "1.1.0"
category: agent-reference
effort: high
---
# Environment Variable Manager (env-manager)

**Comprehensive environment variable validation, security scanning, and management for modern web applications.**

[![Security Audited](https://img.shields.io/badge/security-audited-success)](references/security.md)
[![Test Coverage](https://img.shields.io/badge/coverage-85%25%2B-success)](tests/)
[![Performance](https://img.shields.io/badge/performance-80x_faster-success)](#performance)

## Overview

The env-manager skill provides systematic environment variable management across local development, CI/CD pipelines, and deployment platforms. It prevents common issues like missing variables, exposed secrets, and framework-specific configuration errors.

**Key Features:**
- **Framework-Aware Validation**: Next.js, Vite, React, Node.js, Flask support
- **Security-First**: Never logs secrets, detects exposed credentials
- **Platform Integration**: Ready for Vercel, Railway, Heroku, and CI/CD
- **Fast**: Validates 1000 variables in 0.025s (80x faster than 2s target)
- **Zero Dependencies**: Pure Python, works anywhere

## Why Use env-manager?

**Common problems this solves:**
- "Works on my machine, but not in production" (missing env vars)
- Accidentally exposing API keys in client-side code (NEXT_PUBLIC_ with secrets)
- Missing required variables during deployment
- Inconsistent .env files across team members
- No documentation of required environment variables
- Security vulnerabilities from exposed secrets

## Quick Start

### Installation

No installation needed! env-manager is a bundled skill in Claude MPM.

**Requirements:**
- Python 3.7+
- No external dependencies

### 5-Minute Quick Start

```bash
# 1. Validate your .env file
python3 scripts/validate_env.py .env

# 2. Check for framework-specific issues (Next.js example)
python3 scripts/validate_env.py .env --framework nextjs

# 3. Compare with .env.example to find missing vars
python3 scripts/validate_env.py .env --compare-with .env.example

# 4. Generate .env.example for documentation
python3 scripts/validate_env.py .env --generate-example .env.example

# 5. Get JSON output for CI/CD integration
python3 scripts/validate_env.py .env --json
```

That's it! Environment variables are now validated professionally.

## Where Each Task Lives

This file holds what every env-manager task needs. Everything else sits in one
file per task under `references/`, beside this file. Read the file your task
needs, and only that file. A pointer elsewhere that names a section of
`env-manager` resolves here: find the section name in the middle column.

| Task | Sections | File |
|---|---|---|
| Run the validator: basic, per framework, against `.env.example`, generating `.env.example`, in CI, strict or quiet; walk a common scenario | "Usage Examples", "Common Use Cases" | [references/usage-examples.md](references/usage-examples.md) |
| Fix a validator error or warning; debug a pass-locally, fail-in-CI run | "Common Issues", "Troubleshooting" | [references/common-issues.md](references/common-issues.md) |
| Go deeper on validation workflows and checks | "Purpose", "Validation Hierarchy", "Structure Validation", "Completeness Validation", "Naming Convention Validation", "Framework-Specific Validation", "Summary", "Related References" | [references/validation.md](references/validation.md) |
| Scan for secrets; respond to an exposed one | "Purpose", "Security Principles", "Secret Pattern Detection", "Git History Scanning", "Gitignore Validation", "Format Validation", "Security Best Practices", "Incident Response", "Validation Error Messages", "Summary", "Related References" | [references/security.md](references/security.md) |
| Sync variables with a platform or secret manager | "Purpose", "Synchronization Principles", "Platform Comparison Workflows", "Platform-Specific Integration", "Secret Manager Integration", "Safe Sync Patterns", "Summary", "Related References" | [references/synchronization.md](references/synchronization.md) |
| Apply one framework's conventions | "Purpose", "Framework Detection", "Next.js", "Express/Node.js", "Flask/Python", "Django", "Vite", "Create React App", "Framework Comparison Table", "Summary", "Related References" | [references/frameworks.md](references/frameworks.md) |
| Work the debugging checklist and quick fixes | "Purpose", "Common Issues", "Debugging Checklist", "Quick Fixes", "Prevention Tips", "Summary", "Related References" | [references/troubleshooting.md](references/troubleshooting.md) |

## Supported Frameworks

| Framework | Prefix | Client-Exposed | Notes |
|-----------|--------|----------------|-------|
| **Next.js** | `NEXT_PUBLIC_*` | Yes | Auto-exposed in browser |
| **Vite** | `VITE_*` | Yes | Bundled into client code |
| **React (CRA)** | `REACT_APP_*` | Yes | Embedded in production build |
| **Node.js** | N/A | No | Server-side only |
| **Flask** | N/A | No | Server-side only |

**Security warning:** Never put secrets in client-exposed variables (NEXT_PUBLIC_, VITE_, REACT_APP_). env-manager will warn you if it detects common secret patterns.

## CLI Reference

### Command Structure

```bash
python3 scripts/validate_env.py <file> [options]
```

### Options

| Option | Description | Example |
|--------|-------------|---------|
| `--compare-with FILE` | Compare with .env.example | `--compare-with .env.example` |
| `--framework {nextjs\|vite\|react\|nodejs\|flask\|generic}` | Framework-specific validation | `--framework nextjs` |
| `--strict` | Treat warnings as errors | `--strict` |
| `--json` | JSON output for automation | `--json` |
| `--quiet` | Only show errors | `--quiet` |
| `--generate-example OUTPUT` | Generate .env.example | `--generate-example .env.example` |

### Exit Codes

| Code | Meaning | When |
|------|---------|------|
| `0` | Success | No errors (warnings OK unless --strict) |
| `1` | Validation errors | Structural issues, duplicates, etc. |
| `2` | File not found | Specified .env file doesn't exist |
| `3` | Warnings in strict mode | Warnings exist and --strict enabled |

## Performance

env-manager is designed for speed:

**Benchmarks:**
- Validates 1000 variables in **0.025s**
- 80x faster than 2s target
- Zero external dependencies
- Minimal memory footprint

**Why it matters:**
- Fast feedback during development
- No CI/CD slowdown
- Works in resource-constrained environments

## Security Notes

**Critical security features:**

1. **Never Logs Secrets**: env-manager NEVER displays actual secret values in output
2. **Client-Exposure Detection**: Warns when secrets are in NEXT_PUBLIC_, VITE_, REACT_APP_ variables
3. **Secret Sanitization**: When generating .env.example, replaces secrets with safe placeholders
4. **No Network Calls**: All validation is local, no data leaves your machine

**Security-audited:** This skill has undergone security review. See [references/security.md](references/security.md) for details.

**Best practices:**
- Never commit .env files with secrets
- Always use .env.example for documentation
- Use platform secret managers (Vercel, Railway, etc.) for production
- Validate before every deployment
- Run security scan regularly

## Advanced Usage

### Custom Validation Patterns

See [references/validation.md](references/validation.md) for advanced validation patterns.

### Platform-Specific Deployment

See [references/synchronization.md](references/synchronization.md) for Vercel, Railway, Heroku integration patterns.

### Framework-Specific Guides

See [references/frameworks.md](references/frameworks.md) for comprehensive framework guides.

## Related Documentation

- **[Validation Reference](references/validation.md)**: Complete validation workflows and checks
- **[Security Reference](references/security.md)**: Secret scanning and security patterns
- **[Synchronization Reference](references/synchronization.md)**: Platform sync patterns (coming soon)
- **[Frameworks Reference](references/frameworks.md)**: Framework-specific patterns and conventions
- **[Troubleshooting Guide](references/troubleshooting.md)**: Common issues and solutions

## Integration with Claude MPM

env-manager is a bundled skill in Claude MPM. Agents can use it for:
- Pre-deployment validation
- Security scanning
- Environment setup verification
- Documentation generation

See [INTEGRATION.md](INTEGRATION.md) for agent integration patterns.

## Contributing

env-manager follows Claude MPM contribution guidelines:

1. Run `make lint-fix` during development
2. Run `make quality` before commits
3. Add tests for new features (85%+ coverage required)
4. Update documentation

See [CONTRIBUTING.md](../../../../../../CONTRIBUTING.md) for details.

## License

MIT License - Part of Claude MPM project

## Support

- **Issues**: Report bugs via GitHub Issues
- **Documentation**: See references/ directory
- **Examples**: See examples/ directory
- **Integration**: See INTEGRATION.md

---

**Version**: 1.0.0
**Status**: Stable, Security-Audited
**Test Coverage**: 85%+
**Performance**: 80x faster than target
