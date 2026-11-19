//! Home Assistant client: the WebSocket session (auth, the start
//! requests, events, the dashboard's service calls, keepalive) and the
//! reconnects. The messages and the entity store are touchos_hass.

use ::log::{info, warn};
use alloc::vec;
use serde_json::Value;
use spin::Mutex;

use crate::config;
use crate::ws::WebSocket;
use touchos_hass::{self as hass, Applied, Auth};
use touchos_netc::tls::RECORD_BUF;
use touchos_netc::{self as net, Conn, NetError, TcpStream, TlsStream};
use touchos_proto::decode;
use touchos_proto::ha::{CALL_SLOT, ServiceCall};
use touchos_rt::shm::{Ring, Shared};
use touchos_rt::{exec, time};

/// The entity store, published for the dashboard by `pump` in main.rs.
pub static STATE: Mutex<hass::State> = Mutex::new(hass::State::new());

#[derive(Debug)]
enum HaError {
    Net(NetError),
    Auth,
    Protocol(&'static str),
}

impl core::fmt::Display for HaError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            HaError::Net(e) => write!(f, "{e}"),
            HaError::Auth => write!(f, "access token rejected"),
            HaError::Protocol(what) => write!(f, "{what}"),
        }
    }
}

impl From<NetError> for HaError {
    fn from(e: NetError) -> Self {
        HaError::Net(e)
    }
}

/// For the metrics board: sessions that got through the login, and
/// messages received.
pub static SESSIONS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static EVENTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Connect, stay connected, reconnect: forever. `calls` is the ring of
/// the dashboard's service calls, sent while a session is up.
pub async fn task(calls: Shared<Ring>) {
    let mut backoff = 2_000;
    loop {
        // Read the settings for every attempt: they may have changed.
        let cfg = config::get();
        let ha = &cfg.homeassistant;
        let (Some(host), Some(token)) = (ha.host.clone(), ha.token.clone()) else {
            warn!("no homeassistant host/token configured");
            while !config::ha_changed() {
                exec::sleep_ms(1000).await;
            }
            continue;
        };
        let port = ha.port;
        net::wait_configured().await;
        info!("connecting to {host}:{port}");
        let t0 = time::now_ms();
        let r = session(&host, port, &token, &calls).await;
        STATE.lock().view.connected = false;
        crate::state_changed();
        match r {
            Err(HaError::Auth) => {
                warn!("access token rejected; retrying in 5 min");
                backoff = 300_000;
            }
            Err(e) => warn!("connection ended: {e}"),
            Ok(()) => {
                info!("settings changed; reconnecting");
                backoff = 0;
            }
        }
        if time::now_ms() - t0 > 60_000 {
            backoff = 2_000; // it was up for a while: reconnect quickly
        }
        // Wait before the next attempt, unless the settings change meanwhile:
        // only ha_changed() takes in new settings, and nothing else calls it
        // while there is no session (a fix for a wrong port would otherwise
        // never be tried).
        let until = time::now_ms() + backoff;
        let mut changed = false;
        while time::now_ms() < until {
            if config::ha_changed() {
                info!("settings changed; reconnecting");
                changed = true;
                break;
            }
            exec::sleep_ms(500).await;
        }
        backoff = if changed {
            2_000
        } else {
            (backoff * 2).clamp(2_000, 60_000)
        };
    }
}

