# env-manager — usage examples and common use cases

## Usage Examples

### Basic Validation

Validate a .env file for structural issues:

```bash
python3 scripts/validate_env.py .env
```

**What it checks:**
- Valid key=value format
- No duplicate keys
- Proper naming conventions (UPPERCASE_WITH_UNDERSCORES)
- No empty values (unless explicitly allowed)
- Proper quoting for values with spaces

**Example output:**
```
✅ Validation successful!
   - 15 variables validated
   - 0 errors
   - 0 warnings
```

### Framework-Specific Validation

#### Next.js

Validate Next.js environment variables:

```bash
python3 scripts/validate_env.py .env.local --framework nextjs
```

**Next.js-specific checks:**
- NEXT_PUBLIC_* variables are client-exposed (warns if secrets detected)
- .env.local, .env.production, .env file hierarchy
- Detects secrets in client-side variables

**Example:**
```bash
# .env.local
NEXT_PUBLIC_API_URL=https://api.example.com
NEXT_PUBLIC_API_KEY=secret123  # ⚠️  WARNING: Secret in client-exposed variable!
DATABASE_URL=postgresql://...   # ✅ Server-side only
```

#### Vite

```bash
python3 scripts/validate_env.py .env --framework vite
```

**Vite-specific checks:**
- VITE_* variables are client-exposed
- Warns if secrets detected in VITE_ prefixed vars

#### React (Create React App)

```bash
python3 scripts/validate_env.py .env --framework react
```

**React-specific checks:**
- REACT_APP_* variables are client-exposed
- Warns if secrets in REACT_APP_ prefixed vars

#### Node.js/Express

```bash
python3 scripts/validate_env.py .env --framework nodejs
```

**Node.js-specific checks:**
- Common NODE_ENV, PORT, DATABASE_URL patterns
- Standard Node.js conventions

#### Flask/Python

```bash
python3 scripts/validate_env.py .env --framework flask
```

**Flask-specific checks:**
- FLASK_APP, FLASK_ENV variables
- SQLAlchemy DATABASE_URL format

### Comparing with .env.example

Ensure your .env has all required variables:

```bash
python3 scripts/validate_env.py .env --compare-with .env.example
```

**What it checks:**
- All variables in .env.example exist in .env
- No extra undocumented variables in .env

**Example output:**
```
❌ Missing variables:
   - DATABASE_URL (required in .env.example)
   - STRIPE_SECRET_KEY (required in .env.example)

⚠️  Extra variables not in .env.example:
   - DEBUG_MODE (consider adding to .env.example)
```

**Perfect for:**
- Onboarding new team members
- CI/CD validation
- Deployment pre-checks

### Generating .env.example

Create documentation for your environment variables:

```bash
python3 scripts/validate_env.py .env --generate-example .env.example
```

**What it does:**
- Reads your .env file
- Sanitizes secret values (replaces with placeholders)
- Generates .env.example with safe defaults

**Example:**

```bash
# Input: .env
DATABASE_URL=postgresql://user:pass@localhost/db  # pragma: allowlist secret
STRIPE_SECRET_KEY=sk_live_abc123xyz
NEXT_PUBLIC_API_URL=https://api.example.com

# Output: .env.example
DATABASE_URL=postgresql://user:password@localhost/dbname  # pragma: allowlist secret
STRIPE_SECRET_KEY=your_stripe_secret_key_here
NEXT_PUBLIC_API_URL=https://api.example.com
```

**Security note:** env-manager detects common secret patterns and replaces them with safe placeholders.

### CI/CD Integration

Get machine-readable JSON output for automated workflows:

```bash
python3 scripts/validate_env.py .env.example --strict --json
```

**JSON output format:**
```json
{
  "valid": true,
  "errors": [],
  "warnings": [],
  "stats": {
    "total_vars": 15,
    "errors": 0,
    "warnings": 0
  }
}
```

**Exit codes:**
- `0`: Validation passed
- `1`: Validation errors found
- `2`: Missing required file
- `3`: Warnings found (only in --strict mode)

**GitHub Actions example:**

```yaml
name: Validate Environment Variables

on: [push, pull_request]

jobs:
  validate-env:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Validate .env.example
        run: |
          python3 scripts/validate_env.py .env.example --strict --json
        working-directory: ./path/to/skill

      - name: Check for framework-specific issues
        run: |
          python3 scripts/validate_env.py .env.example --framework nextjs --json
        working-directory: ./path/to/skill
```

### Strict Mode

Treat warnings as errors (useful for CI/CD):

```bash
python3 scripts/validate_env.py .env --strict
```

**When to use:**
- Pre-deployment validation
- CI/CD pipelines
- Release gates
- Team standard enforcement

### Quiet Mode

Show only errors, suppress warnings:

```bash
python3 scripts/validate_env.py .env --quiet
```

**When to use:**
- You've already reviewed warnings
- Automated scripts that only care about errors
- Noisy environments where warnings are distracting

## Common Use Cases

### Scenario 1: New Developer Onboarding

```bash
# New developer clones repo
git clone <repo>
cd <project>

# Copy example and fill in values
cp .env.example .env
# Edit .env with actual values...

# Validate setup
python3 scripts/validate_env.py .env --compare-with .env.example

# If missing variables, fix them
# Validation passes ✅
```

### Scenario 2: Pre-Deployment Check

```bash
# Before deploying to Vercel/Railway/Heroku
python3 scripts/validate_env.py .env.production --framework nextjs --strict

# Fix any errors
# Deploy with confidence ✅
```

### Scenario 3: Security Audit

```bash
# Check for accidentally exposed secrets
python3 scripts/validate_env.py .env.local --framework nextjs

# Look for warnings like:
# ⚠️  NEXT_PUBLIC_STRIPE_SECRET: Contains potential secret in client-exposed variable
```

### Scenario 4: Team Documentation

```bash
# After adding new environment variable
echo "NEW_API_KEY=abc123" >> .env

# Regenerate .env.example
python3 scripts/validate_env.py .env --generate-example .env.example

# Commit updated .env.example
git add .env.example
git commit -m "docs: add NEW_API_KEY to environment variables"
```

### Scenario 5: CI/CD Quality Gate

```yaml
# In your CI pipeline
- name: Validate environment configuration
  run: |
    python3 scripts/validate_env.py .env.example --strict --json > validation.json

    # Fail pipeline if validation fails
    if [ $? -ne 0 ]; then
      cat validation.json
      exit 1
    fi
```
