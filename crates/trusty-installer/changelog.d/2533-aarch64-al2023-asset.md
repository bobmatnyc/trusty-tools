Fixed

- On an arm64 Linux host with glibc below 2.39, `trusty-search` / `trusty-analyze` installs now download the new `aarch64-linux-al2023` release asset instead of the `aarch64-unknown-linux-gnu` one, which is built on Ubuntu 24.04 and cannot load there (Amazon Linux 2023 on Graviton being the confirmed case). Mirrors the existing x86_64 routing (follow-up to [#2533](https://github.com/bobmatnyc/trusty-tools/issues/2533), predecessor [#4822](https://github.com/bobmatnyc/trusty-tools/pull/4822)).
