#!/usr/bin/env bash
# Start squid in the foreground; mirror access.log to stdout so `docker logs`
# shows allows and denials. Evidence file: /var/log/squid/access.log. Refs #11054
set -euo pipefail

mkdir -p /var/log/squid
touch /var/log/squid/access.log /var/log/squid/cache.log
chown -R proxy:proxy /var/log/squid

# Refuse to start if a dstdomain ACL lacks -n (IP-literal CONNECT via PTR). Refs #11054
if grep -E '^[[:space:]]*acl[[:space:]]+[^[:space:]]+[[:space:]]+dstdom(ain|_regex)[[:space:]]' /etc/squid/squid.conf \
    | grep -vE '[[:space:]]-n[[:space:]]'; then
    echo "squid.conf: dstdomain ACL above lacks -n; refusing to start" >&2
    exit 1
fi
squid -k parse -f /etc/squid/squid.conf
tail -n 0 -F /var/log/squid/access.log &
exec squid -N -f /etc/squid/squid.conf
