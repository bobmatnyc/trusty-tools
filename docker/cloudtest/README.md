# cloudtest: headless trusty stack on EVO (#11054)

Runs `tm daemon`, `trusty-memory` and `trusty-search` in one Ubuntu 26.04
container. Unix sockets only, no network, no published ports. Binaries come from
the released tarballs (trusty-mpm 1.12.2, trusty-memory 0.36.7,
trusty-search 0.63.1); the Dockerfile downloads nothing.

## Build

```bash
docker/cloudtest/fetch.sh docker/cloudtest/dl     # downloads and sha256-verifies; fails closed
docker build -t tm-trusty-cloud-cloudtest:<short-sha> \
  --build-arg UID="$(id -u)" --build-arg GID="$(id -g)" docker/cloudtest
```

`fetch.sh` uses `gh` (or `~/.local/bin/gh`, else curl). The build re-checks every
`.sha256`. The UID build args make the daemon user (`cloudtest`, home
`/data/home`) own the host state directory.

## Run

```bash
mkdir -p ~/cloudtest/tm-trusty-cloud-cloudtest/state
docker run -d --name tm-trusty-cloud-cloudtest --cpus 4 --memory 6g --network none \
  -v ~/cloudtest/tm-trusty-cloud-cloudtest/state:/data \
  -v ~/.cache/fastembed:/models/fastembed:ro \
  tm-trusty-cloud-cloudtest:<short-sha>
```

No `-p`. Mount only `~/.cache/fastembed`, read-only. Never mount
`~/.cache/huggingface`. Do not use `docker run --user`.

## Check

The daemons accept a socket peer only if it has their uid (SO_PEERCRED), so exec
as the daemon user:

```bash
docker exec -u cloudtest tm-trusty-cloud-cloudtest check.sh
docker restart tm-trusty-cloud-cloudtest
docker exec -u cloudtest tm-trusty-cloud-cloudtest check.sh --verify-persist
docker exec -u cloudtest tm-trusty-cloud-cloudtest sh -c 'cat /data/logs/check-*.log'
```

Each step prints PASS or FAIL; raw output goes to `/data/logs/check-<timestamp>.log`.
Exit code is non-zero if any step fails. Search indexing tries semantic first and
falls back to `--lexical-only`; the mode is printed and saved in
`/data/work/search-mode`. Daemon logs: `/data/logs/{tm,trusty-memory,trusty-search,supervisor}.log`.

## Persistence

State lives in the `/data` bind mount and survives `docker restart`. Nothing
survives an EVO reboot: the container does not restart on its own, and the state
is not backed up. Treat both as gone after a reboot.
