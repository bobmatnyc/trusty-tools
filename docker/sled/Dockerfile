# Sled image: headless trusty stack (tm daemon, trusty-memory, trusty-search),
# Claude Code and tailscaled behind the egress proxy (DOC-90, ADR-0077).
# Derived from docker/cloudtest. Refs #11054 #11073. Owner ruling D1b
# (2026-11-11 21:23Z, "Accept all eight as recommended"); ruling D20 (OQ-3)
# adds tailscaled. Nothing from trusty-tools is downloaded here: run fetch.sh
# first, which downloads and sha256-verifies the release tarballs.
#
#   docker/sled/fetch.sh docker/sled/dl
#   docker build -t tm-trusty-cloud-sled:<short-sha> docker/sled
#
# No secret, token or .env is copied or set in any layer. Tokens are files
# mounted read-only at run time under /run/secrets (see README.md).

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

# ---- stage 2: Claude Code CLI (as docker/cloudtest) -------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c AS claude
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

# ---- stage 3: tailscale and tailscaled (ruling D20) ------------------------
# Tailscale's static amd64 tarball from pkgs.tailscale.com, pinned by version
# and sha256 (the published .sha256 for this version); any mismatch fails the
# build. Not in the Ubuntu archive. Only the build reaches pkgs.tailscale.com.
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c AS tailscale
ARG TAILSCALE_VERSION=1.104.6
ARG TAILSCALE_SHA256=82a85429f733e52fd1a95dedb2c8dd8cd46d6603504e566a56b1779c9a7a4809
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    case "${TAILSCALE_VERSION}" in ''|*[!0-9.]*) echo "bad TAILSCALE_VERSION" >&2; exit 1;; esac; \
    case "${TAILSCALE_SHA256}" in *[!0-9a-f]*|'') echo "bad TAILSCALE_SHA256" >&2; exit 1;; esac; \
    [ "${#TAILSCALE_SHA256}" -eq 64 ]; \
    name="tailscale_${TAILSCALE_VERSION}_amd64"; \
    mkdir -p /out /t; \
    curl -fsSL --proto '=https' --tlsv1.2 "https://pkgs.tailscale.com/stable/${name}.tgz" -o /t/ts.tgz; \
    echo "${TAILSCALE_SHA256}  /t/ts.tgz" | sha256sum -c -; \
    tar -xzf /t/ts.tgz -C /t; \
    install -m 0755 "/t/${name}/tailscale" /out/tailscale; \
    install -m 0755 "/t/${name}/tailscaled" /out/tailscaled

# ---- stage 4: runtime -----------------------------------------------------
FROM ubuntu:26.04@sha256:6a06111c3b0a8600feadb270aec771dc239bf407b25625f93f44ab00e11f4d3c

# Ubuntu archive only; no third-party repositories. gh is the archive build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git tmux libssl3t64 ca-certificates libstdc++6 curl gh \
    && rm -rf /var/lib/apt/lists/*

# Non-root daemon user; passwd home is the state volume. The daemons check that
# a socket peer has the same uid (SO_PEERCRED), so this is also the default USER
# for `docker exec`. Pass --build-arg UID=$(id -u) GID=$(id -g) so the host state
# dir and the mounted token files are readable; do not override it with
# `docker run --user`. The base image's uid-1000 "ubuntu" user is removed first.
ARG UID=1000
ARG GID=1000
RUN (userdel -r ubuntu 2>/dev/null || true) \
    && groupadd --gid "${GID}" sled \
    && useradd --uid "${UID}" --gid "${GID}" --no-create-home \
        --home-dir /data/home --shell /bin/bash sled \
    && mkdir -p /data /models/fastembed /var/run/tailscale \
    && chown sled:sled /data /var/run/tailscale

COPY --from=unpack /out/ /usr/local/bin/
COPY --from=claude /out/claude /usr/local/bin/claude
COPY --from=tailscale /out/ /usr/local/bin/
COPY entrypoint.sh check.sh exercise.sh claude-run gh-token-run /usr/local/bin/
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/check.sh \
        /usr/local/bin/exercise.sh /usr/local/bin/claude-run /usr/local/bin/gh-token-run \
    && git config --system credential.https://github.com.helper '!gh auth git-credential'

# One volume at /data holds all state. TRUSTY_DATA_DIR_OVERRIDE is not set.
# TRUSTY_SKIP_RAM_CHECK: the 16 GB guard would refuse a 6 GB container.
# The proxy host name below must equal the proxy container name in README.md.
# DOC-90 section 7 adds the second block. TAGENT_NO_API (#6433) and
# TRUSTY_CONSOLE_TOKEN_FILE (#11074) are set ahead of the code that reads them.
# TS_NO_LOGS_NO_SUPPORT: tailscaled sends no client logs (D20, DOC-90 section 6).
# GH_NO_UPDATE_NOTIFIER: gh makes no release check (ruling 4).
ENV HOME=/data/home \
    XDG_DATA_HOME=/data/xdg \
    FASTEMBED_CACHE_DIR=/models/fastembed \
    TRUSTY_SKIP_RAM_CHECK=1 \
    HTTPS_PROXY=http://tm-trusty-cloud-sled-egress:3128 \
    https_proxy=http://tm-trusty-cloud-sled-egress:3128 \
    NO_PROXY=localhost,127.0.0.1,::1 \
    no_proxy=localhost,127.0.0.1,::1 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_TELEMETRY=1 \
    DISABLE_ERROR_REPORTING=1 \
    DISABLE_AUTOUPDATER=1 \
    ENABLE_CLAUDEAI_MCP_SERVERS=false \
    TRUSTY_NO_UPDATE_CHECK=1 \
    TRUSTY_UV_FETCH=0 \
    HF_HUB_OFFLINE=1 \
    TRUSTY_MPM_PR_CLEANUP_INTERVAL=0 \
    TAGENT_NO_API=1 \
    TRUSTY_CONSOLE_TOKEN_FILE=/run/secrets/trusty-console-token \
    TS_NO_LOGS_NO_SUPPORT=true \
    GH_NO_UPDATE_NOTIFIER=1
VOLUME /data
USER sled
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
