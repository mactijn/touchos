//! The socket service other processes call: CONNECT, LISTEN, ACCEPT,
//! RESOLVE, STATUS, MDNS_REGISTER/UNREGISTER; the connections, listeners
//! and services it keeps for them, and moving their data between smoltcp
//! and the shared socket memory.
//!
//! It is the boundary other processes talk to: every request is checked,
//! and a client that is gone (its notification answers Dead) loses its
//! connections; a restarted client's LISTEN replaces its old listener.

use alloc::vec;
use alloc::vec::Vec;
use log::info;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{dns, tcp};
use smoltcp::wire::{IpAddress, Ipv4Address};
use touchos_abi::{Error, Tag};
use touchos_proto::encode;
use touchos_proto::net::{self as proto, SOCK_SIZE, Sock, req, state};
use touchos_rt::{caps::Mapping, ipc, sys, time};

use crate::{State, status};

/// A TCP connection of a client process.
pub(crate) struct Conn {
    handle: SocketHandle,
    generation: u32,
    sock: Sock,
    _map: Mapping,
    mem: u32,
    client: u32,
    client_bits: usize,
    reported: u32,
    /// The client closed: when (ms). We still send what it wrote, then FIN.
    closing_since: Option<u64>,
}

/// How long a closed connection may take to deliver its last data.
const LINGER_MS: u64 = 10_000;

/// A listening port: `backlog` sockets in LISTEN state.
pub(crate) struct Listener {
    id: u32,
    port: u16,
    backlog: usize,
    generation: u32,
    waiting: Vec<SocketHandle>,
    /// A client waiting in ACCEPT: (notification, bits).
    notify: Option<(u32, usize)>,
}

use touchos_rt::ipc::reply;

/// Make a client's connection out of a smoltcp socket.
fn new_conn(
    st: &mut State,
    handle: SocketHandle,
    client: u32,
    client_bits: usize,
) -> Option<(u32, Tag)> {
    let mem = sys::mem_create(SOCK_SIZE as u64, 0).ok()?;
    let map = Mapping::map(mem, true).ok()?;
    let sock = unsafe { Sock::at(map.ptr()) };
    sock.set_state(state::CONNECTING);
    st.next_id += 1;
    let id = st.next_id;
    let mut data = Vec::new();
    data.extend_from_slice(&id.to_le_bytes());
    data.extend_from_slice(&(st.server_bits as u64).to_le_bytes()); // u64 on the wire
    let tag = ipc::prepare(proto::OK, &data, &[mem, st.server]);
    st.conns.push(Conn {
        handle,
        generation: st.generation,
        sock,
        _map: map,
        mem,
        client,
        client_bits,
        reported: u32::MAX,
        closing_since: None,
    });
    Some((id, tag))
}

fn tcp_socket() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; 64 * 1024]),
        tcp::SocketBuffer::new(vec![0; 16 * 1024]),
    );
    s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(30)));
    s
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

/// A request we can't use: drop the capabilities it brought (the kernel
/// put copies in our table), and say so.
fn bad_request(msg: &ipc::Msg) -> Tag {
    for &c in msg.caps() {
        let _ = sys::cap_drop(c);
    }
    reply(proto::BAD_REQUEST, &[])
}

