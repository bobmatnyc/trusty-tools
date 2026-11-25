Fixed
- `ta list` prints the assistant instances, one id per line. An instance is a directory under `~/trusty-agents/` that holds `instructions.md`; other entries there are not listed. With none, it prints one line on stderr and exits 0 (#11742).
- `ta ask <unknown-instance>` names the available instances in its error, and still exits non-zero (#11742).
- `--help` and `-h` on `ask`, `handoff`, `list`, `mcp-serve` and `ask-daemon` print that verb's usage and exit 0; they were usage errors before. `ta config --help` names the program `ta config` (#11742).
- A cold `ta ask` prints `ta: starting <instance> assistant…` on stderr while its daemon starts. stdout still carries only the answer, and a warm ask prints no progress line (#11742).
- The crate README documents `ta ask`, `ta handoff`, `ta list`, the first step for a new user and where instances live (#11742).