async fn session(host: &str, port: u16, token: &str, calls: &Shared<Ring>) -> Result<(), HaError> {
    let ip = net::resolve(host).await?;
    let tcp = TcpStream::connect(ip, port).await?;
    let use_tls = config::get().homeassistant.use_tls();
    let (mut rx, mut tx) = (vec![0u8; RECORD_BUF], vec![0u8; RECORD_BUF]);
    let conn = if use_tls {
        let t0 = time::now_ms();
        let tls = TlsStream::connect(tcp, host, &mut rx, &mut tx).await?;
        info!(
            "TLS session established, certificate verified ({} ms)",
            time::now_ms() - t0
        );
        Conn::Tls(tls)
    } else {
        Conn::Plain(tcp)
    };
    let mut ws = WebSocket::handshake(conn, host, "/api/websocket").await?;

    let hello = parse(&ws.recv_text(10_000).await?)?;
    let version = hass::server_version(&hello).ok_or(HaError::Protocol("expected auth_required"))?;
    info!("server version {version}");
    ws.send_text(&hass::auth(token)).await?;
    match hass::auth_result(&parse(&ws.recv_text(10_000).await?)?) {
        Auth::Ok => {}
        Auth::Invalid => return Err(HaError::Auth),
        Auth::Unexpected => return Err(HaError::Protocol("unexpected auth reply")),
    }
    for r in hass::START {
        ws.send_text(r).await?;
    }
    let mut next_id = hass::id::FIRST_FREE;
    let mut last_ping = time::now_ms();
    let mut ping_outstanding: Option<u64> = None;
    let mut call = vec![0u8; CALL_SLOT as usize];
    // The dashboard wakes us when it queues a call.
    let calls_bit = exec::alloc_bit();
    let _ = touchos_rt::sys::mem_wait(calls.mem(), calls_bit);
    SESSIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let r = run(&mut ws, calls, &mut call, calls_bit, &mut next_id, &mut last_ping, &mut ping_outstanding).await;
    exec::free_bit(calls_bit);
    r
}

/// The session's loop: messages in, service calls out, pings.
async fn run(
    ws: &mut WebSocket<'_>,
    calls: &Ring,
    call: &mut [u8],
    calls_bit: usize,
    next_id: &mut u64,
    last_ping: &mut u64,
    ping_outstanding: &mut Option<u64>,
) -> Result<(), HaError> {
    loop {
        let mut changed = false;
        while let Some(text) = ws.poll_text().await? {
            let msg = parse(&text)?;
            EVENTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let applied = STATE.lock().apply(&msg);
            changed = true;
            match applied {
                Applied::Pong(id) if id == *ping_outstanding => *ping_outstanding = None,
                a => log_applied(&a),
            }
        }
        if changed {
            crate::state_changed();
        }
        // Outgoing service calls from the dashboard.
        while let Some(n) = calls.pop(call) {
            let Some(c) = decode::<ServiceCall>(&call[..n]) else { continue };
            ws.send_text(&hass::call_service(*next_id, &c)).await?;
            *next_id += 1;
            info!("{}.{} -> {}", c.domain, c.service, c.entity_id);
        }
        // Application-level keepalive: notice dead connections within a minute.
        if time::now_ms() - *last_ping > PING_EVERY_MS {
            if ping_outstanding.is_some() {
                return Err(HaError::Protocol("ping timeout"));
            }
            ws.send_text(&hass::ping(*next_id)).await?;
            *ping_outstanding = Some(*next_id);
            *next_id += 1;
            *last_ping = time::now_ms();
        }
        if config::ha_changed() {
            return Ok(());
        }
        // Until a message arrives, the dashboard queues a call, or the next
        // ping; at least once a second for a settings change.
        let ping_in = (*last_ping + PING_EVERY_MS).saturating_sub(time::now_ms());
        exec::wait_bits_or_ms(ws.bit() | calls_bit, ping_in.clamp(1, 1000)).await;
    }
}

/// The application-level keepalive's interval.
const PING_EVERY_MS: u64 = 30_000;

fn parse(text: &str) -> Result<Value, HaError> {
    serde_json::from_str(text).map_err(|_| HaError::Protocol("invalid JSON"))
}

fn log_applied(a: &Applied) {
    let st = STATE.lock();
    let v = &st.view;
    match a {
        Applied::States => {
            info!("connected, {} entities", v.entities.len());
            for t in &config::get().tiles {
                if !v.entities.contains_key(&t.entity) {
                    warn!("configured tile {} does not exist in Home Assistant", t.entity);
                }
            }
        }
        Applied::Registry => info!(
            "{} areas, {} entities in a room, {} hidden",
            v.areas.len(),
            v.entity_area.len(),
            v.hidden.len()
        ),
        Applied::Changed(id) => {
            if let Some(e) = v.entities.get(id)
                && (e.domain() == "light" || e.domain() == "switch")
            {
                info!("{} = {}", e.id, e.state);
            }
        }
        Applied::Failed { id, message } => match id {
            Some(id) => warn!("request {id} failed: {message}"),
            None => warn!("request ? failed: {message}"),
        },
        Applied::Pong(_) | Applied::Nothing => {}
    }
}
