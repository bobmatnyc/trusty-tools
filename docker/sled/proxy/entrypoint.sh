#!/usr/bin/env bash
# Start squid in the foreground in the steady profile; mirror access.log to
# stdout so `docker logs` shows allows and denials. Every start, restart
# included, returns to steady and records it in profile.log. Squid runs a copy
# of squid.conf bound to the internal-network address only (sled-bind-addr,
# #11156). Derived from docker/cloudtest/proxy. Refs #11054 #11073
set -euo pipefail

mkdir -p /var/log/squid /etc/squid/profile.d
touch /var/log/squid/access.log /var/log/squid/cache.log /var/log/squid/profile.log
cp /etc/squid/steady.conf /etc/squid/profile.d/active.conf
echo "$(date +%s) steady" >> /var/log/squid/profile.log
chown -R proxy:proxy /var/log/squid

rm -f /run/squid.pid
# Fails closed: no single internal address, no squid.
sled-bind-addr /etc/squid/squid.conf /run/squid-run.conf
chmod 0644 /run/squid-run.conf
squid -k parse -f /run/squid-run.conf
tail -n 0 -F /var/log/squid/access.log &
exec squid -N -f /run/squid-run.conf
