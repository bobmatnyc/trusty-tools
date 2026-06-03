# trusty-search systemd unit

Production systemd unit for the `trusty-search` HTTP daemon on Linux/EC2 (EBS).

## Why this unit exists (#694)

On the Duetto EC2/EBS fleet, an abrupt SIGKILL during a shutdown flush
corrupted `index.redb` (unrecoverable).  Two mitigations were already shipped:

1. **redb 4.x** (trusty-common 0.12+) defaults to `Durability::Immediate` —
   every commit is fully fsynced before the call returns, so a crash between
   commits leaves the database in a clean prior state rather than a torn write.
2. **Graceful SIGTERM drain** (#534) — the daemon finishes all in-flight axum
   HTTP requests before exiting, so searches and reindex jobs land cleanly.

This unit adds the third layer: `TimeoutStopSec=120` gives the daemon 120
seconds after SIGTERM before systemd escalates to SIGKILL.  Without a generous
stop timeout, systemd defaults to 90 s — potentially interrupting a slow EBS
fsync mid-transaction.

Pair with **EBS snapshots -> S3** for point-in-time volume-level recovery
(issue #704): graceful stop prevents corruption, but only volume backups
protect against accidental deletion or volume failure.

## Prerequisites

1. **Binary installed** on the target host (typically via
   `cargo install trusty-search --locked`), default path `/usr/local/bin/trusty-search`.
2. **`trusty-embedderd` installed** alongside it:
   `cargo install trusty-embedderd --locked`.
3. **Service account** created:
   ```bash
   useradd --system --no-create-home --shell /sbin/nologin trusty
   ```
4. **EBS data directory** created and owned by the service account:
   ```bash
   mkdir -p /data/trusty-search
   chown trusty:trusty /data/trusty-search
   ```

## Install

```bash
# Copy the unit file
sudo cp deploy/systemd/trusty-search.service /etc/systemd/system/

# Review and adjust placeholders (binary path, User/Group, WorkingDirectory,
# TRUSTY_DATA_DIR, port) before enabling.

# Reload systemd and enable + start the service
sudo systemctl daemon-reload
sudo systemctl enable --now trusty-search

# Verify it is running
sudo systemctl status trusty-search
journalctl -u trusty-search -f
```

## Graceful restart (the safe way)

```bash
sudo systemctl restart trusty-search
```

`systemctl restart` sends SIGTERM and waits up to `TimeoutStopSec=120` for the
daemon to shut down cleanly before starting the new process.  This is safe to
run at any time, including during an active reindex — the daemon will complete
the current redb transaction and fsync before exiting.

For a zero-downtime rolling upgrade, stop the old daemon first, install the new
binary, then start:

```bash
sudo systemctl stop trusty-search    # waits for graceful shutdown (up to 120 s)
cargo install trusty-search --locked # or copy the new binary to /usr/local/bin/
sudo systemctl start trusty-search
```

## Stop and verify clean shutdown

```bash
sudo systemctl stop trusty-search
# systemd sends SIGTERM and waits up to TimeoutStopSec=120 before SIGKILL.
# A clean shutdown logs: "graceful shutdown complete" at INFO level.
journalctl -u trusty-search --since "1 minute ago"
```

## Key directives and rationale

| Directive | Value | Rationale |
|---|---|---|
| `KillSignal` | `SIGTERM` | Explicit graceful-stop signal; daemon's SIGTERM handler (#534) drains requests then exits. |
| `TimeoutStopSec` | `120` | **#694 mitigation**: 120 s budget for in-flight HTTP drain + redb Immediate fsync on EBS before SIGKILL escalation. |
| `Type` | `simple` | Daemon stays in foreground via `--foreground`; does not use sd_notify. |
| `Restart` | `on-failure` | Automatic restart on crashes; no restart on clean `systemctl stop`. |
| `LimitNOFILE` | `65536` | Raised for mmap-heavy redb files + HNSW snapshots + HTTP keep-alive sockets. |

## Foreground invocation

systemd requires the supervised process to remain in the foreground.
`trusty-search start` (without flags) self-forks a detached background child
and returns immediately — that mode is **wrong** for systemd.

The `--foreground` flag (added alongside issue #534) keeps the daemon in the
foreground:

```
ExecStart=/usr/local/bin/trusty-search start --foreground --no-auto-discover --port 7878
```

## Environment variables

Edit the `Environment=` lines in the unit file to match your deployment:

| Variable | Required | Purpose |
|---|---|---|
| `RUST_LOG` | recommended | Log level (`info` for production, `debug` for bring-up). |
| `TRUSTY_DATA_DIR` | **required on EC2** | Absolute path to the EBS data directory (indexes.toml, redb files, HNSW snapshots). |
| `TRUSTY_EMBEDDERD_BIN` | if not on PATH | Absolute path to the `trusty-embedderd` sidecar binary. |
| `TRUSTY_DEVICE` | optional | `auto` (default), `cpu`, or `gpu`. |
| `TRUSTY_NO_KG` | optional | Set `1` to disable the Knowledge Graph on all indexes. |
| `TRUSTY_MEMORY_LIMIT_MB` | optional | Override auto-tuned RSS ceiling (MiB). |
| `TRUSTY_GPU_MEM_LIMIT_BYTES` | CUDA hosts | VRAM ceiling in bytes; default 12 GiB (safe for 16 GB T4). |
| `ORT_DYLIB_PATH` | Amazon Linux 2023 + CUDA | Path to `libonnxruntime.so` (required when glibc < 2.38). |

## Troubleshooting

```bash
# Live log stream
journalctl -u trusty-search -f

# Check daemon health via CLI (from any shell on the same host)
trusty-search status

# Check listening port
trusty-search port

# Verify health endpoint
curl http://127.0.0.1:$(trusty-search port)/health
```

If the daemon exits immediately with a non-zero code, check:
- Binary path in `ExecStart` exists and is executable.
- `TRUSTY_DATA_DIR` directory exists and is writable by `User=trusty`.
- `trusty-embedderd` is on PATH or `TRUSTY_EMBEDDERD_BIN` is set.
- System has at least 16 GB RAM (enforced at daemon startup).
