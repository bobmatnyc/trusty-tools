---
spec_refs:
  - id: SPEC-MCPCRED-01~draft
    path: docs/specs/remote-mcp-credential-delivery.md
    anchor: SPEC-MCPCRED-01~draft
  - id: SPEC-MCPCRED-02~draft
    path: docs/specs/remote-mcp-credential-delivery.md
    anchor: SPEC-MCPCRED-02~draft
  - id: SPEC-MCPCRED-03~draft
    path: docs/specs/remote-mcp-credential-delivery.md
    anchor: SPEC-MCPCRED-03~draft
  - id: SPEC-MCPCRED-04~draft
    path: docs/specs/remote-mcp-credential-delivery.md
    anchor: SPEC-MCPCRED-04~draft
  - id: SPEC-MCPCRED-05~draft
    path: docs/specs/remote-mcp-credential-delivery.md
    anchor: SPEC-MCPCRED-05~draft
---

# DOC-45 — Remote MCP Credential Delivery for Fleet Sessions

**Status:** Draft
**Subsystem:** trusty-mpm — MCP bridging / credential injection / session launch
**Owner:** Engineering (trusty-tools platform)
**Last-updated:** 2026-07-18
**Spec ID:** `SPEC-MCPCRED-01~draft` … `SPEC-MCPCRED-05~draft` (DOC-45)
**Epic:** issue #2739 / PR #3033 (custom remote MCP servers in fleet sessions)
**Builds on:**
- Issue #2739 (native trusty MCP servers don't reach fleet sessions)
- PR #3033 (bridge custom remote + local MCPs; **security decision flagged but unresolved**)
- DOC-38 Spec-Linked Documentation standard
- DOC-34 managed-session `CLAUDE_CONFIG_DIR` provisioning
- Claude Code documentation (`.mcp.json` format, `${VAR}` expansion, OAuth, `headersHelper`)
**Cross-ref:** 
- `crates/trusty-mpm/src/core/session_launch/custom_mcp.rs` (PR #3033 injector, currently rejects headers-bearing remotes fail-closed)
- `crates/trusty-mpm/src/core/session_launch/native_mcp.rs` (secret-routing reused by custom injector)
- `crates/trusty-mpm/src/core/runtime/claude_code.rs::env_bin_prefix` (session env composition, SINGLE choke point for all session spawns)
- Claude Code `.mcp.json` reader (upstream docs at code.claude.com/docs/en/mcp.md)

---

## 1. Problem {#SPEC-MCPCRED-01~draft}

**ID:** SPEC-MCPCRED-01~draft
**Status:** Draft

PR #3033 extends #2739 to bridge custom remote MCP servers into fleet sessions but flags a **security decision as unresolved**: remote servers carrying credentials in `headers` fields are rejected **fail-closed** (no partial injection), and credentials embedded in URLs are not detected or stripped. There is **no established out-of-band delivery channel** for HTTP header/URL secrets analogous to `.env.local` for stdio `env` fields.

### Goals of this spec

Design and gate a **safe, least-surprise credential-delivery mechanism** that:
- Keeps credentials OUT of git-tracked `.mcp.json` files and logs
- Supports the three primary transport patterns (OAuth via `/mcp`, static headers, short-lived tokens via `headersHelper`)
- Composes cleanly with fleet-session provisioning and the project-scope trust model
- Detects and prevents common secret-leak patterns (URL-embedded secrets)
- Provides actionable feedback when credential injection fails

### Out-of-scope (implementation)

This spec defines **what the system must do** (the behavior contract and threat model), not implementation. Code changes, test harnesses, or new CLI commands are follow-ups governed by this spec but not part of it.

---

## 2. Threat Model & Design Principles {#SPEC-MCPCRED-02~draft}

**ID:** SPEC-MCPCRED-02~draft
**Status:** Draft

### 2.1 Threat scenarios

1. **Git-tracked secret leak** — a credential hardcoded into `.mcp.json` and committed
2. **Log/transcript leak** — a secret emitted in session logs, diagnostics, or a user's `claude --debug` output
3. **Process-inspection leak** — a credential visible in `ps -ef` or a crash dump
4. **Revocation gap** — changing a remote server's credential requires manual intervention across multiple files/locations
5. **Least-surprise violation** — a user adds a remote server and expects credentials to "just work" without discovering a cryptic rejection or needing to learn a new env-var convention

### 2.2 Design hierarchy (preference-ordered mechanisms)

Remote servers MUST use one of the following, in order of preference:

**Mechanism A: OAuth 2.0 via `/mcp` endpoint** (PREFERRED)
- The remote server exposes an OAuth 2.0 endpoint at `/.well-known/oauth-provider.json` or similar
- Claude Code's `.mcp.json` reader supports `oauth2` credential type (or equivalent) pointing to the remote's OAuth endpoint
- Token is stored in the system keychain, auto-refreshed; no static secret needed
- **Gatekeeping:** tm MUST detect OAuth-capable servers (via HEAD request or documented marker in the registry) and prefer them over static headers

**Mechanism B: Static credential injected into session environment** (ACCEPTABLE)
- The credential (API key, token, basic-auth password) lives in the **tm-managed registry** (`<CLAUDE_CONFIG_DIR>/.claude.json` via `tm mcp add`, or project `.trusty-mpm/manifest.toml` for project-scoped servers)
- At **session spawn time**, tm injects a `${TM_MCP_<SERVER_NAME>_TOKEN}`-style environment variable into the launched Claude Code process via `env_bin_prefix`
- The `.mcp.json` written to the workspace references the variable, e.g., `"headers": { "Authorization": "Bearer ${TM_MCP_DUETTO_TOKEN}" }`
- Claude Code's config reader expands `${VAR}` and `${VAR:-default}` from the process environment (documented upstream; spec MUST verify this empirically in acceptance tests)

**Mechanism C: Dynamic headers via `headersHelper` command** (ESCAPE HATCH)
- The remote server requires short-lived tokens or SSO-integration that cannot be stored as a static secret
- `tm mcp` allows registration with a `headersHelper` field pointing to a local command (e.g., `get-oauth-token`, `get-saml-headers`)
- At connection time, Claude Code invokes the command; it returns a JSON object with header fields
- tm does NOT manage or cache the token — the command is operator-supplied; revocation is the command's responsibility

### 2.3 Fallback behavior (fail-closed, not silent)

If a remote server cannot be registered via any of the above mechanisms:
- **Log explicitly** that the server was skipped and why (e.g., "Server `<name>` requires headers; use OAuth or specify a static credential via `tm mcp set <name> --token …`")
- **Do not inject the server** — fail-closed, never leak a partial/incorrect registration
- **Surface in `tm doctor`** — list skipped servers and their blockers

---

## 3. Requirements {#SPEC-MCPCRED-03~draft}

**ID:** SPEC-MCPCRED-03~draft
**Status:** Draft

### 3.1 Registry & secret storage

- **User scope:** `tm mcp add <name> --url <http-url> [--token <secret> | --oauth]` stores the server in the managed registry (currently `<CLAUDE_CONFIG_DIR>/.claude.json` `mcpServers` map, or a dedicated `secrets.json` if registry hardening requires separate file perms).
- **Project scope:** `[mcp.custom.<name>]` table in `.trusty-mpm/manifest.toml` (reuses existing manifest-precedence machinery).
- **At-rest hardening (MVP phase):** Registry writes with `0600` file permissions (user read/write only); existing registries are remediated on next startup or via `tm mcp repair --hardenhow`.
- **Keyring integration (hardening phase):** Long-term: move secret storage to the system keychain (trusty-common's `keyring_store.rs`, currently unwired); interim MVP uses plaintext at `0600`.

### 3.2 Session environment injection {#SPEC-MCPCRED-03-SESSION-ENV~draft}

- **Injection point:** `crates/trusty-mpm/src/core/runtime/claude_code.rs::env_bin_prefix` (~125–133 in the original code), the **single choke point** where the tmux `send-keys` command is composed with session environment.
- **Mechanism:** For each remote server with a static credential, emit a `TM_MCP_<NORMALIZED_NAME>_TOKEN='<secret>'` environment variable in the prefix. Naming: uppercase, alphanumeric + underscore, with non-alphanumeric replaced by `_` (e.g., `duetto-code-intelligence` → `TM_MCP_DUETTO_CODE_INTELLIGENCE_TOKEN`).
- **Shell quoting:** Use POSIX shell-quoting rules (trusty-mpm's existing `shell_single_quote` utility or similar); secrets containing `'` must be escaped.
- **Ordering:** POSIX `env` grammar requires `-u` flag BEFORE NAME=VALUE assignments; if unsetting vars (e.g., `env -u ANTHROPIC_API_KEY`), emit them BEFORE all credential assignments.
- **Spawn + resume parity:** Both `spawn_command` and `resume_command` MUST compose the prefix identically; diff the two code paths and add tests for parity.

### 3.3 Workspace `.mcp.json` generation

- **Reference pattern:** When a remote server is to be injected with a static credential, the workspace `.mcp.json` entry for that server MUST reference the variable, not the secret itself:
  ```json
  {
    "url": "https://duetto-api.example.com",
    "headers": {
      "Authorization": "Bearer ${TM_MCP_DUETTO_CODE_INTELLIGENCE_TOKEN}"
    }
  }
  ```
- **OAuth marker:** If the server is OAuth-capable, add a **comment or metadata field** (if `.mcp.json` reader permits) marking it as such, so future UX/diagnostics can prefer it: `"_authType": "oauth"` or similar (non-load-bearing; for visibility only).
- **Atomic writes:** Use atomic-with-backup helper (trusty-common's `write_atomic` if available, or fs-write-to-temp + rename); do NOT use plain `fs::write`.

### 3.4 URL-embedded-secret handling {#SPEC-MCPCRED-04~draft}

**ID:** SPEC-MCPCRED-04~draft (related to 3.4)
**Status:** Draft

- **Detection heuristic:** Before writing a remote server to `.mcp.json`, scan the URL for common secret patterns:
  - Query parameters named `key`, `token`, `api_key`, `apikey`, `secret`, `password`, `auth`, `credential`
  - Userinfo segment (anything before `@` in `https://user:pass@host`)
  - Non-empty password in userinfo
- **Policy:**
  - If any pattern is detected, **refuse to write the entry** (fail-closed)
  - Log: `"Server '<name>' URL contains a credential pattern (found in query/userinfo). Extract it to a secure location and use 'tm mcp set <name> --token …' instead. Details: <matched_pattern>."`
  - **Offer automatic rewrite** (UX, not required in MVP): prompt user to move the secret to the registry, then rewrite URL with `${VAR}` reference
  - Heuristic may be conservative (false positives OK; false negatives bad) but MUST include the operator's real example (`duetto-code-intelligence` with API key in `?key=…` query param)

### 3.5 Project-scope trust & credential access control {#SPEC-MCPCRED-05~draft}

**ID:** SPEC-MCPCRED-05~draft
**Status:** Draft

- **Trust seeding:** When a remote server is injected into a fleet session, its entry in the workspace `.mcp.json` is seeded into `CLAUDE_CONFIG_DIR/.claude.json` trust record (or equivalent, per DOC-34 provisioning).
  - User-scope servers: trust seeded once (per user, or per session — TBD in phasing)
  - Project-scope servers: trust seeded when project is trusted via `tm project trust`
- **Credential visibility:** The session environment variables (`TM_MCP_*`) are composed AFTER trust is seeded, so the `.mcp.json` references the variable name, not the secret itself. This keeps the `.mcp.json` entry trust-safe.
- **Revocation:** Changing or revoking a credential via `tm mcp set <name> --token <new>` immediately affects all NEW session spawns (environment recomposition). Existing running sessions retain their old environment snapshot. This is acceptable (sessions are ephemeral; old sessions will wind down naturally).

---

## 4. Acceptance Tests {#SPEC-MCPCRED-04~draft}

**ID:** SPEC-MCPCRED-04~draft (spans multiple sections; grouping here for brevity)
**Status:** Draft

All tests MUST pass before this spec transitions to Accepted.

### 4.1 Tmux environment inheritance (EMPIRICAL, CRITICAL)

**Test:** `env_bin_prefix_tmux_inherit`
- **Setup:** Compose an `env_bin_prefix` with a session variable `TM_MCP_FOO_TOKEN='secret123'`
- **Action:** Spawn a tmux session via the composed prefix; inside the session, run `echo $TM_MCP_FOO_TOKEN`
- **Assertion:** The variable is visible in the spawned session with the correct value
- **Why:** Verifies that Claude Code (a child process of tmux) inherits the injected environment correctly. This is standard Unix semantics, but confirming it empirically prevents regress.

### 4.2 Static credential injection into `.mcp.json` references

**Test:** `remote_server_static_credential_injection`
- **Setup:** Register a remote server via `tm mcp add duetto --url https://api.example.com --token mytoken123`
- **Action:** Spawn a fleet session for a test project
- **Assertion:**
  - Workspace `.mcp.json` contains the server entry with `"headers": { "Authorization": "Bearer ${TM_MCP_DUETTO_TOKEN}" }` (or equivalent)
  - Session environment contains `TM_MCP_DUETTO_TOKEN='mytoken123'`
  - `tm doctor` lists the server as injected with source `user-scope`

### 4.3 URL-embedded-secret rejection

**Test:** `url_embedded_secret_rejected`
- **Setup:** Attempt `tm mcp add evil --url "https://api.example.com?key=supersecret"`
- **Action:** Attempt to register or inject
- **Assertion:**
  - Entry is rejected (not written to registry or `.mcp.json`)
  - Log message includes the detection pattern and remediation instructions

### 4.4 Project-scope server bridges correctly

**Test:** `project_scope_custom_mcp_bridges`
- **Setup:** Define `[mcp.custom.my-server]` in `.trusty-mpm/manifest.toml` for a test project
- **Action:** Spawn a fleet session
- **Assertion:**
  - Server appears in workspace `.mcp.json` with project scope
  - Server does NOT appear in sessions of OTHER projects

### 4.5 OAuth-capable server detection and preference

**Test:** `oauth_server_marked_and_preferred`
- **Setup:** Register a remote server known to support OAuth
- **Action:** Inspect the server entry in `.mcp.json` and the registry
- **Assertion:**
  - Server is NOT rejected for missing a static credential
  - Server metadata (if applicable) marks it as OAuth-capable
  - UX/logs indicate: "Server uses OAuth; token managed by Claude Code keychain"

### 4.6 Claude Code config reader expands `${VAR}` from environment

**Test:** `claude_code_config_reader_var_expansion`
- **Setup:** Write a test `.mcp.json` with `"headers": { "Authorization": "Bearer ${TEST_VAR}" }`; set `TEST_VAR=testvalue` in the process environment
- **Action:** Invoke `claude mcp list` (or equivalent introspection)
- **Assertion:**
  - The variable is expanded to the real value
  - (If unexpanded, document as a **blocking empirical finding** — claude Code does not support this; design must pivot)

### 4.7 Registry file permissions harden to 0600

**Test:** `registry_file_permissions_0600`
- **Setup:** Add a credential via `tm mcp add`
- **Action:** Check the file permissions of `<CLAUDE_CONFIG_DIR>/.claude.json`
- **Assertion:**
  - Permissions are `-rw-------` (0600, user only)

### 4.8 Spawn + resume env parity

**Test:** `spawn_resume_env_prefix_parity`
- **Setup:** Compose environment prefixes for both `spawn_command` and `resume_command` with the same fleet session
- **Action:** Run both code paths; capture the tmux command strings
- **Assertion:**
  - The credential assignments are identical (same variables, same order, same quoting)

---

## 5. Phasing {#SPEC-MCPCRED-05~draft}

**ID:** SPEC-MCPCRED-05~draft
**Status:** Draft

### Phase 1: MVP (blocks PR #3033 merge gate)

- ✅ Static credential injection via environment variables (Mechanism B)
- ✅ URL-embedded-secret detection and rejection
- ✅ Workspace `.mcp.json` generation with `${VAR}` references
- ✅ Registry file permissions hardened to 0600
- ✅ Acceptance tests 4.1–4.3, 4.7–4.8 pass
- ✅ Documentation: `tm mcp --help` and tm-cli-operations skill updated with credential guidance

### Phase 2: OAuth & UX (post-MVP)

- Mechanism A: OAuth detection and preference marking
- Acceptance test 4.5 passes
- Mechanism C: `headersHelper` support (CLI + bridge injection)
- `tm doctor` surface skipped servers and their blockers
- Acceptance test 4.6 passes (conditional: if Claude Code supports OAuth natively)

### Phase 3: Keyring integration (hardening)

- Migrate secret storage from plaintext (0600) to system keychain
- Unwire trusty-common's `keyring_store.rs` (currently unused)
- Backward-compat migration: prompt on first daemon start to move secrets to keyring
- Acceptance test: verify no plaintext credentials in any serialized file after migration

---

## 6. Open Questions / Owner-Decision Checklist

These items MUST be decided by the spec owner (Bob) before Phase 1 implementation begins:

1. **Credential naming convention**: Use `TM_MCP_<NORMALIZED_NAME>_TOKEN` or a different pattern? (E.g., `TM_MCP_CRED_<NAME>`, `TRUSTY_MCP_*`, etc.)
   - ⬜ **Decision:** ___________________

2. **OAuth fallback**: If a server is marked as OAuth-capable but Claude Code fails to connect (e.g., missing keychain setup), should tm offer a static-credential fallback, or fail-closed?
   - ⬜ **Decision:** ___________________

3. **User-scope trust seeding cadence**: Trust-seed user-scope servers once per daemon lifetime, or once per session spawn? (Once per daemon = faster; once per session = more robust to trust revocation.)
   - ⬜ **Decision:** ___________________

4. **Keyring backend**: For Phase 3, target the system keychain (macOS Keychain, Linux `secret-service`, Windows Credential Manager) or adopt a neutral abstraction (e.g., `keyring` Python library)?
   - ⬜ **Decision:** ___________________

5. **URL-secret heuristic scope**: Extend heuristic to detect common API key patterns in other URL segments (fragment, hostname, port)? Or keep it to query/userinfo only?
   - ⬜ **Decision:** ___________________

6. **headersHelper sandboxing (Phase 2)**: If the `headersHelper` command is user-supplied, should tm sandbox it (e.g., with restricted PATH, no shell interpretation) or trust it as operator-owned code?
   - ⬜ **Decision:** ___________________

---

## 7. References

- **Issue #2739:** tm mcp add doesn't reach fleet sessions
- **PR #3033:** bridge custom remote+local MCP servers into fleet sessions
- **DOC-34:** Managed sessions launch with a tm-owned CLAUDE_CONFIG_DIR
- **DOC-38:** Spec-Linked Documentation standard
- **Claude Code MCP documentation:** code.claude.com/docs/en/mcp.md (variable expansion, OAuth, headersHelper)
- **trusty-common crate:** `keyring_store.rs` (unwired keyring backend for Phase 3)
- **trusty-mpm source:** 
  - `crates/trusty-mpm/src/core/session_launch/custom_mcp.rs` (current injector, PR #3033)
  - `crates/trusty-mpm/src/core/runtime/claude_code.rs` (env_bin_prefix)
  - `crates/trusty-mpm/src/core/session_launch/native_mcp.rs` (secret-routing primitives)
