# env-manager — common issues and troubleshooting

## Common Issues

### "Missing equals sign" error

**Cause:** Line in .env doesn't have = separator

**Fix:**
```bash
# ❌ Bad
API_KEY

# ✅ Good
API_KEY=your_key_here
```

### "Duplicate key" error

**Cause:** Same variable defined multiple times

**Fix:**
```bash
# ❌ Bad
API_KEY=value1
API_KEY=value2

# ✅ Good
API_KEY=value2
```

### "Invalid variable name" warning

**Cause:** Variable name doesn't follow UPPERCASE_WITH_UNDERSCORES convention

**Fix:**
```bash
# ❌ Bad
apiKey=value
api-key=value

# ✅ Good
API_KEY=value
```

### "Potential secret in client-exposed variable" warning

**Cause:** NEXT_PUBLIC_, VITE_, or REACT_APP_ variable contains secret-like value

**Fix:**
```bash
# ❌ Bad (secret exposed to client!)
NEXT_PUBLIC_STRIPE_SECRET=sk_live_abc123

# ✅ Good (server-side only)
STRIPE_SECRET_KEY=sk_live_abc123
NEXT_PUBLIC_STRIPE_PUBLISHABLE=pk_live_xyz789
```

### "Empty value" warning

**Cause:** Variable has no value

**Fix:**
```bash
# ❌ Bad
DATABASE_URL=

# ✅ Good (if optional, document it)
DATABASE_URL=  # Optional, uses SQLite if not set

# ✅ Better
DATABASE_URL=postgresql://localhost/mydb
```

### File not found error

**Cause:** Specified .env file doesn't exist

**Fix:**
```bash
# Check file exists
ls -la .env

# Or create it
touch .env
```

## Troubleshooting

### Validation passes locally but fails in CI

**Check:**
1. Line endings (CRLF vs LF)
2. File encoding (UTF-8 expected)
3. File permissions
4. Python version (3.7+ required)

### Warnings about client-exposed variables

**This is intentional!** env-manager is warning you that variables like NEXT_PUBLIC_API_KEY will be visible in the browser.

**Options:**
1. Move secret to server-side variable (remove NEXT_PUBLIC_ prefix)
2. Use public/publishable keys only in client-exposed variables
3. If truly not a secret, ignore the warning

### .env.example generation replaces too much

env-manager is conservative about secrets. If it over-sanitizes:
1. Manually edit .env.example after generation
2. Use specific placeholder values in .env that won't trigger sanitization
