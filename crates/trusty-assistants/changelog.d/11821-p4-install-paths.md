Changed
- The shared MCP server file is `<config_dir>/.trusty-tools/mcp/servers.toml`, following the install paths. A broken `install.toml` fails the MCP tools with an error naming the install file instead of writing under `$HOME` (#11821).
