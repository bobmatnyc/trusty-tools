# Log Drain — Destinations, Key Layout, and Manifest Semantics

> Reference for `trusty_common::log_drain`, added by
> [#6533](https://github.com/bobmatnyc/trusty-tools/issues/6533) (epic phase
> 1+2). This page covers the drain **core**: destination URIs, the key layout,
> manifest semantics, the scrub, and the limits. It is not a configuration
> guide — nothing reads a config file or schedules a run yet. See
> [What Phase 3 adds](#what-phase-3-adds).

The module is gated behind the `log-drain` feature and is not compiled by
default:

```bash
cargo test -p trusty-common --features log-drain --no-fail-fast
```

## Why it exists

Every trusty daemon writes logs to a local path that nothing prunes and nothing
collects — `~/.trusty-mpm/logs/trusty-mpm.log.YYYY-MM-DD`,
`~/Library/Logs/trusty-agents/daemon-*.log`, and so on. Diagnosing a failure on
another machine means asking a human to find and send a file. The drain moves
those bytes somewhere durable, scrubbed of credentials, without re-uploading
what has not changed.

## Destination URIs

`DestinationUri::parse` accepts a closed set of schemes. There is no general URI
crate behind it: the grammar is small, and a closed enum is what lets a
Google-storage URI produce a message naming what *is* supported instead of a
syntax error.

| Form | Meaning |
|---|---|
| `s3://bucket` | Bucket root, region from the AWS credential chain |
| `s3://bucket/prefix` | Every object written beneath `prefix` |
| `s3://bucket/prefix?region=us-west-2` | Region override; the ONLY accepted query parameter |
| `file:///abs/path` | A local directory. Used by the hermetic tests, and by an operator staging output before pointing at a bucket |
| `gs://…`, `az://…` | **Reserved and refused** with `DrainError::UnsupportedScheme` |

Rejected forms, each with `DrainError::Uri`: a missing `://`, an empty bucket
(`s3:///prefix`), a relative `file://relative/path`, `file:///` (the filesystem
root), a query on a `file://` URI, an empty or misspelled query parameter. A
misspelled `?reigon=` is refused rather than ignored — silently dropping it
would send logs to the wrong region with no signal.

Schemes are matched case-insensitively; the rest of the URI is not.

### S3 credentials

The S3 adapter loads the **AWS default provider chain** — environment
variables, `~/.aws/credentials`, SSO, IMDS — the same chain
`inference::bedrock` uses, reused rather than reimplemented per the
common-entry-point rule in [`CLAUDE.md`](../../CLAUDE.md). It is bridged into
`object_store`'s own `CredentialProvider`, and re-fetched on each request so a
rotated session token is picked up without rebuilding the store.

**No bucket or region is ever hardcoded.** The region resolves as
`?region=` → the credential chain → refusal with `DrainError::Credentials`.

## Key layout

```text
<destination prefix>/<github_id>/<session_id>/logs/<crate>/<relative_file>
```

The destination prefix comes from the URI (`s3://bucket/PREFIX`) and is joined
by the adapter; everything after it is built by `DrainTarget`. For
`file://` destinations the URI's path is the store root, so the prefix is empty.

`DrainTarget { github_id, session_id }` is **supplied by the caller**. The core
does not resolve GitHub identity — that is Phase 3's job. An empty or
whitespace-only `github_id` or `session_id` is refused with
`DrainError::MissingIdentity` **before anything touches the filesystem or the
network**: the drain never writes under an unknown id, because an empty
component collapses one user's logs into a shared, unattributable prefix.

`session_id` is opaque to the drain — it is whatever the consuming crate calls
its own session.

## Manifest semantics

Each target carries a JSON manifest at
`<…>/logs/.drain-manifest.json`, listing one entry per uploaded file:

```json
{
  "version": 1,
  "entries": [
    {
      "relative_file": "trusty-mpm/daemon.log",
      "size": 48122,
      "mtime_unix": 1788271952,
      "sha256": "9f2c…",
      "uploaded_at": "2026-09-01T14:12:32.273982+00:00"
    }
  ]
}
```

`size` and `sha256` describe the **plaintext source file**, not the gzipped
body — so a change to the level filter or the compression level never
invalidates every recorded digest.

### Two copies, and which one wins

- **Remote** (`<…>/logs/.drain-manifest.json`) is **authoritative**. It is the
  only copy that describes what is actually in the bucket.
- **Local cache** (`<state_dir>/log-drain/<github_id>/<session_id>/manifest.json`)
  exists so an unchanged run costs no network read at all.

When both exist and disagree, the remote copy wins and the cache is rewritten
from it. A stale cache that won would make the drain skip files that were never
uploaded. A cache write failure is logged and swallowed — it costs the next run
one extra remote read and nothing else.

An **undecodable** manifest — corrupt JSON, or an unrecognised `version` — is
treated as **absent**, never as an error. Re-uploading a file is strictly safer
than skipping one that was never written.

### Skip decision, and why SHA-256 wins

Per source file, in order:

1. **Stat-only fast path.** If a manifest entry exists and *both* `size` and
   `mtime_unix` match, the file is skipped **without being opened**. This is
   where an unchanged run's cheapness comes from: an unchanged machine reads no
   log bytes at all.
2. **Size ceiling.** A file over `max_file_bytes` is skipped — see
   [Limits](#limits).
3. **SHA-256 tiebreak.** Otherwise the file is read and hashed. **If the digest
   matches the recorded entry, the file is still skipped**, and the entry's
   `size`/`mtime_unix` are refreshed so the next run takes the fast path.

Step 3 is the rule worth stating plainly: **content identity wins over mtime.**
A file whose timestamp moved but whose bytes are identical — a `touch`, a log
rotated back into place, a checkout that rewrote timestamps — is **not**
re-uploaded. `mtime` is only ever an optimisation for avoiding the read.

The manifest is rewritten once after a batch, reflecting only what actually
landed, so a run interrupted partway still leaves the next run able to skip what
did upload.

## The scrub

Every body passes through `credentials::scrub_secrets` before compression, with
the caller-supplied secret list. The pipeline order is fixed inside the
collector — decode, level-filter, **scrub**, gzip — so no body can reach a
destination having skipped it. The `log-drain` feature *implies* `credentials`
for exactly this reason: a build with the drain but without the scrubber would
upload log text nothing had cleaned.

**`scrub_secrets` removes values it is GIVEN.** It does not detect
secret-shaped strings, and it ignores needles below its own minimum length. A
caller that passes an empty list gets no scrubbing.

## Level filtering

When a `LogSource` sets `level_filter`, lines below that level are dropped
before upload — a daemon's DEBUG output is the bulk of its bytes and almost
never the reason anyone reads the log later.

The collector recognises the `tracing_subscriber::fmt` default line shape,
`<timestamp> <LEVEL> <target>: <message>`, including ANSI-colourised output.
Two rules keep it from destroying a file it does not understand:

- A line carrying **no recognisable level** is a *continuation* — a wrapped
  message, a backtrace frame — and inherits the disposition of the line above
  it.
- A file containing **no recognisable level line at all** is not tracing output
  and is passed through **verbatim** rather than filtered to nothing.

## Limits

| Limit | Value | Behaviour |
|---|---|---|
| `max_file_bytes` | 64 MiB default | Oversize files are **skipped**, never truncated or chunked |
| `LIST_LIMIT` | 10 000 entries | `list` truncates with a `warn!` |

**Why oversize files are skipped rather than streamed.** The scrub has to see a
whole body to catch a secret that straddles a chunk boundary, so a chunked
upload path could only ever write partially-scrubbed text. Refusing to upload is
the safe half of that trade, and `DrainReport::skipped_too_large` plus a `warn!`
say so out loud rather than silently truncating.

## What the caller owns

- **Identity** — `DrainTarget` is supplied, never resolved here.
- **Single-flight** — `run_once` is one pass with no locking. Two concurrent
  runs against one target will both upload and both rewrite the manifest, and
  the loser's entries are lost. Mutual exclusion belongs to the scheduler.
- **The secret list** — see [The scrub](#the-scrub).

A **per-file** failure is collected into `DrainReport::errors` and the batch
continues; one unreadable file must not strand every other log on the machine.
Only a manifest read/write failure aborts a run.

## What Phase 3 adds

Not in this module, deliberately:

- The **scheduler** — a `tokio::time::interval` + `CancellationToken` loop
  beside `orphan_gc_loop`, with the single-flight guard `run_once` does not
  provide.
- **GitHub identity resolution** — one cached `gh api user` lookup feeding
  `DrainTarget.github_id`, with the config value taking precedence.
- **Configuration** — a `[log_drain]` section, and the `LogSource` list for the
  daemons that actually produce logs.
- **Consumers** — nothing calls `run_once` yet.
- **Pruning** — the drain is upload-only; `prune_after_upload` is not
  implemented.
