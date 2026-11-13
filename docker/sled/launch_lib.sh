# shellcheck shell=bash disable=SC2016,SC2034  # jq programs are single-quoted; launch.sh reads the constants
# launch_lib.sh — checks for docker/sled/launch.sh (#11156). Sourced, never run.
#
# Why: the sled boundary (DOC-90 §1, §12) holds only when the app sits on an
#   --internal network behind the egress proxy, hardened, with no published
#   port, no second network and no token in its environment. These functions
#   decide that from `docker inspect` JSON, so the decision is testable without
#   docker.
# What: each check prints one "REFUSE <reason>" line per violation and nothing
#   when clean. An unparsable input is itself a violation (fail-closed).
# Test: scripts/check_sled_launch.sh drives each check through launch.sh with a
#   stub docker.

# Docker Engine 26.0.0 is the first release line that fixes CVE-2024-29018:
# containers on an --internal network could resolve external names through the
# daemon's embedded DNS and so leak data. The internal network is the boundary.
SLED_MIN_DOCKER=26.0.0

# Env names allowed although they look token-like: each names a file path,
# never a value (DOC-90 TOK-1).
SLED_ENV_ALLOW='TRUSTY_CONSOLE_TOKEN_FILE'
SLED_ENV_DENY_RE='TOKEN|SECRET|KEY|PASSWORD|_PAT$'

# Caps squid needs: the entrypoint chowns its logs (CHOWN), squid drops to the
# proxy user (SETUID, SETGID), and root appends to proxy-owned logs and signals
# squid from sled-profile (DAC_OVERRIDE, KILL).
SLED_PROXY_CAPS='CHOWN DAC_OVERRIDE KILL SETGID SETUID'

# version_ge A B — true when dotted version A >= B. Ignores a suffix after the
# third number ("26.1.4+dfsg1"). A version it cannot read is false.
version_ge() {
    local a b i x y
    a="$(printf '%s' "$1" | sed -E 's/^([0-9]+)\.([0-9]+)\.([0-9]+).*/\1 \2 \3/')"
    b="$(printf '%s' "$2" | sed -E 's/^([0-9]+)\.([0-9]+)\.([0-9]+).*/\1 \2 \3/')"
    case "$a" in *[!0-9\ ]*|'') return 1 ;; esac
    for i in 1 2 3; do
        x="$(echo "$a" | cut -d' ' -f"$i")"; y="$(echo "$b" | cut -d' ' -f"$i")"
        [ "$x" -gt "$y" ] && return 0
        [ "$x" -lt "$y" ] && return 1
    done
    return 0
}

# squid_subnet FILE — the single `acl internal_net src` value of squid.conf.
# Fails unless there is exactly one, so the network can never drift from it.
squid_subnet() {
    local v
    v="$(awk '$1=="acl" && $2=="internal_net" && $3=="src" {print $4}' "$1" 2>/dev/null)"
    if [ -z "$v" ] || [ "$(printf '%s\n' "$v" | wc -l | tr -d ' ')" != 1 ]; then return 1; fi
    printf '%s\n' "$v"
}

# mode_owner PATH — "<octal mode> <uid>" for GNU or BSD stat.
mode_owner() {
    stat -c '%a %u' "$1" 2>/dev/null || stat -f '%Lp %u' "$1" 2>/dev/null
}

