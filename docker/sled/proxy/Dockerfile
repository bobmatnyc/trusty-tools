# Egress proxy for the sled (DOC-90 section 6). Refs #11054 #11073.
# Derived from docker/cloudtest/proxy. Allowlist and owner rulings: squid.conf.
#
#   docker build -t tm-trusty-cloud-sled-egress:<short-sha> docker/sled/proxy
#
# Same pinned base as the app image. No secret in any layer.
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c

# Ubuntu archive only; no third-party repositories.
RUN apt-get update \
    && apt-get install -y --no-install-recommends squid \
    && rm -rf /var/lib/apt/lists/*

COPY squid.conf login.conf steady.conf /etc/squid/
COPY entrypoint.sh sled-profile /usr/local/bin/
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/sled-profile \
    && mkdir -p /var/log/squid /etc/squid/profile.d \
    && cp /etc/squid/steady.conf /etc/squid/profile.d/active.conf \
    && chown -R proxy:proxy /var/log/squid

# 3128 is not published; the app container reaches it over the internal network.
EXPOSE 3128
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
