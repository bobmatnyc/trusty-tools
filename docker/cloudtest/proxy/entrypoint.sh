#!/usr/bin/env bash
# Start squid in the foreground; mirror access.log to stdout so `docker logs`
# shows allows and denials. Evidence file: /var/log/squid/access.log. Refs #11054
set -euo pipefail

mkdir -p /var/log/squid
touch /var/log/squid/access.log /var/log/squid/cache.log
chown -R proxy:proxy /var/log/squid

squid -k parse -f /etc/squid/squid.conf
tail -n 0 -F /var/log/squid/access.log &
exec squid -N -f /etc/squid/squid.conf