pub(crate) fn handle(st: &mut State, msg: ipc::Msg) -> Tag {
    let d = msg.data();
    match msg.label {
        req::STATUS => {
            let s = status(st);
            reply(proto::OK, &encode(&s))
        }
        req::MDNS_REGISTER | req::MDNS_UNREGISTER => {
            let Some(p) = touchos_proto::decode::<proto::Service>(d) else {
                return bad_request(&msg);
            };
            let s = touchos_mdns::Service { kind: p.kind, instance: p.instance, port: p.port, txt: p.txt };
            if !s.is_valid() {
                return bad_request(&msg);
            }
            // Kept in the state for the next interface; the current one's
            // responder gets it too.
            st.services.retain(|x| !(x.kind == s.kind && x.port == s.port));
            let responder = st.net.as_mut().and_then(|n| n.mdns.as_mut()).map(|m| &mut m.responder);
            if msg.label == req::MDNS_REGISTER {
                info!("mDNS: announcing {} on port {}", s.kind, s.port);
                if let Some(r) = responder {
                    r.add_service(s.clone(), time::now_ms());
                }
                st.services.push(s);
            } else if let Some(r) = responder {
                r.remove_service(&s.kind, s.port);
            }
            let _ = sys::signal(st.server, st.server_bits); // the stack sends
            reply(proto::OK, &[])
        }
        req::CONNECT => {
            let (Some(ip), Some(port), Some(bits), &[client]) =
                (d.get(..4), u16_at(d, 4), u64_at(d, 6).map(|b| b as usize), msg.caps())
            else {
                return bad_request(&msg);
            };
            let ip = IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]));
            let Some(net) = st.net.as_mut().filter(|n| n.ipv4.is_some()) else {
                let _ = sys::cap_drop(client);
                return reply(proto::LINK_DOWN, &[]);
            };
            let mut s = tcp_socket();
            let local = net.ephemeral_port();
            if s.connect(net.iface.context(), (ip, port), local).is_err() {
                let _ = sys::cap_drop(client);
                return reply(proto::FAILED, &[]);
            }
            let handle = net.sockets.add(s);
            match new_conn(st, handle, client, bits) {
                Some((_, tag)) => tag,
                None => {
                    if let Some(net) = st.net.as_mut() {
                        net.sockets.remove(handle);
                    }
                    let _ = sys::cap_drop(client);
                    reply(proto::FAILED, &[])
                }
            }
        }
        req::RESOLVE => {
            let Ok(name) = core::str::from_utf8(d) else {
                return reply(proto::BAD_REQUEST, &[]);
            };
            let generation = st.generation;
            let Some(net) = st.net.as_mut() else {
                return reply(proto::LINK_DOWN, &[]);
            };
            let cx = net.iface.context();
            match net.sockets.get_mut::<dns::Socket>(net.dns).start_query(
                cx,
                name,
                smoltcp::wire::DnsQueryType::A,
            ) {
                Ok(q) => {
                    st.next_id += 1;
                    let id = st.next_id;
                    st.queries.push((id, q, generation));
                    reply(proto::OK, &id.to_le_bytes())
                }
                Err(_) => reply(proto::FAILED, &[]),
            }
        }
        req::RESOLVE_RESULT => {
            let Some(id) = u32_at(d, 0) else {
                return reply(proto::BAD_REQUEST, &[]);
            };
            let Some(i) = st.queries.iter().position(|q| q.0 == id) else {
                return reply(proto::FAILED, &[]);
            };
            let (_, q, generation) = st.queries[i];
            let r = match st.net.as_mut().filter(|_| generation == st.generation) {
                Some(net) => net
                    .sockets
                    .get_mut::<dns::Socket>(net.dns)
                    .get_query_result(q),
                None => Err(dns::GetQueryResultError::Failed),
            };
            match r {
                Err(dns::GetQueryResultError::Pending) => reply(proto::PENDING, &[]),
                Ok(addrs) => {
                    st.queries.remove(i);
                    match addrs.iter().find_map(|a| match a {
                        IpAddress::Ipv4(v4) => Some(v4.octets()),
                    }) {
                        Some(ip) => reply(proto::ADDRESS, &ip),
                        None => reply(proto::FAILED, &[]),
                    }
                }
                Err(_) => {
                    st.queries.remove(i);
                    reply(proto::FAILED, &[])
                }
            }
        }
        req::LISTEN => {
            let (Some(port), Some(backlog)) = (u16_at(d, 0), u16_at(d, 2)) else {
                return bad_request(&msg);
            };
            // One listener per port: a second LISTEN comes from a client
            // that was restarted (web), and the old one has nobody behind
            // it. Its waiting sockets go, or they would keep taking
            // connections that nobody accepts.
            if let Some(i) = st.listeners.iter().position(|l| l.port == port) {
                let old = st.listeners.remove(i);
                if let Some(net) = st.net.as_mut().filter(|_| old.generation == st.generation) {
                    for h in old.waiting {
                        net.sockets.get_mut::<tcp::Socket>(h).abort();
                        net.sockets.remove(h);
                    }
                }
                if let Some((n, _)) = old.notify {
                    let _ = sys::cap_drop(n);
                }
                info!("TCP port {port}: replacing the earlier listener (its owner was restarted)");
            }
            st.next_id += 1;
            let id = st.next_id;
            st.listeners.push(Listener {
                id,
                port,
                backlog: (backlog as usize).clamp(1, 8),
                generation: u32::MAX, // sockets are created by the stack task
                waiting: Vec::new(),
                notify: None,
            });
            info!("listening on TCP port {port}");
            reply(proto::OK, &id.to_le_bytes())
        }
        req::ACCEPT => {
            let (Some(id), Some(bits), &[client]) =
                (u32_at(d, 0), u64_at(d, 4).map(|b| b as usize), msg.caps())
            else {
                return bad_request(&msg);
            };
            let Some(li) = st.listeners.iter().position(|l| l.id == id) else {
                let _ = sys::cap_drop(client);
                return reply(proto::FAILED, &[]);
            };
            // A connected socket waiting in the backlog?
            let ready = st.net.as_mut().and_then(|net| {
                st.listeners[li].waiting.iter().position(|&h| {
                    let s = net.sockets.get::<tcp::Socket>(h);
                    s.is_active()
                        && s.state() != tcp::State::Listen
                        && s.state() != tcp::State::SynReceived
                })
            });
            match ready {
                Some(k) => {
                    let h = st.listeners[li].waiting.remove(k);
                    match new_conn(st, h, client, bits) {
                        Some((_, tag)) => tag,
                        None => {
                            if let Some(net) = st.net.as_mut() {
                                net.sockets.get_mut::<tcp::Socket>(h).abort();
                                net.sockets.remove(h);
                            }
                            let _ = sys::cap_drop(client);
                            reply(proto::FAILED, &[])
                        }
                    }
                }
                None => {
                    if let Some((old, _)) = st.listeners[li].notify.replace((client, bits)) {
                        let _ = sys::cap_drop(old);
                    }
                    reply(proto::PENDING, &[])
                }
            }
        }
        _ => bad_request(&msg),
    }
}

