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

## Rung 2: Claude Code PM session with egress only to Anthropic

The app container sits on an `--internal` network with no route out. A squid
proxy container sits on that network and on the default bridge, and allows
`CONNECT api.anthropic.com:443` only. No port is published. The token is the
single line `CLAUDE_CODE_OAUTH_TOKEN=<value>` in a host file, mounted read-only;
place it with `RUNG2-TOKEN-CHECKLIST.md`. Never pass it with `--env-file`, `-e`
or a build arg.

```bash
SHA=$(git rev-parse --short HEAD)
docker build -t tm-trusty-cloud-egress:$SHA docker/cloudtest/proxy
docker build -t tm-trusty-cloud-cloudtest:$SHA \
  --build-arg UID="$(id -u)" --build-arg GID="$(id -g)" docker/cloudtest   # after fetch.sh

docker network create --internal --subnet 10.250.77.0/24 tm-trusty-cloud-internal

# proxy: attach both networks before start, so squid sees both interfaces
docker create --name tm-trusty-cloud-egress --cpus 1 --memory 256m \
  --network tm-trusty-cloud-internal tm-trusty-cloud-egress:$SHA
docker network connect bridge tm-trusty-cloud-egress
docker start tm-trusty-cloud-egress

# app: internal network only
docker run -d --name tm-trusty-cloud-cloudtest --cpus 4 --memory 6g \
  --network tm-trusty-cloud-internal \
  -v ~/cloudtest/tm-trusty-cloud-cloudtest/state:/data \
  -v ~/.cache/fastembed:/models/fastembed:ro \
  -v ~/cloudtest/tm-trusty-cloud-cloudtest/secrets/claude.env:/run/secrets/claude.env:ro \
  tm-trusty-cloud-cloudtest:$SHA

docker exec -u cloudtest tm-trusty-cloud-cloudtest check.sh     # stores the memory sentinel
docker exec -u cloudtest tm-trusty-cloud-cloudtest rung2.sh
docker exec tm-trusty-cloud-egress cat /var/log/squid/access.log   # evidence: TCP_TUNNEL/200 api.anthropic.com, TCP_DENIED/403 example.com
```

The proxy subnet ACL in `proxy/squid.conf` matches `10.250.77.0/24`; change both
together. If a deny line names `platform.claude.com` or `claude.ai`, uncomment
the matching `acl` and `http_access allow` lines in `squid.conf` and rebuild the
proxy. Claude Code is the native `linux-x64` binary from downloads.claude.ai,
pinned by `CLAUDE_CODE_VERSION` and `CLAUDE_CODE_SHA256` build args and checked
against Anthropic's `manifest.json`; the build fails on any mismatch.
`claude-run` reads the token file into one `claude` process only; the daemons
never see it.

### Cleanup when rung 2 ends

```bash
rm -f ~/cloudtest/tm-trusty-cloud-cloudtest/secrets/claude.env
docker rm -f tm-trusty-cloud-cloudtest tm-trusty-cloud-egress
docker network rm tm-trusty-cloud-internal
docker rmi tm-trusty-cloud-cloudtest:$SHA tm-trusty-cloud-egress:$SHA
```
