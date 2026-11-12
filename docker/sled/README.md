# sled: trusty stack behind the egress proxy (#11073)

The sled image runs `tm daemon`, `trusty-memory`, `trusty-search`, Claude Code
and `tailscaled` in one Ubuntu 26.04 container on an `--internal` network. The
only way out is one squid proxy with a host allowlist. Rules: DOC-90 and
ADR-0076 (PR #11072), owner rulings D1b and D20. Derived from
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
docker exec -u sled tm-trusty-cloud-sled exercise.sh       # Claude session (or the credential-free API probe), git fetch, gh api
docker/sled/check_v1.sh tm-trusty-cloud-sled tm-trusty-cloud-sled-egress
docker exec tm-trusty-cloud-sled-egress cat /var/log/squid/access.log
```

`exercise.sh --tailscale-login` also runs `tailscale up` with no auth key, which
asks the control plane for a login URL; the URL goes to the log redacted.
`check_v1.sh` runs on the docker host, from the repo checkout. It reads
`squid.conf`, `login.conf` and `active.conf` from the running proxy and fails
on any rule that differs from its V1 policy or from `docker/sled/proxy/`. It
checks that the app has no default route, reaches no TCP port 1-65535 and gets
no answer on UDP 53, 123, 443 or 41641 from the internal network's gateway, and
reaches the proxy on 3128 only. It sends raw `CONNECT`s for hex, dotted-hex,
octal and integer forms of `1.1.1.1` and requires a 403 or 400 for each. It
sends the example.com
and raw-IP probes, then grades `access.log`: each probe added a fresh
`TCP_DENIED/403` line, every `TCP_TUNNEL` names a steady host, at least one
tunnel reached `api.anthropic.com`, nothing reached crates.io, huggingface.co,
registry.npmjs.org, pypi.org or downloads.claude.ai, and `--noproxy` and DNS
fail to connect. It prints PASS or FAIL per row and exits non-zero on any FAIL;
an unreadable log, an empty `profile.log` or a stopped container is a FAIL.

Logs: the entrypoint, `check.sh` and `exercise.sh` run with `umask 077`, and
`/data/logs` is mode 0700, so every log there is 0600 and owner-only. The
`tailscaled` log holds a live login URL until the node is logged in.

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

`tailscaled` runs as the `sled` user with `--tun=userspace-networking` (no TUN
device, no capability), state in `/data/tailscale`, and the proxy env. The
internal network gives it no UDP route, so direct WireGuard and STUN should
not work and peers should be reached over DERP only. That is UNVERIFIED: no
tailnet has been joined. Joining a tailnet and `tailscale serve` need an owner
login (`tailscale up`, then the URL), not done yet.

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

## Residual risk

- Token files are readable by every process that runs as `sled`. The daemons,
  Claude Code and any code they run share that uid, so any of them can read
  `/run/secrets/*`. Scope the GitHub token to the minimum: the sled's repos
  and the permissions the work needs.
- Ruling D20 residual, accepted by the owner. The allowed Tailscale hosts let
  code in the sled run `tailscale up --auth-key=<other>`, join another tailnet
  and tunnel out over DERP. The controls are tailnet lock or ACL tags on the
  owner's tailnet; the proxy cannot tell tailnets apart.
- A tunnel opened during a login window stays open after `sled-profile
  steady`: a reconfigure applies to new connections only. `docker restart
  tm-trusty-cloud-sled-egress` drops it.
- Raw-IP forms: squid 7.7 rewrites hex, dotted-hex and integer literals
  (`0x01010101`, `0x01.0x01.0x01.0x01`, `16843009`) to dotted decimal before
  it matches ACLs, so `raw_ip` denies them; octal forms are digits and dots
  only, so `raw_ip` matches them as written (measured, #11156). V1 sends each
  form as a raw `CONNECT` and fails on any status but 403 or 400. A PTR record
  that maps an IP to an allowed name is not tested.

- Host requirement: the docker host's firewall must drop all traffic from the
  sled's `--internal` bridge to the host. Only the proxy peer on 3128 is
  reachable. V1 checks this by behavior, not by reading the firewall rules: it
  connects from the app to every TCP port 1-65535 on the network's gateway
  (256 parallel workers, 1 s per port, at most about 5 minutes, bound 900 s)
  and sends UDP probes to 53, 123, 443 and 41641. An open port, a TCP refusal,
  a UDP reply or an ICMP port-unreachable is a FAIL; only a silent drop or no
  route passes. Each worker also connects to the proxy's 3128, which must
  open, and the embedded DNS at 127.0.0.11 must answer the UDP probe, so a
  broken probe cannot pass. A short count, a missing tool or a scan past the
  bound is a FAIL. A UDP service that answers nothing, such as WireGuard, is
  not distinguishable from a drop. Do not run the sled on a host where a
  gateway row fails.

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