# check_private PATH UID KIND — refuse unless PATH is a KIND (f or d), not a
# symlink, owned by UID, with no group or other bits (files: 600 or stricter).
check_private() {
    local p="$1" uid="$2" kind="$3" mo mode owner
    if [ -L "$p" ] || { [ "$kind" = f ] && [ ! -f "$p" ]; } || { [ "$kind" = d ] && [ ! -d "$p" ]; }; then
        echo "REFUSE secrets: ${p} is missing, a symlink, or the wrong type"; return
    fi
    if ! mo="$(mode_owner "$p")"; then echo "REFUSE secrets: cannot stat ${p}"; return; fi
    mode="${mo% *}"; owner="${mo#* }"
    case "$mode" in ''|*[!0-7]*) echo "REFUSE secrets: cannot read the mode of ${p}"; return ;; esac
    [ "$owner" = "$uid" ] || echo "REFUSE secrets: ${p} is owned by uid ${owner}, not ${uid}"
    if [ "$kind" = f ]; then
        [ $((8#$mode & 8#7177)) -eq 0 ] || echo "REFUSE secrets: ${p} has mode ${mode}; need 600 or stricter"
    else
        [ $((8#$mode & 8#077)) -eq 0 ] || echo "REFUSE secrets: ${p} has mode ${mode}; need 700"
    fi
}

# Shared jq prelude: $c is the one inspected container; anything else fails.
SLED_JQ_ONE='if (type=="array" and length==1) then .[0] else error("not one container") end'

# jq_refusals JSON PROGRAM [jq args] — run PROGRAM on JSON; a jq error is a refusal.
jq_refusals() {
    local json="$1" prog="$2" out
    shift 2
    if ! out="$(printf '%s' "$json" | jq -r "$@" "${SLED_JQ_ONE} | ${prog}" 2>&1)"; then
        echo "REFUSE inspect: cannot read docker inspect output"; return
    fi
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/REFUSE /'
    return 0
}

# check_running JSON — the container is running.
check_running() {
    jq_refusals "$1" 'if .State.Running == true then empty else "state: not running" end'
}

# check_ports JSON — no published or bound port.
check_ports() {
    jq_refusals "$1" '
      ( if (.HostConfig.PortBindings // {} | to_entries | map(select(.value != null and (.value|length) > 0)) | length) > 0
        then "ports: port bindings \(.HostConfig.PortBindings|keys|join(","))" else empty end ),
      ( if .HostConfig.PublishAllPorts == true then "ports: publish-all" else empty end ),
      ( if (.NetworkSettings.Ports // {} | to_entries | map(select(.value != null and (.value|length) > 0)) | length) > 0
        then "ports: published \(.NetworkSettings.Ports|keys|join(","))" else empty end )'
}

# check_networks JSON MODE NETS — NetworkMode equals MODE and the attached
# networks are exactly NETS (space-separated; empty for container: mode).
check_networks() {
    jq_refusals "$1" '
      ( if (.HostConfig.NetworkMode // "") as $m | ($mode | split("|") | index($m)) then empty
        else "network: mode \(.HostConfig.NetworkMode // "unset"), want \($mode)" end ),
      ( (.NetworkSettings.Networks // {} | keys | sort) as $have
        | ($nets | split(" ") | map(select(length > 0)) | sort) as $want
        | if $have == $want then empty
          else "network: attached \($have|join(",")), want \($want|join(","))" end )' \
        --arg mode "$2" --arg nets "$3"
}

# check_privileged JSON — not privileged, no device passed through.
check_privileged() {
    jq_refusals "$1" '
      ( if .HostConfig.Privileged == true then "privileged: privileged mode" else empty end ),
      ( if (.HostConfig.Devices // [] | length) > 0 then "privileged: host device passed through" else empty end )'
}

# check_caps JSON ALLOWED — CapDrop has ALL and CapAdd is a subset of ALLOWED.
check_caps() {
    jq_refusals "$1" '
      ( if ((.HostConfig.CapDrop // []) | map(ascii_upcase) | index("ALL")) then empty
        else "caps: CapDrop lacks ALL" end ),
      ( ($allowed | split(" ") | map(select(length > 0))) as $ok
        | (.HostConfig.CapAdd // [])[]
        | (ascii_upcase | sub("^CAP_"; "")) as $c
        | if ($ok | index($c)) then empty else "caps: added \($c)" end )' \
        --arg allowed "$2"
}

# check_namespaces JSON — no host or borrowed pid, ipc or uts namespace.
check_namespaces() {
    jq_refusals "$1" '
      (.HostConfig.PidMode // ""), (.HostConfig.IpcMode // ""), (.HostConfig.UTSMode // ""),
      (.HostConfig.UsernsMode // ""), (.HostConfig.NetworkMode // "")
      | select(. == "host" or (startswith("container:") and (. as $m | ($netmode | split("|") | index($m)) | not)))
      | "namespace: \(.)"' --arg netmode "${2:-}"
}

# check_hardening JSON — no-new-privileges, read-only root (unless RO=no),
# pids, memory, swap equal to memory, and a cpu limit.
check_hardening() {
    jq_refusals "$1" '
      .HostConfig as $h
      | ( if (($h.SecurityOpt // []) | map(select(test("^no-new-privileges(:true|=true)?$"))) | length) > 0
          then empty else "hardening: no-new-privileges unset" end ),
        ( if $ro == "yes" and $h.ReadonlyRootfs != true then "hardening: root filesystem is writable" else empty end ),
        ( if ($h.PidsLimit // 0) > 0 then empty else "hardening: no pids limit" end ),
        ( if ($h.Memory // 0) > 0 then empty else "hardening: no memory limit" end ),
        ( if ($h.Memory // 0) > 0 and $h.MemorySwap == $h.Memory then empty else "hardening: swap not capped at memory" end ),
        ( if ($h.NanoCpus // 0) > 0 then empty else "hardening: no cpu limit" end )' \
        --arg ro "${2:-yes}"
}

# check_mounts JSON ALLOW TMPFS — every mount is in ALLOW, lines of
# "<destination> <source> <rw true|false>"; every tmpfs path is in TMPFS.
check_mounts() {
    jq_refusals "$1" '
      ($allow | split("\n") | map(select(length > 0) | split(" ") | {(.[0]): {src: .[1], rw: .[2]}}) | add // {}) as $a
      | ( .Mounts // [] | .[]
          | . as $m
          | if ($a[$m.Destination] == null) then "mounts: \($m.Destination) is not allowed"
            elif ($a[$m.Destination].src != $m.Source and $a[$m.Destination].src != ($m.Name // ""))
              then "mounts: \($m.Destination) comes from \($m.Source), want \($a[$m.Destination].src)"
            elif (($m.RW|tostring) != $a[$m.Destination].rw) then "mounts: \($m.Destination) rw=\($m.RW), want \($a[$m.Destination].rw)"
            else empty end ),
        ( .HostConfig.Tmpfs // {} | keys[]
          | select(. as $t | ($tmpfs | split(" ") | index($t)) | not)
          | "mounts: tmpfs \(.) is not allowed" )' \
        --arg allow "$2" --arg tmpfs "$3"
}

# check_env JSON — no env name that looks like a token, except SLED_ENV_ALLOW.
check_env() {
    jq_refusals "$1" '
      (.Config.Env // [])[] | split("=")[0]
      | select(test($deny; "i")) | select(. as $n | ($allow | split(" ") | index($n)) | not)
      | "env: \(.) looks like a token"' \
        --arg deny "$SLED_ENV_DENY_RE" --arg allow "$SLED_ENV_ALLOW"
}

# check_network_json JSON SUBNET MEMBERS — the internal network is internal,
# IPv6 off, has exactly SUBNET, and holds no container outside MEMBERS.
check_network_json() {
    jq_refusals "$1" '
      ( if .Internal == true then empty else "network: not --internal" end ),
      ( if .EnableIPv6 == true then "network: IPv6 enabled" else empty end ),
      ( [.IPAM.Config[]?.Subnet] as $s
        | if $s == [$subnet] then empty else "network: subnets \($s|join(",")), want \($subnet)" end ),
      ( .Containers // {} | to_entries[] | .value.Name
        | select(. as $n | ($members | split(" ") | index($n)) | not)
        | "network: unexpected container \(.)" )' \
        --arg subnet "$2" --arg members "$3"
}