/// Move data between smoltcp and the clients' rings; report state changes.
pub(crate) fn pump_conns(st: &mut State) {
    let generation = st.generation;
    let mut buf = [0u8; 4096];
    let net = st.net.as_mut();
    let mut net = net;
    st.conns.retain_mut(|c| {
        let alive = c.generation == generation && net.is_some();
        if c.sock.close_requested() {
            // Graceful close: deliver what the client wrote, then FIN; free
            // the socket once it's closed (or after LINGER_MS).
            let now = time::now_ms();
            let Some(n) = net.as_mut().filter(|_| alive) else {
                let _ = sys::cap_drop(c.mem);
                let _ = sys::cap_drop(c.client);
                return false;
            };
            let s = n.sockets.get_mut::<tcp::Socket>(c.handle);
            send_pending(s, &c.sock, &mut buf);
            if c.closing_since.is_none() {
                c.closing_since = Some(now);
                let _ = sys::cap_drop(c.client);
            }
            if c.sock.tx.is_empty() {
                s.close();
            }
            let done = !s.is_open() || now - c.closing_since.unwrap() > LINGER_MS;
            if done {
                s.abort();
                n.sockets.remove(c.handle);
                let _ = sys::cap_drop(c.mem);
                return false;
            }
            return true;
        }
        let mut moved = false;
        let new_state = match net.as_mut().filter(|_| alive) {
            None => state::FAILED,
            Some(n) => {
                let s = n.sockets.get_mut::<tcp::Socket>(c.handle);
                moved |= send_pending(s, &c.sock, &mut buf);
                while s.can_recv() && c.sock.rx.free() > 0 {
                    let room = c.sock.rx.free().min(buf.len());
                    let Ok(k) = s.recv_slice(&mut buf[..room]) else {
                        break;
                    };
                    if k == 0 {
                        break;
                    }
                    c.sock.rx.write(&buf[..k]);
                    moved = true;
                }
                match s.state() {
                    tcp::State::SynSent | tcp::State::SynReceived => state::CONNECTING,
                    tcp::State::Established => state::ESTABLISHED,
                    tcp::State::CloseWait | tcp::State::LastAck | tcp::State::TimeWait
                        if !s.can_recv() =>
                    {
                        state::PEER_CLOSED
                    }
                    tcp::State::Closed if c.reported == state::CONNECTING => state::FAILED,
                    tcp::State::Closed => state::PEER_CLOSED,
                    _ => state::ESTABLISHED,
                }
            }
        };
        if new_state != c.reported {
            c.sock.set_state(new_state);
            c.reported = new_state;
            moved = true;
        }
        if moved && sys::signal(c.client, c.client_bits) == Err(Error::Dead) {
            // The client is gone (crashed, or restarted): nobody will read
            // or close this connection.
            if let Some(n) = net.as_mut().filter(|_| alive) {
                n.sockets.get_mut::<tcp::Socket>(c.handle).abort();
                n.sockets.remove(c.handle);
            }
            let _ = sys::cap_drop(c.mem);
            let _ = sys::cap_drop(c.client);
            return false;
        }
        true
    });
}

