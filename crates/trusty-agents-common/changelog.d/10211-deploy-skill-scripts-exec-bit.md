Fixed

- `skills::deployer` writes a skill's `scripts/*` files (and any source file already executable) as mode 755 and restores the bit on an unchanged managed script; `agents::manifest::atomic_write_executable` and `skills::deployer::is_executable_skill_file` are the shared contract (Refs [#10211](https://github.com/bobmatnyc/trusty-tools/issues/10211))
