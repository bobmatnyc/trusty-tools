Added

- In-process `secret://` resolution for `tm secrets exec` ([#7525](https://github.com/bobmatnyc/trusty-tools/issues/7525), DOC-74 §9.4, §15.8):
  - `store::resolve_reference` resolves one reference: an unscoped `secret://KEY` tries the project vault, then the owner vault; `secret://<owner>/KEY` and `secret://<owner>/<repo>/KEY` read only the vault they name. A miss is `SecretsError::NotFound`.
  - With `agent_parent = true`, a key not flagged "agents may use" is refused with `SecretsError::AgentUseRefused`, before the backend is read.
  - `store::resolve_env` resolves an ordered `NAME=raw` env map all or nothing. It validates POSIX names and parses every reference before any read, and passes non-reference values through. Any bad entry fails the whole call with `InvalidEnvEntry` or `EnvResolution`, naming the entry and the reference, never a value.
  - `store::parse_dotenv` reads a documented `.env` subset (`KEY=value`, `export`, `#` comments, single and double quotes). It rejects multi-line values, escapes, `$` expansion and duplicate names with `SecretsError::DotenvSyntax`, naming the line number, never the line.
