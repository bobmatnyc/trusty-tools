# Egress proxy for the cloudtest container (rung 2). Refs #11054.
# Allows CONNECT to api.anthropic.com:443 only; everything else is denied and logged.
#
#   docker build -t tm-trusty-cloud-egress:<short-sha> docker/cloudtest/proxy
#
# Same pinned base as the app image. No secret in any layer.
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c

# Ubuntu archive only; no third-party repositories.
RUN apt-get update \
    && apt-get install -y --no-install-recommends squid \
    && rm -rf /var/lib/apt/lists/*

COPY squid.conf /etc/squid/squid.conf
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/entrypoint.sh \
    && mkdir -p /var/log/squid \
    && chown -R proxy:proxy /var/log/squid

# 3128 is not published; the app container reaches it over the internal network.
EXPOSE 3128
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
