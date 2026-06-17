#!/usr/bin/env bash
# e2e-docker.sh — Build the trusty-e2e Docker image and run the four-tool
# install-from-crates.io + smoke-test scenarios.
#
# Usage:
#   bash scripts/e2e-docker.sh [OPTIONS]
#
# Options:
#   --search-version VERSION   Pin trusty-search version (default: latest)
#   --memory-version VERSION   Pin trusty-memory version (default: latest)
#   --mpm-version VERSION      Pin trusty-mpm version (default: latest)
#   --analyze-version VERSION  Pin trusty-analyze version (default: latest)
#   --no-cache                 Pass --no-cache to docker build
#   --help                     Show this help
#
# The script:
#   1. Builds the Docker image from docker/e2e/Dockerfile.
#   2. Runs the container with TRUSTY_SKIP_RAM_CHECK=1 (required: CI runners
#      typically have < 16 GB RAM; we index a tiny fixture, not production data).
#   3. Streams the container output and exits with the container's exit code.
#
# Example (latest published versions):
#   bash scripts/e2e-docker.sh
#
# Example (pinned versions):
#   bash scripts/e2e-docker.sh \
#     --search-version 0.24.10 \
#     --memory-version 0.15.3 \
#     --mpm-version 0.6.2 \
#     --analyze-version 0.5.0

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
SEARCH_VERSION=""
MEMORY_VERSION=""
MPM_VERSION=""
ANALYZE_VERSION=""
NO_CACHE=""
IMAGE_TAG="trusty-e2e:latest"
DOCKERFILE_DIR="docker/e2e"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --search-version)  SEARCH_VERSION="$2"; shift 2 ;;
        --memory-version)  MEMORY_VERSION="$2"; shift 2 ;;
        --mpm-version)     MPM_VERSION="$2"; shift 2 ;;
        --analyze-version) ANALYZE_VERSION="$2"; shift 2 ;;
        --no-cache)        NO_CACHE="--no-cache"; shift ;;
        --help|-h)
            sed -n '/^# Usage/,/^[^#]/p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Pre-flight: ensure docker is available
# ---------------------------------------------------------------------------
if ! command -v docker > /dev/null 2>&1; then
    echo "ERROR: docker not found in PATH." >&2
    echo "Install Docker and ensure the daemon is running, then retry." >&2
    exit 1
fi

if ! docker info > /dev/null 2>&1; then
    echo "ERROR: Docker daemon is not running or not accessible." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Resolve script root to the repo root (works from any cwd).
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "============================================================"
echo "  trusty-tools Docker E2E smoke test"
echo "============================================================"
echo "  Repo root      : ${REPO_ROOT}"
echo "  Dockerfile dir : ${REPO_ROOT}/${DOCKERFILE_DIR}"
echo "  Image tag      : ${IMAGE_TAG}"
echo ""
echo "  Versions (empty = latest published):"
echo "    trusty-search  : ${SEARCH_VERSION:-<latest>}"
echo "    trusty-memory  : ${MEMORY_VERSION:-<latest>}"
echo "    trusty-mpm     : ${MPM_VERSION:-<latest>}"
echo "    trusty-analyze : ${ANALYZE_VERSION:-<latest>}"
echo "============================================================"
echo ""

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
echo ">>> Building Docker image ${IMAGE_TAG} ..."
docker build \
    ${NO_CACHE} \
    --build-arg "TRUSTY_SEARCH_VERSION=${SEARCH_VERSION}" \
    --build-arg "TRUSTY_MEMORY_VERSION=${MEMORY_VERSION}" \
    --build-arg "TRUSTY_MPM_VERSION=${MPM_VERSION}" \
    --build-arg "TRUSTY_ANALYZE_VERSION=${ANALYZE_VERSION}" \
    -t "${IMAGE_TAG}" \
    "${REPO_ROOT}/${DOCKERFILE_DIR}"

echo ""
echo ">>> Image built successfully. Running smoke tests ..."
echo ""

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
docker run --rm \
    -e TRUSTY_SKIP_RAM_CHECK=1 \
    -e XDG_DATA_HOME=/tmp/trusty-data \
    -e HOME=/root \
    "${IMAGE_TAG}"

EXIT_CODE=$?

echo ""
if [ "${EXIT_CODE}" -eq 0 ]; then
    echo ">>> All scenarios PASSED."
else
    echo ">>> One or more scenarios FAILED (exit code: ${EXIT_CODE})."
fi

exit "${EXIT_CODE}"
