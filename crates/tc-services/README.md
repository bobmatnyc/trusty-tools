# tc-services

Host-agnostic service adapters for the Granola API and Google Workspace. Each
adapter emits OpenAI-compatible function schemas and dispatches tool calls, so
a Rust host can register the tools in its own tool abstraction.

**License**: MIT

`publish = false`. No workspace package depends on this crate today.

## Modules

### `granola`

Native `reqwest` client over the Granola public API (v1).

- `GRANOLA_TOOL_NAMES`: `granola_list`, `granola_get`,
  `granola_get_transcript`, `granola_search`, `granola_get_shared_document`.
- `granola_services()` returns one `GranolaService` per tool name.
- `GranolaService::name()`, `schema()` and `execute(args)`; `execute`
  returns a `GranolaOutcome`.
- `GRANOLA_API_KEY` (`ENV_GRANOLA_API_KEY`) is read at call time.

### `gworkspace`

Bridge over `trusty-gworkspace` for the Calendar and Tasks subset.

- `GWORKSPACE_TOOL_NAMES`: `manage_calendars`, `manage_events`,
  `query_free_busy`, `manage_task_lists`, `manage_tasks`, `list_tasks`,
  `complete_task`.
- `gworkspace_services()` returns one `GworkspaceService` per tool name.
- `GworkspaceService::name()`, `schema()` and `execute(args)`; `execute`
  returns a `GworkspaceOutcome`.
- Schemas come from `trusty_gworkspace::tools`; calls go to the matching
  `trusty_gworkspace::api::services` handler.

## Usage

```rust,ignore
use tc_services::granola::granola_services;

for service in granola_services() {
    let schema = service.schema(); // register with the host
    let _ = (service.name(), schema);
}
```

## Testing

```bash
cargo test -p tc-services --no-fail-fast
```

## See Also

- `crates/trusty-gworkspace/README.md` for the Google Workspace client
- `docs/tc-services/` for the specification
