# Headless trusty stack (tm daemon, trusty-memory, trusty-search) for EVO.
# Refs #11054. Nothing is downloaded here: run ./fetch.sh dl first, which
# downloads and sha256-verifies the release tarballs into the build context.
#
#   ./fetch.sh docker/cloudtest/dl
#   docker build -t tm-trusty-cloud-cloudtest:<short-sha> docker/cloudtest
#
# No secret, token or .env is copied or set in any layer. Runtime has no network.

# ---- stage 1: verify and unpack the tarballs ------------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c AS unpack
COPY dl/*.tar.gz dl/*.tar.gz.sha256 /dl/
# Second sha256 check inside the build; the stage fails if any tarball differs.
RUN set -eux; \
    cd /dl; \
    sha256sum -c ./*.sha256; \
    mkdir -p /out /x; \
    for t in /dl/*.tar.gz; do tar -xzf "$t" -C /x; done; \
    for b in tm trusty-mpm trusty-memory trusty-search trusty-embedderd trusty-embedderd-py; do \
        install -m 0755 "$(find /x -type f -name "$b" | head -n 1)" /out/"$b"; \
    done

# ---- stage 2: runtime -----------------------------------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c

# Ubuntu archive only; no third-party repositories.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git tmux libssl3t64 ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

# Non-root daemon user; passwd home is the state volume. The daemons check that
# a socket peer has the same uid (SO_PEERCRED), so this is also the default USER
# for `docker exec`. Pass --build-arg UID=$(id -u) GID=$(id -g) so the host state
# dir is writable; do not override it with `docker run --user`. The base image's
# uid-1000 "ubuntu" user is removed first so the uid is free.
ARG UID=1000
ARG GID=1000
RUN (userdel -r ubuntu 2>/dev/null || true) \
    && groupadd --gid "${GID}" cloudtest \
    && useradd --uid "${UID}" --gid "${GID}" --no-create-home \
        --home-dir /data/home --shell /bin/bash cloudtest \
    && mkdir -p /data /models/fastembed \
    && chown cloudtest:cloudtest /data

COPY --from=unpack /out/ /usr/local/bin/
COPY entrypoint.sh check.sh /usr/local/bin/
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/check.sh

# One volume at /data holds all state. TRUSTY_DATA_DIR_OVERRIDE is not set.
# TRUSTY_SKIP_RAM_CHECK: the 16 GB guard would refuse a 6 GB container.
ENV HOME=/data/home \
    XDG_DATA_HOME=/data/xdg \
    FASTEMBED_CACHE_DIR=/models/fastembed \
    TRUSTY_SKIP_RAM_CHECK=1
VOLUME /data
USER cloudtest
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
