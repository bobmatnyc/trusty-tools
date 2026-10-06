//! HNSW inserts that never write to process stdout (#9187).
//!
//! Why: `hnsw_rs` 0.3.4 `println!`s from `PointIndexation::generate_new_point`
//! each time its point counter reaches a multiple of [`HNSW_RS_PRINT_PERIOD`]
//! (`hnsw.rs:519-520`). A daemon serving MCP over stdio uses stdout as the
//! JSON-RPC channel, so the 50,000th point of a palace corrupted the protocol
//! stream, on open or on upsert. 0.3.4 is the latest release and has no switch
//! for the print. The counter is private and cumulative per index, so splitting
//! the inserts into smaller calls cannot avoid reaching it.
//! What: between inserts the counter is exact, so the insert that will land on
//! a print point is known before it runs. [`insert_quietly`] runs every other
//! insert as before, and runs that one serially on the calling thread under
//! [`with_stdout_silenced`]. [`super::replay`] uses it to keep its parallel
//! batches clear of print points. The redirect therefore happens once per
//! 50,000 inserts, on one thread, inside Rust's stdout lock.
//! Test: `no_hnsw_insert_writes_to_stdout`,
//! `a_failed_stdout_redirect_skips_the_closure_and_leaves_stdout_alone`.

use std::path::Path;

use hnsw_rs::prelude::{DistCosine, Hnsw};

/// `hnsw_rs` 0.3.4 prints when its point count becomes a multiple of this.
pub(super) const HNSW_RS_PRINT_PERIOD: usize = 50_000;

/// How many inserts can run from a point count of `count` before the next
/// insert would land on a print point. Zero means the next insert prints.
pub(super) fn inserts_before_print_point(count: usize) -> usize {
    HNSW_RS_PRINT_PERIOD - 1 - count % HNSW_RS_PRINT_PERIOD
}

/// Insert one point into `index`, with stdout silenced if this insert is a
/// print point.
///
/// Why (#9187): see the module header.
/// What: reads the point count, then inserts. A caller must not insert into
/// `index` from another thread at the same time, or the count it read is stale;
/// `HnswStore::upsert` holds its insert gate. On `Err` stdout could not be
/// silenced and nothing was inserted, except when only the restore of fd 1
/// failed: then the point is in the graph and stdout points at `/dev/null`.
/// Test: `no_hnsw_insert_writes_to_stdout`.
pub(super) fn insert_quietly(
    index: &Hnsw<'static, f32, DistCosine>,
    vector: &[f32],
    id: usize,
) -> std::io::Result<()> {
    if inserts_before_print_point(index.get_nb_point()) == 0 {
        with_stdout_silenced(|| index.insert_slice((vector, id)))
    } else {
        index.insert_slice((vector, id));
        Ok(())
    }
}

/// Run `f` with process stdout pointed at `/dev/null`.
///
/// Why (#9187): the only way to drop a third-party `println!`; see the module
/// header.
/// What: holds Rust's stdout lock for the whole window, so `println!` and
/// `tokio::io::stdout` (which writes through `std::io::Stdout`) on other
/// threads wait instead of writing into `/dev/null`. A `println!` inside `f`
/// on this thread re-enters the lock. Fails closed: if the redirect cannot be
/// set up, `f` does not run.
/// Test: `no_hnsw_insert_writes_to_stdout`,
/// `a_failed_stdout_redirect_skips_the_closure_and_leaves_stdout_alone`.
pub(super) fn with_stdout_silenced<R>(f: impl FnOnce() -> R) -> std::io::Result<R> {
    with_stdout_redirected_to(Path::new("/dev/null"), f)
}

/// [`with_stdout_silenced`] with the sink as a parameter, so a test can make
/// the redirect fail.
#[cfg(unix)]
pub(super) fn with_stdout_redirected_to<R>(
    sink: &Path,
    f: impl FnOnce() -> R,
) -> std::io::Result<R> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    // Bytes another writer left in Rust's buffer belong on the real stdout.
    lock.flush()?;
    let sink = std::fs::OpenOptions::new().write(true).open(sink)?;
    // SAFETY: `dup` takes a plain fd number and returns a new fd or -1.
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `saved` is a fresh fd from `dup` that nothing else owns.
    let mut saved = SavedStdout(Some(unsafe { OwnedFd::from_raw_fd(saved) }));
    // SAFETY: both arguments are open fds; the result is checked.
    if unsafe { libc::dup2(sink.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        // Nothing was redirected; dropping `saved` closes the copy.
        saved.0 = None;
        return Err(std::io::Error::last_os_error());
    }
    let out = f();
    // `f`'s line sits in Rust's buffer until a newline or flush; send it to
    // the sink before fd 1 points at the real stdout again.
    let flushed = lock.flush();
    saved.restore()?;
    flushed?;
    Ok(out)
}

#[cfg(not(unix))]
pub(super) fn with_stdout_redirected_to<R>(
    _sink: &Path,
    _f: impl FnOnce() -> R,
) -> std::io::Result<R> {
    // #9187: no fd redirect here, so refuse rather than let the print through.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "silencing stdout around an hnsw_rs insert needs a unix target",
    ))
}

/// The real stdout, held aside while fd 1 points at the sink.
///
/// Why: a panic inside the guarded closure must not leave stdout redirected,
/// so `Drop` restores it; [`SavedStdout::restore`] is the path that can
/// report a failure.
#[cfg(unix)]
struct SavedStdout(Option<std::os::fd::OwnedFd>);

#[cfg(unix)]
impl SavedStdout {
    fn restore(&mut self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let Some(saved) = self.0.take() else {
            return Ok(());
        };
        // SAFETY: both arguments are open fds; the result is checked.
        if unsafe { libc::dup2(saved.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for SavedStdout {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            tracing::error!(error = %e, "#9187: could not restore stdout after an hnsw_rs insert");
        }
    }
}