/// Keep `backlog` sockets listening on each port; wake waiting ACCEPTs.
pub(crate) fn pump_listeners(st: &mut State) {
    let generation = st.generation;
    let Some(net) = st.net.as_mut() else { return };
    for l in &mut st.listeners {
        if l.generation != generation {
            l.waiting.clear();
            l.generation = generation;
        }
        // Drop sockets that died before being accepted.
        l.waiting.retain(|&h| {
            let s = net.sockets.get::<tcp::Socket>(h);
            let ok = s.is_open();
            if !ok {
                net.sockets.remove(h);
            }
            ok
        });
        // Keep `backlog` sockets listening (a browser opens several
        // connections at once; a SYN that finds no listening socket gets a
        // reset), and at most twice that many not yet accepted.
        let mut listening = l
            .waiting
            .iter()
            .filter(|&&h| net.sockets.get::<tcp::Socket>(h).state() == tcp::State::Listen)
            .count();
        while listening < l.backlog && l.waiting.len() < 2 * l.backlog {
            let mut s = tcp_socket();
            if s.listen(l.port).is_err() {
                break;
            }
            l.waiting.push(net.sockets.add(s));
            listening += 1;
        }
        let connected = l
            .waiting
            .iter()
            .any(|&h| net.sockets.get::<tcp::Socket>(h).state() == tcp::State::Established);
        if connected && let Some((n, bits)) = l.notify.take() {
            let _ = sys::signal(n, bits);
            let _ = sys::cap_drop(n);
        }
    }
}

/// Move what the client wrote into the socket, as far as the socket takes
/// it. True if anything moved.
fn send_pending(s: &mut tcp::Socket, sock: &Sock, buf: &mut [u8]) -> bool {
    let mut moved = false;
    while s.can_send() && !sock.tx.is_empty() {
        let room = s.send_capacity().saturating_sub(s.send_queue()).min(buf.len());
        let k = sock.tx.read(&mut buf[..room]);
        if k == 0 {
            break;
        }
        let _ = s.send_slice(&buf[..k]);
        moved = true;
    }
    moved
}
