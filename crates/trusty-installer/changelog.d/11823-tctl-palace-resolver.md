Fixed
- `tctl ensure` now names the project palace through the shared trusty-common resolver, so a project with a git remote gets the same `owner-repo` palace that `tm` and the memory hook read, instead of the folder name. Like the daemon, it ignores an exported `TRUSTY_MEMORY_PALACE`, so the name it sends is the one the daemon accepts (#11823).
- `tctl ensure` reports a malformed, empty or invalid palace pin as a failed `palace-create` step naming the pin file, instead of silently creating a palace under the folder name (#11823).
