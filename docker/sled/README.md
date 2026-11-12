# sled: trusty stack behind the egress proxy (#11073)

The sled image runs `tm daemon`, `trusty-memory`, `trusty-search`, Claude Code
and `tailscaled` in one Ubuntu 26.04 container on an `--internal` network. The
only way out is one squid proxy with a host allowlist. Rules: DOC-90 and
ADR-0077 (PR #11072), owner rulings D1b and D20. Derived from
`docker/cloudtest/` (#11054), which stays unchanged.

## Names

| Thing | Name |
|---|---|
| App container and image | `tm-trusty-cloud-sled` |
| Proxy container and image | `tm-trusty-cloud-sled-egress` |
| Internal network | `tm-trusty-cloud-sled-internal`, subnet `10.250.78.0/24` |

The proxy name is the host in the Dockerfile's `HTTPS_PROXY`. The subnet is the
`internal_net` ACL in `proxy/squid.conf`. Change each pair together.

## Build

```bash
SHA=$(git rev-parse --short HEAD)
docker/sled/fetch.sh docker/sled/dl          # downloads and sha256-verifies; fails closed
docker build -t tm-trusty-cloud-sled-egress:$SHA docker/sled/proxy
docker build -t tm-trusty-cloud-sled:$SHA \
  --build-arg UID="$(id -u)" --build-arg GID="$(id -g)" docker/sled
```

The build reaches the network; the running sled does not. Claude Code is pinned
by `CLAUDE_CODE_VERSION`/`CLAUDE_CODE_SHA256`, Tailscale by
`TAILSCALE_VERSION`/`TAILSCALE_SHA256` (the static tarball from
pkgs.tailscale.com, which the Ubuntu archive does not carry). Any mismatch
fails the build.

## Secrets: files, read-only, never env

| File in the container | Holds | Read by |
|---|---|---|
| `/run/secrets/github-token` | one fine-grained GitHub token for this sled's repos | `gh-token-run`, per command |
| `/run/secrets/trusty-console-token` | console token (DOC-90 section 4) | nothing yet, see below |
| `/run/secrets/claude.env` (optional) | `CLAUDE_CODE_OAUTH_TOKEN=<value>` | `claude-run`, per command |

Mount each as a file with `-v <host-file>:<path>:ro`, mode `0400` or `0600`,
owned by the build `UID`. Never pass a token with `-e`, `--env-file` or a build
arg. `.dockerignore` excludes all three names. Check a file with `stat`, never
`cat`. `gh-token-run git fetch ...` and `gh-token-run gh api ...` put the GitHub
token into that one process; git reaches it through `gh auth git-credential`.

## Run

```bash
ST=~/sled-state/tm-trusty-cloud-sled
mkdir -p $ST/state $ST/secrets && chmod 700 $ST/state $ST/secrets
# put the two token files in $ST/secrets, mode 0400

docker network create --internal --subnet 10.250.78.0/24 tm-trusty-cloud-sled-internal

# proxy: attach both networks before start, so squid sees both interfaces
docker create --name tm-trusty-cloud-sled-egress --cpus 1 --memory 256m \
  --network tm-trusty-cloud-sled-internal tm-trusty-cloud-sled-egress:$SHA
docker network connect bridge tm-trusty-cloud-sled-egress
docker start tm-trusty-cloud-sled-egress

# app: internal network only, no -p
docker run -d --name tm-trusty-cloud-sled --cpus 4 --memory 6g \
  --network tm-trusty-cloud-sled-internal \
  -v $ST/state:/data \
  -v ~/.cache/fastembed:/models/fastembed:ro \
  -v $ST/secrets/github-token:/run/secrets/github-token:ro \
  -v $ST/secrets/trusty-console-token:/run/secrets/trusty-console-token:ro \
  tm-trusty-cloud-sled:$SHA
```

## Verify (DOC-90 section 9, V1)

```bash
docker exec -u sled tm-trusty-cloud-sled check.sh          # stack check
docker exec -u sled tm-trusty-cloud-sled exercise.sh       # Claude session, git fetch, gh api
docker/sled/check_v1.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
docker exec tm-trusty-cloud-sled-egress cat /var/log/squid/access.log
```

`exercise.sh --tailscale-login` also runs `tailscale up` with no auth key, which
asks the control plane for a login URL; the URL goes to the log redacted.
`check_v1.sh` runs on the docker host. It sends the example.com and raw-IP
probes, then grades `access.log`: every `TCP_TUNNEL` names a steady host, the
probes are `TCP_DENIED/403`, nothing reached crates.io, huggingface.co,
registry.npmjs.org, pypi.org or downloads.claude.ai, and `--noproxy` and DNS
fail. It prints PASS or FAIL per row and exits non-zero on any FAIL.

## Login profile (off by default)

`steady` allows `api.anthropic.com`, `github.com`, `api.github.com`,
`controlplane.tailscale.com`, `login.tailscale.com` and the DERP relays
(`^derp[0-9]+[a-z]*(-all)?\.tailscale\.com$`), `CONNECT :443` only. A raw-IP
`CONNECT` is denied before any allow. `login` adds `platform.claude.com` and
`claude.ai` for a Claude Code `/login`. The proxy starts in `steady` on every
start and restart.

```bash
docker exec tm-trusty-cloud-sled-egress sled-profile login     # on
ssh -t <host> docker exec -it -u sled tm-trusty-cloud-sled claude   # /login, paste the code
docker exec tm-trusty-cloud-sled-egress sled-profile steady    # off, right after
docker exec tm-trusty-cloud-sled-egress sled-profile show      # prints steady or login
```

Each switch is recorded in `/var/log/squid/profile.log`. `check_v1.sh` accepts
a login-host tunnel only inside a recorded login window. A new squid
configuration applies to new connections; `docker restart
tm-trusty-cloud-sled-egress` also drops open tunnels and returns to `steady`.

## Image environment

The `ENV` layer adds DOC-90 section 7 to cloudtest's: `TRUSTY_NO_UPDATE_CHECK=1`,
`TRUSTY_UV_FETCH=0`, `HF_HUB_OFFLINE=1`, `TRUSTY_MPM_PR_CLEANUP_INTERVAL=0`,
`TAGENT_NO_API=1`, `TRUSTY_CONSOLE_TOKEN_FILE=/run/secrets/trusty-console-token`,
plus `TS_NO_LOGS_NO_SUPPORT=true` for tailscaled and `GH_NO_UPDATE_NOTIFIER=1`.

Not yet enforced by the binaries:

- `TRUSTY_CONSOLE_TOKEN_FILE`: the console does not enforce it yet (#11074).
  Until it does, the console requires no token.
- `TAGENT_NO_API`: `tagent` does not enforce it yet (#6433). Until it does,
  nothing refuses `tagent --api`; the entrypoint never starts it.
- `HF_HUB_OFFLINE`: the pinned hf-hub does not read it (DOC-90 OQ-4); the
  `:ro` model cache and the allowlist carry the guarantee.

## Tailscale

`tailscaled` runs as the `sled` user with `--tun=userspace-networking` (no TUN
device, no capability), state in `/data/tailscale`, and the proxy env. The
internal network gives it no UDP route, so direct WireGuard and STUN cannot
work and peers are reached over DERP only. Joining a tailnet and `tailscale
serve` need an owner login (`tailscale up`, then the URL), not done yet.

Measured on a Linux docker host with Tailscale 1.104.6, logged out (DOC-90 section 6 marked
proxy support Unverified):

- Logged out and idle, `tailscaled` opened no connection.
- `tailscale up` used the proxy: `tshttpproxy` logged `CONNECT
  controlplane.tailscale.com:443 ... 200`, and squid logged one
  `TCP_TUNNEL/200` for that host. No other host reached squid.
- Its bootstrap-DNS fallback dialled DERP servers by raw IP on TCP 443
  directly, not through the proxy. Each dial failed with `network is
  unreachable` (no route out), so nothing left the sled.
- The login URL is written to `/data/logs/tailscaled.log` by `tailscaled`
  itself. Treat that log as sensitive until the node is logged in.

## Teardown

```bash
docker rm -f tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
docker network rm tm-trusty-cloud-sled-internal
docker rmi tm-trusty-cloud-sled:$SHA tm-trusty-cloud-sled-egress:$SHA
rm -rf ~/sled-state/tm-trusty-cloud-sled                # state and secret copies
test ! -e ~/sled-state/tm-trusty-cloud-sled && echo state-gone
```

Revoke the GitHub token and any Claude login on the provider side when the
sled is retired.
