# Runbook — Deploy trusty-search 0.27.1 (CUDA) on Amazon Linux 2023

**Issue:** [#1554](https://github.com/bobmatnyc/trusty-tools/issues/1554) — indexing walk returned 0 files on AL2023 CUDA hosts (repos under `/data/...` were pruned).
**Goal:** update an AL2023 / Tesla T4 host from `0.26.1-cuda` to `0.27.1-cuda` to pick up the #1554 fix.
**Why a source build:** the default `bundled-ort` feature static-links an ONNX Runtime needing glibc ≥ 2.38 (AL2023 has 2.34), and there is no prebuilt CUDA+AL2023 release artifact (CI's AL2023 asset is CPU-only). So build with `--no-default-features --features cuda` + a dynamically-loaded ORT. Source: `crates/trusty-search/Cargo.toml` (lines 211-269) and `.github/workflows/release.yml`.

## 0. Set variables (edit, then paste)
```bash
export TS_INDEX="main"                       # the index that returned total_files=0
export TS_PORT="7878"
export ORT_VERSION="1.20.1"
export ORT_PREFIX="/opt/onnxruntime"
export ORT_DYLIB_PATH="${ORT_PREFIX}/lib/libonnxruntime.so.${ORT_VERSION}"
```

## 1. Pre-flight checks
```bash
ldd --version | head -1                       # expect glibc 2.34 (AL2023)
nvidia-smi --query-gpu=name,driver_version --format=csv,noheader
trusty-search --version || echo "not yet installed on PATH"
which cargo || echo "install rustup first: https://rustup.rs"
```

## 2. Install a glibc-compatible ORT 1.20.1 GPU runtime (once)
```bash
curl -fsSL --retry 3 \
  "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-gpu-${ORT_VERSION}.tgz" \
  | sudo tar xz -C /opt
sudo ln -sfn "/opt/onnxruntime-linux-x64-gpu-${ORT_VERSION}" "${ORT_PREFIX}"
ls -l "${ORT_DYLIB_PATH}"
```

## 3. Build + install trusty-search 0.27.1 with CUDA (no bundled ORT)
```bash
SKIP_UI_BUILD=1 cargo install trusty-search --version 0.27.1 \
  --locked --no-default-features --features cuda
trusty-search --version                        # expect: trusty-search 0.27.1
```

## 4. Persist ORT_DYLIB_PATH in the daemon environment
**systemd-managed:**
```bash
sudo systemctl edit trusty-search
# under [Service]:  Environment=ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so.1.20.1
sudo systemctl daemon-reload
```
**custom wrapper/screen/tmux:** add `export ORT_DYLIB_PATH=...` to the wrapper before `trusty-search start`.

## 5. Restart the daemon (graceful)
```bash
sudo systemctl restart trusty-search                       # systemd
# OR manual:
export ORT_DYLIB_PATH="${ORT_DYLIB_PATH}"; trusty-search stop && trusty-search start
journalctl -u trusty-search -n 50 --no-pager | grep -i provider   # expect: provider=CUDA (CUDA GPU)
```

## 6. Reindex + verify the fix
```bash
curl -X POST "http://localhost:${TS_PORT}/indexes/${TS_INDEX}/reindex"
curl -N "http://localhost:${TS_PORT}/indexes/${TS_INDEX}/reindex/stream"
```
Success = the `walk_complete` event shows `total_files > 0` (was `0` before).
```bash
curl -s "http://localhost:${TS_PORT}/health" | python3 -m json.tool   # status ok, warm_boot_degraded false
```

## 7. Rollback
```bash
SKIP_UI_BUILD=1 cargo install trusty-search --version 0.26.1 \
  --locked --no-default-features --features cuda
sudo systemctl restart trusty-search
```

## Troubleshooting
- **`total_files` still 0** → not running 0.27.1; re-check `trusty-search --version` and that the daemon actually restarted (no stale process). This is the version, not git `safe.directory` (the #1554 walk is internal, so host git config has no effect).
- **Log shows `provider=CPU` / CUDA EP failed to register** → CUDA/cuDNN mismatch. ORT 1.20.1 GPU is built against CUDA 12.x + cuDNN 9.x; if the host toolkit is CUDA 13.x, install matching CUDA 12 runtime libs or use an ORT GPU build matching your CUDA major. Indexing still works on CPU meanwhile (the #1554 fix is provider-independent).
- **`libonnxruntime.so` not found at startup** → `ORT_DYLIB_PATH` not in the daemon's persistent env (step 4); verify with `sudo systemctl show trusty-search -p Environment`.
