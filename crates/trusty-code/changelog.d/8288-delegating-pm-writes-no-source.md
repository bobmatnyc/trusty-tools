Security
- A delegating PM can no longer write the bound project's source: its own `write_file`, `write_files`, `edit` and `bash` calls are denied, not asked, on both `run-task` paths, and `delegate_to_agent` naming the PM's own card runs that card with the same lock. `--permission-mode allow-asks` and a remembered grant do not lift it. A `--no-delegate` run keeps those tools (#8288).
