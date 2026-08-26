Breaking
- **`TurnMemorySink` and `RecallSessionTool` take the trusty-memory socket path rather than a base URL** (#6286, ADR-0032), and `memory_envelope::call_tool_wrapped` takes a `&Path`. `TurnMemorySink::base_url` is `socket`
- The env override that pins the daemon is `TRUSTY_MEMORY_SOCKET`, not `TRUSTY_MEMORY_URL` — it names a socket path now, because there is no listener for a URL to address
