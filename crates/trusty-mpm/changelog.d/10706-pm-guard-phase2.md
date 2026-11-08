Fixed

- pm-guard kill-server protection: removed `env -i` fallback that treated pre-expanded socket variables as fresh scratch servers, and added repo+content allowlist for `scripts/sandbox_*.sh`. Refused unsafe options on reader programs (`rg --pre`, `sort -o`, `uniq` output files, `less -o`, `sed` write/execute commands). Fixed symlink canonicalization in script trust verification and zombie process leak on git write failure.
