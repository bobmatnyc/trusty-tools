Fixed
- The `tm hook --pm-guard` force-push floor no longer reads a shell redirect (`2>&1`, `> log`, `</dev/null`) or the `)` closing a subshell as a refspec, so `git push -f origin 2>&1` on the default branch is denied. `--no-force` no longer clears `--force-with-lease`, and `--recurse-submodules` takes its value as a separate word (#8878).
