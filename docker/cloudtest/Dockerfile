# Headless trusty stack (tm daemon, trusty-memory, trusty-search) for EVO.
# Refs #11054. Nothing is downloaded here: run ./fetch.sh dl first, which
# downloads and sha256-verifies the release tarballs into the build context.
#
#   ./fetch.sh docker/cloudtest/dl
#   docker build -t tm-trusty-cloud-cloudtest:<short-sha> docker/cloudtest
#
# No secret, token or .env is copied or set in any layer. Rung 2 runs on an
# --internal network; the only way out is the squid proxy (see proxy/).

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

# ---- stage 2: Claude Code CLI -----------------------------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c AS claude
# Claude Code: Anthropic's native linux-x64 binary from downloads.claude.ai, as
# claude.ai/install.sh fetches it (layout: <base>/<version>/manifest.json and
# <base>/<version>/linux-x64/claude). The pin below is checked against the
# manifest and the binary; any mismatch fails the build. No npm.
ARG CLAUDE_CODE_VERSION=2.1.311
ARG CLAUDE_CODE_SHA256=86dc1fcc69498159eaf15f446613e847010f9036bfbbdffd4c49350be8489ada
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates jq \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    case "${CLAUDE_CODE_VERSION}" in ''|*[!0-9.]*) echo "bad CLAUDE_CODE_VERSION" >&2; exit 1;; esac; \
    case "${CLAUDE_CODE_SHA256}" in *[!0-9a-f]*|'') echo "bad CLAUDE_CODE_SHA256" >&2; exit 1;; esac; \
    [ "${#CLAUDE_CODE_SHA256}" -eq 64 ]; \
    base="https://downloads.claude.ai/claude-code-releases/${CLAUDE_CODE_VERSION}"; \
    mkdir -p /out /m; \
    curl -fsSL --proto '=https' --tlsv1.2 "${base}/manifest.json" -o /m/manifest.json; \
    [ "$(jq -re '.version' /m/manifest.json)" = "${CLAUDE_CODE_VERSION}" ]; \
    [ "$(jq -re '.platforms["linux-x64"].checksum' /m/manifest.json)" = "${CLAUDE_CODE_SHA256}" ]; \
    curl -fsSL --proto '=https' --tlsv1.2 "${base}/linux-x64/claude" -o /m/claude; \
    echo "${CLAUDE_CODE_SHA256}  /m/claude" | sha256sum -c -; \
    [ "$(stat -c %s /m/claude)" = "$(jq -re '.platforms["linux-x64"].size' /m/manifest.json)" ]; \
    install -m 0755 /m/claude /out/claude

# ---- stage 3: runtime -----------------------------------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c

# Ubuntu archive only; no third-party repositories.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git tmux libssl3t64 ca-certificates libstdc++6 curl \
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
COPY --from=claude /out/claude /usr/local/bin/claude
COPY entrypoint.sh check.sh rung2.sh claude-run /usr/local/bin/
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/check.sh \
        /usr/local/bin/rung2.sh /usr/local/bin/claude-run

# One volume at /data holds all state. TRUSTY_DATA_DIR_OVERRIDE is not set.
# TRUSTY_SKIP_RAM_CHECK: the 16 GB guard would refuse a 6 GB container.
ENV HOME=/data/home \
    XDG_DATA_HOME=/data/xdg \
    FASTEMBED_CACHE_DIR=/models/fastembed \
    TRUSTY_SKIP_RAM_CHECK=1 \
    HTTPS_PROXY=http://tm-trusty-cloud-egress:3128 \
    https_proxy=http://tm-trusty-cloud-egress:3128 \
    NO_PROXY=localhost,127.0.0.1,::1 \
    no_proxy=localhost,127.0.0.1,::1 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_TELEMETRY=1 \
    DISABLE_ERROR_REPORTING=1 \
    DISABLE_AUTOUPDATER=1 \
    ENABLE_CLAUDEAI_MCP_SERVERS=false
VOLUME /data
USER cloudtest
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
