#!/usr/bin/env bash
# Start squid in the foreground in the steady profile; mirror access.log to
# stdout so `docker logs` shows allows and denials. Every start, restart
# included, returns to steady and records it in profile.log. Derived from
# docker/cloudtest/proxy. Refs #11054 #11073
set -euo pipefail

mkdir -p /var/log/squid /etc/squid/profile.d
touch /var/log/squid/access.log /var/log/squid/cache.log /var/log/squid/profile.log
cp /etc/squid/steady.conf /etc/squid/profile.d/active.conf
echo "$(date +%s) steady" >> /var/log/squid/profile.log
chown -R proxy:proxy /var/log/squid

rm -f /run/squid.pid
squid -k parse -f /etc/squid/squid.conf
tail -n 0 -F /var/log/squid/access.log &
exec squid -N -f /etc/squid/squid.conf
