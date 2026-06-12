//! TCP port auto-walking helper for trusty-* daemons.
//!
//! Why: running multiple instances or restarting before the kernel releases
//! the prior socket should not produce a noisy failure. Auto-incrementing
//! gives a friendlier developer experience.

use anyhow::Result;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// Bind to `addr`; if the port is in use, walk forward up to `max_attempts`
/// ports and return the first listener that binds.
///
/// Why: Running multiple instances of a trusty-* daemon (or restarting before
/// the kernel releases the prior socket) shouldn't produce a noisy failure —
/// auto-incrementing gives a friendlier developer experience while still
/// honouring the user's preferred starting port.
/// What: returns the first successful `tokio::net::TcpListener`. Callers can
/// inspect `local_addr()` to discover where it landed and report it however
/// they prefer — this function does not perform any I/O on stdout/stderr.
/// `max_attempts == 0` means "try `addr` exactly once".
/// Test: `auto_port_walks_forward` binds a port, then calls this with the
/// occupied port and confirms a different free port is returned.
pub async fn bind_with_auto_port(addr: SocketAddr, max_attempts: u16) -> Result<TcpListener> {
    use std::io::ErrorKind;
    let mut current = addr;
    for attempt in 0..=max_attempts {
        match TcpListener::bind(current).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == ErrorKind::AddrInUse && attempt < max_attempts => {
                let next_port = current.port().saturating_add(1);
                if next_port == 0 {
                    anyhow::bail!("ran out of ports while searching for free slot");
                }
                tracing::warn!("port {} in use, trying {}", current.port(), next_port);
                current.set_port(next_port);
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("could not find free port after {max_attempts} attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auto_port_walks_forward() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let next = bind_with_auto_port(addr, 8).await.unwrap();
        let got = next.local_addr().unwrap().port();
        assert_ne!(got, port, "expected walk-forward to a different port");
    }

    #[tokio::test]
    async fn auto_port_zero_attempts_still_binds_free() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let l = bind_with_auto_port(addr, 0).await.unwrap();
        assert!(l.local_addr().unwrap().port() > 0);
    }
}
