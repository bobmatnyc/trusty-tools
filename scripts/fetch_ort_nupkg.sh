#!/usr/bin/env bash
# fetch_ort_nupkg.sh — download ONNX Runtime from NuGet, verify, extract.
#
# Why: github.com/microsoft/onnxruntime release assets return 404 (#11458), so
#   CI fetches the `Microsoft.ML.OnnxRuntime` nupkg (a zip) from nuget.org and
#   pins its SHA-256 here, in one place, for al2023-build.yml and release.yml.
# What: usage `fetch_ort_nupkg.sh <version> <x64|aarch64> <dest-dir>`.
#   Downloads the nupkg, compares its SHA-256 with the pin below (exit 1 on a
#   mismatch or an unpinned version), extracts
#   `runtimes/linux-<rid>/native/libonnxruntime.so*` into `<dest-dir>/lib/`,
#   and prints the absolute path of `libonnxruntime.so` on stdout (the last
#   line; everything else goes to stderr). No headers are extracted or needed:
#   the `ort` crate dlopen()s the library (load-dynamic).
# Test: run in CI by the al2023-build.yml / release.yml ORT download steps.
set -euo pipefail

VERSION="${1:?usage: fetch_ort_nupkg.sh <version> <x64|aarch64> <dest-dir>}"
ARCH="${2:?usage: fetch_ort_nupkg.sh <version> <x64|aarch64> <dest-dir>}"
DEST="${3:?usage: fetch_ort_nupkg.sh <version> <x64|aarch64> <dest-dir>}"

# Pinned SHA-256 of the nupkg per version. Add a line when ORT_VERSION moves.
case "${VERSION}" in
  1.24.2) EXPECTED_SHA256="782564d3d68e87269ea1812cc94710ad06fe014faf093f01a6f89d0bd58719d3" ;;
  *) echo "ERROR: no pinned SHA-256 for ORT ${VERSION} in $0" >&2; exit 1 ;;
esac

case "${ARCH}" in
  x64)     RID="linux-x64" ;;
  aarch64) RID="linux-arm64" ;;
  *) echo "ERROR: unsupported arch '${ARCH}' (want x64 or aarch64)" >&2; exit 1 ;;
esac

URL="https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/${VERSION}/microsoft.ml.onnxruntime.${VERSION}.nupkg"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
NUPKG="${WORK}/ort.nupkg"

echo "Downloading ORT ${VERSION} nupkg from ${URL}" >&2
curl -fsSL --retry 3 --retry-delay 5 -o "${NUPKG}" "${URL}" >&2

ACTUAL_SHA256="$(sha256sum "${NUPKG}" | cut -d' ' -f1)"
if [ "${ACTUAL_SHA256}" != "${EXPECTED_SHA256}" ]; then
  echo "ERROR: SHA-256 mismatch for ${URL}" >&2
  echo "  expected: ${EXPECTED_SHA256}" >&2
  echo "  actual:   ${ACTUAL_SHA256}" >&2
  exit 1
fi
echo "SHA-256 verified: ${ACTUAL_SHA256}" >&2

mkdir -p "${DEST}/lib"
# -j drops the runtimes/<rid>/native/ prefix; -o overwrites without prompting.
unzip -o -j -q "${NUPKG}" "runtimes/${RID}/native/libonnxruntime.so*" -d "${DEST}/lib" >&2

DYLIB_PATH="${DEST}/lib/libonnxruntime.so"
if [ ! -f "${DYLIB_PATH}" ]; then
  echo "ERROR: ${DYLIB_PATH} missing after extracting runtimes/${RID}/native/" >&2
  ls -l "${DEST}/lib" >&2 || true
  exit 1
fi

echo "$(cd "$(dirname "${DYLIB_PATH}")" && pwd)/libonnxruntime.so"
