Added
- New `trusty-browser` crate skeleton (0.7.0) with parity fixtures captured from the Python 0.7.0 broker and MCP server, and a test target that parses every fixture
- Library layer of the `trusty-browser` port: Python-exact JSON (`pyjson`), the length-framed codec, refusal types, the pairing HMAC proof, the `audit.jsonl` writer with 5 MiB rotation, the `registry.sqlite3` schema and pairing rows, and the AES-GCM vault with a pluggable Keychain store; parity tests compare each against the Python 0.7.0 fixtures byte for byte
