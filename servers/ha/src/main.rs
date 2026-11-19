//! The Home Assistant client: keeps a live copy of the entity states over
//! the WebSocket API (TLS when configured) and publishes it on a board for
//! the dashboard; executes the dashboard's service calls.
//!
//! Capabilities: "net" (TCP), "config" (FULL: it needs the token),
//! "state" (the board, read-write), "calls" (the ring from the dashboard),
//! "cacerts" (the TLS roots, read-only).

#![no_std]
#![no_main]

extern crate alloc;

mod ha;
mod ws;

use log::{info, warn};
use touchos_rt::shm::{self, Board, Shared};
use touchos_rt::{exec, time};

touchos_rt::entry!(main);

/// Settings from the config server (touchos_config::client::settings).
pub mod config {
    pub use touchos_config::Config;
    pub use touchos_config::client::settings::get;

    /// True (once) after the Home Assistant settings changed.
    pub fn ha_changed() -> bool {
        touchos_config::client::settings::poll().is_some_and(|(old, new)| old.homeassistant != new.homeassistant)
    }
}

/// Our bit for "the state changed" (the session raises it).
static CHANGED_BIT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Wake `pump`: the state may have changed.
pub fn state_changed() {
    exec::raise(CHANGED_BIT.load(core::sync::atomic::Ordering::Relaxed));
}

/// Publish the state for the dashboard when it changes (and wake the
/// dashboard).
async fn pump(board: Shared<Board>) {
    let bit = exec::alloc_bit();
    CHANGED_BIT.store(bit, core::sync::atomic::Ordering::Relaxed);
    let mut seen = (u64::MAX, u64::MAX, false);
    loop {
        let snap = {
            let st = ha::STATE.lock();
            let v = &st.view;
            let cur = (v.version, v.registry_version, v.connected);
            (cur != seen).then(|| (cur, v.clone()))
        };
        if let Some((cur, snap)) = snap {
            if !board.write_as(&snap) {
                warn!("state too large for the board");
            }
            let _ = touchos_rt::sys::mem_notify(board.mem());
            seen = cur;
        }
        // At most 20 snapshots a second: a burst of changes is one.
        exec::wait_bits(bit).await;
        exec::sleep_ms(50).await;
    }
}

/// Network check: fetch `debug.http_test` ("[https://]host[:port]/path")
/// once and log the result.
async fn http_test() {
    // (A network check in the HA client: it is the process that links TLS.)
    use touchos_netc::tls::RECORD_BUF;
    use touchos_netc::{Conn, NetError, TcpStream, TlsStream};
    let Some(spec) = config::get().debug.http_test.clone() else {
        return;
    };
    let (tls, rest) = match spec.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, spec.strip_prefix("http://").unwrap_or(&spec)),
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let default_port = if tls { 443 } else { 80 };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(default_port)),
        None => (hostport, default_port),
    };
    touchos_netc::wait_online().await;
    let t0 = time::now_ms();
    let result: Result<usize, NetError> = async {
        let ip = touchos_netc::resolve(host).await?;
        let tcp = TcpStream::connect(ip, port).await?;
        let (mut rx, mut tx) = (alloc::vec![0u8; RECORD_BUF], alloc::vec![0u8; RECORD_BUF]);
        let mut s = if tls {
            Conn::Tls(TlsStream::connect(tcp, host, &mut rx, &mut tx).await?)
        } else {
            Conn::Plain(tcp)
        };
        let req =
            alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await?;
        let mut total = 0;
        let mut buf = alloc::vec![0u8; RECORD_BUF];
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(NetError::Closed) => break,
                Ok(n) => total += n,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }
    .await;
    match result {
        Ok(n) => info!("http_test {spec}: {n} bytes in {} ms", time::now_ms() - t0),
        Err(e) => warn!("http_test {spec} failed: {e}"),
    }
}

fn main() {
    let (Some(board), Some(calls)) = (shm::board("state", true), shm::ring("calls")) else {
        warn!("no shared memory for the dashboard");
        return;
    };
    exec::spawn(pump(board));
    exec::spawn(ha::task(calls));
    exec::spawn(http_test());
    exec::run()
}
