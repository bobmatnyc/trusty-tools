# sled: trusty stack behind the egress proxy (#11073)

The sled image runs `tm daemon`, `trusty-memory`, `trusty-search` and Claude
Code in one Ubuntu 26.04 container on an `--internal` network. `tailscaled`
runs in a sidecar container, as its own uid, in the app's network namespace
(#11156). The only way out is one squid proxy with a host allowlist. Rules: DOC-90 and
ADR-0076 (PR #11072), owner rulings D1b and D20. Derived from
`docker/cloudtest/` (#11054), which stays unchanged.

## Names

| Thing | Name |
|---|---|
| App container and image | `tm-trusty-cloud-sled` |
| Proxy container and image | `tm-trusty-cloud-sled-egress` |
| Tailscale sidecar container and image | `tm-trusty-cloud-sled-tailscale`, state volume of the same name |
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
docker build --target tailscale-sidecar -t tm-trusty-cloud-sled-tailscale:$SHA \
  --build-arg UID="$(id -u)" docker/sled
```

The sidecar's `tailscale` user has uid `TS_UID` (default 1999). The build
refuses a `TS_UID` of 0 or equal to `UID`.

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

## Launch (the only supported way)

Every sled reaches the internet only through the egress proxy surface, and
`docker/sled/launch.sh --tag $SHA` is the only supported way to start one
(DOC-90 §12). It creates the `--internal` network on the `internal_net`
subnet of `proxy/squid.conf`, starts the proxy and the app hardened (all caps
dropped, `no-new-privileges`, read-only root, pids, memory, swap and cpu
limits), starts the Tailscale sidecar when its image exists, and refuses on a
published port, a second network, privileged mode, an added cap, a host
namespace, a mount outside its allowlist, a token-like env name (allowlist:
`TRUSTY_CONSOLE_TOKEN_FILE`), a secret file looser than 600 or not owned by
`SLED_UID`, or Docker older than 26.0.0. The proxy entrypoint binds squid to
its internal-network address only, so nothing on the default bridge gets an
answer on 3128. After `check.sh` and `exercise.sh`, launch runs V1 and V5 and
prints `RESULT: READY (V1 PASS, V5 PASS; V2-V4 not scripted, see #11167)`.
`launch.sh --check-only` re-inspects a running sled; it acts only on
containers carrying the `trusty.sled.launch` label launch sets. It needs `jq`
on the host. `scripts/check_sled_launch.sh` tests it offline with a stub
`docker`.

## Run

Reference only: the commands below show what `launch.sh` does. Do not start a
sled with them.

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

# tailscaled sidecar: the app's network namespace, its own uid and filesystem
docker run -d --name tm-trusty-cloud-sled-tailscale --cpus 0.5 --memory 256m \
  --network container:tm-trusty-cloud-sled \
  -v tm-trusty-cloud-sled-tailscale:/var/lib/tailscale \
  tm-trusty-cloud-sled-tailscale:$SHA
```

The sidecar lives in the app's network namespace, so restart it after any
restart of the app. Mount nothing from `$ST/secrets` into it.

## Verify (DOC-90 section 9, V1)

```bash
docker exec -u sled tm-trusty-cloud-sled check.sh          # stack check
docker exec -u sled tm-trusty-cloud-sled exercise.sh       # Claude session (or the credential-free API probe), git fetch, gh api
docker/sled/check_v1.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
docker/sled/check_tailscale.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-tailscale   # V5
docker exec tm-trusty-cloud-sled-egress cat /var/log/squid/access.log
```

`check_tailscale.sh` (V5) runs on the docker host. As the app's `sled` uid it
runs `tailscale up`, `set --ssh`, `funnel` and `nc`; each must fail to reach
a daemon. It checks that the app has no `tailscaled` process or binary and no
writable socket, and that the sidecar runs `tailscaled` as another uid with a
0700 socket directory, SSH off, no Funnel, and no TCP listener (SOCKS5,
HTTP proxy or debug) in the shared network namespace.
`scripts/check_sled_tailscale_isolation.sh` checks the same layout statically,
without docker, in the `sled-static` job of `ci.yml`, which `CI gate` covers.
`check_v1.sh` runs on the docker host, from the repo checkout. It reads
`squid.conf`, `login.conf` and `active.conf` from the running proxy and fails
on any rule that differs from its V1 policy or from `docker/sled/proxy/`. It
checks that the app has no default route, reaches no TCP port on the internal
network's gateway, and reaches the proxy on 3128 only. It sends the example.com
and raw-IP probes, then grades `access.log`: each probe added a fresh
`TCP_DENIED/403` line, every `TCP_TUNNEL` names a steady host, at least one
tunnel reached `api.anthropic.com`, nothing reached crates.io, huggingface.co,
registry.npmjs.org, pypi.org or downloads.claude.ai, and `--noproxy` and DNS
fail to connect. It prints PASS or FAIL per row and exits non-zero on any FAIL;
an unreadable log, an empty `profile.log` or a stopped container is a FAIL.

Logs: the entrypoint, `check.sh` and `exercise.sh` run with `umask 077`, and
`/data/logs` is mode 0700, so every log there is 0600 and owner-only.
`tailscaled` logs to the sidecar's `docker logs`, outside the app.

### Unverified

- A real Claude Code session. No login is stored in the test sled, so
  `exercise.sh` prints `UNVERIFIED` and runs a credential-free request to
  `api.anthropic.com` (HTTP 401) as the substitute. It shows the steady path,
  not the hosts a logged-in session uses.
- Relay-only (DERP) tailnet traffic and `tailscale serve`. No tailnet has been
  joined, so neither has been exercised.

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

`tailscaled` runs in the `tm-trusty-cloud-sled-tailscale` sidecar as the
`tailscale` user (`TS_UID`), with `--tun=userspace-networking` (no TUN device,
no capability), state in the sidecar's own volume, and the proxy env. It
shares only the app's network namespace, so `tailscale serve` reaches the
console on `127.0.0.1:7788`. Its LocalAPI socket,
`/var/run/tailscale/tailscaled.sock`, is a path in the sidecar's filesystem in
a 0700 directory, so no process in the app can open it. No sled agent or
Claude Code process can run `tailscale up`, `login`, `set`, `funnel`, `serve`
or `nc` against it, and none gets a read-only status either. The app image
carries the `tailscale` client only, so V5 can show it reaches no daemon.
An earlier image kept state in `/data/tailscale`; the entrypoint deletes it.

The internal network gives `tailscaled` no UDP route, so direct WireGuard and
STUN should not work and peers should be reached over DERP only. That is
UNVERIFIED: no tailnet has been joined.

### Joining a tailnet (operator)

Run every Tailscale command from the docker host through the sidecar:

```bash
# one-time, tagged, pre-approved auth key; read from stdin, never argv or a file in the app
docker exec -i tm-trusty-cloud-sled-tailscale \
  tailscale up --auth-key=file:/dev/stdin --advertise-tags=tag:sled < <host-key-file>
docker exec tm-trusty-cloud-sled-tailscale tailscale serve --bg http://127.0.0.1:7788
docker exec tm-trusty-cloud-sled-tailscale tailscale status
```

`--auth-key=file:/dev/stdin` is UNVERIFIED with Tailscale 1.104.6; no tailnet
has been joined. The repo cannot enforce the tailnet side. Before a sled
joins, the tailnet owner must have:

- **A tagged pre-auth key.** One-time, short expiry, carrying the sled tag
  (`tag:sled` above, or the tailnet's own name for it). Never a reusable key,
  and never an untagged key, which would make the node act as the user who
  made the key.
- **Tailnet lock** on, with the sled's node signed by a trusted lock key (or
  the auth key pre-signed). A node that is not signed cannot talk to the
  tailnet.
- **An ACL policy that denies SSH and Funnel to the sled tag:** no `ssh`
  rule whose `dst` names the sled tag, and no `nodeAttrs` entry granting
  `funnel` to it. Tailscale denies both by default, so the requirement is that
  no rule grants them. Grant inbound only to the console port, from the
  owner's devices.

Measured on a Linux docker host with Tailscale 1.104.6, logged out, when
`tailscaled` still ran in the app container (DOC-90 section 6 marked proxy
support Unverified):

- Logged out and idle, `tailscaled` opened no connection.
- `tailscale up` used the proxy: `tshttpproxy` logged `CONNECT
  controlplane.tailscale.com:443 ... 200`, and squid logged one
  `TCP_TUNNEL/200` for that host. No other host reached squid.
- Its bootstrap-DNS fallback dialled DERP servers by raw IP on TCP 443
  directly, not through the proxy. Each dial failed with `network is
  unreachable` (no route out), so nothing left the sled.
- The login URL is written to the `tailscaled` log by `tailscaled` itself.
  It now goes to the sidecar's `docker logs`. Treat that log as sensitive
  until the node is logged in.

## Residual risk

- Token files are readable by every process that runs as `sled`. The daemons,
  Claude Code and any code they run share that uid, so any of them can read
  `/run/secrets/*`. Scope the GitHub token to the minimum: the sled's repos
  and the permissions the work needs.
- Ruling D20 residual, accepted by the owner. Code in the app cannot drive the
  sled's own `tailscaled` (#11156). It can still bring a `tailscaled` binary
  of its own, for example in a git clone from github.com, and run it in
  userspace mode through the proxy. That node could join another tailnet,
  tunnel out over DERP, and take inbound traffic over that tailnet, Funnel
  included. The proxy cannot tell tailnets apart, and tailnet lock or ACLs on
  the owner's tailnet do not govern another tailnet. V5 fails while such a
  process runs (no `tailscaled` in the app); it cannot stop one being
  started.
- A tunnel opened during a login window stays open after `sled-profile
  steady`: a reconfigure applies to new connections only. `docker restart
  tm-trusty-cloud-sled-egress` drops it.
- Raw-IP forms with letters, such as hex (`0x01010101`, `0x7f.0.0.1`), are not
  matched by the `raw_ip` ACL. The final `http_access deny all` denies them,
  because no allow names them. Octal and bare-integer forms are digits and
  dots only, so `raw_ip` matches them.

- Host requirement: the docker host's firewall must stop containers on the
  sled's `--internal` bridge from reaching host services. Only the proxy peer
  on 3128 is reachable. The V1 row "app reaches no TCP port on the internal
  gateway" verifies this. Do not run the sled on a host where that row fails.

## Teardown

```bash
docker rm -f tm-trusty-cloud-sled-tailscale tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
docker network rm tm-trusty-cloud-sled-internal
docker volume rm tm-trusty-cloud-sled-tailscale        # sidecar state: the node key
docker rmi tm-trusty-cloud-sled:$SHA tm-trusty-cloud-sled-egress:$SHA tm-trusty-cloud-sled-tailscale:$SHA
rm -rf ~/sled-state/tm-trusty-cloud-sled                # state and secret copies
test ! -e ~/sled-state/tm-trusty-cloud-sled && echo state-gone
```

Revoke the GitHub token and any Claude login on the provider side when the
sled is retired.
