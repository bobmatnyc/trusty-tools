# trusty-browser parity fixtures

Captured from the Python trusty-browser 0.7.0 (branch `feat/trusty-browser-pypi-0.7.0`)
by `capture.py`, run through `capture.sh`. Versions are in `../manifest.json`.

Regenerate (macOS only):

```sh
uv venv /tmp/tb-venv --python 3.12
VIRTUAL_ENV=/tmp/tb-venv uv pip install <checkout>/python/trusty-browser
crates/trusty-browser/tests/fixtures/capture/capture.sh /tmp/tb-venv/bin/python crates/trusty-browser/tests/fixtures
```

A rerun writes identical bytes: nonces, ids, AES-GCM nonces, uuid4 and the audit
clock come from a seeded stream.

Safety:

- The Python process runs under a scratch `HOME` and a temp state dir. The live
  `~/.trusty/browser` and launchd `com.trusty.browser` are never touched.
- `capture.sh` creates a throwaway keychain in a temp dir and deletes it on exit.
  Every `security` call names that file explicitly. The default and search
  keychain lists are never read or changed, and the login keychain is never used.
  Python `keyring`'s macOS backend cannot target a keychain file (keyring #623),
  so the vault's `Keyring` is replaced by a `/usr/bin/security` bridge; the
  vault's own decode and AES-GCM code run unchanged.
- The only key material is throwaway: the AES key in `vault.json` and the
  browser keys there and in `proof_vectors.json` are derived from a fixed seed.

Contents: `registry.sqlite3` (two pairings, one non-ASCII email), `vault.json`
(test service name, AES key, pairing credentials), `proof_vectors.json` (HMAC
transcript bytes, key, MAC for both roles), `frames/` (one length-prefixed frame
per file, `index.json` names socket, direction and message), `refusals.json`,
`audit.jsonl`, `mcp/` (`initialize` and `tools/list` in connect and workflow mode).

MCP tool calls (phase 5): `capture_mcp.py` runs `trusty-browser-mcp` against a
scripted fake `agent.sock` (no broker, vault or keychain) and writes
`mcp/{connect,noroot,workflow}.exchange.json`, `mcp/validation.json` and
`../../src/mimetypes.tsv`:

```sh
/tmp/tb-venv/bin/python -I crates/trusty-browser/tests/fixtures/capture/capture_mcp.py --out crates/trusty-browser/tests/fixtures
```

Install layout (phase 4): `install/` holds Python 0.7.0's output under a scratch
`HOME=/tmp/p4-cap/home` (never the real one; `install-service` was not run).
`launchd-plist` gives `python.plist`; `install-host` gives the two launchers and
the two manifests; `extension-path` publishes the tree that `install_tree.py`
lists into `assets_fresh.tsv` (assets/ and extension/ lines only).
`install_scenario.py` builds the old `extension -> assets/<hash>` layout;
`extension-path` over it gives `assets_migrated.tsv`:

```sh
HOME=/tmp/p4-cap/home trusty-browser --state /tmp/p4-cap/home/.trusty/browser extension-path
python -I install_tree.py /tmp/p4-cap/home/.trusty/browser
python -I install_scenario.py /tmp/p4-cap/mig/state
HOME=/tmp/p4-cap/mig trusty-browser --state /tmp/p4-cap/mig/state extension-path
python -I install_tree.py /tmp/p4-cap/mig/state
```
