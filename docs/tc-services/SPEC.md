# tc-services — Service Layer Adapters

**Purpose**: Host-agnostic service adapters for the Granola API and Google Workspace.

**License**: MIT

## Service Adapters

### Granola Integration
- Native HTTP client over the Granola public API (v1)
- Five tools: list, get, get transcript, search, get shared document
- `GRANOLA_API_KEY` read from the environment at call time

### Google Workspace Integration
- Calendar and Tasks subset, bridged over `trusty-gworkspace`
- Schemas from `trusty_gworkspace::tools`, dispatch to `trusty_gworkspace::api::services`

## Architecture

- **Shape**: one service per published tool, each with `name()`, `schema()` and `execute(args)`
- **Schemas**: OpenAI-compatible function schemas as `serde_json::Value`; hosts own any typed bridge
- **Errors**: each `execute` returns an outcome type that carries success or an error message
- **Async runtime**: tokio-based async I/O

## Configuration

- `GRANOLA_API_KEY` for Granola
- Google Workspace OAuth credentials, through `trusty-gworkspace`

## See Also

- `crates/tc-services/README.md` for API details
- `crates/trusty-gworkspace/README.md` for Google Workspace integration
