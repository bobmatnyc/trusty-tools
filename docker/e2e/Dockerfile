# Docker E2E smoke-test image: installs published trusty-* crates from crates.io
# and runs a minimal end-to-end scenario for each tool.
#
# Usage:
#   docker build -t trusty-e2e docker/e2e/
#   docker run --rm -e TRUSTY_SKIP_RAM_CHECK=1 trusty-e2e
#
# Build args (override to pin versions):
#   TRUSTY_SEARCH_VERSION  — trusty-search version to install (default: latest)
#   TRUSTY_MEMORY_VERSION  — trusty-memory version to install (default: latest)
#   TRUSTY_MPM_VERSION     — trusty-mpm version to install (default: latest)
#   TRUSTY_ANALYZE_VERSION — trusty-analyze version to install (default: latest)
#
# The image uses rust:slim (Debian-based) which bundles Cargo and avoids a
# separate rustup step. The cargo registry cache layer is ordered before the
# install layer so repeated builds skip the network fetch when versions are
# unchanged.

FROM rust:slim AS base

# Install system dependencies required by the crates:
#   - pkg-config + libssl-dev: for TLS in HTTP clients
#   - curl: used by the smoke-test script for daemon health probes
#   - procps: provides `kill` used in script process management
#   - jq: JSON parsing in the smoke script
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        curl \
        procps \
        jq \
        build-essential \
        cmake \
    && rm -rf /var/lib/apt/lists/*

# Build args for version pinning. Empty string means "install latest published".
ARG TRUSTY_SEARCH_VERSION=""
ARG TRUSTY_MEMORY_VERSION=""
ARG TRUSTY_MPM_VERSION=""
ARG TRUSTY_ANALYZE_VERSION=""

# Expose build args as environment so the install script can read them.
ENV TRUSTY_SEARCH_VERSION=${TRUSTY_SEARCH_VERSION}
ENV TRUSTY_MEMORY_VERSION=${TRUSTY_MEMORY_VERSION}
ENV TRUSTY_MPM_VERSION=${TRUSTY_MPM_VERSION}
ENV TRUSTY_ANALYZE_VERSION=${TRUSTY_ANALYZE_VERSION}

# Install all four tools from crates.io.
# Each install is a separate RUN layer so Docker can cache them independently.
# version suffix: "@X.Y.Z" is appended when the build arg is non-empty.
RUN set -eux; \
    if [ -n "${TRUSTY_SEARCH_VERSION}" ]; then \
        cargo install trusty-search --version "${TRUSTY_SEARCH_VERSION}" --locked; \
    else \
        cargo install trusty-search --locked; \
    fi

RUN set -eux; \
    if [ -n "${TRUSTY_MEMORY_VERSION}" ]; then \
        cargo install trusty-memory --version "${TRUSTY_MEMORY_VERSION}" --locked; \
    else \
        cargo install trusty-memory --locked; \
    fi

RUN set -eux; \
    if [ -n "${TRUSTY_MPM_VERSION}" ]; then \
        cargo install trusty-mpm --version "${TRUSTY_MPM_VERSION}" --locked; \
    else \
        cargo install trusty-mpm --locked; \
    fi

RUN set -eux; \
    if [ -n "${TRUSTY_ANALYZE_VERSION}" ]; then \
        cargo install trusty-analyze --version "${TRUSTY_ANALYZE_VERSION}" --locked; \
    else \
        cargo install trusty-analyze --locked; \
    fi

# Copy the fixture repo used by trusty-search and trusty-analyze scenarios.
# NOTE: The directory must NOT be named "fixtures" — trusty-search's walker
# skips any path component named "fixtures" (it is in SKIP_DIRS as a test-data
# exclusion). We use "sample-code" to keep the name unambiguous.
COPY fixtures/sample-repo /e2e/sample-code

# Copy the smoke-test entrypoint.
COPY smoke.sh /e2e/smoke.sh
RUN chmod +x /e2e/smoke.sh

# Env vars that apply to all scenarios.
# TRUSTY_SKIP_RAM_CHECK=1 bypasses the 16 GB RAM guard — Docker CI runners
# typically have much less RAM; we are indexing a tiny fixture not production
# corpora so the guard is unnecessary here.
ENV TRUSTY_SKIP_RAM_CHECK=1
# XDG_DATA_HOME: keeps all daemon state inside the container's /tmp so nothing
# escapes to the image layer.
ENV XDG_DATA_HOME=/tmp/trusty-data
# HOME must be set for discovery-file paths that fall back to $HOME.
ENV HOME=/root

WORKDIR /e2e

ENTRYPOINT ["/e2e/smoke.sh"]
